use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use colored::Colorize;
use dialoguer::console::{Key, Term};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use harness_core::{
    Agent, Config, DefaultToolRegistry, HarnessError, ModelProvider, SessionBinding, ToolRegistry,
    TurnOutcome,
};
use harness_evolver::{ApplyUpdateTool, CodebaseEvolver, InspectSourceTool, ProposePatchTool};
use harness_memory::{
    ForgetMemoryTool, SaveMemoryTool, SqliteMemoryEngine, UpdateMemoryTool,
};
use harness_notes::{
    ReadNoteTool, SaveNoteTool, SearchNotesTool, SqliteNotesEngine, UpdateNoteTool,
};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

use super::approval::{approval_settings, build_approver};
use super::sessions::{open_store, resolve_session, SessionSelection};

/// `config` is already resolved (defaults → file → `LUNA_*` → flags); this function only reads it,
/// so the engines/filesystem paths come from one place instead of each engine guessing `~/.luna/…`.
fn get_terminal_width() -> usize {
    Term::stdout()
        .size_checked()
        .map(|(_, w)| w as usize)
        .filter(|&w| w > 0)
        .unwrap_or(80)
}

#[derive(Clone)]
struct ChatUi {
    input_buf: Arc<Mutex<String>>,
    is_attended: bool,
}

impl ChatUi {
    fn new(is_attended: bool) -> Self {
        Self {
            input_buf: Arc::new(Mutex::new(String::new())),
            is_attended,
        }
    }

    fn print_above(&self, text: &str) {
        if !self.is_attended {
            println!("{}", text);
            return;
        }
        let buf = self.input_buf.lock().unwrap();
        let width = get_terminal_width();
        let divider = "─".repeat(width).dimmed().to_string();

        print!("\r\x1b[2K\x1b[1B\r\x1b[2K\x1b[2A\r\x1b[2K");
        println!("{}", text);
        println!("{}", divider);
        println!();
        println!("{}", divider);
        print!("\x1b[2A\r{}{}", "User > ".bold().yellow(), *buf);
        let _ = io::stdout().flush();
    }

    fn redraw_input_line(&self) {
        if !self.is_attended {
            return;
        }
        let buf = self.input_buf.lock().unwrap();
        print!("\r\x1b[K{}{}", "User > ".bold().yellow(), *buf);
        let _ = io::stdout().flush();
    }

    fn draw_initial_box(&self) {
        if !self.is_attended {
            print!("{}", "User > ".bold().yellow());
            let _ = io::stdout().flush();
            return;
        }
        let width = get_terminal_width();
        let divider = "─".repeat(width).dimmed().to_string();
        println!("{}", divider);
        println!();
        println!("{}", divider);
        print!("\x1b[2A\r{}", "User > ".bold().yellow());
        let _ = io::stdout().flush();
    }

    fn clear_box_on_exit(&self) {
        if self.is_attended {
            print!("\r\x1b[2K\x1b[1B\r\x1b[2K\x1b[2A\r\x1b[2K");
            let _ = io::stdout().flush();
        }
    }
}

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
    let provider_id = config.model.provider.clone();
    let model = config.model.default.clone();

    let provider = Arc::new(GenericProviderClient::new(
        provider_id.clone(),
        model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&provider_id),
    )?);

    // Every turn is appended to this session; a resumed one replays its recent transcript.
    let store = Arc::new(open_store(&config)?);
    let session_id = resolve_session(store.as_ref(), selection).await?;
    println!("Session:  {}", session_id.green());
    println!("Resume:   luna --session {}\n", session_id);

    // The compaction call is a plain model call: one provider, one model, resolved from
    // `agent.compaction_provider` / `agent.compaction_model` (both default to the session's).
    let compaction_provider_id = config
        .agent
        .compaction_provider
        .clone()
        .unwrap_or_else(|| provider_id.clone());
    let compaction_model = config
        .agent
        .compaction_model
        .clone()
        .unwrap_or_else(|| model.clone());
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

    let mut agent = Agent::new(
        provider,
        memory,
        notes,
        evolver,
        registry,
        config.agent.clone(),
    )
    .with_approval(build_approver(&config), approval_settings(&config))
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: session_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    })
    .with_observer(Arc::new(move |event| match event {
        harness_core::AgentEvent::Thinking(text) => {
            let mut out = format!("{}\n", "Thinking:".bold().dimmed());
            for line in text.lines() {
                out.push_str(&format!("  {}\n", line.dimmed()));
            }
            ui_obs.print_above(&out);
        }
        harness_core::AgentEvent::ToolExecuting { name, arguments } => {
            let args_preview = if arguments.is_null() || arguments == serde_json::json!({}) {
                String::new()
            } else if let Some(obj) = arguments.as_object() {
                let parts: Vec<String> = obj
                    .iter()
                    .map(|(k, v)| {
                        let val_str = match v {
                            serde_json::Value::String(s) => {
                                if s.len() > 60 {
                                    format!("\"{}…\"", &s[..57])
                                } else {
                                    format!("\"{}\"", s)
                                }
                            }
                            other => other.to_string(),
                        };
                        format!("{}={}", k, val_str)
                    })
                    .collect();
                parts.join(" ")
            } else {
                arguments.to_string()
            };
            ui_obs.print_above(&format!("{} {} {}", "•".cyan(), name.bold(), args_preview.dimmed()));
        }
        harness_core::AgentEvent::ToolExecuted { is_error, output, .. } => {
            let trimmed = output.trim();
            let preview = if trimmed.lines().count() > 3 || trimmed.len() > 160 {
                let first = trimmed.lines().next().unwrap_or("").trim();
                format!("{} ({} lines)", first, trimmed.lines().count())
            } else if trimmed.is_empty() {
                "done".to_string()
            } else {
                trimmed.to_string()
            };

            let line = if is_error {
                format!("  {} {}", "✖".red(), preview.red())
            } else {
                format!("  {} {}", "✓".green(), preview.dimmed())
            };
            ui_obs.print_above(&line);
        }
    }));
    if let Some(setup) = compaction {
        agent = agent.with_compaction(setup);
    }

    if !is_attended {
        let cancel_token = CancellationToken::new();
        loop {
            print!("{}", "User > ".bold().yellow());
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
            match agent.run_turn(query, cancel_token.child_token()).await {
                Ok(outcome) => {
                    println!("\n{}\n{}\n", "Luna >".bold().green(), outcome.reply);
                }
                Err(e) => {
                    eprintln!("\n{} Error: {}\n", "✖".red().bold(), e);
                }
            }
        }
        return Ok(());
    }

    ui.draw_initial_box();

    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<Key>();
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

    let agent = Arc::new(tokio::sync::Mutex::new(agent));
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut current_cancel = CancellationToken::new();
    let mut active_turn: Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>> = None;
    let mut first_turn = true;

    loop {
        tokio::select! {
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

                        let user_header = format!("{} {}", "User > ".bold().yellow(), query);
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
                        if first_turn {
                            if let Some(replay) = &outcome.replayed {
                                if replay.replayed < replay.total {
                                    ui.print_above(&format!(
                                        "resumed {}: replaying {} of {} messages",
                                        replay.session_id, replay.replayed, replay.total
                                    ).dimmed().to_string());
                                }
                            }
                        }
                        if let Some(compaction) = &outcome.compaction {
                            ui.print_above(&format!(
                                "compacted {} turns into 1 summary (session {})",
                                compaction.turns, compaction.session_id
                            ).dimmed().to_string());
                        }
                        ui.print_above(&format!("\n{}\n{}\n", "Luna >".bold().green(), outcome.reply));
                    }
                    Ok(Err(HarnessError::Cancelled)) => {
                        ui.print_above(&format!("{}\n", "✖ Cancelled".red()));
                    }
                    Ok(Err(e)) => {
                        ui.print_above(&format!("\n{} Error: {}\n", "✖".red().bold(), e));
                    }
                    Err(join_err) => {
                        ui.print_above(&format!("\n{} Task join error: {}\n", "✖".red().bold(), join_err));
                    }
                }
                first_turn = false;

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
    Ok(())
}
