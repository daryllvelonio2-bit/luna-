//! composer

use colored::Colorize;

use super::*;

/// Whether the agent is free right now.
///
/// A running turn holds this mutex until it finishes, so `lock().await` on the UI thread freezes
/// the whole screen — no redraw, no keys, no approval prompt. `try_lock` is the only way to ask
/// without blocking; the command waits in `deferred_commands` instead.
pub fn agent_is_free<T>(agent: &Arc<tokio::sync::Mutex<T>>) -> bool {
    match agent.try_lock() {
        Ok(guard) => {
            drop(guard);
            true
        }
        Err(_) => false,
    }
}

pub async fn submit_composer(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<Flow, HarnessError> {
    let submitted = st.composer.take_for_submit();
    sync_slash_popup(env, st);

    if submitted.display_text.is_empty() {
        return Ok(Flow::Continue);
    }

    if !submitted.image_data_urls.is_empty() {
        st.pending_images
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(submitted.image_data_urls);
    }

    let history_entry =
        crate::commands::prompt_images::remove_image_markers(&submitted.expanded_text);
    let history_trimmed = history_entry.trim();
    if !history_trimmed.is_empty()
        && st.composer.prompt_history.last().map(String::as_str) != Some(history_trimmed)
    {
        st.composer.prompt_history.push(history_trimmed.to_string());
        if let Some(ref path) = st.history_path {
            append_prompt_history(Some(path), history_trimmed);
        }
    }

    let command_text =
        crate::commands::prompt_images::strip_leading_image_markers(&submitted.expanded_text);

    if command_text.eq_ignore_ascii_case("exit") || command_text.eq_ignore_ascii_case("quit") {
        return Ok(Flow::Break);
    }

    if command_text.starts_with('/') && !looks_like_path(command_text) {
        return run_slash_command(env, st, terminal, command_text, &submitted.display_text).await;
    }

    // Chatting while voice TTS is actively speaking immediately stops playback (barge-in).
    if st.voice_active {
        if let Some(c) = st.voice_reply_cancel.take() {
            c.cancel();
        }
        st.voice_reply_cancel = Some(CancellationToken::new());
        st.voice_speech_queue.interrupt();
        st.voice_speech_queue.set_turn_muted(false);
        st.voice_stream_offset = 0;
        st.voice_stream_in_code = false;
        st.voice_spoken_any = false;
        st.voice_ack_sent = false;
    }

    // Normal user prompt: queue/steer if a turn is active, else launch turn immediately.
    if env.compacting.load(Ordering::SeqCst) {
        st.queue.push_back(submitted.expanded_text);
        env.ui.set_queue_depth(st.queue.len());
        return Ok(Flow::Continue);
    }

    if st.active_turn.is_some() {
        let text = submitted.expanded_text;
        env.in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(TurnProcessItem::user_message(&text));
        st.jump_to_latest();
        match env.interrupt_handle.interrupt(&text) {
            harness_core::InterruptOutcome::Streaming => {
                st.system_notices.push(SystemNotice::new(
                    "↳ Redirecting — stopping this reply to take your message",
                ));
            }
            harness_core::InterruptOutcome::Queued => {
                st.system_notices.push(SystemNotice::new(
                    "↳ Held until the running tool finishes, then sent",
                ));
            }
            harness_core::InterruptOutcome::Idle => {
                st.queue.push_back(text);
                env.ui.set_queue_depth(st.queue.len());
            }
        }
        return Ok(Flow::Continue);
    }

    let display_opt = (submitted.display_text != submitted.expanded_text)
        .then_some(submitted.display_text.as_str());
    env.ui.clear_transient_error();
    st.jump_to_latest();
    st.system_notices.clear();
    if let Some(ref orch_name) = st.active_orchestrator {
        if let Some(orch) = env.config.orchestrators.get(orch_name) {
            let prompt_block =
                super::orchestrator::build_orchestrator_prompt_block(orch_name, orch);
            if let Ok(mut ag) = env.agent.try_lock() {
                ag.set_orchestrator_prompt(Some(prompt_block));
            }
        }
    } else if let Ok(mut ag) = env.agent.try_lock() {
        ag.set_orchestrator_prompt(None);
    }
    launch_turn(
        &submitted.expanded_text,
        display_opt,
        &env.ui,
        &st.turns,
        st.process_expanded,
        &st.pending_images,
        &env.plan_mode,
        &env.agent,
        &env.in_flight_tools,
        &mut st.current_cancel,
        &mut st.current_query,
        &mut st.turn_start,
        &mut st.active_turn,
    );
    if st.voice_active && !st.voice_speech_queue.is_turn_muted() {
        let ack = crate::commands::voice::generate_instant_voice_ack(&submitted.expanded_text);
        env.in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(crate::commands::chat::render::TurnProcessItem::assistant_text(&ack));
        st.voice_speech_queue.enqueue(
            &ack,
            env.config,
            &env.cred_manager,
            Some(st.voice_tx.clone()),
        );
        st.voice_spoken_any = true;
        st.voice_ack_sent = true;
    }
    Ok(Flow::Continue)
}

/// Map a `/default_model` or `/compact-model` argument to the scope it asks for.
pub(crate) fn model_scope_arg(arg: Option<&str>) -> Option<bool> {
    match arg
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("global" | "--global" | "g") => Some(true),
        Some("session" | "--session" | "this" | "local" | "s") => Some(false),
        _ => None,
    }
}

/// `/compact-model` treats every non-reset argument as an explicit scope, where only
/// `global`/`--global` means global.
pub(crate) fn compact_scope_arg(arg: Option<&str>) -> Option<bool> {
    arg.map(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "global" | "--global"
        )
    })
}

pub(crate) fn show_profile_usage(st: &mut TuiState) {
    show_text_modal(
        st,
        "Profiles",
        "Usage: /profile list | show [name] [--full] | path [name] | create [name] | delete <name>",
    );
}

/// `/profile` in the TUI: plain reports open a text modal, while create and delete run the same
/// filesystem helpers as the CLI wizard without any of its terminal drawing.
pub(crate) fn route_profile_command(env: &mut Env<'_>, st: &mut TuiState, arg: Option<&str>) {
    let Some(arg) = arg else {
        show_profile_usage(st);
        return;
    };
    let mut parts = arg.split_whitespace();
    let Some(subcommand) = parts.next() else {
        show_profile_usage(st);
        return;
    };

    match subcommand.to_ascii_lowercase().as_str() {
        "list" | "ls" => {
            let text = crate::commands::profile::list_text(&env.config.paths.profiles_dir);
            show_text_modal(st, "Profiles", &text);
        }
        "show" => {
            let name = parts.next().map(str::to_string);
            let full = parts.any(|part| part == "--full");
            match crate::commands::profile::show_text(
                name.as_deref()
                    .unwrap_or(harness_core::profiles::DEFAULT_PROFILE),
                &env.config.paths.profiles_dir,
                full,
            ) {
                Ok(text) => show_text_modal(st, "Profile", &text),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Profile failed: {error}"))),
            }
        }
        "path" => {
            let name = parts.next().map(str::to_string);
            match crate::commands::profile::path_text(
                name.as_deref()
                    .unwrap_or(harness_core::profiles::DEFAULT_PROFILE),
                env.config,
            ) {
                Ok(text) => show_text_modal(st, "Profile Path", &text),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Profile failed: {error}"))),
            }
        }
        "create" | "new" => match parts.next().map(str::to_string) {
            Some(name) => match harness_core::profiles::validate_name(&name) {
                Ok(()) => open_profile_create_summary(st, name),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Invalid name: {error}"))),
            },
            None => open_profile_create_name(st),
        },
        "delete" | "rm" => match parts.next().map(str::to_string) {
            Some(name) => {
                st.modal = ActiveModal::SelectModal {
                    title: "Delete Profile".to_string(),
                    items: vec![
                        format!("Yes, delete '{name}' and all of its data"),
                        "No, keep it".to_string(),
                    ],
                    selected: 1,
                    action: SelectAction::ProfileDeleteConfirm { name },
                };
            }
            None => show_profile_usage(st),
        },
        _ => show_profile_usage(st),
    }
}

pub async fn run_slash_command(
    env: &mut Env<'_>,
    st: &mut TuiState,
    _terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cmd: &str,
    display: &str,
) -> Result<Flow, HarnessError> {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let name = parts[0];
    let arg = parts
        .get(1)
        .copied()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // Handle native TUI routes first so there's zero screen flash.
    super::trace_event(&format!("slash start {name}"));
    match name {
        "/help" | "/?" if arg.is_none() => {
            st.modal = ActiveModal::Help { scroll: 0 };
            return Ok(Flow::Continue);
        }
        // `/mcp` and `/mcp list` open the interactive list in a popup. `/mcp on|off|tools` and
        // anything else still go through the dispatcher.
        "/mcp" if arg.is_none() || arg == Some("list") => {
            super::open_mcp_list(env, st);
            return Ok(Flow::Continue);
        }
        // Same for `/skills`: the popup pins skills. `/skills pin|unpin|on|off` still dispatch.
        "/skills" if arg.is_none() || arg == Some("list") => {
            super::open_skills_list(env, st).await;
            return Ok(Flow::Continue);
        }
        "/sessions" | "/session" if arg.is_none() => {
            open_session_picker(env, st).await;
            return Ok(Flow::Continue);
        }
        "/model" | "/models" | "/provider" => {
            if let Some(target) = arg {
                let find_prov = |id: &str| {
                    harness_providers::catalog::find_provider(id).or_else(|| {
                        env.config.providers.get(id).map(|o| {
                            harness_providers::catalog::custom_provider_descriptor(
                                id,
                                o.base_url.as_deref().unwrap_or(""),
                            )
                        })
                    })
                };
                if let Some((prov_prefix, model_rest)) = target.split_once('/') {
                    if let Some(prov) = find_prov(prov_prefix) {
                        if !model_rest.trim().is_empty() {
                            if let Err(e) =
                                apply_model_selection(env, st, &prov.id, model_rest.trim(), true)
                                    .await
                            {
                                env.ui
                                    .set_transient_error(Some(format!("Model switch failed: {e}")));
                            }
                            return Ok(Flow::Continue);
                        }
                    }
                }
                if let Some(prov) = find_prov(target) {
                    open_models_for_provider(env, st, &prov.id, String::new()).await;
                } else {
                    open_provider_picker_with_query(env, st, target.to_string()).await;
                }
            } else {
                open_provider_picker(env, st).await;
            }
            return Ok(Flow::Continue);
        }
        "/expand" if arg.is_none() => {
            set_all_process_expanded(st, true);
            return Ok(Flow::Continue);
        }
        "/collapse" if arg.is_none() => {
            set_all_process_expanded(st, false);
            return Ok(Flow::Continue);
        }
        "/new" if arg.is_none() => {
            create_new_session(env, st).await?;
            return Ok(Flow::Continue);
        }
        "/automation" if arg.is_none() => {
            st.composer
                .set_text(harness_scheduler::AUTOMATION_PREFILL.to_string());
            return Ok(Flow::Continue);
        }
        "/plugin" | "/plugins" if arg.is_none() => {
            st.composer
                .set_text(harness_plugins::PLUGIN_PREFILL.to_string());
            return Ok(Flow::Continue);
        }
        "/thinking" | "/think" | "/reasoning" if arg.is_none() => {
            open_thinking_picker(env, st).await;
            return Ok(Flow::Continue);
        }
        "/skills" if arg.is_some_and(|a| a.starts_with("pin")) => {
            let rest = arg.unwrap().strip_prefix("pin").unwrap_or("").trim();
            if rest.is_empty() {
                let skills_arc = env.skills_arc.clone();
                let skills: &[harness_plugins::Skill] =
                    skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);
                let items: Vec<String> = skills.iter().map(|s| s.name.clone()).collect();
                if items.is_empty() {
                    env.ui.emit_above("No skills installed to pin.");
                } else {
                    st.modal = ActiveModal::SelectModal {
                        title: "Select Skill to Pin".to_string(),
                        items: items.clone(),
                        selected: 0,
                        action: SelectAction::SkillPinSelect { skills: items },
                    };
                }
                return Ok(Flow::Continue);
            }
        }
        "/voice" => {
            handle_voice_slash_command(env, st, arg).await?;
            return Ok(Flow::Continue);
        }
        "/update" => {
            let config_clone = env.config.clone();
            env.ui
                .emit_above(&format!("{} Checking for updates...", "•".cyan()));
            match crate::commands::update::run_update_headless(&config_clone, false).await {
                Ok(Some((from, to))) => {
                    env.ui.emit_above(&format!(
                        "{} Successfully updated from {} to {}! Restart luna to apply.",
                        "✔".green().bold(),
                        from,
                        to
                    ));
                    st.update_notice = None;
                }
                Ok(None) => {
                    env.ui.emit_above(&format!(
                        "{} Luna is already up to date ({}).",
                        "✔".green().bold(),
                        crate::commands::update::CURRENT_DISPLAY
                    ));
                }
                Err(e) => {
                    env.ui
                        .emit_above(&format!("{} Update failed: {}", "✖".red().bold(), e));
                }
            }
            return Ok(Flow::Continue);
        }
        "/settings" if arg.is_none() => {
            open_settings_menu(env, st, 0);
            return Ok(Flow::Continue);
        }
        "/auto-compact" | "/autocompact" if arg.is_none() => {
            open_auto_compact_threshold(env, st).await;
            return Ok(Flow::Continue);
        }
        "/compact" if arg.is_none() => {
            if st.active_turn.is_some()
                || env.compacting.load(Ordering::SeqCst)
                || env.ui.is_compacting()
            {
                env.ui.set_transient_error(Some(
                    "Cannot compact while a turn is running or compaction is in progress"
                        .to_string(),
                ));
                return Ok(Flow::Continue);
            }
            env.ui.start_compaction(true);
            env.compacting.store(true, Ordering::SeqCst);
            env.ui.notify_tui();
            let ui = env.ui.clone();
            let agent = env.agent.clone();
            let compacting = env.compacting.clone();
            let context_max = crate::commands::context_window::context_window(
                env.config,
                env.discovery.as_ref(),
                env.current_provider,
                env.current_model,
            );
            tokio::spawn(async move {
                let Ok(ag) = agent.try_lock() else {
                    compacting.store(false, Ordering::SeqCst);
                    ui.finish_compaction();
                    ui.emit_above(
                        "✖ Cannot compact while a turn is running: interrupt it, then run /compact",
                    );
                    return;
                };
                match ag.compact_current_session().await {
                    Ok(_notice) => {
                        let context_used = ag.estimate_session_tokens().await.unwrap_or(0);
                        ui.update_status(context_used, context_max);
                    }
                    Err(e) => {
                        compacting.store(false, Ordering::SeqCst);
                        ui.finish_compaction();
                        ui.emit_above(&format!("✖ Failed to compact session: {e}"));
                    }
                }
            });
            return Ok(Flow::Continue);
        }
        "/connect" => {
            open_connect_menu(st);
            return Ok(Flow::Continue);
        }
        "/jobs" | "/tasks" | "/subagents" => {
            open_task_list(env, st).await;
            return Ok(Flow::Continue);
        }
        "/profile" | "/profiles" => {
            route_profile_command(env, st, arg);
            return Ok(Flow::Continue);
        }
        "/dir" | "/workspace" => {
            let raw = arg.map(str::trim).filter(|value| !value.is_empty());
            let is_reset = raw.is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "reset" | "--reset" | "-r"
                )
            });
            if is_reset {
                if let Err(error) = reset_workspace(&env.ui, env.config, &env.agent).await {
                    env.ui
                        .set_transient_error(Some(format!("Workspace reset failed: {error}")));
                }
                return Ok(Flow::Continue);
            }
            let (path, scope) = parse_dir_args(raw);
            match path {
                Some(path) => match scope {
                    Some(is_global) => {
                        let expanded = harness_core::config::expand_tilde(&path);
                        if !std::path::PathBuf::from(&expanded).is_dir() {
                            env.ui
                                .set_transient_error(Some(format!("Directory not found: {path}")));
                        } else if let Err(error) =
                            apply_dir_choice(&env.ui, env.config, &env.agent, &path, is_global)
                                .await
                        {
                            env.ui.set_transient_error(Some(format!(
                                "Workspace change failed: {error}"
                            )));
                        }
                    }
                    None => open_dir_scope(st, path),
                },
                None => open_dir_input(st, scope),
            }
            return Ok(Flow::Continue);
        }
        "/default_model" | "/default-model" => {
            let global = model_scope_arg(arg);
            open_provider_picker_for(env, st, ModelTarget::Default { global }).await;
            return Ok(Flow::Continue);
        }
        "/instructions" => {
            super::instructions::open_instructions_list(env.config, st);
            return Ok(Flow::Continue);
        }
        "/bg-model" | "/compact-model" | "/bgsaver-model" => {
            let is_reset = arg.is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "reset" | "default" | "off"
                )
            });
            if !is_reset {
                let global = compact_scope_arg(arg);
                open_provider_picker_for(env, st, ModelTarget::BgModel { global }).await;
                return Ok(Flow::Continue);
            }
        }
        "/memory" => {
            let trimmed = arg.map(str::trim).filter(|s| !s.is_empty());
            match trimmed {
                None => {
                    open_memory_project_picker(st).await;
                    return Ok(Flow::Continue);
                }
                Some(val) => {
                    let lower = val.to_ascii_lowercase();
                    if matches!(lower.as_str(), "list" | "projects") {
                        open_memory_project_picker(st).await;
                        return Ok(Flow::Continue);
                    }
                    if matches!(lower.as_str(), "reset" | "all" | "default" | "clear") {
                        st.memory_project_filter = None;
                    } else if !matches!(
                        lower.as_str(),
                        "on" | "off"
                            | "enable"
                            | "disable"
                            | "enabled"
                            | "disabled"
                            | "true"
                            | "false"
                    ) {
                        st.memory_project_filter = Some(val.to_string());
                    }
                }
            }
        }
        "/worker" => {
            if let Some(raw) = arg {
                let name = raw.trim().to_ascii_lowercase();
                let is_delete = name == "delete" || name.starts_with("delete ");
                if !name.is_empty() && name != "list" && !is_delete {
                    if let Err(error) = harness_core::config::validate_worker_name(&name) {
                        env.ui
                            .set_transient_error(Some(format!("Worker setup failed: {error}")));
                        return Ok(Flow::Continue);
                    }
                    if RESERVED_COMMAND_NAMES.iter().any(|reserved| {
                        reserved.trim_start_matches('/').eq_ignore_ascii_case(&name)
                    }) {
                        env.ui.set_transient_error(Some(format!(
                            "Worker name '{name}' is a built-in command; pick another name"
                        )));
                        return Ok(Flow::Continue);
                    }
                    open_provider_picker_for(env, st, ModelTarget::Worker(name)).await;
                    return Ok(Flow::Continue);
                }
            }
        }
        "/orchestrator" => {
            let trimmed = arg.unwrap_or("").trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("edit") {
                super::orchestrator::open_orchestrator_edit_list(env, st);
            } else if let Some(rest) = trimmed.strip_prefix("create") {
                let name = rest.trim();
                let name_opt = if name.is_empty() {
                    None
                } else {
                    Some(name.to_string())
                };
                super::orchestrator::open_orchestrator_create(env, st, name_opt).await;
            } else if let Some(rest) = trimmed.strip_prefix("use") {
                let name = rest.trim();
                if name.is_empty() {
                    super::orchestrator::open_orchestrator_use_list(env, st);
                } else if let Err(e) = super::orchestrator::apply_orchestrator(env, st, name).await
                {
                    env.ui.set_transient_error(Some(e.to_string()));
                }
            } else {
                env.ui.set_transient_error(Some(
                    "Usage: /orchestrator create [name] | /orchestrator edit | /orchestrator use <name>".into(),
                ));
            }
            return Ok(Flow::Continue);
        }
        "/plan" => {
            let next = if st.mode == AgentMode::Plan {
                AgentMode::Build
            } else {
                AgentMode::Plan
            };
            apply_agent_mode(env, st, next);
            return Ok(Flow::Continue);
        }
        "/yolo" => {
            let next = if st.mode == AgentMode::Auto {
                AgentMode::Build
            } else {
                AgentMode::Auto
            };
            apply_agent_mode(env, st, next);
            return Ok(Flow::Continue);
        }
        "/clear" => {
            st.turns.clear();
            st.turn_meta.clear();
            st.settled_cache.clear();
            st.system_notices.clear();
            st.jump_to_latest();
            return Ok(Flow::Continue);
        }
        "/exit" | "/quit" => {
            return Ok(Flow::Break);
        }
        _ => {}
    }

    // Composer-level control commands (`/interrupt`, `/stop`, `/queue ...`)
    if handle_composer_command(
        cmd,
        &mut st.queue,
        &env.steer_handle,
        &env.ui,
        &mut st.active_turn,
        &mut st.active_goal,
        &st.current_cancel,
        &env.interrupt_handle,
        &mut st.turns,
        &env.in_flight_tools,
        &env.interruptions,
        st.turn_start,
        &st.current_query,
        st.process_expanded,
        &env.agent,
        context_window(
            env.config,
            env.discovery.as_ref(),
            env.current_provider,
            env.current_model,
        ),
    )
    .await
    {
        sync_turn_meta_len(env, st);
        drain_tui_notices(env, st);
        if st.active_turn.is_none()
            && !env.compacting.load(Ordering::SeqCst)
            && !st.queue.is_empty()
        {
            drain_queue(
                &mut st.queue,
                &env.ui,
                &st.turns,
                st.process_expanded,
                &st.pending_images,
                &env.plan_mode,
                &env.agent,
                &env.in_flight_tools,
                &mut st.current_cancel,
                &mut st.current_query,
                &mut st.turn_start,
                &mut st.active_turn,
            );
            st.prune_stale_notices();
        }
        return Ok(Flow::Continue);
    }

    // A running turn holds the agent lock until it finishes. Awaiting it here would freeze the
    // whole UI (no redraw, no keys, no approval prompt), so the command waits in the queue and
    // the event loop runs it as soon as the lock is free.
    if !agent_is_free(&env.agent) {
        st.deferred_commands.push_back(cmd.to_string());
        st.system_notices.push(SystemNotice::new(format!(
            "⏸ Queued {name} — runs when the current turn finishes"
        )));
        return Ok(Flow::Continue);
    }

    let skills_arc = env.skills_arc.clone();
    let skills: &[harness_plugins::Skill] = skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);

    let prev_len = st.turns.len();
    env.ui.begin_command_capture();
    let outcome = execute_slash_command(
        cmd,
        None,
        &env.ui,
        env.config,
        &env.cred_manager,
        &env.discovery,
        &env.agent,
        &env.store,
        env.session_id,
        env.current_provider,
        env.current_model,
        env.compact_provider,
        env.compact_model,
        &mut st.turns,
        &mut st.process_expanded,
        &env.todo_scope,
        env.skills_readout.as_deref(),
        skills,
        env.feature_toggles,
        &env.plan_mode,
        st.active_goal.as_ref(),
        Some(env.bg_queue.as_ref()),
        Some(env.delegate_tool.as_ref()),
        Some(&st.pending_images),
        &env.display_settings,
    )
    .await?;

    match outcome {
        SlashOutcome::Continue => {
            finish_slash_command(&env.ui, &mut st.turns, display, st.process_expanded, false);
            st.settled_cache.clear();
            if st.turns.len() < prev_len {
                st.turn_meta.truncate(st.turns.len());
                if st.turns.is_empty() {
                    st.reset_diff_counter();
                }
            }
            sync_turn_meta_len(env, st);
            drain_tui_notices(env, st);
            refresh_tabs(env, st).await;
            st.jump_to_latest();
            Ok(Flow::Continue)
        }
        SlashOutcome::Turn { prompt, display } => {
            let _ = env.ui.end_command_capture();
            if env.compacting.load(Ordering::SeqCst) {
                st.queue.push_back(prompt);
                env.ui.set_queue_depth(st.queue.len());
            } else {
                st.jump_to_latest();
                launch_turn(
                    &prompt,
                    Some(&display),
                    &env.ui,
                    &st.turns,
                    st.process_expanded,
                    &st.pending_images,
                    &env.plan_mode,
                    &env.agent,
                    &env.in_flight_tools,
                    &mut st.current_cancel,
                    &mut st.current_query,
                    &mut st.turn_start,
                    &mut st.active_turn,
                );
            }
            Ok(Flow::Continue)
        }
        SlashOutcome::StartGoal {
            state,
            prompt,
            display,
        } => {
            let _ = env.ui.end_command_capture();
            let iter = state.iteration;
            let max_iter = state.max_iterations;
            st.active_goal = Some(state);
            env.ui.set_goal_status(Some((iter, max_iter)));
            st.jump_to_latest();
            launch_turn(
                &prompt,
                Some(&display),
                &env.ui,
                &st.turns,
                st.process_expanded,
                &st.pending_images,
                &env.plan_mode,
                &env.agent,
                &env.in_flight_tools,
                &mut st.current_cancel,
                &mut st.current_query,
                &mut st.turn_start,
                &mut st.active_turn,
            );
            Ok(Flow::Continue)
        }
        SlashOutcome::WorkerSpawned(info) => {
            let _ = env.ui.end_command_capture();
            env.ui.start_delegate_task(
                &info.child_id,
                &format!("[{}] {}", info.worker_name, info.task),
            );
            let args = serde_json::json!({
                "task_id": info.child_id,
                "worker": info.worker_name,
                "provider": info.provider,
                "model": info.model,
                "tasks": [{ "goal": info.task }],
            });
            let item = TurnProcessItem {
                tool_name: "delegate_task".to_string(),
                call_formatted: format!("Subagent [{}]: {}", info.worker_name, info.task),
                result_formatted: "running · starting (click or Ctrl+B to inspect)".to_string(),
                diff_lines: Vec::new(),
                output: String::new(),
                output_dropped: 0,
                arguments: Some(args),
                duration_secs: None,
                is_error: false,
            };
            st.turns.push(TurnRecord {
                user_query: display.trim().to_string(),
                reasoning: None,
                tools: Vec::new(),
                tool_items: vec![item],
                duration: std::time::Duration::ZERO,
                reply: String::new(),
                command_output: false,
            });
            st.turn_meta.push(TurnMeta {
                mode: format!("Subagent · {}", info.worker_name),
                model: format!("{}/{}", info.provider, info.model),
                output_tokens: None,
            });
            sync_turn_meta_len(env, st);
            st.settled_cache.clear();
            drain_tui_notices(env, st);
            st.jump_to_latest();
            Ok(Flow::Continue)
        }
        SlashOutcome::Exit => Ok(Flow::Break),
    }
}
