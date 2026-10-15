use std::collections::VecDeque;
use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex};
use colored::Colorize;
use dialoguer::console::{Key, Term};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use harness_core::{
    Agent, ApprovalRequest, Config, DefaultToolRegistry, HarnessError, ModelProvider,
    ReasoningEffort, Role, SessionBinding, SessionStore, SqliteSessionStore, ToolRegistry, TurnOutcome,
};
use harness_evolver::{ApplyUpdateTool, CodebaseEvolver, InspectSourceTool, ProposePatchTool};
use harness_memory::{
    ForgetMemoryTool, SaveMemoryTool, SqliteMemoryEngine, UpdateMemoryTool,
};
use harness_notes::{
    ReadNoteTool, SaveNoteTool, SearchNotesTool, SqliteNotesEngine, UpdateNoteTool,
};
use harness_providers::{
    get_default_model, get_provider_catalog, get_context_window, CredentialManager, GenericProviderClient,
    ModelDiscovery,
};

use super::approval::{approval_settings, build_approver, render_approval_box, ChatApprover};
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

/// The composer prompt. The `User >` label is gone, so Luna's crescent marks the input line
/// instead. U+263E LAST QUARTER MOON is a plain text glyph — terminals keep it
/// black-and-white, unlike the colour crescent U+1F319 which renders as an emoji.
const PROMPT: &str = "\u{263E} ";

/// Luna's reply label. Kept beside [`PROMPT`] so the two sides of the transcript are edited
/// in one place rather than hunting for scattered UI literals.
const LUNA_LABEL: &str = "Luna";

/// Right-align the user's own message so the transcript reads as a chat — the user on the
/// right, Luna on the left. Padding is derived from the *visible* width, never the ANSI codes.
fn right_aligned_user_message(query: &str, width: usize) -> String {
    // The message and its glyph are one unit, so they are bolded together.
    let line = format!("{} {}", query, PROMPT.trim_end());
    let pad = width.saturating_sub(1).saturating_sub(line.chars().count());
    format!("{}{}", " ".repeat(pad), line.bold())
}

/// Truncate `text` to at most `max` visible characters, marking the cut with an ellipsis.
fn truncate_visible(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut truncated: String = text.chars().take(max - 1).collect();
    truncated.push('…');
    truncated
}

/// One transcript block kind, tracked so blank lines land only between differing
/// blocks (user → thinking, thinking → tool) and never between repeats (tool → tool).
#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputBlockKind {
    None,
    User,
    Thinking,
    Tool,
    Reply,
}

/// A blank line separates blocks of different kinds. Identical consecutive blocks stay
/// adjacent, and the first block of a turn is not preceded by one.
fn needs_separator(prev: OutputBlockKind, next: OutputBlockKind) -> bool {
    prev != OutputBlockKind::None && prev != next
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
    /// True while the most recently printed log line is an in-progress tool call, so the finished
    /// result can replace that same line instead of appending a second one.
    pending_tool: Arc<Mutex<bool>>,
    is_attended: bool,
}

impl ChatUi {
    fn new(is_attended: bool) -> Self {
        Self {
            input_buf: Arc::new(Mutex::new(String::new())),
            status: Arc::new(Mutex::new(None)),
            pending_tool: Arc::new(Mutex::new(false)),
            is_attended,
        }
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

    fn print_above(&self, text: &str) {
        self.emit_above(text);
        *self.pending_tool.lock().unwrap() = false;
    }

    /// Like [`Self::print_above`], but the line is remembered as an in-progress tool call so
    /// [`Self::print_tool_finished`] can turn it into the normal-colour result line.
    fn print_tool_running(&self, text: &str) {
        self.emit_above(text);
        *self.pending_tool.lock().unwrap() = true;
    }

    /// Rewrite the in-progress tool line with the finished result. Falls back to a fresh line when
    /// there is no pending tool (e.g. an approval prompt scrolled the pending line out of place).
    fn print_tool_finished(&self, text: &str) {
        if !self.is_attended {
            println!("{}", text);
            return;
        }
        if !*self.pending_tool.lock().unwrap() {
            self.print_above(text);
            return;
        }

        let buf = self.input_buf.lock().unwrap();
        let has_status = self.status.lock().unwrap().is_some();
        // The pending tool line sits directly above the box: status line + top divider (and the
        // input line itself) separate it from the cursor.
        let up = if has_status { 3 } else { 2 };
        print!("\x1b[{}A\r\x1b[2K", up);
        print!("{}", text);
        print!("\x1b[{}B\r{}", up, self.prompt_row(&buf));
        let _ = io::stdout().flush();
        *self.pending_tool.lock().unwrap() = false;
    }

    /// Forget a pending tool line, e.g. before a direct `println!` shifts the layout.
    fn clear_pending_tool(&self) {
        *self.pending_tool.lock().unwrap() = false;
    }

    fn emit_above(&self, text: &str) {
        if !self.is_attended {
            println!("{}", text);
            return;
        }
        let buf = self.input_buf.lock().unwrap();
        let width = get_terminal_width();
        let divider = "─".repeat(width).dimmed().to_string();

        let has_status = self.status.lock().unwrap().is_some();
        if has_status {
            print!("\x1b[1B\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K");
        } else {
            print!("\x1b[1B\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K");
        }

        println!("{}", text);

        if let Some(status_str) = self.format_status_line(width) {
            println!("{}", status_str);
        }
        println!("{}", divider);
        println!();
        println!("{}", divider);
        print!("\x1b[2A\r{}", self.prompt_row(&buf));
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
        let divider = "─".repeat(width).dimmed().to_string();
        if let Some(status_str) = self.format_status_line(width) {
            println!("{}", status_str);
        }
        println!("{}", divider);
        println!();
        println!("{}", divider);
        print!("\x1b[2A\r{}", PROMPT.bold().yellow());
        let _ = io::stdout().flush();
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
        let has_status = self.status.lock().unwrap().is_some();
        if has_status {
            print!("\x1b[1B\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K");
        } else {
            print!("\x1b[1B\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K");
        }
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
    let session_id = resolve_session(store.as_ref(), selection).await?;

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
    let evolver = Arc::new(CodebaseEvolver::new(config.paths.workspace_root.clone()));

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

    // Register self-improvement evolver tools
    registry.register(Arc::new(InspectSourceTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(ProposePatchTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(ApplyUpdateTool::new(evolver.clone()))).await?;

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

    let last_block = Arc::new(Mutex::new(OutputBlockKind::None));
    let last_block_obs = last_block.clone();

    let mut agent = Agent::new(
        provider,
        memory,
        notes,
        evolver,
        registry,
        config.agent.clone(),
    )
    .with_approval(approver, approval_settings(&config))
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: session_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    })
    .with_observer(Arc::new(move |event| match event {
        harness_core::AgentEvent::Thinking(text) => {
            let mut lb = last_block_obs.lock().unwrap();
            if needs_separator(*lb, OutputBlockKind::Thinking) {
                ui_obs.print_above("");
            }
            *lb = OutputBlockKind::Thinking;
            drop(lb);

            let mut out = String::new();
            for line in text.lines() {
                let trimmed = line.trim_end();
                if !trimmed.is_empty() {
                    out.push_str(&format!("  {}\n", trimmed.dimmed()));
                }
            }
            let trimmed_out = out.trim_end();
            if !trimmed_out.is_empty() {
                ui_obs.print_above(trimmed_out);
            }
        }
        harness_core::AgentEvent::ToolExecuting { name, arguments } => {
            let mut lb = last_block_obs.lock().unwrap();
            if needs_separator(*lb, OutputBlockKind::Tool) {
                ui_obs.print_above("");
            }
            *lb = OutputBlockKind::Tool;
            drop(lb);

            ui_obs.print_above(&format_tool_call(&name, &arguments));
        }
        harness_core::AgentEvent::ToolExecuted { is_error, output, .. } => {
            ui_obs.print_above(&format_tool_result(is_error, &output));
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
        for stored in &replayed.history {
            let content = stored.message.content.as_deref().unwrap_or_default();
            match stored.message.role {
                Role::User => println!("{}", right_aligned_user_message(content, width)),
                Role::Assistant if !content.trim().is_empty() => {
                    println!("{}\n{}", LUNA_LABEL.bold().green(), content)
                }
                // Tool traffic and compaction summaries are noise when the transcript is read
                // back: the user cares what was said, not which calls produced it.
                Role::Assistant | Role::Tool | Role::System => {}
            }
        }
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
                Err(_) => break,
            }
        }
    });

    let mut queue: VecDeque<String> = VecDeque::new();
    let mut current_cancel = CancellationToken::new();
    let mut active_turn: Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>> = None;

    loop {
        tokio::select! {
            maybe_approval = approval_rx.recv() => {
                let (request, decision_tx) = match maybe_approval {
                    Some(pair) => pair,
                    None => continue,
                };

                ui.clear_box_on_exit();
                let decision = prompt_approval_box(&mut key_rx, &request).await;
                let target_label = request.target.as_deref().unwrap_or("-");
                if decision == harness_core::ApprovalDecision::Run {
                    println!("{} Approved: {} ({})", "✔".green().bold(), request.tool.bold(), target_label);
                } else {
                    println!("{} Cancelled: {} ({})", "✖".red().bold(), request.tool.bold(), target_label);
                }
                ui.draw_initial_box();
                let _ = decision_tx.send(decision);
            }

            maybe_key = key_rx.recv() => {
                let key = match maybe_key {
                    Some(k) => k,
                    None => break,
                };

                match key {
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

                        if query.eq_ignore_ascii_case("exit") || query.eq_ignore_ascii_case("quit") {
                            ui.clear_box_on_exit();
                            clear_on_exit();
                            break;
                        }

                        if query.eq_ignore_ascii_case("/model")
                            || query.eq_ignore_ascii_case("/models")
                            || query.eq_ignore_ascii_case("/provider")
                        {
                            ui.redraw_input_line();
                            if let Err(e) = handle_model_switch(
                                Some(&mut key_rx),
                                &ui,
                                &config,
                                &cred_manager,
                                &discovery,
                                &agent,
                                &mut current_provider,
                                &mut current_model,
                            ).await {
                                ui.print_above(&format!("{} Failed to switch model: {}", "✖".red().bold(), e));
                            }
                            continue;
                        }

                        if query.starts_with("/reasoning") {
                            let arg = query.strip_prefix("/reasoning").unwrap().trim();
                            let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                            ui.redraw_input_line();
                            if let Err(e) = handle_reasoning_switch(
                                Some(&mut key_rx),
                                &ui,
                                &agent,
                                arg_opt,
                            ).await {
                                ui.print_above(&format!("{} Failed to set reasoning effort: {}", "✖".red().bold(), e));
                            }
                            continue;
                        }

                        if query.starts_with("/name") {
                            let arg = query.strip_prefix("/name").unwrap().trim();
                            let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                            ui.clear_box_on_exit();
                            if let Err(e) = handle_name_command(
                                &ui,
                                store.as_ref(),
                                &session_id,
                                arg_opt,
                            ).await {
                                ui.draw_initial_box();
                                ui.print_above(&format!("{} Failed to set session name: {}", "✖".red().bold(), e));
                            }
                            continue;
                        }

                        let user_header = right_aligned_user_message(&query, get_terminal_width());
                        {
                            let mut lb = last_block.lock().unwrap();
                            if needs_separator(*lb, OutputBlockKind::User) {
                                ui.print_above("");
                            }
                            *lb = OutputBlockKind::User;
                        }
                        ui.print_above(&user_header);

                        if active_turn.is_some() {
                            queue.push_back(query);
                        } else {
                            current_cancel = CancellationToken::new();
                            let cancel_child = current_cancel.child_token();
                            let ag_ref = agent.clone();
                            let q = query.clone();
                            active_turn = Some(tokio::spawn(async move {
                                let ag = ag_ref.lock().await;
                                ag.run_turn(&q, cancel_child).await
                            }));
                        }
                    }
                    Key::CtrlC => {
                        if active_turn.is_some() {
                            current_cancel.cancel();
                            ui.print_above(&format!("{}\n", "✖ Cancelled".red()));
                        } else {
                            ui.clear_box_on_exit();
                            clear_on_exit();
                            break;
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
                        if let Some(compaction) = &outcome.compaction {
                            ui.print_above(&format!(
                                "compacted {} turns into 1 summary (session {})",
                                compaction.turns, compaction.session_id
                            ).dimmed().to_string());
                        }
                        if !outcome.reply.is_empty() {
                            let mut lb = last_block.lock().unwrap();
                            if needs_separator(*lb, OutputBlockKind::Reply) {
                                ui.print_above("");
                            }
                            *lb = OutputBlockKind::Reply;
                        }
                        ui.print_above(&format!("{}\n{}", LUNA_LABEL.bold().green(), outcome.reply));
                        
                        // Update status bar with current context usage
                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                    Ok(Err(HarnessError::Cancelled)) => {
                        ui.print_above(&format!("{}", "✖ Cancelled".red()));
                        
                        // Update status bar
                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                    Ok(Err(e)) => {
                        ui.print_above(&format!("{} Error: {}", "✖".red().bold(), e));
                        
                        // Update status bar
                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                    Err(join_err) => {
                        ui.print_above(&format!("{} Task join error: {}", "✖".red().bold(), join_err));
                        
                        // Update status bar
                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                }

                if let Some(next_query) = queue.pop_front() {
                    current_cancel = CancellationToken::new();
                    let cancel_child = current_cancel.child_token();
                    let ag_ref = agent.clone();
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
            ui.print_above(&format!("{} Session name cleared", "✔".green().bold()));
        } else {
            ui.print_above(&format!(
                "{} This session has no name — set one with {}",
                "•".cyan(),
                "/name <name>".bold()
            ));
        }
    } else if arg.is_some() {
        ui.print_above(&format!(
            "{} Session name set to '{}'",
            "✔".green().bold(),
            name.cyan()
        ));
    } else {
        ui.print_above(&format!("{} Session name: {}", "•".cyan(), name.cyan()));
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
                    ui.print_above(&format!(
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

    ui.print_above(&format!(
        "{} Reasoning effort set to '{}'",
        "✔".green().bold(),
        effort_label.yellow()
    ));

    Ok(())
}

async fn prompt_approval_box(
    key_rx: &mut mpsc::UnboundedReceiver<Key>,
    request: &ApprovalRequest,
) -> harness_core::ApprovalDecision {
    let width = get_terminal_width().min(76).max(50);
    let mut selected = 0; // 0 = Approve, 1 = Cancel

    // Drain any leftover keys in channel
    while key_rx.try_recv().is_ok() {}

    let (initial_text, mut line_count) = render_approval_box(request, selected, width);
    print!("{}", initial_text);
    let _ = io::stdout().flush();

    while let Some(key) = key_rx.recv().await {
        match key {
            Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown | Key::Tab | Key::BackTab => {
                selected = 1 - selected;
            }
            Key::Char('y') | Key::Char('Y') | Key::Char('a') | Key::Char('A') => {
                selected = 0;
                break;
            }
            Key::Char('n') | Key::Char('N') | Key::Char('c') | Key::Char('C') | Key::Escape => {
                selected = 1;
                break;
            }
            Key::Enter => {
                break;
            }
            _ => continue,
        }

        print!("\r\x1b[{}A", line_count);
        let (new_text, new_count) = render_approval_box(request, selected, width);
        print!("{}", new_text);
        line_count = new_count;
        let _ = io::stdout().flush();
    }

    // Erase the approval box from screen
    print!("\r\x1b[{}A", line_count);
    for _ in 0..line_count {
        print!("\x1b[2K\x1b[1B");
    }
    print!("\r\x1b[{}A", line_count);
    let _ = io::stdout().flush();

    if selected == 0 {
        harness_core::ApprovalDecision::Run
    } else {
        harness_core::ApprovalDecision::Deny
    }
}

/// Longest a tool argument may render before it is ellipsised, so one call stays on one line.
const MAX_TOOL_ARG_CHARS: usize = 68;

/// Longest a tool result summary line may render before it is ellipsised.
const MAX_TOOL_RESULT_CHARS: usize = 120;

/// A result this short is printed in full; anything longer collapses to its first line plus
/// a line count so a large tool payload cannot flood the transcript.
const MAX_INLINE_RESULT_LINES: usize = 3;

/// Render a tool call as one transcript line that names the tool *and* the operation it is
/// performing. A search spells out its mode (`search_files(content)`) so the reader never has
/// to guess whether a scan went over file contents or file names.
fn format_tool_call(name: &str, arguments: &serde_json::Value) -> String {
    let bullet = "•".cyan();
    // Absent and empty are the same thing to a reader: a model that sends `"path": ""` means
    // "no path given", not "a path called nothing".
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let number = |key: &str| arguments.get(key).and_then(|v| v.as_u64());
    let suffix = |detail: String| format!(" {}", detail.dimmed());
    let quoted = |value: &str| format!("\"{}\"", truncate_visible(value, MAX_TOOL_ARG_CHARS)).cyan();

    match name {
        "terminal" | "shell" | "bash" => {
            if let Some(command) = text("command") {
                let first_line = command.lines().next().unwrap_or(command).trim();
                let cwd = text("workdir")
                    .map(|w| suffix(format!("in {}", w)))
                    .unwrap_or_default();
                return format!(
                    "{} {} {}{}",
                    bullet,
                    "$".bold().cyan(),
                    truncate_visible(first_line, MAX_TOOL_ARG_CHARS).bold(),
                    cwd
                );
            }
        }
        "read_file" => {
            if let Some(path) = text("path") {
                let range = match (number("offset"), number("limit")) {
                    (Some(offset), Some(limit)) => {
                        suffix(format!("lines {}-{}", offset, offset + limit.saturating_sub(1)))
                    }
                    (Some(offset), None) => suffix(format!("from line {}", offset)),
                    (None, Some(limit)) => suffix(format!("first {} lines", limit)),
                    _ => String::new(),
                };
                return format!("{} {} {}{}", bullet, "read".bold(), path.cyan(), range);
            }
        }
        "write_file" => {
            if let Some(path) = text("path") {
                let lines = text("content").map(|c| c.lines().count()).unwrap_or(0);
                return format!(
                    "{} {} {}{}",
                    bullet,
                    "write".bold(),
                    path.cyan(),
                    suffix(format!("{} lines", lines))
                );
            }
        }
        "patch" => {
            if let Some(path) = text("path") {
                let every = match arguments.get("replace_all").and_then(|v| v.as_bool()) {
                    Some(true) => suffix("all occurrences".to_string()),
                    _ => String::new(),
                };
                return format!("{} {} {}{}", bullet, "patch".bold(), path.cyan(), every);
            }
        }
        "search_files" => {
            let pattern = text("pattern").unwrap_or("");
            let path = text("path").unwrap_or(".");
            let mode = text("output_mode").unwrap_or("content");
            let include = text("file_glob")
                // `*` filters nothing, so it carries no information for the reader.
                .filter(|glob| *glob != "*")
                .map(|g| suffix(format!("matching {}", g)))
                .unwrap_or_default();
            return format!(
                "{} {} {} {} {}{}",
                bullet,
                format!("search_files({})", mode).bold(),
                quoted(pattern),
                "in".dimmed(),
                path.cyan(),
                include
            );
        }
        "save_memory" => {
            let subject = text("subject").unwrap_or("?");
            let predicate = text("predicate").unwrap_or("?");
            let object = text("object").unwrap_or("?");
            return format!(
                "{} {} {}{}{}",
                bullet,
                "save_memory".bold(),
                format!("{}.{}", subject, predicate).cyan(),
                " = ".dimmed(),
                quoted(object)
            );
        }
        "update_memory" => {
            if let Some(id) = text("memory_id") {
                return format!("{} {} {}", bullet, "update_memory".bold(), id.cyan());
            }
        }
        "forget_memory" => {
            if let Some(id) = text("memory_id") {
                return format!("{} {} {}", bullet, "forget_memory".bold(), id.cyan());
            }
        }
        "save_note" => {
            let title = text("title").unwrap_or("");
            let lines = text("content").map(|c| c.lines().count()).unwrap_or(0);
            return format!(
                "{} {} {}{}",
                bullet,
                "save_note".bold(),
                quoted(title),
                suffix(format!("{} lines", lines))
            );
        }
        "search_notes" => {
            let query = text("query").unwrap_or("");
            let limit = number("limit")
                .map(|l| suffix(format!("limit {}", l)))
                .unwrap_or_default();
            return format!("{} {} {}{}", bullet, "search_notes".bold(), quoted(query), limit);
        }
        "read_note" | "update_note" => {
            if let Some(id) = text("id") {
                return format!("{} {} {}", bullet, name.bold(), id.cyan());
            }
        }
        "inspect_harness_source" => {
            if let Some(path) = text("path") {
                return format!("{} {} {}", bullet, name.bold(), path.cyan());
            }
        }
        "propose_harness_patch" => {
            if let Some(path) = text("target_file") {
                let lines = text("content").map(|c| c.lines().count()).unwrap_or(0);
                return format!(
                    "{} {} {}{}",
                    bullet,
                    name.bold(),
                    path.cyan(),
                    suffix(format!("{} lines", lines))
                );
            }
        }
        "apply_harness_update" => {
            if let Some(id) = text("patch_id") {
                return format!("{} {} {}", bullet, name.bold(), id.cyan());
            }
        }
        _ => {}
    }

    // MCP tools arrive as `mcp__server__tool`.
    let display_name = match name.strip_prefix("mcp__") {
        Some(stripped) => stripped.replace("__", "/"),
        None => name.to_string(),
    };

    // Fallback: `tool arg=value`, skipping nulls so unset optionals stay invisible.
    let args_str = match arguments.as_object() {
        Some(object) => {
            let parts: Vec<String> = object
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| {
                    let value = match v {
                        serde_json::Value::String(s) => {
                            let first_line = s.lines().next().unwrap_or(s);
                            format!("\"{}\"", truncate_visible(first_line, MAX_TOOL_ARG_CHARS))
                        }
                        other => other.to_string(),
                    };
                    format!("{}={}", k, value)
                })
                .collect();
            if parts.is_empty() {
                String::new()
            } else {
                format!(" {}", parts.join(" "))
            }
        }
        None if arguments.is_null() => String::new(),
        None => format!(" {}", arguments),
    };

    format!("{} {}{}", bullet, display_name.bold(), args_str.dimmed())
}

fn format_tool_result(is_error: bool, output: &str) -> String {
    let trimmed = output.trim();
    if is_error {
        let first_line = trimmed.lines().next().unwrap_or(trimmed).trim();
        return format!(
            "  {} {}",
            "✖".red(),
            truncate_visible(first_line, MAX_TOOL_RESULT_CHARS).red()
        );
    }

    if trimmed.is_empty() {
        return format!("  {} {}", "✓".green(), "done".dimmed());
    }

    let line_count = trimmed.lines().count();
    if line_count == 1 {
        return format!(
            "  {} {}",
            "✓".green(),
            truncate_visible(trimmed, MAX_TOOL_RESULT_CHARS).dimmed()
        );
    }

    if line_count <= MAX_INLINE_RESULT_LINES && trimmed.len() <= MAX_TOOL_RESULT_CHARS {
        let mut out = String::new();
        for (i, line) in trimmed.lines().enumerate() {
            if i == 0 {
                out.push_str(&format!("  {} {}\n", "✓".green(), line.dimmed()));
            } else {
                out.push_str(&format!("    {}\n", line.dimmed()));
            }
        }
        return out.trim_end().to_string();
    }

    let first = trimmed.lines().next().unwrap_or("").trim();
    format!(
        "  {} {} {}",
        "✓".green(),
        truncate_visible(first, MAX_TOOL_ARG_CHARS).dimmed(),
        format!("(+{} lines)", line_count - 1).dimmed()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, arguments: serde_json::Value) -> String {
        format_tool_call(name, &arguments)
    }

    #[test]
    fn terminal_call_shows_the_command_and_directory() {
        let line = call("terminal", json!({ "command": "cargo test\n--all", "workdir": "/repo" }));
        assert!(line.contains("cargo test"), "got: {}", line);
        assert!(line.contains("in /repo"), "got: {}", line);
        assert!(!line.contains("--all"), "only the first command line is shown");
    }

    #[test]
    fn read_call_shows_the_line_range() {
        assert!(call("read_file", json!({ "path": "src/lib.rs", "offset": 10, "limit": 20 }))
            .contains("lines 10-29"));
        assert!(call("read_file", json!({ "path": "src/lib.rs" })).contains("src/lib.rs"));
    }

    #[test]
    fn write_call_shows_the_payload_size() {
        let line = call("write_file", json!({ "path": "a.rs", "content": "one\ntwo\nthree" }));
        assert!(line.contains("3 lines"), "got: {}", line);
    }

    #[test]
    fn patch_call_flags_replace_all() {
        let one = call("patch", json!({ "path": "a.rs", "old_string": "x", "new_string": "y" }));
        assert!(!one.contains("all occurrences"), "got: {}", one);

        let all = call(
            "patch",
            json!({ "path": "a.rs", "old_string": "x", "new_string": "y", "replace_all": true }),
        );
        assert!(all.contains("all occurrences"), "got: {}", all);
    }

    /// The reader must be able to tell which search mode ran, and empty optionals must not
    /// render as blank gaps.
    #[test]
    fn search_call_names_the_mode_and_defaults_the_path() {
        let content = call("search_files", json!({ "pattern": "needle", "path": "", "file_glob": "*" }));
        assert!(content.contains("search_files(content)"), "got: {}", content);
        assert!(content.contains("\"needle\""), "got: {}", content);
        assert!(content.contains("in ."), "an empty path renders as the workspace root");
        assert!(!content.contains("matching"), "a `*` glob filters nothing and is hidden");

        let files = call("search_files", json!({ "pattern": "needle", "output_mode": "files_only" }));
        assert!(files.contains("search_files(files_only)"), "got: {}", files);

        let scoped = call("search_files", json!({ "pattern": "needle", "file_glob": "*.rs" }));
        assert!(scoped.contains("matching *.rs"), "got: {}", scoped);
    }

    #[test]
    fn memory_and_note_calls_show_their_subject() {
        let memory = call(
            "save_memory",
            json!({ "subject": "user", "predicate": "editor", "object": "neovim" }),
        );
        assert!(memory.contains("user.editor"), "got: {}", memory);
        assert!(memory.contains("neovim"), "got: {}", memory);

        assert!(call("search_notes", json!({ "query": "budget", "limit": 5 })).contains("limit 5"));
        assert!(call("read_note", json!({ "id": "note_123" })).contains("note_123"));
    }

    #[test]
    fn unknown_and_mcp_tools_fall_back_to_named_arguments() {
        let mcp = call("mcp__filesystem__read", json!({ "path": "/etc/hosts", "missing": null }));
        assert!(mcp.contains("filesystem/read"), "got: {}", mcp);
        assert!(mcp.contains("path=\"/etc/hosts\""), "got: {}", mcp);
        assert!(!mcp.contains("missing"), "null arguments are hidden");
    }

    #[test]
    fn long_arguments_are_clipped_to_one_line() {
        let line = call("terminal", json!({ "command": "x".repeat(500) }));
        assert!(
            line.chars().count() < 500,
            "a long command must not wrap the transcript: {} chars",
            line.chars().count()
        );
        assert!(line.contains('…'));
    }

    #[test]
    fn long_results_collapse_to_a_line_count() {
        let output = (0..40).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let rendered = format_tool_result(false, &output);
        assert!(rendered.contains("line 0"), "got: {}", rendered);
        assert!(rendered.contains("(+39 lines)"), "got: {}", rendered);
    }

    #[test]
    fn errors_report_the_first_line() {
        let rendered = format_tool_result(true, "boom: file not found\nmore detail");
        assert!(rendered.contains("boom: file not found"), "got: {}", rendered);
        assert!(!rendered.contains("more detail"), "got: {}", rendered);
    }
}
