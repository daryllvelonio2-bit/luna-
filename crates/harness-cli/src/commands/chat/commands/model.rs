//! The slash commands that change how the model answers rather than what it answers.
//!
//! Provider and model selection, the compaction model, and the reasoning-effort switches. They
//! share one shape: pick a value, persist it to the layer the user asked for, rebind the agent.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_model_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &SqliteSessionStore,
    session_id: &str,
    current_provider: &mut String,
    current_model: &mut String,
    compact_model: &Option<String>,
) -> Result<(), HarnessError> {
    let Some((chosen_provider, chosen_model)) = pick_provider_and_model(
        key_rx.as_deref_mut(),
        ui,
        cred_manager,
        discovery,
        current_provider,
        current_model,
        true,
    )
    .await?
    else {
        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(());
    };

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

    let reasoning_items: Vec<String> = reasoning_options
        .iter()
        .map(|(label, _)| label.to_string())
        .collect();
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
        match select_from_rx(
            rx,
            ui,
            "Select Reasoning Effort",
            &reasoning_items,
            default_reasoning_idx,
            true,
            false,
        )
        .await
        {
            Some(i) => i,
            None => {
                ui.clear_box_on_exit();
                ui.draw_initial_box();
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

    // The switch is session state, not just process state: stamping the row means a later
    // `luna --session <id>` resumes on this pair instead of the config default. A failure to
    // persist is surfaced as a warning — the in-memory switch already happened.
    if let Err(error) = store
        .set_model(session_id, &chosen_provider, &chosen_model)
        .await
    {
        ui.emit_above(&format!(
            "{} Model switched in this session, but persisting it failed: {}",
            "⚠".yellow(),
            error
        ));
    }

    // If compaction model is not custom set, default it to the used model
    if compact_model.is_none() {
        if let Ok(comp_client) = GenericProviderClient::new(
            chosen_provider.clone(),
            chosen_model.clone(),
            cred_manager.clone(),
            discovery.clone(),
            config.providers.get(&chosen_provider),
        ) {
            agent
                .lock()
                .await
                .set_compaction(Some(harness_core::CompactionSetup {
                    provider: Arc::new(comp_client),
                    model: chosen_model.clone(),
                }));
        }
    }

    // The chained pickers redraw the box as they tear down — with the pre-switch model. Erase
    // that stale box so the status bar is drawn once, showing the model now in use.
    let context_max = context_window(config, discovery, &chosen_provider, &chosen_model);
    // Switching models does not touch the transcript, so keep the real usage rather than resetting
    // the bar to 0.
    let context_used = agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    ui.set_model(&chosen_model, context_used, context_max);
    crate::commands::chat::status::refresh_antigravity_status_if_needed(
        ui,
        &chosen_provider,
        &chosen_model,
        cred_manager,
    )
    .await;
    ui.clear_box_on_exit();
    ui.draw_initial_box();

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_compact_model_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &SqliteSessionStore,
    session_id: &str,
    current_provider: &str,
    current_model: &str,
    compact_provider: &mut Option<String>,
    compact_model: &mut Option<String>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    if let Some(a) = arg {
        if a.eq_ignore_ascii_case("reset") || a.eq_ignore_ascii_case("default") {
            *compact_provider = None;
            *compact_model = None;
            let _ = store.clear_compact_model(session_id).await;
            let comp_client = GenericProviderClient::new(
                current_provider.to_string(),
                current_model.to_string(),
                cred_manager.clone(),
                discovery.clone(),
                config.providers.get(current_provider),
            )?;
            agent
                .lock()
                .await
                .set_compaction(Some(harness_core::CompactionSetup {
                    provider: Arc::new(comp_client),
                    model: current_model.to_string(),
                }));
            ui.clear_box_on_exit();
            ui.draw_initial_box();
            ui.emit_above(&format!(
                "{} Compaction model reset to active model: {} ({})",
                "✓".green(),
                current_model,
                current_provider
            ));
            return Ok(());
        }
    }

    let initial_p = compact_provider.as_deref().unwrap_or(current_provider);
    let initial_m = compact_model.as_deref().unwrap_or(current_model);

    let Some((chosen_provider, chosen_model)) = pick_provider_and_model(
        key_rx.as_deref_mut(),
        ui,
        cred_manager,
        discovery,
        initial_p,
        initial_m,
        false,
    )
    .await?
    else {
        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(());
    };

    // Prompt for scope: this session only vs globally
    let is_global = if let Some(a) = arg {
        a.eq_ignore_ascii_case("global") || a.eq_ignore_ascii_case("--global")
    } else if let Some(ref mut rx) = key_rx {
        let scope_options = [
            "This session only (remember for this session)".to_string(),
            "Globally (save as default for all sessions)".to_string(),
        ];
        while rx.try_recv().is_ok() {}
        let sel = select_from_rx(
            rx,
            ui,
            "Compaction Model Scope",
            &scope_options,
            0,
            true,
            false,
        )
        .await;
        match sel {
            Some(1) => true,
            Some(0) => false,
            None => {
                ui.clear_box_on_exit();
                ui.draw_initial_box();
                return Ok(());
            }
            _ => false,
        }
    } else {
        false
    };

    let comp_client = GenericProviderClient::new(
        chosen_provider.clone(),
        chosen_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&chosen_provider),
    )?;

    agent
        .lock()
        .await
        .set_compaction(Some(harness_core::CompactionSetup {
            provider: Arc::new(comp_client),
            model: chosen_model.clone(),
        }));

    *compact_provider = Some(chosen_provider.clone());
    *compact_model = Some(chosen_model.clone());

    if is_global {
        let layers =
            harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
        if let Err(e) = harness_core::config::set_in_file(
            &layers,
            "agent.compaction_provider",
            &chosen_provider,
        ) {
            ui.emit_above(&format!(
                "{} Failed to persist global compaction provider: {}",
                "⚠".yellow(),
                e
            ));
        }
        if let Err(e) =
            harness_core::config::set_in_file(&layers, "agent.compaction_model", &chosen_model)
        {
            ui.emit_above(&format!(
                "{} Failed to persist global compaction model: {}",
                "⚠".yellow(),
                e
            ));
        }
        let _ = store.clear_compact_model(session_id).await;
        ui.emit_above(&format!(
            "{} Compaction model set globally: {} ({})",
            "✓".green(),
            chosen_model,
            chosen_provider
        ));
    } else {
        if let Err(e) = store
            .set_compact_model(session_id, &chosen_provider, &chosen_model)
            .await
        {
            ui.emit_above(&format!(
                "{} Compaction model switched in this session, but persisting it failed: {}",
                "⚠".yellow(),
                e
            ));
        }
        ui.emit_above(&format!(
            "{} Compaction model set for this session: {} ({})",
            "✓".green(),
            chosen_model,
            chosen_provider
        ));
    }

    ui.clear_box_on_exit();
    ui.draw_initial_box();

    Ok(())
}

pub(crate) async fn handle_thinking_switch(
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    handle_reasoning_or_thinking_switch(key_rx, ui, agent, arg, true).await
}

pub(crate) async fn handle_reasoning_switch(
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    handle_reasoning_or_thinking_switch(key_rx, ui, agent, arg, false).await
}

pub(super) async fn handle_reasoning_or_thinking_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
    is_thinking: bool,
) -> Result<(), HarnessError> {
    let concept_name = if is_thinking {
        "thinking level"
    } else {
        "reasoning effort"
    };
    let title = if is_thinking {
        "Select Thinking Level"
    } else {
        "Select Reasoning Effort"
    };

    let chosen_effort = if let Some(s) = arg {
        if s.eq_ignore_ascii_case("off")
            || s.eq_ignore_ascii_case("none")
            || s.eq_ignore_ascii_case("disabled")
            || s == "0"
            || s.eq_ignore_ascii_case("false")
        {
            None
        } else {
            match ReasoningEffort::from_str_loose(s) {
                Some(e) => Some(e),
                None => {
                    ui.draw_initial_box();
                    ui.emit_above(&format!(
                        "{} Unknown {} '{}'. Available: minimal, low, medium, high, xhigh, max, off",
                        "✖".red().bold(),
                        concept_name,
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
        let reasoning_items: Vec<String> = reasoning_options
            .iter()
            .map(|(label, _)| label.to_string())
            .collect();
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
            match select_from_rx(rx, ui, title, &reasoning_items, default_idx, false, false).await {
                Some(i) => reasoning_options[i].1,
                None => {
                    return Ok(());
                }
            }
        } else {
            let cur = agent.lock().await.reasoning_effort();
            let cur_label = cur
                .map(|e| e.to_string())
                .unwrap_or_else(|| "off".to_string());
            ui.emit_above(&format!(
                "{} Current {} is '{}'",
                "ℹ".blue().bold(),
                concept_name,
                cur_label.yellow()
            ));
            return Ok(());
        }
    };

    agent.lock().await.set_reasoning_effort(chosen_effort);
    // Same as the model switch: the picker left a box behind, so erase it before redrawing.
    ui.clear_box_on_exit();
    ui.draw_initial_box();

    let effort_label = chosen_effort
        .map(|e| e.to_string())
        .unwrap_or_else(|| "off".to_string());

    let label_capitalized = if is_thinking {
        "Thinking level"
    } else {
        "Reasoning effort"
    };

    ui.emit_above(&format!(
        "{} {} set to '{}'",
        "✔".green().bold(),
        label_capitalized,
        effort_label.yellow()
    ));

    Ok(())
}
