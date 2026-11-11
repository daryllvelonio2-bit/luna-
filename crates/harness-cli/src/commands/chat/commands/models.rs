//! The slash commands that change how the model answers rather than what it answers.
//!
//! Provider and model selection, the compaction model, and the default model. They share one
//! shape: pick a value, persist it to the layer the user asked for, rebind the agent.

use colored::Colorize;
use std::sync::Arc;
use tokio::sync::mpsc;

use harness_core::{Config, HarnessError, ReasoningEffort, SessionStore, SqliteSessionStore};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

use crate::commands::chat::keys::AppKey;
use crate::commands::chat::status::refresh_antigravity_status_if_needed;
use crate::commands::chat::ui::ChatUi;
use crate::commands::chat::ChatAgent;
use crate::commands::context_window::context_window;

use super::picker::{pick_provider_and_model, select_from_rx};

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
        current_provider.as_str(),
        current_model.as_str(),
        true,
    )
    .await?
    else {
        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(());
    };

    let Some(chosen_effort) = pick_reasoning_effort(&mut key_rx, ui, agent).await else {
        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(());
    };

    apply_model_switch(
        agent,
        config,
        cred_manager,
        discovery,
        &chosen_provider,
        &chosen_model,
        chosen_effort,
    )
    .await?;

    *current_provider = chosen_provider.clone();
    *current_model = chosen_model.clone();

    // The switch is session state, not just process state: stamping the row means a later
    // `luna --session <id>` resumes on this pair instead of the config default. A failure to
    // persist is surfaced as a warning — the in-memory switch already happened.
    persist_session_model(ui, store, session_id, &chosen_provider, &chosen_model).await;

    // If compaction model is not custom set, default it to the used model
    if compact_model.is_none() {
        set_default_compaction(
            agent,
            config,
            cred_manager,
            discovery,
            &chosen_provider,
            &chosen_model,
        )
        .await;
    }

    redraw_model_status(
        ui,
        config,
        discovery,
        agent,
        cred_manager,
        &chosen_provider,
        &chosen_model,
    )
    .await;

    Ok(())
}

/// Ask for the reasoning effort, returning `None` when the picker was cancelled.
async fn pick_reasoning_effort(
    key_rx: &mut Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
) -> Option<Option<ReasoningEffort>> {
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
            None => return None,
        }
    } else {
        default_reasoning_idx
    };

    Some(if reasoning_sel == 0 {
        current_effort
    } else {
        reasoning_options[reasoning_sel].1
    })
}

/// Rebind the live agent to the chosen provider, model and effort.
async fn apply_model_switch(
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    chosen_provider: &str,
    chosen_model: &str,
    chosen_effort: Option<ReasoningEffort>,
) -> Result<(), HarnessError> {
    let new_provider = Arc::new(GenericProviderClient::new(
        chosen_provider.to_string(),
        chosen_model.to_string(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(chosen_provider),
    )?);

    {
        let mut ag = agent.lock().await;
        ag.set_provider(new_provider);
        ag.set_reasoning_effort(chosen_effort);
        ag.set_context_window(Some(
            context_window(config, discovery, chosen_provider, chosen_model) as usize,
        ));
    }
    Ok(())
}

/// Persist the pair on the session row, warning if the write fails.
async fn persist_session_model(
    ui: &ChatUi,
    store: &SqliteSessionStore,
    session_id: &str,
    chosen_provider: &str,
    chosen_model: &str,
) {
    if let Err(error) = store
        .set_model(session_id, chosen_provider, chosen_model)
        .await
    {
        ui.emit_above(&format!(
            "{} Model switched in this session, but persisting it failed: {}",
            "⚠".yellow(),
            error
        ));
    }
}

/// Point compaction at the active model, unless a dedicated compaction model is configured.
async fn set_default_compaction(
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    chosen_provider: &str,
    chosen_model: &str,
) {
    if let Ok(comp_client) = GenericProviderClient::new(
        chosen_provider.to_string(),
        chosen_model.to_string(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(chosen_provider),
    ) {
        agent
            .lock()
            .await
            .set_compaction(Some(harness_core::CompactionSetup {
                provider: Arc::new(comp_client),
                model: chosen_model.to_string(),
            }));
    }
}

/// The chained pickers redraw the box as they tear down — with the pre-switch model. Erase
/// that stale box so the status bar is drawn once, showing the model now in use.
async fn redraw_model_status(
    ui: &ChatUi,
    config: &Config,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    cred_manager: &Arc<CredentialManager>,
    chosen_provider: &str,
    chosen_model: &str,
) {
    let context_max = context_window(config, discovery, chosen_provider, chosen_model);
    // Switching models does not touch the transcript, so keep the real usage rather than resetting
    // the bar to 0.
    let context_used = agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    ui.set_model(chosen_model, context_used, context_max);
    refresh_antigravity_status_if_needed(ui, chosen_provider, chosen_model, cred_manager).await;
    ui.clear_box_on_exit();
    ui.draw_initial_box();
}

/// `/default_model`: choose the provider/model new sessions start on.
///
/// The picker chain is the one `/model` uses — provider, then model — with a trailing scope
/// choice. Session scope stamps the session row, exactly like `/model`; global scope writes
/// `model.provider` / `model.default` and clears the session pin so every future session follows
/// the configured default.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_default_model_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &mut Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &SqliteSessionStore,
    session_id: &str,
    current_provider: &mut String,
    current_model: &mut String,
    compact_model: &Option<String>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let scope = arg.map(|a| a.trim().to_ascii_lowercase());
    let explicit_global = scope
        .as_deref()
        .is_some_and(|a| matches!(a, "global" | "--global" | "g"));
    let explicit_session = scope
        .as_deref()
        .is_some_and(|a| matches!(a, "session" | "--session" | "this" | "local" | "s"));

    let Some((chosen_provider, chosen_model)) = pick_provider_and_model(
        key_rx.as_deref_mut(),
        ui,
        cred_manager,
        discovery,
        current_provider.as_str(),
        current_model.as_str(),
        true,
    )
    .await?
    else {
        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(());
    };

    // Scope comes after the model picker, like `/compact-model`. An explicit argument skips the
    // menu so `/default_model global` remains usable without another prompt.
    let Some(is_global) =
        choose_default_model_scope(key_rx, ui, explicit_global, explicit_session).await
    else {
        return Ok(());
    };

    // Resolve the config layers before touching live state: a broken config file should fail the
    // command rather than half-apply it.
    let layers = if is_global {
        Some(harness_core::config::process_layers(
            crate::commands::config::catalog_defaults(),
        )?)
    } else {
        None
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
        ag.set_context_window(Some(context_window(
            config,
            discovery,
            &chosen_provider,
            &chosen_model,
        ) as usize));
    }

    *current_provider = chosen_provider.clone();
    *current_model = chosen_model.clone();

    let persisted_globally = persist_default_model(
        ui,
        config,
        store,
        session_id,
        chosen_provider.clone(),
        chosen_model.clone(),
        layers,
    )
    .await;

    // Keep compaction on the active model while no dedicated compaction model is configured,
    // matching `/model`.
    apply_default_model_live(
        ui,
        config,
        cred_manager,
        discovery,
        agent,
        compact_model,
        chosen_provider.clone(),
        chosen_model.clone(),
    )
    .await;

    if persisted_globally {
        ui.emit_above(&format!(
            "{} Default model set globally: {} ({})",
            "✓".green(),
            chosen_model,
            chosen_provider
        ));
    } else if is_global {
        // The warning above explains the failed write; name the live switch too.
        ui.emit_above(&format!(
            "{} Default model switched for this session: {} ({})",
            "✓".green(),
            chosen_model,
            chosen_provider
        ));
    } else {
        ui.emit_above(&format!(
            "{} Default model set for this session: {} ({})",
            "✓".green(),
            chosen_model,
            chosen_provider
        ));
    }

    Ok(())
}

/// Ask for the scope of a `/default_model` switch, or honor an explicit argument.
///
/// `None` means the picker was cancelled; the box was already restored.
async fn choose_default_model_scope(
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    explicit_global: bool,
    explicit_session: bool,
) -> Option<bool> {
    if explicit_global {
        return Some(true);
    }
    if explicit_session {
        return Some(false);
    }
    let Some(rx) = key_rx else {
        return Some(false);
    };
    let scope_options = [
        "This session only (remember for this session)".to_string(),
        "Globally (save as default for all sessions)".to_string(),
    ];
    while rx.try_recv().is_ok() {}
    match select_from_rx(
        rx,
        ui,
        "Default Model Scope",
        &scope_options,
        0,
        true,
        false,
    )
    .await
    {
        Some(1) => Some(true),
        Some(0) => Some(false),
        None => {
            ui.clear_box_on_exit();
            ui.draw_initial_box();
            None
        }
        _ => Some(false),
    }
}

/// Persist the chosen pair: to the config files when global, otherwise to the session row.
///
/// Returns whether the global write succeeded.
async fn persist_default_model(
    ui: &ChatUi,
    config: &mut Config,
    store: &SqliteSessionStore,
    session_id: &str,
    chosen_provider: String,
    chosen_model: String,
    layers: Option<harness_core::config::Layers>,
) -> bool {
    let mut persisted_globally = false;
    if let Some(layers) = layers {
        let provider_write =
            harness_core::config::set_in_file(&layers, "model.provider", &chosen_provider);
        let model_write =
            harness_core::config::set_in_file(&layers, "model.default", &chosen_model);

        match (&provider_write, &model_write) {
            (Ok(_), Ok(_)) => {
                config.model.provider = chosen_provider.clone();
                config.model.default = chosen_model.clone();
                // The session pin outranks the config default on resume, so drop it: "global"
                // means this session follows the configured default too.
                if let Err(error) = store.clear_model(session_id).await {
                    ui.emit_above(&format!(
                        "{} Default model saved globally, but clearing this session's model pin failed: {}",
                        "⚠".yellow(),
                        error
                    ));
                }
                persisted_globally = true;
            }
            _ => {
                if let Err(error) = &provider_write {
                    ui.emit_above(&format!(
                        "{} Failed to persist global provider: {}",
                        "⚠".yellow(),
                        error
                    ));
                }
                if let Err(error) = &model_write {
                    ui.emit_above(&format!(
                        "{} Failed to persist global model: {}",
                        "⚠".yellow(),
                        error
                    ));
                }
                // The global write failed, so at least keep the pair on this session's row.
                if let Err(error) = store
                    .set_model(session_id, &chosen_provider, &chosen_model)
                    .await
                {
                    ui.emit_above(&format!(
                        "{} Model switched, but persisting it for this session failed: {}",
                        "⚠".yellow(),
                        error
                    ));
                }
            }
        }
    } else if let Err(error) = store
        .set_model(session_id, &chosen_provider, &chosen_model)
        .await
    {
        ui.emit_above(&format!(
            "{} Model switched, but persisting it for this session failed: {}",
            "⚠".yellow(),
            error
        ));
    }

    persisted_globally
}

/// Rebind the live agent, keep compaction on the active model, and redraw the status bar.
#[allow(clippy::too_many_arguments)]
async fn apply_default_model_live(
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    compact_model: &Option<String>,
    chosen_provider: String,
    chosen_model: String,
) {
    // Keep compaction on the active model while no dedicated compaction model is configured,
    // matching `/model`.
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

    let context_max = context_window(config, discovery, &chosen_provider, &chosen_model);
    let context_used = agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    ui.set_model(&chosen_model, context_used, context_max);
    refresh_antigravity_status_if_needed(ui, &chosen_provider, &chosen_model, cred_manager).await;
    ui.clear_box_on_exit();
    ui.draw_initial_box();
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
            return reset_compaction_model(
                ui,
                config,
                cred_manager,
                discovery,
                agent,
                store,
                session_id,
                current_provider,
                current_model,
                compact_provider,
                compact_model,
            )
            .await;
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

    let Some(is_global) = pick_compaction_scope(&mut key_rx, ui, arg).await else {
        return Ok(());
    };

    apply_compaction_model(
        agent,
        config,
        cred_manager,
        discovery,
        &chosen_provider,
        &chosen_model,
    )
    .await?;

    *compact_provider = Some(chosen_provider.clone());
    *compact_model = Some(chosen_model.clone());

    persist_compaction_scope(
        ui,
        store,
        session_id,
        &chosen_provider,
        &chosen_model,
        is_global,
    )
    .await?;

    ui.clear_box_on_exit();
    ui.draw_initial_box();

    Ok(())
}

/// Restore compaction to the session's active model.
#[allow(clippy::too_many_arguments)]
async fn reset_compaction_model(
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
) -> Result<(), HarnessError> {
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
    Ok(())
}

/// Prompt for scope: this session only vs globally. `None` means the picker was cancelled.
async fn pick_compaction_scope(
    key_rx: &mut Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    arg: Option<&str>,
) -> Option<bool> {
    if let Some(a) = arg {
        return Some(a.eq_ignore_ascii_case("global") || a.eq_ignore_ascii_case("--global"));
    }
    let Some(ref mut rx) = key_rx else {
        return Some(false);
    };
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
        Some(1) => Some(true),
        Some(0) => Some(false),
        None => {
            ui.clear_box_on_exit();
            ui.draw_initial_box();
            None
        }
        _ => Some(false),
    }
}

/// Rebind compaction to the chosen provider and model.
async fn apply_compaction_model(
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    chosen_provider: &str,
    chosen_model: &str,
) -> Result<(), HarnessError> {
    let comp_client = GenericProviderClient::new(
        chosen_provider.to_string(),
        chosen_model.to_string(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(chosen_provider),
    )?;

    agent
        .lock()
        .await
        .set_compaction(Some(harness_core::CompactionSetup {
            provider: Arc::new(comp_client),
            model: chosen_model.to_string(),
        }));
    Ok(())
}

/// Persist the compaction pair globally or on the session row, and report the result.
async fn persist_compaction_scope(
    ui: &ChatUi,
    store: &SqliteSessionStore,
    session_id: &str,
    chosen_provider: &str,
    chosen_model: &str,
    is_global: bool,
) -> Result<(), HarnessError> {
    if is_global {
        let layers =
            harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
        if let Err(e) =
            harness_core::config::set_in_file(&layers, "agent.compaction_provider", chosen_provider)
        {
            ui.emit_above(&format!(
                "{} Failed to persist global compaction provider: {}",
                "⚠".yellow(),
                e
            ));
        }
        if let Err(e) =
            harness_core::config::set_in_file(&layers, "agent.compaction_model", chosen_model)
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
            .set_compact_model(session_id, chosen_provider, chosen_model)
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
    Ok(())
}
