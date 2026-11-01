//! The slash commands that act on the session itself: fork, rewind, name, new, delete, switch.
//!
//! All of these rewrite *which* conversation the agent is bound to, which is the concern they
//! share: after one runs, the screen, the todo scope and the stored session have to still agree
//! with each other.

use super::*;

/// Fork the current session into a new one.
///
/// The fork continues the work of the conversation it came from, so the todo list comes
/// with it: the new session starts with the tasks the old one was working on.
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
            // The fork carries the list with it, so the panel above the composer shows
            // the tasks the new session continues to work on.
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
        ui.emit_above(&format!("    Restore one with {}", "/rewind <turn>".bold()));
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
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
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
        let sessions = store.list().await?;
        if sessions.is_empty() {
            ui.emit_above(&format!("{} No saved sessions found", "•".cyan()));
            return Ok(());
        }

        let items: Vec<String> = sessions
            .iter()
            .map(|s| {
                let is_current = s.id == *session_id;
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
            .position(|s| s.id == *session_id)
            .unwrap_or(0);

        let chosen_idx = if let Some(ref mut rx) = key_rx {
            while rx.try_recv().is_ok() {}
            match select_from_rx(rx, ui, "Select Session", &items, default_idx, false, false).await
            {
                Some(i) => i,
                None => {
                    ui.clear_box_on_exit();
                    ui.draw_initial_box();
                    return Ok(());
                }
            }
        } else {
            println!("Available sessions:");
            for (i, item) in items.iter().enumerate() {
                println!("  {}. {}", i + 1, item);
            }
            return Ok(());
        };

        sessions[chosen_idx].id.clone()
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

    // Restore target session's compaction model (or fall back to global config / active model)
    let (target_comp_p, target_comp_m) = match &record.compact_model {
        Some((p, m)) => (p.clone(), m.clone()),
        None => {
            let p = config
                .agent
                .compaction_provider
                .clone()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| current_provider.clone());
            let m = config
                .agent
                .compaction_model
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| current_model.clone());
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
