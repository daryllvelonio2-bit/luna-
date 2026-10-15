use std::io::{self, Write};
use std::sync::Arc;
use colored::Colorize;
use tokio_util::sync::CancellationToken;

use harness_core::{
    Agent, Config, DefaultToolRegistry, HarnessError, ModelProvider, SessionBinding, ToolRegistry,
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
    dialoguer::console::Term::stdout()
        .size_checked()
        .map(|(_, w)| w as usize)
        .filter(|&w| w > 0)
        .unwrap_or(80)
}

fn draw_input_box(is_attended: bool) {
    if !is_attended {
        print!("{}", "User > ".bold().yellow());
        let _ = io::stdout().flush();
        return;
    }
    let width = get_terminal_width();
    let divider = "─".repeat(width).dimmed();
    println!("{}", divider);
    println!();
    println!("{}", divider);
    print!("\x1b[2A\r{}", "User > ".bold().yellow());
    let _ = io::stdout().flush();
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
    .with_observer(Arc::new(|event| match event {
        harness_core::AgentEvent::Thinking(text) => {
            println!("{}", "Thinking:".bold().dimmed());
            for line in text.lines() {
                println!("  {}", line.dimmed());
            }
            println!();
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
            println!("{} {} {}", "•".cyan(), name.bold(), args_preview.dimmed());
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

            if is_error {
                println!("  {} {}", "✖".red(), preview.red());
            } else {
                println!("  {} {}", "✓".green(), preview.dimmed());
            }
        }
    }));
    if let Some(setup) = compaction {
        agent = agent.with_compaction(setup);
    }

    let is_attended = dialoguer::console::Term::stdout().features().is_attended();
    let cancel_token = CancellationToken::new();
    let mut first_turn = true;

    draw_input_box(is_attended);

    loop {
        let mut input = String::new();
        let read = io::stdin().read_line(&mut input).map_err(|e| HarnessError::Io(e.to_string()))?;
        // EOF (piped or redirected stdin): without this the loop would spin on empty input
        // forever instead of ending the session.
        if read == 0 {
            if is_attended {
                print!("\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K");
                let _ = io::stdout().flush();
            }
            clear_on_exit();
            break;
        }
        let query = input.trim();

        if query.eq_ignore_ascii_case("exit") || query.eq_ignore_ascii_case("quit") {
            if is_attended {
                print!("\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K");
                let _ = io::stdout().flush();
            }
            clear_on_exit();
            break;
        }

        if query.is_empty() {
            if is_attended {
                print!("\x1b[1A\r\x1b[K{}", "User > ".bold().yellow());
                let _ = io::stdout().flush();
            } else {
                print!("{}", "User > ".bold().yellow());
                let _ = io::stdout().flush();
            }
            continue;
        }

        if is_attended {
            print!("\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K");
            println!("{} {}\n", "User > ".bold().yellow(), query);
            let _ = io::stdout().flush();
        }

        match agent.run_turn(query, cancel_token.child_token()).await {
            Ok(outcome) => {
                // The replay line belongs to the resume, not to every turn: print it once.
                if first_turn {
                    if let Some(replay) = &outcome.replayed {
                        if replay.replayed < replay.total {
                            println!(
                                "{}",
                                format!(
                                    "resumed {}: replaying {} of {} messages (session.resume_max_messages = {})",
                                    replay.session_id,
                                    replay.replayed,
                                    replay.total,
                                    config.session.resume_max_messages
                                )
                                .dimmed()
                            );
                        } else if replay.total > 0 {
                            println!(
                                "{}",
                                format!(
                                    "resumed {}: {} messages replayed",
                                    replay.session_id, replay.replayed
                                )
                                .dimmed()
                            );
                        }
                    }
                }
                if let Some(compaction) = &outcome.compaction {
                    println!(
                        "{}",
                        format!(
                            "compacted {} turns into 1 summary (session {})",
                            compaction.turns, compaction.session_id
                        )
                        .dimmed()
                    );
                }
                println!("\n{}\n{}\n", "Luna >".bold().green(), outcome.reply);
            }
            Err(e) => {
                eprintln!("\n{} Error: {}\n", "✖".red().bold(), e);
            }
        }
        first_turn = false;
        draw_input_box(is_attended);
    }

    clear_on_exit();
    Ok(())
}
