use std::collections::VecDeque;
use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex};
use colored::Colorize;
use dialoguer::console::{Key, Term};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use harness_core::{
    Agent, AllowAll, ApprovalRequest, Config, DefaultToolRegistry, HarnessError, Message,
    ModelProvider, ReasoningEffort, Role, SessionBinding, SessionStore, SqliteSessionStore,
    ToolRegistry, TurnOutcome,
};
use harness_evolver::{
    apply_update_at_startup, CheckImprovementTool, CodebaseEvolver, DelegateImprovementTool,
    InspectSourceTool, ListImprovementJobsTool, PtyScreenCaptureTool, VerifyAndStageUpdateTool,
};
use harness_memory::{
    ForgetMemoryTool, SaveMemoryTool, SqliteMemoryEngine, UpdateMemoryTool,
};
use harness_notes::{
    DeleteNoteTool, ReadNoteTool, SaveNoteTool, SearchNotesTool, SqliteNotesEngine, UpdateNoteTool,
};
use harness_providers::{
    get_default_model, get_provider_catalog, get_context_window, CredentialManager, GenericProviderClient,
    ModelDiscovery,
};

use super::approval::{
    approval_settings, build_approver, render_approval_box, ApprovalChoice, ChatApprover,
    APPROVAL_ACTIONS,
};
use super::sessions::{open_store, resolve_session, SessionSelection};
use tokio::sync::oneshot;

/// Holds the terminal in raw mode for the whole interactive session, restoring the original
/// settings when dropped.
///
/// `console`'s key reader only switches raw mode on for the duration of a single read, so
/// between keystrokes the tty sits in canonical mode and the *kernel* echoes what is typed.
/// A pasted or fast-typed burst is echoed before the app can read it, and the echo wraps onto
/// extra rows that the single-row composer redraw cannot erase — the screen fills with copies
/// of the input. Owning raw mode for the session keeps echoing where it belongs: in the UI.
struct RawTerminal {
    fd: libc::c_int,
    original: libc::termios,
}

impl RawTerminal {
    /// Returns `None` when stdin is not a terminal, or when termios is unavailable.
    fn acquire() -> Option<Self> {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        let fd = libc::STDIN_FILENO;
        let mut current = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, current.as_mut_ptr()) } != 0 {
            return None;
        }
        let original = unsafe { current.assume_init() };
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        // Keep output post-processing so `\n` still returns the carriage, and keep ISIG so
        // Ctrl-C stays a signal — the CLI routes it into the key channel itself.
        raw.c_oflag = original.c_oflag;
        raw.c_lflag |= libc::ISIG;
        if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &raw) } != 0 {
            return None;
        }
        Some(Self { fd, original })
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSADRAIN, &self.original);
        }
    }
}

/// `config` is already resolved (defaults → file → `LUNA_*` → flags); this function only reads it,
/// so the engines/filesystem paths come from one place instead of each engine guessing `~/.luna/…`.
fn get_terminal_width() -> usize {
    Term::stdout()
        .size_checked()
        .map(|(_, w)| w as usize)
        .filter(|&w| w > 0)
        .unwrap_or(80)
}

/// Interactive chat: the composer, the transcript, the live region above it, and the pickers.
/// Everything pure — how a call, a result or a reasoning row is worded — lives in [`render`].
mod render;

use render::{
    activity_line, format_approval_verdict, format_tool_call, format_tool_result, needs_separator,
    parse_turns_from_history, reasoning_rows, render_all_turns, right_aligned_user_message,
    tool_activity_detail, OutputBlockKind, TurnProcessItem, TurnRecord, LUNA_LABEL, PROMPT,
};

/// Rows of reasoning kept in the live region before the oldest roll into the transcript. Keeping
/// a shallow tail is what makes long reasoning stream smoothly instead of scrolling the screen.
const LIVE_REASONING_ROWS: usize = 4;

/// How often the live region advances a spinner frame and refreshes its elapsed time.
const ANIMATION_INTERVAL: std::time::Duration = std::time::Duration::from_millis(80);

/// A tool call currently in flight, as far as the animation is concerned.
#[derive(Clone)]
struct Activity {
    /// What the tool is doing, when the call itself does not already say it.
    detail: Option<String>,
    started: std::time::Instant,
    frame: usize,
}

/// The in-place region between the transcript and the composer: the tail of the reasoning as it
/// streams in, and the animated line for a running tool. It is rewritten rather than appended to,
/// so a long turn does not flood the transcript with partial lines.
#[derive(Default)]
struct LiveRegion {
    /// Reasoning accumulated for the current block and not yet settled into the transcript.
    reasoning: String,
    /// The running tool, if any.
    activity: Option<Activity>,
    /// Rows currently on screen, so the next frame can erase exactly this many.
    rows: Vec<String>,
}

#[derive(Clone)]
struct ChatStatus {
    model: String,
    context_used: u32,
    context_max: u32,
}

#[derive(Clone)]
struct ChatUi {
    input_buf: Arc<Mutex<String>>,
    status: Arc<Mutex<Option<ChatStatus>>>,
    /// The in-place region above the composer: streamed reasoning and the running tool.
    live: Arc<Mutex<LiveRegion>>,
    /// The kind of the last transcript block, so blank lines land only between differing blocks.
    last_block: Arc<Mutex<OutputBlockKind>>,
    is_attended: bool,
}

impl ChatUi {
    fn new(is_attended: bool) -> Self {
        Self {
            input_buf: Arc::new(Mutex::new(String::new())),
            status: Arc::new(Mutex::new(None)),
            live: Arc::new(Mutex::new(LiveRegion::default())),
            last_block: Arc::new(Mutex::new(OutputBlockKind::None)),
            is_attended,
        }
    }

    /// Open a transcript block, writing the blank line that separates it from a block of a
    /// different kind. Returns nothing to say — the separator is the whole effect.
    fn begin_block(&self, kind: OutputBlockKind) {
        let mut last = self.last_block.lock().unwrap();
        if needs_separator(*last, kind) {
            self.emit_above("");
        }
        *last = kind;
    }

    /// Feed one reasoning increment. It lands in the live region rather than the transcript, so
    /// the block grows in place while the model is still writing it; the animation tick settles
    /// rows that scroll past the tail.
    fn push_reasoning(&self, text: &str) {
        self.live.lock().unwrap().reasoning.push_str(text);
    }

    /// Close the reasoning block: everything still in the live region is written to the
    /// transcript, so the block reads in order once a tool call or the reply follows it.
    fn finish_reasoning(&self) {
        let rows = {
            let mut live = self.live.lock().unwrap();
            if live.reasoning.is_empty() {
                return;
            }
            let text = std::mem::take(&mut live.reasoning);
            reasoning_rows(&text, get_terminal_width())
        };
        if !rows.is_empty() {
            self.begin_block(OutputBlockKind::Thinking);
            self.emit_above(&rows.join("\n"));
        }
    }

    /// Start the animated line for a tool that is now running.
    fn start_activity(&self, name: &str, arguments: &serde_json::Value) {
        let mut live = self.live.lock().unwrap();
        live.activity = Some(Activity {
            detail: tool_activity_detail(name, arguments),
            started: std::time::Instant::now(),
            frame: 0,
        });
    }

    /// Stop the animated line; the result line that follows takes its place.
    fn stop_activity(&self) {
        self.live.lock().unwrap().activity = None;
    }

    /// Erase the live region and the composer box cleanly, e.g. before an approval prompt.
    fn clear_live_and_box(&self) {
        self.clear_box_on_exit();
    }

    fn set_status(&self, model: &str, context_used: u32, context_max: u32) {
        let mut status = self.status.lock().unwrap();
        *status = Some(ChatStatus {
            model: model.to_string(),
            context_used,
            context_max,
        });
    }

    fn set_model(&self, model: &str, context_used: u32, context_max: u32) {
        let mut status = self.status.lock().unwrap();
        if let Some(ref mut s) = *status {
            s.model = model.to_string();
            s.context_used = context_used;
            s.context_max = context_max;
        }
    }

    fn update_status(&self, context_used: u32, context_max: u32) {
        {
            let mut status = self.status.lock().unwrap();
            if let Some(ref mut s) = *status {
                s.context_used = context_used;
                s.context_max = context_max;
            }
        }
        self.redraw_status_line();
    }

    /// Status line above the input box: model name on the left, context usage on the right.
    fn format_status_line(&self, width: usize) -> Option<String> {
        let status = self.status.lock().unwrap();
        let s = status.as_ref()?;

        let left = s.model.clone();
        let right = format!("ctx: {}/{}", s.context_used, s.context_max);

        let left_len = left.chars().count();
        let right_len = right.chars().count();

        if left_len + right_len + 4 > width {
            let available = width.saturating_sub(right_len + 4);
            let left_truncated = if left_len > available {
                let cut = available.saturating_sub(1);
                format!("{}…", left.chars().take(cut).collect::<String>())
            } else {
                left
            };
            let left_len = left_truncated.chars().count();
            let spaces = width.saturating_sub(left_len + right_len);
            Some(format!(
                "{}{}{}",
                left_truncated.dimmed(),
                " ".repeat(spaces),
                right.dimmed()
            ))
        } else {
            let spaces = width - left_len - right_len;
            Some(format!(
                "{}{}{}",
                left.dimmed(),
                " ".repeat(spaces),
                right.dimmed()
            ))
        }
    }

    /// Erase the four composer rows plus any live region rows, leaving the cursor at the top row
    /// where the live region started.
    fn erase_live_and_box(&self, previous_live_rows: usize) {
        if !self.is_attended {
            return;
        }
        let total = previous_live_rows + 4;
        print!("\x1b[1B");
        for i in 0..total {
            print!("\r\x1b[2K");
            if i + 1 < total {
                print!("\x1b[1A");
            }
        }
    }

    /// Write transcript text above the live region and the composer.
    fn emit_above(&self, text: &str) {
        if !self.is_attended {
            println!("{}", text);
            return;
        }
        let buf = self.input_buf.lock().unwrap();
        let width = get_terminal_width();
        let previous = {
            let mut live = self.live.lock().unwrap();
            let prev = live.rows.len();
            live.rows.clear();
            prev
        };
        self.erase_live_and_box(previous);
        println!("{}", text);
        self.draw_box(width, &buf);
    }

    /// Replace the live region with `rows`, erasing the previous frame in place. This is the only
    /// path that draws the region, so its height can never drift from what is on screen.
    fn set_live(&self, rows: Vec<String>) {
        if !self.is_attended {
            for row in &rows {
                println!("{}", row);
            }
            return;
        }
        let buf = self.input_buf.lock().unwrap();
        let width = get_terminal_width();
        let previous = {
            let mut live = self.live.lock().unwrap();
            let prev = live.rows.len();
            live.rows = rows.clone();
            prev
        };
        self.erase_live_and_box(previous);
        for row in &rows {
            println!("{}", row);
        }
        self.draw_box(width, &buf);
    }

    /// Advance the live region one frame: refresh the elapsed time, step the spinner, and show
    /// the tail of the streaming reasoning.
    fn animate_tick(&self) {
        if !self.is_attended {
            return;
        }
        let width = get_terminal_width();

        let rows = {
            let mut live = self.live.lock().unwrap();
            let all = if live.reasoning.is_empty() {
                Vec::new()
            } else {
                reasoning_rows(&live.reasoning, width)
            };
            let start = all.len().saturating_sub(LIVE_REASONING_ROWS);
            let mut rows = all[start..].to_vec();

            if let Some(activity) = &mut live.activity {
                activity.frame = activity.frame.wrapping_add(1);
                rows.push(activity_line(
                    activity.frame,
                    activity.detail.as_deref(),
                    activity.started.elapsed(),
                ));
            }
            rows
        };

        if rows.is_empty() && self.live.lock().unwrap().rows.is_empty() {
            return;
        }

        self.set_live(rows);
    }

    /// Draw the composer: status line, divider, prompt row, divider.
    fn draw_box(&self, width: usize, buf: &str) {
        let divider = "─".repeat(width).dimmed().to_string();
        if let Some(status_str) = self.format_status_line(width) {
            println!("{}", status_str);
        }
        println!("{}", divider);
        println!();
        println!("{}", divider);
        print!("\x1b[2A\r{}", self.prompt_row(buf));
        let _ = io::stdout().flush();
    }

    /// The tail of the composer buffer that fits on one terminal row, prefixed with `…` when
    /// the front is scrolled off. Without this the rendered line wraps onto a second physical
    /// row while every redraw clears only one — each keystroke then repaints the line and the
    /// screen fills with copies of the input.
    fn visible_input(&self, buf: &str) -> String {
        let room = get_terminal_width()
            .saturating_sub(PROMPT.chars().count())
            .saturating_sub(1);
        let len = buf.chars().count();
        if len <= room {
            return buf.to_string();
        }
        let keep = room.saturating_sub(1);
        format!("…{}", buf.chars().skip(len - keep).collect::<String>())
    }

    /// The composer row: prompt plus the visible tail of the buffer.
    fn prompt_row(&self, buf: &str) -> String {
        format!("{}{}", PROMPT.bold().yellow(), self.visible_input(buf))
    }

    fn redraw_input_line(&self) {
        if !self.is_attended {
            return;
        }
        let buf = self.input_buf.lock().unwrap();
        print!("\r\x1b[K{}", self.prompt_row(&buf));
        let _ = io::stdout().flush();
    }

    fn draw_initial_box(&self) {
        if !self.is_attended {
            print!("{}", PROMPT.bold().yellow());
            let _ = io::stdout().flush();
            return;
        }
        let width = get_terminal_width();
        let buf = self.input_buf.lock().unwrap();
        self.draw_box(width, &buf);
    }

    fn redraw_status_line(&self) {
        if !self.is_attended {
            return;
        }
        let width = get_terminal_width();
        if let Some(status_str) = self.format_status_line(width) {
            let buf = self.input_buf.lock().unwrap();
            print!("\x1b[2A\r\x1b[2K{}\x1b[2B\r{}", status_str, self.prompt_row(&buf));
            let _ = io::stdout().flush();
        }
    }

    fn clear_box_on_exit(&self) {
        if !self.is_attended {
            return;
        }
        let previous = {
            let mut live = self.live.lock().unwrap();
            let prev = live.rows.len();
            live.rows.clear();
            live.activity = None;
            prev
        };
        self.erase_live_and_box(previous);
        let _ = io::stdout().flush();
    }
}

type ChatAgent = Agent<
    GenericProviderClient,
    SqliteMemoryEngine,
    SqliteNotesEngine,
    CodebaseEvolver,
    DefaultToolRegistry,
>;

/// Wipe the terminal on session exit, so only the shell prompt remains.
/// Cosmetic: a failure to write is ignored on purpose (see `commands::clear_screen`).
fn clear_on_exit() {
    crate::commands::clear_screen();
}

pub async fn run_interactive_chat(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    selection: SessionSelection,
) -> Result<(), HarnessError> {
    let mut current_provider = config.model.provider.clone();
    let mut current_model = config.model.default.clone();

    if current_provider.is_empty() {
        return Err(HarnessError::ConfigurationError(
            "No LLM provider configured or loaded with credentials. Run 'luna auth' to register a credential or specify '--provider <name>'.".to_string(),
        ));
    }

    let provider = Arc::new(GenericProviderClient::new(
        current_provider.clone(),
        current_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&current_provider),
    )?);

    // Every turn is appended to this session; a resumed one replays its recent transcript.
    let store = Arc::new(open_store(&config)?);
    let mut session_id = resolve_session(store.as_ref(), selection).await?;

    // The compaction call is a plain model call: one provider, one model, resolved from
    // `agent.compaction_provider` / `agent.compaction_model` (both default to the session's).
    let compaction_provider_id = config
        .agent
        .compaction_provider
        .clone()
        .unwrap_or_else(|| current_provider.clone());
    let compaction_model = config
        .agent
        .compaction_model
        .clone()
        .unwrap_or_else(|| current_model.clone());
    let compaction = match GenericProviderClient::new(
        compaction_provider_id.clone(),
        compaction_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&compaction_provider_id),
    ) {
        Ok(client) => {
            let compaction_provider: Arc<dyn ModelProvider> = Arc::new(client);
            Some(harness_core::CompactionSetup {
                provider: compaction_provider,
                model: compaction_model,
            })
        }
        // An unusable compaction provider is a warning, not a startup failure: the session still
        // runs, and the turn that cannot compact says so.
        Err(error) => {
            eprintln!(
                "warning: context compaction is unavailable ({}): {}",
                compaction_provider_id, error
            );
            None
        }
    };

    let memory = Arc::new(SqliteMemoryEngine::new(Some(config.paths.memory_db.clone()))?);
    let notes = Arc::new(SqliteNotesEngine::new(Some(config.paths.notes_db.clone()))?);
    let evolver = Arc::new(
        CodebaseEvolver::new(
            config.paths.workspace_root.clone(),
            config.paths.jobs_dir.clone(),
        )
        .with_provider(provider.clone()),
    );

    let registry = Arc::new(DefaultToolRegistry::new());

    // Register native memory tools
    registry.register(Arc::new(SaveMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(UpdateMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(ForgetMemoryTool::new(memory.clone()))).await?;

    // Register native note tools
    registry.register(Arc::new(SaveNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(SearchNotesTool::new(notes.clone()))).await?;
    registry.register(Arc::new(ReadNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(UpdateNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(DeleteNoteTool::new(notes.clone()))).await?;

    // Register self-improvement evolver tools
    registry.register(Arc::new(InspectSourceTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(DelegateImprovementTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(ListImprovementJobsTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(CheckImprovementTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(VerifyAndStageUpdateTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(PtyScreenCaptureTool::new(config.paths.workspace_root.clone()))).await?;

    // Register skills from skills_dir
    let _ = std::fs::create_dir_all(&config.paths.skills_dir);
    if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&config.paths.skills_dir) {
        for skill in skills {
            registry
                .register(Arc::new(harness_plugins::SkillToolAdapter::new(skill)))
                .await?;
        }
    }

    // Register core filesystem and shell tools (§2.2)
    let read_cache = harness_core::tools::ReadCache::new();
    let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
    harness_core::tools::register_core_tools(&reg_dyn, &config, read_cache).await?;

    let is_attended = Term::stdout().features().is_attended();
    let ui = Arc::new(ChatUi::new(is_attended));
    let ui_obs = ui.clone();

    let (approval_tx, mut approval_rx) =
        mpsc::unbounded_channel::<(ApprovalRequest, oneshot::Sender<harness_core::ApprovalDecision>)>();

    let approver: Arc<dyn harness_core::Approver> =
        if is_attended && config.tools.approval == harness_core::ApprovalMode::Ask {
            Arc::new(ChatApprover::new(approval_tx))
        } else {
            build_approver(&config)
        };

    let agent = Agent::new(
        provider,
        memory,
        notes,
        evolver.clone(),
        registry,
        config.agent.clone(),
    )
    .with_approval(approver, approval_settings(&config))
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: session_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let in_flight_tools: Arc<Mutex<Vec<TurnProcessItem>>> = Arc::new(Mutex::new(Vec::new()));
    let in_flight_tools_obs = in_flight_tools.clone();

    let mut agent = agent.with_observer(Arc::new(move |event| match event {
        // Reasoning lands in the live region and is settled into the transcript by the animation
        // tick, so it is visible while it is still being produced.
        harness_core::AgentEvent::ReasoningDelta(text) => ui_obs.push_reasoning(&text),
        harness_core::AgentEvent::ToolExecuting { name, arguments } => {
            ui_obs.finish_reasoning();
            let call_fmt = format_tool_call(&name, &arguments);
            ui_obs.begin_block(OutputBlockKind::Tool);
            ui_obs.emit_above(&call_fmt);
            ui_obs.start_activity(&name, &arguments);
            in_flight_tools_obs.lock().unwrap().push(TurnProcessItem {
                call_formatted: call_fmt,
                result_formatted: String::new(),
            });
        }
        harness_core::AgentEvent::ToolExecuted { is_error, output, .. } => {
            ui_obs.stop_activity();
            let res_fmt = format_tool_result(is_error, &output);
            ui_obs.emit_above(&res_fmt);
            if let Some(last) = in_flight_tools_obs.lock().unwrap().last_mut() {
                last.result_formatted = res_fmt;
            }
        }
    }));
    if let Some(setup) = compaction {
        agent = agent.with_compaction(setup);
    }

    let agent = Arc::new(tokio::sync::Mutex::new(agent));

    if !is_attended {
        let cancel_token = CancellationToken::new();
        loop {
            print!("{}", PROMPT.bold().yellow());
            io::stdout().flush().map_err(|e| HarnessError::Io(e.to_string()))?;

            let mut input = String::new();
            let read = io::stdin().read_line(&mut input).map_err(|e| HarnessError::Io(e.to_string()))?;
            if read == 0 {
                break;
            }
            let query = input.trim();
            if query.eq_ignore_ascii_case("exit") || query.eq_ignore_ascii_case("quit") {
                break;
            }
            if query.is_empty() {
                continue;
            }
            if query.eq_ignore_ascii_case("/model")
                || query.eq_ignore_ascii_case("/models")
                || query.eq_ignore_ascii_case("/provider")
            {
                handle_model_switch(
                    None,
                    &ui,
                    &config,
                    &cred_manager,
                    &discovery,
                    &agent,
                    &mut current_provider,
                    &mut current_model,
                )
                .await?;
                continue;
            }
            if query == "/" || query.eq_ignore_ascii_case("/help") {
                for cmd in SLASH_COMMANDS {
                    println!("  {:<12} {}", cmd.name.cyan(), cmd.details.dimmed());
                }
                continue;
            }
            if query.starts_with("/reasoning") {
                let arg = query.strip_prefix("/reasoning").unwrap().trim();
                let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                handle_reasoning_switch(None, &ui, &agent, arg_opt).await?;
                continue;
            }
            if query.starts_with("/name") {
                let arg = query.strip_prefix("/name").unwrap().trim();
                let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                handle_name_command(&ui, store.as_ref(), &session_id, arg_opt).await?;
                continue;
            }
            if query.starts_with("/summarize") || query.starts_with("/compact") {
                let prefix = if query.starts_with("/summarize") {
                    "/summarize"
                } else {
                    "/compact"
                };
                let arg = query.strip_prefix(prefix).unwrap().trim();
                let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                handle_summarize_and_new_command(
                    &ui,
                    &store,
                    &config,
                    &agent,
                    &mut session_id,
                    &current_provider,
                    &current_model,
                    arg_opt,
                )
                .await?;
                continue;
            }
            if query.starts_with("/new") {
                let arg = query.strip_prefix("/new").unwrap().trim();
                let (is_summarize, remaining_arg) = if arg.starts_with("-s ") || arg == "-s" {
                    (true, arg.strip_prefix("-s").unwrap().trim())
                } else if arg.starts_with("--summarize ") || arg == "--summarize" {
                    (true, arg.strip_prefix("--summarize").unwrap().trim())
                } else if arg.starts_with("--summary ") || arg == "--summary" {
                    (true, arg.strip_prefix("--summary").unwrap().trim())
                } else {
                    (false, arg)
                };
                let arg_opt = if remaining_arg.is_empty() { None } else { Some(remaining_arg) };
                if is_summarize {
                    handle_summarize_and_new_command(
                        &ui,
                        &store,
                        &config,
                        &agent,
                        &mut session_id,
                        &current_provider,
                        &current_model,
                        arg_opt,
                    )
                    .await?;
                } else {
                    handle_new_command(
                        &ui,
                        &store,
                        &config,
                        &agent,
                        &mut session_id,
                        &current_provider,
                        &current_model,
                        arg_opt,
                    )
                    .await?;
                }
                continue;
            }
            let ag_ref = agent.clone();
            let q = query.to_string();
            let cancel_child = cancel_token.child_token();
            let res = {
                let ag = ag_ref.lock().await;
                ag.run_turn(&q, cancel_child).await
            };
            match res {
                Ok(outcome) => {
                    println!("\n{}\n{}\n", LUNA_LABEL.bold().green(), outcome.reply);
                }
                Err(e) => {
                    eprintln!("\n{} Error: {}\n", "✖".red().bold(), e);
                }
            }
        }
        return Ok(());
    }

    // Check for pending self-improvement update before clearing the screen
    if is_attended {
        if let Some(pending) = evolver.get_pending_update() {
            print!(
                "An improved version of Luna is ready (job {}: {}). Apply now? [Y/n] ",
                pending.job_id, pending.task
            );
            let _ = io::stdout().flush();
            let mut answer = String::new();
            if io::stdin().read_line(&mut answer).is_ok() {
                let trimmed = answer.trim();
                if trimmed.is_empty()
                    || trimmed.eq_ignore_ascii_case("y")
                    || trimmed.eq_ignore_ascii_case("yes")
                {
                    match apply_update_at_startup(
                        evolver.repo_root(),
                        &config.paths.jobs_dir,
                    ) {
                        Ok(Some(msg)) => {
                            println!("{}", msg.green());
                            std::thread::sleep(std::time::Duration::from_millis(800));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            eprintln!("{} Failed to apply update: {}", "✖".red(), e);
                            std::thread::sleep(std::time::Duration::from_millis(1200));
                        }
                    }
                } else {
                    println!("Update deferred.");
                }
            }
        }
    }

    // Start from a clean screen: launching the REPL shouldn't leave whatever the shell was
    // showing above the input box. Attended-only by construction — the non-attended path
    // returned above and `clear_screen` no-ops on a piped stdout anyway.
    crate::commands::clear_screen();

    // Print the stored transcript before the box so a resumed session reopens with its history
    // on screen — without it a continuation is indistinguishable from a fresh session until the
    // first reply lands.
    let replayed = store
        .replay(&session_id, config.session.resume_max_messages)
        .await?;
    let mut turns: Vec<TurnRecord> = parse_turns_from_history(&replayed.history);
    let mut process_expanded = false;

    if replayed.replayed > 0 {
        let width = get_terminal_width();
        if replayed.truncated() {
            println!(
                "{}\n",
                format!(
                    "… {} earlier messages not shown (last {} resumed)",
                    replayed.total - replayed.replayed,
                    replayed.replayed
                )
                .dimmed()
            );
        }
        render_all_turns(&turns, process_expanded, width);
        println!();
    }

    let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
    // Seed the bar from the real transcript, not 0: a resumed session already carries history, and
    // reporting "0 used" made the continuation look like a fresh start.
    let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
    ui.set_status(&current_model, context_used, context_max);
    ui.draw_initial_box();

    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<Key>();

    // Raw mode belongs to the session, not to each read — see `RawTerminal`. Held here so the
    // guard drops (and the terminal is restored) when this function returns on any exit path.
    let _raw_terminal = RawTerminal::acquire();

    // Ctrl-C stays a signal because raw mode is entered with ISIG left on. Left to its default
    // action it would kill the process before the quit path runs and the resume hint never
    // prints; funnelling the signal into the key channel gives it the same path as `exit`.
    {
        let key_tx = key_tx.clone();
        tokio::spawn(async move {
            while tokio::signal::ctrl_c().await.is_ok() {
                if key_tx.send(Key::CtrlC).is_err() {
                    break;
                }
            }
        });
    }

    std::thread::spawn(move || {
        let term = Term::stdout();
        loop {
            match term.read_key() {
                Ok(key) => {
                    if key_tx.send(key).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    // SIGINT or other signal interrupted read syscall - do not terminate reader thread!
                    continue;
                }
                Err(_) => {
                    if key_tx.is_closed() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    });

    let mut queue: VecDeque<String> = VecDeque::new();
    let mut current_cancel = CancellationToken::new();
    let mut active_turn: Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>> = None;
    let mut current_query = String::new();
    let mut turn_start = std::time::Instant::now();

    // Drives the live region: streamed reasoning and the running-tool spinner advance on this
    // tick rather than on every delta, so a fast token stream cannot outrun the redraw.
    let mut animation = tokio::time::interval(ANIMATION_INTERVAL);
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = animation.tick() => {
                ui.animate_tick();
            }

            maybe_approval = approval_rx.recv() => {
                let (request, decision_tx) = match maybe_approval {
                    Some(pair) => pair,
                    None => continue,
                };

                ui.clear_live_and_box();
                let choice = prompt_approval_choice(&mut key_rx, &request, &ui).await;
                if matches!(choice, ApprovalChoice::AllSession) {
                    // Sticky "approve all": flip the gate once and it stays for this chat.
                    agent.lock().await.set_approval(Arc::new(AllowAll));
                }
                let is_run = matches!(choice, ApprovalChoice::Once | ApprovalChoice::AllSession);
                let decision = if is_run {
                    harness_core::ApprovalDecision::Run
                } else {
                    harness_core::ApprovalDecision::Deny
                };
                let verdict_str = format_approval_verdict(is_run, &request);
                ui.emit_above(&verdict_str);
                in_flight_tools.lock().unwrap().push(TurnProcessItem {
                    call_formatted: verdict_str,
                    result_formatted: String::new(),
                });
                if is_run {
                    let args = request.arguments.clone();
                    ui.start_activity(&request.tool, &args);
                }
                let _ = decision_tx.send(decision);
            }

            maybe_key = key_rx.recv() => {
                let key = match maybe_key {
                    Some(k) => k,
                    None => break,
                };

                match key {
                    Key::Char('\x0f') => {
                        // Ctrl+O: toggle process expansion
                        if active_turn.is_none() && !turns.is_empty() {
                            process_expanded = !process_expanded;
                            if ui.is_attended {
                                crate::commands::clear_screen();
                                render_all_turns(&turns, process_expanded, get_terminal_width());
                                ui.draw_initial_box();
                            }
                        }
                        continue;
                    }
                    Key::Char('/') if ui.input_buf.lock().unwrap().is_empty() => {
                        ui.input_buf.lock().unwrap().push('/');
                        ui.redraw_input_line();
                        if let Some(cmd) = select_slash_command(&mut key_rx, &ui).await {
                            ui.input_buf.lock().unwrap().clear();
                            ui.redraw_input_line();
                            if execute_slash_command(
                                &cmd,
                                &mut key_rx,
                                &ui,
                                &config,
                                &cred_manager,
                                &discovery,
                                &agent,
                                &store,
                                &mut session_id,
                                &mut current_provider,
                                &mut current_model,
                                &mut turns,
                                &mut process_expanded,
                            )
                            .await?
                            {
                                break;
                            }
                        }
                    }
                    Key::Char(c) => {
                        ui.input_buf.lock().unwrap().push(c);
                        ui.redraw_input_line();
                    }
                    Key::Backspace => {
                        ui.input_buf.lock().unwrap().pop();
                        ui.redraw_input_line();
                    }
                    Key::Enter => {
                        let query = {
                            let mut b = ui.input_buf.lock().unwrap();
                            let s = b.trim().to_string();
                            b.clear();
                            s
                        };

                        if query.is_empty() {
                            ui.redraw_input_line();
                            continue;
                        }

                        if query == "/" || query.eq_ignore_ascii_case("/help") {
                            if let Some(cmd) = select_slash_command(&mut key_rx, &ui).await {
                                ui.input_buf.lock().unwrap().clear();
                                ui.redraw_input_line();
                                if execute_slash_command(
                                    &cmd,
                                    &mut key_rx,
                                    &ui,
                                    &config,
                                    &cred_manager,
                                    &discovery,
                                    &agent,
                                    &store,
                                    &mut session_id,
                                    &mut current_provider,
                                    &mut current_model,
                                    &mut turns,
                                    &mut process_expanded,
                                )
                                .await?
                                {
                                    break;
                                }
                            }
                            continue;
                        }

                        if query.starts_with('/')
                            || query.eq_ignore_ascii_case("exit")
                            || query.eq_ignore_ascii_case("quit")
                        {
                            if execute_slash_command(
                                &query,
                                &mut key_rx,
                                &ui,
                                &config,
                                &cred_manager,
                                &discovery,
                                &agent,
                                &store,
                                &mut session_id,
                                &mut current_provider,
                                &mut current_model,
                                &mut turns,
                                &mut process_expanded,
                            )
                            .await?
                            {
                                break;
                            }
                            continue;
                        }

                        ui.begin_block(OutputBlockKind::User);
                        ui.emit_above(&right_aligned_user_message(&query, get_terminal_width()));

                        if active_turn.is_some() {
                            queue.push_back(query);
                        } else {
                            current_cancel = CancellationToken::new();
                            let cancel_child = current_cancel.child_token();
                            let ag_ref = agent.clone();
                            let q = query.clone();
                            current_query = query.clone();
                            turn_start = std::time::Instant::now();
                            in_flight_tools.lock().unwrap().clear();
                            active_turn = Some(tokio::spawn(async move {
                                let ag = ag_ref.lock().await;
                                ag.run_turn(&q, cancel_child).await
                            }));
                        }
                    }
                    Key::CtrlC => {
                        if let Some(handle) = active_turn.take() {
                            current_cancel.cancel();
                            handle.abort();
                            ui.stop_activity();
                            let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
                            let turn_dur = turn_start.elapsed();

                            let record = TurnRecord {
                                user_query: current_query.clone(),
                                reasoning: None,
                                tools: Vec::new(),
                                tool_items: current_tools,
                                duration: turn_dur,
                                reply: "✖ Cancelled".to_string(),
                            };
                            turns.push(record);

                            if ui.is_attended {
                                crate::commands::clear_screen();
                                render_all_turns(&turns, process_expanded, get_terminal_width());
                                ui.draw_initial_box();
                            } else {
                                println!("{}", "✖ Cancelled".red());
                            }

                            let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                            let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                            ui.update_status(context_used, context_max);
                        } else {
                            ui.clear_box_on_exit();
                            clear_on_exit();
                            break;
                        }
                    }
                    Key::Escape => {
                        if let Some(handle) = active_turn.take() {
                            current_cancel.cancel();
                            handle.abort();
                            ui.stop_activity();
                            let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
                            let turn_dur = turn_start.elapsed();

                            let record = TurnRecord {
                                user_query: current_query.clone(),
                                reasoning: None,
                                tools: Vec::new(),
                                tool_items: current_tools,
                                duration: turn_dur,
                                reply: "✖ Cancelled".to_string(),
                            };
                            turns.push(record);

                            if ui.is_attended {
                                crate::commands::clear_screen();
                                render_all_turns(&turns, process_expanded, get_terminal_width());
                                ui.draw_initial_box();
                            } else {
                                println!("{}", "✖ Cancelled".red());
                            }

                            let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                            let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                            ui.update_status(context_used, context_max);
                        }
                    }
                    _ => {}
                }
            }

            turn_res = async {
                if let Some(ref mut handle) = active_turn {
                    handle.await
                } else {
                    std::future::pending().await
                }
            } => {
                active_turn = None;
                match turn_res {
                    Ok(Ok(outcome)) => {
                        ui.stop_activity();
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
                        let turn_dur = turn_start.elapsed();

                        let record = TurnRecord {
                            user_query: current_query.clone(),
                            reasoning: outcome.reasoning.clone(),
                            tools: outcome.tool_calls.clone(),
                            tool_items: current_tools,
                            duration: turn_dur,
                            reply: outcome.reply.clone(),
                        };
                        turns.push(record);

                        if ui.is_attended {
                            crate::commands::clear_screen();
                            render_all_turns(&turns, process_expanded, get_terminal_width());
                            ui.draw_initial_box();
                        } else {
                            if !outcome.reply.is_empty() {
                                println!("{}\n{}", LUNA_LABEL.bold().green(), outcome.reply);
                            }
                        }

                        // Update status bar with current context usage
                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                    Ok(Err(HarnessError::Cancelled)) => {
                        ui.stop_activity();
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
                        let turn_dur = turn_start.elapsed();

                        let record = TurnRecord {
                            user_query: current_query.clone(),
                            reasoning: None,
                            tools: Vec::new(),
                            tool_items: current_tools,
                            duration: turn_dur,
                            reply: "✖ Cancelled".to_string(),
                        };
                        turns.push(record);

                        if ui.is_attended {
                            crate::commands::clear_screen();
                            render_all_turns(&turns, process_expanded, get_terminal_width());
                            ui.draw_initial_box();
                        } else {
                            println!("{}", "✖ Cancelled".red());
                        }

                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                    Ok(Err(e)) => {
                        ui.stop_activity();
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
                        let turn_dur = turn_start.elapsed();

                        let record = TurnRecord {
                            user_query: current_query.clone(),
                            reasoning: None,
                            tools: Vec::new(),
                            tool_items: current_tools,
                            duration: turn_dur,
                            reply: format!("✖ Error: {}", e),
                        };
                        turns.push(record);

                        if ui.is_attended {
                            crate::commands::clear_screen();
                            render_all_turns(&turns, process_expanded, get_terminal_width());
                            ui.draw_initial_box();
                        } else {
                            println!("{} Error: {}", "✖".red().bold(), e);
                        }

                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                    Err(join_err) => {
                        ui.stop_activity();
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
                        let turn_dur = turn_start.elapsed();

                        let record = TurnRecord {
                            user_query: current_query.clone(),
                            reasoning: None,
                            tools: Vec::new(),
                            tool_items: current_tools,
                            duration: turn_dur,
                            reply: format!("✖ Task join error: {}", join_err),
                        };
                        turns.push(record);

                        if ui.is_attended {
                            crate::commands::clear_screen();
                            render_all_turns(&turns, process_expanded, get_terminal_width());
                            ui.draw_initial_box();
                        } else {
                            println!("{} Task join error: {}", "✖".red().bold(), join_err);
                        }

                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                }

                if let Some(next_query) = queue.pop_front() {
                    current_cancel = CancellationToken::new();
                    let cancel_child = current_cancel.child_token();
                    let ag_ref = agent.clone();
                    current_query = next_query.clone();
                    turn_start = std::time::Instant::now();
                    in_flight_tools.lock().unwrap().clear();
                    active_turn = Some(tokio::spawn(async move {
                        let ag = ag_ref.lock().await;
                        ag.run_turn(&next_query, cancel_child).await
                    }));
                }
            }
        }
    }

    clear_on_exit();
    // The screen is wiped on exit, so leave behind just the line that gets the user back in.
    // Both id and name are shown: the name is what the user recognises, the id is the
    // unambiguous handle, and `--session` accepts either.
    let session_name = store.get(&session_id).await.map(|s| s.name).unwrap_or_default();
    if session_name.is_empty() {
        println!("Resume: luna --session {}", session_id);
    } else {
        println!("Resume: luna --session {}    (name: {})", session_id, session_name);
    }
    Ok(())
}

pub struct SlashCommand {
    pub name: &'static str,
    pub details: &'static str,
}

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/model",
        details: "Switch provider and model",
    },
    SlashCommand {
        name: "/reasoning",
        details: "Set reasoning effort (off, low, medium, high)",
    },
    SlashCommand {
        name: "/name",
        details: "Set or view session name",
    },
    SlashCommand {
        name: "/new",
        details: "Start a fresh chat session",
    },
    SlashCommand {
        name: "/summarize",
        details: "Summarize and start a new session with it",
    },
    SlashCommand {
        name: "/expand",
        details: "Expand intermediate tool and reasoning processes (or Ctrl+O)",
    },
    SlashCommand {
        name: "/collapse",
        details: "Collapse intermediate tool and reasoning processes (or Ctrl+O)",
    },
    SlashCommand {
        name: "/clear",
        details: "Clear terminal screen",
    },
    SlashCommand {
        name: "/exit",
        details: "Exit Luna chat session",
    },
];

async fn execute_slash_command(
    cmd: &str,
    key_rx: &mut mpsc::UnboundedReceiver<Key>,
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &Arc<SqliteSessionStore>,
    session_id: &mut String,
    current_provider: &mut String,
    current_model: &mut String,
    turns: &mut Vec<TurnRecord>,
    process_expanded: &mut bool,
) -> Result<bool, HarnessError> {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let name = parts[0];
    let arg = parts.get(1).copied().map(|s| s.trim()).filter(|s| !s.is_empty());

    match name {
        "/model" | "/models" | "/provider" => {
            ui.redraw_input_line();
            if let Err(e) = handle_model_switch(
                Some(key_rx),
                ui,
                config,
                cred_manager,
                discovery,
                agent,
                current_provider,
                current_model,
            )
            .await
            {
                ui.emit_above(&format!("{} Failed to switch model: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/reasoning" => {
            ui.redraw_input_line();
            if let Err(e) = handle_reasoning_switch(Some(key_rx), ui, agent, arg).await {
                ui.emit_above(&format!("{} Failed to set reasoning effort: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/name" => {
            if let Some(a) = arg {
                ui.clear_box_on_exit();
                if let Err(e) = handle_name_command(ui, store.as_ref(), session_id, Some(a)).await {
                    ui.draw_initial_box();
                    ui.emit_above(&format!("{} Failed to set session name: {}", "✖".red().bold(), e));
                }
            } else {
                ui.input_buf.lock().unwrap().clear();
                ui.input_buf.lock().unwrap().push_str("/name ");
                ui.redraw_input_line();
            }
            Ok(false)
        }
        "/summarize" | "/compact" => {
            turns.clear();
            ui.clear_box_on_exit();
            if let Err(e) = handle_summarize_and_new_command(
                ui,
                store,
                config,
                agent,
                session_id,
                current_provider,
                current_model,
                arg,
            )
            .await
            {
                ui.draw_initial_box();
                ui.emit_above(&format!("{} Failed to summarize session: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/new" => {
            turns.clear();
            let (is_summarize, remaining_arg) = if let Some(a) = arg {
                if a.starts_with("-s ") || a == "-s" {
                    (true, a.strip_prefix("-s").unwrap().trim())
                } else if a.starts_with("--summarize ") || a == "--summarize" {
                    (true, a.strip_prefix("--summarize").unwrap().trim())
                } else if a.starts_with("--summary ") || a == "--summary" {
                    (true, a.strip_prefix("--summary").unwrap().trim())
                } else {
                    (false, a)
                }
            } else {
                (false, "")
            };
            let rem_opt = if remaining_arg.is_empty() { None } else { Some(remaining_arg) };
            ui.clear_box_on_exit();
            if is_summarize {
                if let Err(e) = handle_summarize_and_new_command(
                    ui,
                    store,
                    config,
                    agent,
                    session_id,
                    current_provider,
                    current_model,
                    rem_opt,
                )
                .await
                {
                    ui.draw_initial_box();
                    ui.emit_above(&format!("{} Failed to summarize session: {}", "✖".red().bold(), e));
                }
            } else {
                if let Err(e) = handle_new_command(
                    ui,
                    store,
                    config,
                    agent,
                    session_id,
                    current_provider,
                    current_model,
                    rem_opt,
                )
                .await
                {
                    ui.draw_initial_box();
                    ui.emit_above(&format!("{} Failed to start new session: {}", "✖".red().bold(), e));
                }
            }
            Ok(false)
        }
        "/expand" => {
            *process_expanded = true;
            if ui.is_attended {
                crate::commands::clear_screen();
                render_all_turns(turns, *process_expanded, get_terminal_width());
                ui.draw_initial_box();
            }
            Ok(false)
        }
        "/collapse" => {
            *process_expanded = false;
            if ui.is_attended {
                crate::commands::clear_screen();
                render_all_turns(turns, *process_expanded, get_terminal_width());
                ui.draw_initial_box();
            }
            Ok(false)
        }
        "/clear" => {
            turns.clear();
            crate::commands::clear_screen();
            ui.draw_initial_box();
            Ok(false)
        }
        "exit" | "quit" | "/exit" | "/quit" => {
            ui.clear_box_on_exit();
            clear_on_exit();
            Ok(true)
        }
        _ => Ok(false),
    }
}

async fn select_slash_command(
    key_rx: &mut mpsc::UnboundedReceiver<Key>,
    ui: &ChatUi,
) -> Option<String> {
    if !ui.is_attended {
        return None;
    }

    const PAGE_SIZE: usize = 5;
    const BOX_ROWS: usize = PAGE_SIZE + 2;

    let mut selected: usize = 0;

    let get_filtered = |filter: &str| -> Vec<(usize, &'static SlashCommand)> {
        let f = filter.strip_prefix('/').unwrap_or(filter).trim().to_lowercase();
        if f.is_empty() {
            SLASH_COMMANDS.iter().enumerate().collect()
        } else {
            SLASH_COMMANDS
                .iter()
                .enumerate()
                .filter(|(_, cmd)| {
                    cmd.name.to_lowercase().contains(&f)
                        || cmd.details.to_lowercase().contains(&f)
                })
                .collect()
        }
    };

    let render_box = |sel: usize, filt: &str| -> Vec<String> {
        let width = get_terminal_width().min(76).max(44);
        let inner_width = width.saturating_sub(4);
        let filtered = get_filtered(filt);

        let mut lines = Vec::with_capacity(BOX_ROWS);
        lines.push(format!("┌{}┐", "─".repeat(width.saturating_sub(2))).dimmed().to_string());

        if filtered.is_empty() {
            let empty_msg = "(no matching commands)";
            let pad = inner_width.saturating_sub(empty_msg.chars().count() + 1);
            lines.push(format!("│ {}{} │", empty_msg.dimmed(), " ".repeat(pad)));
            for _ in 1..PAGE_SIZE {
                lines.push(format!("│ {} │", " ".repeat(inner_width)));
            }
        } else {
            let start = if sel >= PAGE_SIZE {
                (sel + 1 - PAGE_SIZE).min(filtered.len().saturating_sub(PAGE_SIZE))
            } else {
                0
            };
            let end = (start + PAGE_SIZE).min(filtered.len());

            for idx in start..end {
                let (_, cmd) = filtered[idx];
                let is_sel = idx == sel;
                let pfx = if is_sel { "❯ ".cyan().bold().to_string() } else { "  ".to_string() };
                let cmd_styled = if is_sel { cmd.name.bold().to_string() } else { cmd.name.cyan().to_string() };

                let pfx_len = 2;
                let name_len = cmd.name.chars().count();
                let gap_len = 2;
                let remaining_width = inner_width.saturating_sub(pfx_len + name_len + gap_len);

                let details_disp = if cmd.details.chars().count() > remaining_width {
                    format!("{}…", cmd.details.chars().take(remaining_width.saturating_sub(1)).collect::<String>())
                } else {
                    cmd.details.to_string()
                };
                let details_len = details_disp.chars().count();
                let pad_len = inner_width.saturating_sub(pfx_len + name_len + gap_len + details_len);

                lines.push(format!(
                    "│ {}{}  {}{}{} │",
                    pfx,
                    cmd_styled,
                    details_disp.dimmed(),
                    " ".repeat(pad_len),
                    "".clear()
                ));
            }

            for _ in (end - start)..PAGE_SIZE {
                lines.push(format!("│ {} │", " ".repeat(inner_width)));
            }
        }

        lines.push(format!("└{}┘", "─".repeat(width.saturating_sub(2))).dimmed().to_string());
        lines
    };

    // Ensure space below: scroll terminal if needed to provide canvas
    let buf = ui.input_buf.lock().unwrap().clone();
    print!("\x1b[2B\r");
    for _ in 0..BOX_ROWS {
        println!();
    }
    print!("\x1b[{}A\r{}", BOX_ROWS + 2, ui.prompt_row(&buf));
    let _ = io::stdout().flush();

    let draw_menu = |sel: usize, filt: &str, buf_str: &str| {
        let lines = render_box(sel, filt);
        print!("\x1b[2B\r");
        for (i, line) in lines.iter().enumerate() {
            print!("\x1b[2K{}", line);
            if i + 1 < lines.len() {
                print!("\x1b[1B\r");
            }
        }
        print!("\x1b[{}A\r{}", BOX_ROWS + 1, ui.prompt_row(buf_str));
        let _ = io::stdout().flush();
    };

    let erase_menu = |buf_str: &str| {
        print!("\x1b[2B\r");
        for i in 0..BOX_ROWS {
            print!("\x1b[2K");
            if i + 1 < BOX_ROWS {
                print!("\x1b[1B\r");
            }
        }
        print!("\x1b[{}A\r{}", BOX_ROWS + 1, ui.prompt_row(buf_str));
        let _ = io::stdout().flush();
    };

    let cur_filter = {
        let b = ui.input_buf.lock().unwrap();
        b.clone()
    };
    draw_menu(selected, &cur_filter, &cur_filter);

    let mut result = None;

    while let Some(key) = key_rx.recv().await {
        let filter_str = ui.input_buf.lock().unwrap().clone();
        let filtered = get_filtered(&filter_str);

        match key {
            Key::ArrowDown => {
                if !filtered.is_empty() {
                    selected = (selected + 1) % filtered.len();
                }
                draw_menu(selected, &filter_str, &filter_str);
            }
            Key::ArrowUp => {
                if !filtered.is_empty() {
                    selected = if selected == 0 {
                        filtered.len().saturating_sub(1)
                    } else {
                        selected - 1
                    };
                }
                draw_menu(selected, &filter_str, &filter_str);
            }
            Key::Enter => {
                if !filtered.is_empty() && selected < filtered.len() {
                    result = Some(filtered[selected].1.name.to_string());
                }
                break;
            }
            Key::Tab => {
                if !filtered.is_empty() && selected < filtered.len() {
                    let cmd = filtered[selected].1.name;
                    let mut b = ui.input_buf.lock().unwrap();
                    b.clear();
                    b.push_str(cmd);
                    b.push(' ');
                    drop(b);
                    break;
                }
            }
            Key::Escape | Key::CtrlC => {
                ui.input_buf.lock().unwrap().clear();
                break;
            }
            Key::Backspace => {
                let mut b = ui.input_buf.lock().unwrap();
                b.pop();
                let is_empty = b.is_empty();
                let new_buf = b.clone();
                drop(b);
                ui.redraw_input_line();

                if is_empty {
                    break;
                } else {
                    selected = 0;
                    draw_menu(selected, &new_buf, &new_buf);
                }
            }
            Key::Char(c) if c != '\n' && c != '\r' && !c.is_control() => {
                let mut b = ui.input_buf.lock().unwrap();
                b.push(c);
                let new_buf = b.clone();
                drop(b);
                ui.redraw_input_line();
                selected = 0;
                draw_menu(selected, &new_buf, &new_buf);
            }
            _ => continue,
        }
    }

    let final_buf = ui.input_buf.lock().unwrap().clone();
    erase_menu(&final_buf);
    result
}

async fn select_from_rx(
    key_rx: &mut mpsc::UnboundedReceiver<Key>,
    ui: &ChatUi,
    prompt: &str,
    items: &[String],
    default_idx: usize,
    is_chained: bool,
    leave_open: bool,
) -> Option<usize> {
    if items.is_empty() {
        return None;
    }

    let mut filter = String::new();

    let get_filtered = |filter: &str| -> Vec<(usize, &String)> {
        if filter.is_empty() {
            items.iter().enumerate().collect()
        } else {
            let filter_lower = filter.to_lowercase();
            items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.to_lowercase().contains(&filter_lower))
                .collect()
        }
    };

    let initial_filtered = get_filtered(&filter);
    let mut selected_filtered_idx = initial_filtered
        .iter()
        .position(|(orig_idx, _)| *orig_idx == default_idx)
        .unwrap_or(0);

    const PAGE_SIZE: usize = 7;
    const MENU_HEIGHT: usize = PAGE_SIZE + 1; // 1 header + 7 items

    let render_menu_lines = |selected: usize, filter: &str| -> Vec<String> {
        let term_width = get_terminal_width().saturating_sub(4).max(30);
        let filtered = get_filtered(filter);

        let mut lines = Vec::with_capacity(MENU_HEIGHT);

        let total_pages = if filtered.is_empty() {
            1
        } else {
            (filtered.len() + PAGE_SIZE - 1) / PAGE_SIZE
        };
        let current_page = if filtered.is_empty() {
            1
        } else {
            (selected / PAGE_SIZE) + 1
        };

        let filter_display = if filter.is_empty() {
            String::new()
        } else {
            format!(" [search: \"{}\"]", filter)
        };

        let page_indicator = if total_pages > 1 {
            format!(" ({}/{})", current_page, total_pages)
        } else {
            String::new()
        };

        let raw_header = format!("{}{}:{}", prompt, filter_display, page_indicator);
        let header = if raw_header.len() > term_width {
            format!("{}…", &raw_header[..term_width.saturating_sub(1)])
        } else {
            raw_header
        };
        lines.push(format!("\r\x1b[2K{}", header.bold().cyan()));

        if filtered.is_empty() {
            lines.push(format!("\r\x1b[2K  {}", "(no matching items)".dimmed()));
            for _ in 1..PAGE_SIZE {
                lines.push("\r\x1b[2K".to_string());
            }
        } else {
            let half = PAGE_SIZE / 2;
            let start = if selected > half && filtered.len() > PAGE_SIZE {
                (selected - half).min(filtered.len() - PAGE_SIZE)
            } else {
                0
            };
            let end = (start + PAGE_SIZE).min(filtered.len());

            for idx in start..end {
                let (_, item) = filtered[idx];
                let item_width = term_width.saturating_sub(6);
                let display_item = if item.len() > item_width {
                    format!("{}…", &item[..item_width.saturating_sub(1)])
                } else {
                    item.clone()
                };

                if idx == selected {
                    lines.push(format!(
                        "\r\x1b[2K  {} {}",
                        "❯".cyan().bold(),
                        display_item.bold()
                    ));
                } else {
                    lines.push(format!(
                        "\r\x1b[2K    {}",
                        display_item.dimmed()
                    ));
                }
            }

            let rendered = end - start;
            for _ in rendered..PAGE_SIZE {
                lines.push("\r\x1b[2K".to_string());
            }
        }

        lines
    };

    let width = get_terminal_width();
    let divider = "─".repeat(width).dimmed().to_string();

    if ui.is_attended {
        if is_chained {
            // Menu is already open above the box! Just overwrite the MENU_HEIGHT rows
            print!("\r\x1b[{}A", MENU_HEIGHT + 2);
            let initial_lines = render_menu_lines(selected_filtered_idx, &filter);
            for line in &initial_lines {
                println!("{}", line);
            }
            print!("\x1b[2B\r{}", PROMPT.bold().yellow());
            let _ = io::stdout().flush();
        } else {
            // Smoothly open MENU_HEIGHT lines above the status bar
            let has_status = ui.status.lock().unwrap().is_some();
            if has_status {
                print!("\x1b[1B\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K");
            } else {
                print!("\x1b[1B\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K");
            }

            let initial_lines = render_menu_lines(selected_filtered_idx, &filter);
            for line in &initial_lines {
                println!("{}", line);
            }

            if let Some(status_str) = ui.format_status_line(width) {
                println!("{}", status_str);
            }
            println!("{}", divider);
            println!();
            println!("{}", divider);
            print!("\x1b[2A\r{}", PROMPT.bold().yellow());
            let _ = io::stdout().flush();
        }
    } else {
        println!("{}:", prompt);
        for (i, item) in items.iter().enumerate() {
            println!("  {}. {}", i + 1, item);
        }
    }

    let mut result = None;

    while let Some(key) = key_rx.recv().await {
        let filtered = get_filtered(&filter);

        match key {
            Key::ArrowUp => {
                if !filtered.is_empty() {
                    if selected_filtered_idx > 0 {
                        selected_filtered_idx -= 1;
                    } else {
                        selected_filtered_idx = filtered.len() - 1;
                    }
                }
            }
            Key::ArrowDown => {
                if !filtered.is_empty() {
                    if selected_filtered_idx + 1 < filtered.len() {
                        selected_filtered_idx += 1;
                    } else {
                        selected_filtered_idx = 0;
                    }
                }
            }
            Key::Backspace => {
                if !filter.is_empty() {
                    filter.pop();
                    selected_filtered_idx = 0;
                }
            }
            Key::Char(c) if c != '\n' && c != '\r' && !c.is_control() => {
                filter.push(c);
                selected_filtered_idx = 0;
            }
            Key::Enter => {
                if !filtered.is_empty() && selected_filtered_idx < filtered.len() {
                    result = Some(filtered[selected_filtered_idx].0);
                }
                break;
            }
            Key::Escape | Key::CtrlC => {
                if !filter.is_empty() {
                    filter.clear();
                    selected_filtered_idx = 0;
                } else {
                    result = None;
                    break;
                }
            }
            _ => continue,
        }

        if ui.is_attended {
            // Distance from User > to top of menu is MENU_HEIGHT + 2 lines
            print!("\r\x1b[{}A", MENU_HEIGHT + 2);
            let updated_lines = render_menu_lines(selected_filtered_idx, &filter);
            for line in &updated_lines {
                println!("{}", line);
            }
            // Move cursor back down to User >
            print!("\x1b[2B\r{}", PROMPT.bold().yellow());
            let _ = io::stdout().flush();
        }
    }

    if ui.is_attended && (!leave_open || result.is_none()) {
        // Erase menu rows and box rows, then redraw box in its original position
        print!("\r\x1b[{}A", MENU_HEIGHT + 2);
        for _ in 0..(MENU_HEIGHT + 4) {
            print!("\r\x1b[2K\x1b[1B");
        }
        print!("\r\x1b[{}A", MENU_HEIGHT + 4);
        ui.draw_initial_box();
    }

    result
}

async fn input_from_rx(
    key_rx: &mut mpsc::UnboundedReceiver<Key>,
    prompt: &str,
) -> Option<String> {
    print!("{}: ", prompt.bold().cyan());
    let _ = io::stdout().flush();
    let mut buf = String::new();

    while let Some(key) = key_rx.recv().await {
        match key {
            Key::Char(c) => {
                buf.push(c);
                print!("{}", c);
                let _ = io::stdout().flush();
            }
            Key::Backspace => {
                if buf.pop().is_some() {
                    print!("\x08 \x08");
                    let _ = io::stdout().flush();
                }
            }
            Key::Enter => {
                print!("\r\x1b[2K");
                let _ = io::stdout().flush();
                let res = buf.trim().to_string();
                if res.is_empty() {
                    return None;
                }
                return Some(res);
            }
            Key::Escape | Key::CtrlC => {
                print!("\r\x1b[2K");
                let _ = io::stdout().flush();
                return None;
            }
            _ => {}
        }
    }
    None
}

async fn handle_model_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<Key>>,
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    current_provider: &mut String,
    current_model: &mut String,
) -> Result<(), HarnessError> {
    let catalog = get_provider_catalog();
    let loaded = cred_manager.get_loaded_providers().await;

    let items: Vec<String> = catalog
        .iter()
        .map(|p| {
            let mut tags = Vec::new();
            if p.id.eq_ignore_ascii_case(current_provider) {
                tags.push("active");
            }
            if loaded.iter().any(|l| l.eq_ignore_ascii_case(&p.id)) {
                tags.push("configured");
            }
            let tag_str = if tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", tags.join(", "))
            };
            format!("{:<28} ({}){}", p.display_name, p.id, tag_str)
        })
        .collect();

    let default_idx = catalog
        .iter()
        .position(|p| p.id.eq_ignore_ascii_case(current_provider))
        .unwrap_or(0);

    let sel = if let Some(ref mut rx) = key_rx {
        while rx.try_recv().is_ok() {}
        match select_from_rx(rx, ui, "Select Provider", &items, default_idx, false, true).await {
            Some(i) => i,
            None => {
                return Ok(());
            }
        }
    } else {
        default_idx
    };

    let chosen_provider = catalog[sel].id.clone();

    // Fetch models for chosen provider
    let mut models = if chosen_provider == "antigravity" || chosen_provider == "agy" {
        vec![
            "gemini-3.8-flash".to_string(),
            "claude-sonnet-4-6".to_string(),
            "claude-opus-4-6".to_string(),
            "gpt-oss-120b".to_string(),
        ]
    } else {
        match discovery.get_models(&chosen_provider, cred_manager, false).await {
            Ok(m) => m,
            Err(_) => Vec::new(),
        }
    };

    let default_model = get_default_model(&chosen_provider).to_string();
    if !default_model.is_empty() && !models.contains(&default_model) {
        models.insert(0, default_model.clone());
    }

    let mut model_items = models.clone();
    model_items.push("[Custom model name...]".to_string());

    let default_model_idx = models
        .iter()
        .position(|m| m.eq_ignore_ascii_case(current_model))
        .unwrap_or(0);

    let model_sel = if let Some(ref mut rx) = key_rx {
        while rx.try_recv().is_ok() {}
        match select_from_rx(rx, ui, &format!("Select Model [{}]", chosen_provider), &model_items, default_model_idx, true, true).await {
            Some(i) => i,
            None => {
                return Ok(());
            }
        }
    } else {
        default_model_idx
    };

    let chosen_model = if model_sel < models.len() {
        model_items[model_sel].clone()
    } else if let Some(ref mut rx) = key_rx {
        while rx.try_recv().is_ok() {}
        match input_from_rx(rx, "Enter custom model name").await {
            Some(name) => name,
            None => {
                return Ok(());
            }
        }
    } else {
        default_model
    };

    if chosen_model.is_empty() {
        return Ok(());
    }

    // Reasoning effort selection
    let reasoning_options = [
        ("Keep current / Default", None),
        ("minimal", Some(ReasoningEffort::Minimal)),
        ("low", Some(ReasoningEffort::Low)),
        ("medium", Some(ReasoningEffort::Medium)),
        ("high", Some(ReasoningEffort::High)),
        ("xhigh", Some(ReasoningEffort::Xhigh)),
        ("max", Some(ReasoningEffort::Max)),
        ("off / disabled", None),
    ];

    let reasoning_items: Vec<String> = reasoning_options.iter().map(|(label, _)| label.to_string()).collect();
    let current_effort = agent.lock().await.reasoning_effort();
    let default_reasoning_idx = match current_effort {
        None => 0,
        Some(ReasoningEffort::Minimal) => 1,
        Some(ReasoningEffort::Low) => 2,
        Some(ReasoningEffort::Medium) => 3,
        Some(ReasoningEffort::High) => 4,
        Some(ReasoningEffort::Xhigh) => 5,
        Some(ReasoningEffort::Max) => 6,
    };

    let reasoning_sel = if let Some(ref mut rx) = key_rx {
        while rx.try_recv().is_ok() {}
        match select_from_rx(rx, ui, "Select Reasoning Effort", &reasoning_items, default_reasoning_idx, true, false).await {
            Some(i) => i,
            None => {
                return Ok(());
            }
        }
    } else {
        default_reasoning_idx
    };

    let chosen_effort = if reasoning_sel == 0 {
        current_effort
    } else {
        reasoning_options[reasoning_sel].1
    };

    let new_provider = Arc::new(GenericProviderClient::new(
        chosen_provider.clone(),
        chosen_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&chosen_provider),
    )?);

    {
        let mut ag = agent.lock().await;
        ag.set_provider(new_provider);
        ag.set_reasoning_effort(chosen_effort);
    }

    *current_provider = chosen_provider.clone();
    *current_model = chosen_model.clone();

    // The chained pickers redraw the box as they tear down — with the pre-switch model. Erase
    // that stale box so the status bar is drawn once, showing the model now in use.
    let context_max = get_context_window(&chosen_provider, &chosen_model).unwrap_or(32_768);
    // Switching models does not touch the transcript, so keep the real usage rather than resetting
    // the bar to 0.
    let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
    ui.set_model(&chosen_model, context_used, context_max);
    ui.clear_box_on_exit();
    ui.draw_initial_box();

    Ok(())
}

/// `/name [name]`: set the current session's user-assigned name, or report it when no name is given.
async fn handle_name_command(
    ui: &ChatUi,
    store: &SqliteSessionStore,
    session_id: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    if let Some(raw) = arg {
        store.set_name(session_id, raw).await?;
    }

    let name = store.get(session_id).await?.name;
    ui.draw_initial_box();

    if name.is_empty() {
        if arg.is_some() {
            ui.emit_above(&format!("{} Session name cleared", "✔".green().bold()));
        } else {
            ui.emit_above(&format!(
                "{} This session has no name — set one with {}",
                "•".cyan(),
                "/name <name>".bold()
            ));
        }
    } else if arg.is_some() {
        ui.emit_above(&format!(
            "{} Session name set to '{}'",
            "✔".green().bold(),
            name.cyan()
        ));
    } else {
        ui.emit_above(&format!("{} Session name: {}", "•".cyan(), name.cyan()));
    }

    Ok(())
}

/// `/new [name]`: start a fresh chat session, rebinding the agent and clearing the screen.
async fn handle_new_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &mut String,
    current_provider: &str,
    current_model: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let new_id = resolve_session(store.as_ref(), SessionSelection::New).await?;
    let name = arg.map(|s| s.trim()).unwrap_or("");
    if !name.is_empty() {
        store.set_name(&new_id, name).await?;
    }

    *session_id = new_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: new_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let context_max = get_context_window(current_provider, current_model).unwrap_or(32_768);
    ui.set_status(current_model, 0, context_max);

    if ui.is_attended {
        crate::commands::clear_screen();
        ui.draw_initial_box();
        if name.is_empty() {
            ui.emit_above(&format!(
                "{} Started new session {}",
                "✔".green().bold(),
                session_id.cyan()
            ));
        } else {
            ui.emit_above(&format!(
                "{} Started new session '{}' ({})",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed()
            ));
        }
    } else {
        if name.is_empty() {
            println!(
                "{} Started new session {}",
                "✔".green().bold(),
                session_id.cyan()
            );
        } else {
            println!(
                "{} Started new session '{}' ({})",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed()
            );
        }
    }

    Ok(())
}

/// `/summarize [name]`, `/compact [name]`, or `/new -s [name]`:
/// Summarize the current session's history and start a new session seeded with the summary.
async fn handle_summarize_and_new_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &mut String,
    current_provider: &str,
    current_model: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let replayed = store.replay(session_id, usize::MAX).await?;
    let has_content = replayed
        .history
        .iter()
        .any(|s| !s.is_summary && s.message.role != Role::System);

    if !has_content {
        ui.draw_initial_box();
        ui.emit_above(&format!(
            "{} Nothing to summarize in this session",
            "•".cyan()
        ));
        return Ok(());
    }

    let old_id = session_id.clone();
    let name = arg.map(|s| s.trim()).unwrap_or("");

    ui.start_activity("summarize", &serde_json::json!({ "session": old_id }));

    let summary_res = {
        let ag = agent.lock().await;
        ag.summarize_current_session(current_model).await
    };

    ui.stop_activity();

    let summary = match summary_res {
        Ok(s) => s,
        Err(e) => {
            ui.draw_initial_box();
            ui.emit_above(&format!(
                "{} Failed to summarize session: {}",
                "✖".red().bold(),
                e
            ));
            return Ok(());
        }
    };

    let new_id = resolve_session(store.as_ref(), SessionSelection::New).await?;
    if !name.is_empty() {
        store.set_name(&new_id, name).await?;
    }

    let summary_header = if !name.is_empty() {
        format!("Context summary from session '{}' ({}):\n{}", name, old_id, summary)
    } else {
        format!("Context summary from session {}:\n{}", old_id, summary)
    };
    let summary_msg = Message::system(summary_header);
    store.append(&new_id, &summary_msg, true).await?;

    *session_id = new_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: new_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let context_max = get_context_window(current_provider, current_model).unwrap_or(32_768);
    let summary_tokens = harness_core::heuristic_tokens(&[summary_msg]);
    ui.set_status(current_model, summary_tokens, context_max);

    if ui.is_attended {
        crate::commands::clear_screen();
        ui.draw_initial_box();
        if name.is_empty() {
            ui.emit_above(&format!(
                "{} Started new session {} with summary from {}",
                "✔".green().bold(),
                session_id.cyan(),
                old_id.dimmed()
            ));
        } else {
            ui.emit_above(&format!(
                "{} Started new session '{}' ({}) with summary from {}",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed(),
                old_id.dimmed()
            ));
        }
        ui.emit_above(&format!(
            "\n{}\n{}\n",
            "Summary:".bold(),
            summary.dimmed()
        ));
    } else {
        if name.is_empty() {
            println!(
                "{} Started new session {} with summary from {}",
                "✔".green().bold(),
                session_id.cyan(),
                old_id.dimmed()
            );
        } else {
            println!(
                "{} Started new session '{}' ({}) with summary from {}",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed(),
                old_id.dimmed()
            );
        }
        println!("\n{}\n{}\n", "Summary:".bold(), summary);
    }

    Ok(())
}

async fn handle_reasoning_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<Key>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let chosen_effort = if let Some(s) = arg {
        if s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("none") {
            None
        } else {
            match ReasoningEffort::from_str_loose(s) {
                Some(e) => Some(e),
                None => {
                    ui.draw_initial_box();
                    ui.emit_above(&format!(
                        "{} Unknown reasoning effort '{}'. Available: minimal, low, medium, high, xhigh, max, off",
                        "✖".red().bold(),
                        s
                    ));
                    return Ok(());
                }
            }
        }
    } else {
        let reasoning_options = [
            ("off / default", None),
            ("minimal", Some(ReasoningEffort::Minimal)),
            ("low", Some(ReasoningEffort::Low)),
            ("medium", Some(ReasoningEffort::Medium)),
            ("high", Some(ReasoningEffort::High)),
            ("xhigh", Some(ReasoningEffort::Xhigh)),
            ("max", Some(ReasoningEffort::Max)),
        ];
        let reasoning_items: Vec<String> = reasoning_options.iter().map(|(label, _)| label.to_string()).collect();
        let current_effort = agent.lock().await.reasoning_effort();
        let default_idx = match current_effort {
            None => 0,
            Some(ReasoningEffort::Minimal) => 1,
            Some(ReasoningEffort::Low) => 2,
            Some(ReasoningEffort::Medium) => 3,
            Some(ReasoningEffort::High) => 4,
            Some(ReasoningEffort::Xhigh) => 5,
            Some(ReasoningEffort::Max) => 6,
        };

        if let Some(ref mut rx) = key_rx {
            while rx.try_recv().is_ok() {}
            match select_from_rx(rx, ui, "Select Reasoning Effort", &reasoning_items, default_idx, false, false).await {
                Some(i) => reasoning_options[i].1,
                None => {
                    return Ok(());
                }
            }
        } else {
            None
        }
    };

    agent.lock().await.set_reasoning_effort(chosen_effort);
    // Same as the model switch: the picker left a box behind, so erase it before redrawing.
    ui.clear_box_on_exit();
    ui.draw_initial_box();

    let effort_label = chosen_effort
        .map(|e| e.to_string())
        .unwrap_or_else(|| "off".to_string());

    ui.emit_above(&format!(
        "{} Reasoning effort set to '{}'",
        "✔".green().bold(),
        effort_label.yellow()
    ));

    Ok(())
}

async fn prompt_approval_choice(
    key_rx: &mut mpsc::UnboundedReceiver<Key>,
    request: &ApprovalRequest,
    ui: &ChatUi,
) -> ApprovalChoice {
    let width = get_terminal_width();
    let mut selected = 0; // index into APPROVAL_ACTIONS

    // Drain any leftover keys in channel
    while key_rx.try_recv().is_ok() {}

    // The popup renders above the composer: erase box + live region, draw the popup, redraw
    // the composer beneath it once the answer lands.
    ui.clear_box_on_exit();

    let (initial_text, mut line_count) = render_approval_box(request, selected, width);
    print!("{}", initial_text);
    let _ = io::stdout().flush();

    while let Some(key) = key_rx.recv().await {
        let chosen = match key {
            Key::ArrowUp | Key::ArrowLeft | Key::BackTab => {
                selected = (selected + APPROVAL_ACTIONS.len() - 1) % APPROVAL_ACTIONS.len();
                None
            }
            Key::ArrowDown | Key::ArrowRight | Key::Tab => {
                selected = (selected + 1) % APPROVAL_ACTIONS.len();
                None
            }
            Key::Char('y') | Key::Char('Y') => Some(ApprovalChoice::Once),
            Key::Char('a') | Key::Char('A') => Some(ApprovalChoice::AllSession),
            Key::Char('n') | Key::Char('N') | Key::Char('c') | Key::Char('C') | Key::Escape | Key::CtrlC => {
                Some(ApprovalChoice::Deny)
            }
            Key::Enter => match selected {
                0 => Some(ApprovalChoice::Once),
                1 => Some(ApprovalChoice::AllSession),
                _ => Some(ApprovalChoice::Deny),
            },
            _ => continue,
        };

        if let Some(choice) = chosen {
            // Erase the popup and restore the composer before returning.
            print!("\r\x1b[{}A", line_count);
            for _ in 0..line_count {
                print!("\x1b[2K\x1b[1B");
            }
            print!("\r\x1b[{}A", line_count);
            let _ = io::stdout().flush();
            ui.draw_initial_box();
            return choice;
        }

        // Repaint in place: jump to the first popup row and overwrite every line.
        print!("\r\x1b[{}A", line_count);
        let (new_text, new_count) = render_approval_box(request, selected, width);
        print!("{}", new_text);
        line_count = new_count;
        let _ = io::stdout().flush();
    }

    ui.draw_initial_box();
    ApprovalChoice::Deny
}

