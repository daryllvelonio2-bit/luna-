//! The slash commands that act on the session itself: fork, rewind, diff, name, new, delete,
//! switch, summarize-and-new, compact, and auto-compact.
//!
//! All of these rewrite *which* conversation the agent is bound to, or the transcript and
//! workspace attached to it: after one runs, the screen, the todo scope and the stored session
//! have to still agree with each other.

use colored::Colorize;
use std::sync::Arc;
use tokio::sync::mpsc;

use harness_core::{
    Config, HarnessError, Message, Role, SessionBinding, SessionStore, SqliteSessionStore,
};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

use crate::commands::chat::keys::AppKey;
use crate::commands::chat::render::{parse_turns_from_history, render_transcript, TurnRecord};
use crate::commands::chat::todo;
use crate::commands::chat::ui::{get_terminal_width, ChatUi};
use crate::commands::chat::ChatAgent;
use crate::commands::context_window::context_window_for;
use crate::commands::sessions::{find_by_name, resolve_session, SessionSelection};

use super::picker::select_from_rx;
use super::value_prompt::{prompt_value, PERCENT_PROMPT, THRESHOLD_PROMPT};

pub(crate) async fn handle_fork_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    session_id: &mut String,
    todo_scope: &harness_core::tools::TodoScope,
    config: &Config,
    title: Option<&str>,
) -> Result<(), HarnessError> {
    match store.fork_session(session_id, title).await {
        Ok(new_id) => {
            let old_id = session_id.clone();
            let old_list = todo_scope.path();
            *session_id = new_id.clone();
            todo_scope.set_session(session_id.as_str());
            let _ = std::fs::create_dir_all(config.paths.todos_dir.as_path());
            let _ = std::fs::copy(&old_list, todo_scope.path());
            ui.set_session_name(&format!("fork:{}", &new_id[..new_id.len().min(8)]));
            todo::refresh_panel(ui, todo_scope);
            ui.emit_above(&format!(
                "{} Forked session {} -> {}",
                "✔".green().bold(),
                old_id.dimmed(),
                session_id.cyan().bold()
            ));
        }
        Err(e) => {
            ui.emit_above(&format!("{} Fork failed: {}", "✖".red().bold(), e));
        }
    }
    Ok(())
}

/// Draw the transcript.
///
/// The todo panel is not part of it: the panel is chrome, drawn above the composer.
pub(crate) fn draw_transcript(turns: &[TurnRecord], expanded: bool) {
    render_transcript(turns, expanded, get_terminal_width());
}

/// `/rewind [turn]`: list this session's file checkpoints, or restore one turn's files.
///
/// Restoring is about *files*. The transcript is untouched — a rewind puts the workspace back so
/// the conversation can continue from a known state, it does not un-say what was said.
pub(crate) async fn handle_rewind_command(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let controller = {
        let agent = agent.lock().await;
        agent.checkpoints()
    };
    let Some(controller) = controller else {
        ui.emit_above(&format!(
            "{} Checkpoints are off — enable them with {}",
            "•".cyan(),
            "checkpoints.enabled = true".bold()
        ));
        return Ok(());
    };
    if !controller.enabled() {
        ui.emit_above(&format!(
            "{} Checkpoints are off — enable them with {}",
            "•".cyan(),
            "checkpoints.enabled = true".bold()
        ));
        return Ok(());
    }

    let store = controller.store();
    let checkpoints = store.list(session_id)?;

    let Some(seq) = arg.and_then(|raw| raw.parse::<u32>().ok()) else {
        if checkpoints.is_empty() {
            ui.emit_above(&format!(
                "{} No checkpoints yet — the next turn that writes a file creates one",
                "•".cyan()
            ));
            return Ok(());
        }
        ui.emit_above(&format!(
            "{} {} checkpoint(s) for this session (newest last):",
            "•".cyan(),
            checkpoints.len()
        ));
        let len = checkpoints.len();
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            // The counts come from the *bytes*, so a turn that wrote a file back unchanged reads
            // as unchanged rather than as a scary modification.
            let stat = store
                .diff(session_id, checkpoint.seq)
                .map(|turn| turn.summary().stat_line())
                .unwrap_or_else(|_| format!("{} file(s)", checkpoint.files.len()));
            let marker = if index + 1 == len { " (latest)" } else { "" };
            let prompt = checkpoint
                .prompt
                .as_deref()
                .map(|prompt| {
                    let flat = prompt.replace('\n', " ");
                    let short: String = flat.chars().take(60).collect();
                    format!(" — {}", short.dimmed())
                })
                .unwrap_or_default();
            ui.emit_above(&format!(
                "    {}  {}{}{}",
                format!("turn {}", checkpoint.seq).bold(),
                stat.cyan(),
                marker.green(),
                prompt
            ));
        }
        ui.emit_above(&format!(
            "    Restore one with {}",
            "/rewind <turn>".to_string().bold()
        ));
        return Ok(());
    };

    let report = store.restore(session_id, seq)?;
    if report.is_empty() {
        ui.emit_above(&format!(
            "{} Turn {} changed nothing restorable",
            "•".cyan(),
            seq
        ));
    } else {
        ui.emit_above(&format!(
            "{} Rewound turn {}: {} file(s) restored, {} deleted",
            "✔".green().bold(),
            seq,
            report.restored.len(),
            report.deleted.len()
        ));
        for path in report.restored.iter().chain(report.deleted.iter()) {
            ui.emit_above(&format!("    {}", path.dimmed()));
        }
    }
    for skipped in &report.skipped {
        ui.emit_above(&format!("    {} {}", "skipped".yellow(), skipped.dimmed()));
    }
    ui.emit_above(&format!(
        "{} The transcript is unchanged — files only",
        "•".cyan()
    ));
    Ok(())
}

/// `/diff [turn|--all]`: what the workspace actually changed, read from the turn's checkpoint.
///
/// `/rewind` is a write. This is the read that belongs before it: the same snapshot, diffed against
/// the files as they are now, so a person can see what a turn did before deciding to undo it.
pub(crate) async fn handle_diff_command(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let controller = {
        let agent = agent.lock().await;
        agent.checkpoints()
    };
    let Some(controller) = controller.filter(|controller| controller.enabled()) else {
        ui.emit_above(&format!(
            "{} Checkpoints are off — enable them with {}",
            "•".cyan(),
            "checkpoints.enabled = true".bold()
        ));
        return Ok(());
    };

    let store = controller.store();
    let checkpoints = store.list(session_id)?;
    if checkpoints.is_empty() {
        ui.emit_above(&format!(
            "{} No checkpoints yet — the next turn that writes a file creates one",
            "•".cyan()
        ));
        return Ok(());
    }

    let raw = arg.unwrap_or("").trim();
    let all = raw == "--all" || raw == "all";
    let seq = if all {
        None
    } else {
        match raw.split_whitespace().next().map(str::parse::<u32>) {
            Some(Ok(seq)) => Some(seq),
            _ => checkpoints.last().map(|checkpoint| checkpoint.seq),
        }
    };

    let turns = match seq {
        Some(seq) => vec![store.diff(session_id, seq)?],
        None => store.diff_all(session_id)?,
    };

    for turn in &turns {
        let summary = turn.summary();
        let prompt = turn
            .prompt
            .as_deref()
            .map(|prompt| prompt.replace('\n', " "))
            .map(|prompt| format!(" — {}", prompt.dimmed()))
            .unwrap_or_default();
        ui.emit_above(&format!(
            "{}  {}{}",
            format!("turn {}", turn.seq).bold(),
            summary.stat_line().cyan(),
            prompt
        ));

        if turn.is_empty() && turn.unrecorded.is_empty() {
            ui.emit_above(&format!(
                "    {}",
                "nothing changed (the workspace matches this turn's snapshot)".dimmed()
            ));
            continue;
        }

        for file in turn.changed() {
            ui.emit_above(&format!(
                "    {} {:<9} {} {}",
                file.kind.marker(),
                file.kind.as_str().yellow(),
                file.path.bold(),
                format!("+{} -{}", file.additions, file.deletions).dimmed()
            ));
            if let Some(note) = &file.note {
                ui.emit_above(&format!("        {}", note.dimmed()));
            }
            // Hunks are the point of a diff, but a chat pane is not a pager: cap them and say so
            // rather than letting one regenerated lockfile scroll the conversation away.
            let mut budget = DIFF_HUNK_BUDGET;
            for line in file.diff.trim_end().lines() {
                if budget == 0 {
                    ui.emit_above(&format!(
                        "        {}",
                        "… more hunks not shown in chat — pipe `luna diff` for all of them"
                            .dimmed()
                    ));
                    break;
                }
                budget -= 1;
                let painted =
                    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@")
                    {
                        line.cyan()
                    } else if line.starts_with('+') {
                        line.green()
                    } else if line.starts_with('-') {
                        line.red()
                    } else {
                        line.normal()
                    };
                ui.emit_above(&format!("        {}", painted));
            }
        }
        if !turn.unrecorded.is_empty() {
            ui.emit_above(&format!(
                "    {}",
                format!(
                    "! {} file(s) changed by a shell command — no snapshot, /rewind cannot undo these:",
                    turn.unrecorded.len()
                )
                .yellow()
            ));
            for file in &turn.unrecorded {
                ui.emit_above(&format!(
                    "      {:<9} {}",
                    file.kind.as_str(),
                    file.path.dimmed()
                ));
            }
        }
        ui.emit_above("");
    }

    Ok(())
}

/// How many diff lines a chat reply shows per turned file.
const DIFF_HUNK_BUDGET: usize = 40;

/// `/name [name]`: set the current session's user-assigned name, or report it when no name is given.
pub(crate) async fn handle_name_command(
    ui: &ChatUi,
    store: &SqliteSessionStore,
    session_id: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    if let Some(raw) = arg {
        store.set_name(session_id, raw).await?;
    }

    let name = store.get(session_id).await?.name;
    ui.set_session_name(&name);

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
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_new_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &mut String,
    current_provider: &str,
    current_model: &str,
    todo_scope: &harness_core::tools::TodoScope,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let new_id = resolve_session(store.as_ref(), SessionSelection::New).await?;
    let name = arg.map(|s| s.trim()).unwrap_or("");
    if !name.is_empty() {
        store.set_name(&new_id, name).await?;
    }
    // The new session inherits the model currently in use, so the switch carried into it is
    // not lost the next time this session is resumed.
    store
        .set_model(&new_id, current_provider, current_model)
        .await?;

    *session_id = new_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: new_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let context_max = context_window_for(config, current_provider, current_model);
    ui.set_status(current_model, 0, context_max);
    ui.set_session_name(name);

    // The new conversation starts with an empty list, and the list belongs to the
    // session: the scope follows the session, and the panel above the composer goes
    // with it.
    todo_scope.set_session(&new_id);
    todo::refresh_panel(ui, todo_scope);

    if ui.is_attended {
        crate::commands::clear_screen();
        ui.draw_landing();
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

/// `/delete`: permanently delete the current chat session and start a fresh one.
pub(crate) async fn handle_delete_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &mut String,
    current_provider: &str,
    current_model: &str,
) -> Result<(), HarnessError> {
    let old_id = session_id.clone();
    let old_name = store.get(&old_id).await.map(|s| s.name).unwrap_or_default();

    // Delete current session from SQLite store
    store.delete_session(&old_id).await?;

    // Start a fresh new session
    let new_id = resolve_session(store.as_ref(), SessionSelection::New).await?;
    store
        .set_model(&new_id, current_provider, current_model)
        .await?;

    *session_id = new_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: new_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let context_max = context_window_for(config, current_provider, current_model);
    ui.set_status(current_model, 0, context_max);
    ui.set_session_name("");

    let desc = if old_name.is_empty() {
        old_id.cyan().to_string()
    } else {
        format!("'{}' ({})", old_name.cyan(), old_id.dimmed())
    };

    if ui.is_attended {
        crate::commands::clear_screen();
        ui.draw_landing();
        ui.draw_initial_box();
        ui.emit_above(&format!(
            "{} Deleted session {} and started fresh session {}",
            "✔".green().bold(),
            desc,
            new_id.cyan()
        ));
    } else {
        println!(
            "{} Deleted session {} and started fresh session {}",
            "✔".green().bold(),
            desc,
            new_id.cyan()
        );
    }

    Ok(())
}

/// `/sessions [id|name]`: interactive session picker to switch chat sessions (alias: `/session`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_sessions_switch(
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &Arc<SqliteSessionStore>,
    session_id: &mut String,
    current_provider: &mut String,
    current_model: &mut String,
    compact_provider: &mut Option<String>,
    compact_model: &mut Option<String>,
    turns: &mut Vec<TurnRecord>,
    process_expanded: &mut bool,
    todo_scope: &harness_core::tools::TodoScope,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let Some(target_id) = pick_session_target(key_rx, ui, store, session_id, arg).await? else {
        return Ok(());
    };

    if target_id == *session_id {
        ui.emit_above(&format!(
            "{} Already on session {}",
            "•".cyan(),
            session_id.cyan()
        ));
        return Ok(());
    }

    *session_id = target_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: target_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let record = store.get(&target_id).await?;
    restore_session_model(
        ui,
        config,
        cred_manager,
        discovery,
        agent,
        &record,
        current_provider,
        current_model,
    )
    .await;
    restore_session_compaction(
        config,
        cred_manager,
        discovery,
        agent,
        &record,
        compact_provider,
        compact_model,
        current_provider,
        current_model,
    )
    .await;

    todo_scope.set_session(&target_id);
    todo::refresh_panel(ui, todo_scope);

    let replayed = store
        .replay_transcript(&target_id, config.session.resume_max_messages)
        .await?;
    *turns = parse_turns_from_history(&replayed.history);

    if ui.is_attended {
        crate::commands::clear_screen();
        if replayed.replayed > 0 {
            if replayed.truncated() {
                println!(
                    "{}\n",
                    format!(
                        "… {} earlier messages not shown ({} shown)",
                        replayed.total - replayed.replayed,
                        replayed.replayed
                    )
                    .dimmed()
                );
            }
            draw_transcript(turns, *process_expanded);
            println!();
        }
        ui.draw_initial_box();
    }

    let context_max = context_window_for(config, current_provider, current_model);
    let context_used = agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    ui.set_status(current_model, context_used, context_max);
    ui.set_session_name(&record.name);

    let display_name = if !record.name.is_empty() {
        format!("'{}' ({})", record.name.cyan(), target_id.dimmed())
    } else if !record.title.is_empty() {
        format!("\"{}\" ({})", record.title.cyan(), target_id.dimmed())
    } else {
        target_id.cyan().to_string()
    };

    ui.emit_above(&format!(
        "{} Switched to session {}",
        "✔".green().bold(),
        display_name
    ));

    Ok(())
}

/// Resolve `/sessions`' target: an explicit id or name, or the interactive picker.
///
/// `None` means the user backed out or there was nothing to choose from; the reason was already
/// reported to them, so the caller just returns.
async fn pick_session_target(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    session_id: &str,
    arg: Option<&str>,
) -> Result<Option<String>, HarnessError> {
    let target_id = if let Some(target) = arg.filter(|s| !s.is_empty()) {
        if store.get(target).await.is_ok() {
            target.to_string()
        } else if let Some(id) = find_by_name(store.as_ref(), target).await? {
            id
        } else {
            return Err(HarnessError::SessionError(format!(
                "Session '{}' not found",
                target
            )));
        }
    } else {
        let sessions: Vec<_> = store
            .list()
            .await?
            .into_iter()
            .filter(|session| !session.name.starts_with("worker:"))
            .collect();
        if sessions.is_empty() {
            ui.emit_above(&format!("{} No saved sessions found", "•".cyan()));
            return Ok(None);
        }

        let items: Vec<String> = sessions
            .iter()
            .map(|s| {
                let is_current = s.id.as_str() == session_id;
                let tag = if is_current { " *" } else { "" };
                let name = if s.name.is_empty() {
                    "-".to_string()
                } else {
                    format!("\"{}\"", s.name)
                };
                let title = if s.title.is_empty() {
                    "(new session)"
                } else {
                    s.title.as_str()
                };
                let time = s.updated_at.format("%m/%d %H:%M").to_string();
                format!(
                    "{:<9} {:<14} {:>3} msgs  {:<11} {}{}",
                    s.id, name, s.message_count, time, title, tag
                )
            })
            .collect();

        let default_idx = sessions
            .iter()
            .position(|s| s.id.as_str() == session_id)
            .unwrap_or(0);

        let chosen_idx = if let Some(ref mut rx) = key_rx {
            while rx.try_recv().is_ok() {}
            match select_from_rx(rx, ui, "Select Session", &items, default_idx, false, false).await
            {
                Some(i) => i,
                None => {
                    ui.clear_box_on_exit();
                    ui.draw_initial_box();
                    return Ok(None);
                }
            }
        } else {
            println!("Available sessions:");
            for (i, item) in items.iter().enumerate() {
                println!("  {}. {}", i + 1, item);
            }
            return Ok(None);
        };

        sessions[chosen_idx].id.clone()
    };

    Ok(Some(target_id))
}

/// Rebind the agent to the model the target session last ran with, when it has its own.
#[allow(clippy::too_many_arguments)]
async fn restore_session_model(
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    record: &harness_core::SessionRecord,
    current_provider: &mut String,
    current_model: &mut String,
) {
    if let Some((ref prov, ref mdl)) = record.model {
        if prov != current_provider || mdl != current_model {
            match GenericProviderClient::new(
                prov.clone(),
                mdl.clone(),
                cred_manager.clone(),
                discovery.clone(),
                config.providers.get(prov),
            ) {
                Ok(client) => {
                    agent.lock().await.set_provider(Arc::new(client));
                    *current_provider = prov.clone();
                    *current_model = mdl.clone();
                }
                Err(e) => {
                    ui.emit_above(&format!(
                        "{} Warning: could not restore session model {} ({}): {}",
                        "⚠".yellow().bold(),
                        mdl,
                        prov,
                        e
                    ));
                }
            }
        }
    }
}

/// Restore the target session's compaction model (or fall back to global config / active model).
#[allow(clippy::too_many_arguments)]
async fn restore_session_compaction(
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    record: &harness_core::SessionRecord,
    compact_provider: &mut Option<String>,
    compact_model: &mut Option<String>,
    current_provider: &str,
    current_model: &str,
) {
    let (target_comp_p, target_comp_m) = match &record.compact_model {
        Some((p, m)) => (p.clone(), m.clone()),
        None => {
            let p = config
                .agent
                .compaction_provider
                .clone()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| current_provider.to_string());
            let m = config
                .agent
                .compaction_model
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| current_model.to_string());
            (p, m)
        }
    };
    if let Ok(client) = GenericProviderClient::new(
        target_comp_p.clone(),
        target_comp_m.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&target_comp_p),
    ) {
        agent
            .lock()
            .await
            .set_compaction(Some(harness_core::CompactionSetup {
                provider: Arc::new(client),
                model: target_comp_m.clone(),
            }));
    }
    *compact_provider = record
        .compact_model
        .as_ref()
        .map(|(p, _)| p.clone())
        .or_else(|| {
            config
                .agent
                .compaction_provider
                .clone()
                .filter(|p| !p.is_empty())
        });
    *compact_model = record
        .compact_model
        .as_ref()
        .map(|(_, m)| m.clone())
        .or_else(|| {
            config
                .agent
                .compaction_model
                .clone()
                .filter(|m| !m.is_empty())
        });
}

/// `/summarize [name]`, `/compact [name]`, or `/new -s [name]`:
/// Summarize the current session's history and start a new session seeded with the summary.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_summarize_and_new_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &mut String,
    current_provider: &str,
    current_model: &str,
    todo_scope: &harness_core::tools::TodoScope,
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

    ui.start_activity(
        "summarize session",
        "summarize",
        &serde_json::json!({ "session": old_id }),
    );

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
    // As with `/new`: the follow-up session inherits the model in use when it was created.
    store
        .set_model(&new_id, current_provider, current_model)
        .await?;

    let summary_header = if !name.is_empty() {
        format!(
            "Context summary from session '{}' ({}):\n{}",
            name, old_id, summary
        )
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

    let context_max = context_window_for(config, current_provider, current_model);
    let summary_tokens = harness_core::heuristic_tokens(&[summary_msg]);
    ui.set_status(current_model, summary_tokens, context_max);
    ui.set_session_name(name);

    // The summarized conversation carries its work into the new session, so the list
    // comes with it.
    let old_list = todo_scope.path();
    todo_scope.set_session(&new_id);
    let _ = std::fs::create_dir_all(config.paths.todos_dir.as_path());
    let _ = std::fs::copy(&old_list, todo_scope.path());
    todo::refresh_panel(ui, todo_scope);

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
        ui.emit_above(&format!("\n{}\n{}\n", "Summary:".bold(), summary.dimmed()));
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

/// `/compact`: fold the current session's oldest turns into a summary in place — the session
/// keeps its id and name, only the context shrinks.
///
/// The fold runs as its own task and reports through the agent's compaction events, so the
/// composer stays live and animates while the summary call is in flight. The agent lock is what
/// arbitrates: a turn holds it for its whole run, so a fold asked for mid-turn is refused instead
/// of being silently parked behind the turn.
pub(crate) async fn handle_compact_command(
    ui: &ChatUi,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    current_provider: &str,
    current_model: &str,
) -> Result<(), HarnessError> {
    let ui = ui.clone();
    let agent = agent.clone();
    // Resolved here, on the caller's thread: the fold runs in a task that has no config, and the
    // answer is the same before and after it (the window belongs to the model, not the fold).
    let context_max = context_window_for(config, current_provider, current_model);
    tokio::spawn(async move {
        let Ok(ag) = agent.try_lock() else {
            ui.emit_above(&format!(
                "{} Cannot compact while a turn is running: interrupt it (send a message or press Escape), then run /compact",
                "✖".red().bold()
            ));
            return;
        };
        // Everything that happens inside the fold — the animation, the report, the failure — is
        // emitted as compaction events, so this task only refreshes the gauge afterwards.
        if ag.compact_current_session().await.is_ok() {
            // The summary replaced the folded turns, so the gauge drops for real.
            let context_used = ag.estimate_session_tokens().await.unwrap_or(0);
            ui.update_status(context_used, context_max);
        }
    });

    Ok(())
}

/// `/auto-compact`: open the setup box — the threshold, then the turns — and turn the feature on.
///
/// The two values used to be arguments (`/auto-compact set threshold 200k`). They are asked for in
/// a box instead, the way the approval gate asks for a verdict, so the bounds, an example and the
/// current value are in front of the user while they type. `on` and `off` stay as arguments: a
/// toggle has no value to choose, and a box for it would be a box for nothing.
///
/// `key_rx` is the composer's key channel, so the wizard reads the same keys the chat does. A host
/// with no composer (piped stdin) passes `None` and gets the settings printed instead of a prompt
/// nobody could type into.
pub(crate) async fn handle_auto_compact_command(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
) -> Result<(), HarnessError> {
    let arg = arg.unwrap_or("").trim();

    let layers = harness_core::config::process_layers(Config::default())?;

    if arg.eq_ignore_ascii_case("on") || arg.eq_ignore_ascii_case("enable") {
        return enable_auto_compact(ui, agent, &layers).await;
    }

    if arg.eq_ignore_ascii_case("off") || arg.eq_ignore_ascii_case("disable") {
        return disable_auto_compact(ui, agent, &layers).await;
    }

    // No composer to type into: report the settings and the two arguments this host can use,
    // rather than opening a box that would read keys from a channel no one is writing to.
    let Some(key_rx) = key_rx else {
        report_auto_compact_settings(ui, agent).await;
        return Ok(());
    };

    if !arg.is_empty() {
        warn_unknown_auto_compact_option(ui, arg);
        return Ok(());
    }

    let Some((threshold, percent)) = prompt_auto_compact_values(ui, key_rx, agent).await else {
        return Ok(());
    };

    apply_auto_compact_settings(ui, agent, &layers, threshold, percent).await
}

/// Turn auto-compact on, keeping the current threshold and percent.
async fn enable_auto_compact(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    layers: &harness_core::config::Layers,
) -> Result<(), HarnessError> {
    {
        let mut ag = agent.lock().await;
        ag.set_auto_compact_enabled(true);
    }
    let _ = harness_core::config::set_in_file(layers, "auto_compact_enabled", "true")?;
    let (threshold, percent) = {
        let ag = agent.lock().await;
        let (_, th, pct) = ag.auto_compact_settings();
        (th, pct)
    };
    ui.emit_above(&format!(
        "{} Auto-compact enabled (threshold: {} tokens, compact: {}%)",
        "✓".green().bold(),
        threshold,
        percent
    ));
    Ok(())
}

/// Turn auto-compact off.
async fn disable_auto_compact(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    layers: &harness_core::config::Layers,
) -> Result<(), HarnessError> {
    {
        let mut ag = agent.lock().await;
        ag.set_auto_compact_enabled(false);
    }
    let _ = harness_core::config::set_in_file(layers, "auto_compact_enabled", "false")?;
    ui.emit_above(&format!("{} Auto-compact disabled", "✓".green().bold()));
    Ok(())
}

/// Print the current settings when there is no composer to prompt into.
async fn report_auto_compact_settings(ui: &ChatUi, agent: &Arc<tokio::sync::Mutex<ChatAgent>>) {
    let (enabled, threshold, percent) = {
        let ag = agent.lock().await;
        ag.auto_compact_settings()
    };
    let status = if enabled {
        "enabled".green().bold()
    } else {
        "disabled".dimmed()
    };
    ui.emit_above(&format!(
        "Auto-compact is {} (threshold: {} tokens, compact: {}%)",
        status, threshold, percent
    ));
    ui.emit_above(
        "Usage: /auto-compact opens the setup box in an interactive chat; here it also takes on | off",
    );
}

/// Explain that the argument is not a recognised toggle.
fn warn_unknown_auto_compact_option(ui: &ChatUi, arg: &str) {
    ui.emit_above(&format!(
        "{} Unknown auto-compact option '{}' — {} opens the setup box, {} and {} toggle it",
        "✖".red().bold(),
        arg,
        "/auto-compact".bold(),
        "on".bold(),
        "off".bold()
    ));
}

/// Ask for the threshold, then the percentage. `None` means the user backed out of either box.
async fn prompt_auto_compact_values(
    ui: &ChatUi,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
) -> Option<(usize, usize)> {
    let (_, current_threshold, current_percent) = {
        let ag = agent.lock().await;
        ag.auto_compact_settings()
    };

    // Two boxes, one value each. Esc in either leaves the config exactly as it was: a half
    // answered wizard must not write a threshold the user never confirmed.
    let Some(threshold) = prompt_value(
        ui,
        key_rx,
        &THRESHOLD_PROMPT,
        current_threshold,
        harness_onboarding::compact_step::parse_threshold,
        |_text, _val| None,
    )
    .await
    else {
        ui.emit_above(&format!("{} Auto-compact unchanged", "•".dimmed()));
        return None;
    };

    let Some(percent) = prompt_value(
        ui,
        key_rx,
        &PERCENT_PROMPT,
        current_percent,
        harness_onboarding::compact_step::parse_percent,
        |_text, pct| {
            if pct <= 10 {
                Some(format!(
                    "{} ≤{}%: frees very little context; will quickly hit threshold again",
                    "⚠".yellow().bold(),
                    pct
                ))
            } else if pct <= 60 {
                Some(format!(
                    "{} {}%: good balance of history retention and token headroom",
                    "✓".green().bold(),
                    pct
                ))
            } else {
                Some(format!(
                    "{} {}%: aggressive compaction, clears majority of context",
                    "⚡".cyan().bold(),
                    pct
                ))
            }
        },
    )
    .await
    else {
        ui.emit_above(&format!("{} Auto-compact unchanged", "•".dimmed()));
        return None;
    };

    Some((threshold, percent))
}

/// Store the confirmed values on the agent and in the config layers.
async fn apply_auto_compact_settings(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    layers: &harness_core::config::Layers,
    threshold: usize,
    percent: usize,
) -> Result<(), HarnessError> {
    {
        let mut ag = agent.lock().await;
        ag.set_auto_compact_threshold(threshold);
        ag.set_auto_compact_percent(percent);
        ag.set_auto_compact_enabled(true);
    }
    let _ = harness_core::config::set_in_file(layers, "auto_compact_enabled", "true")?;
    let _ = harness_core::config::set_in_file(
        layers,
        "auto_compact_threshold",
        &threshold.to_string(),
    )?;
    let _ =
        harness_core::config::set_in_file(layers, "auto_compact_percent", &percent.to_string())?;

    ui.emit_above(&format!(
        "{} Auto-compact enabled (threshold: {} tokens, compact: {}%)",
        "✓".green().bold(),
        threshold,
        percent
    ));
    Ok(())
}
