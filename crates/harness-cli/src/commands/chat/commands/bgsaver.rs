//! `/bgsaver-model` — choose the provider and model the background memory saver bills to.
//!
//! The saver is built from `memory.background_provider` / `memory.background_model` at startup.
//! This command reuses the existing provider → model picker, persists the pair globally, and swaps
//! the live saver so the next pass uses it without restarting the session.
//!
//! There is no per-session scope: unlike compaction, the saver has no session row to pin a model
//! to, so the only honest target is the config file.

use std::sync::Arc;

use colored::Colorize;
use tokio::sync::mpsc;

use harness_core::config::Config;
use harness_core::{HarnessError, MemoryEngine, NotesEngine};
use harness_providers::{CredentialManager, ModelDiscovery};

use super::picker::pick_provider_and_model;
use super::{Feature, FeatureToggles};
use crate::commands::chat::keys::AppKey;
use crate::commands::chat::memory_writer::build_saver_for;
use crate::commands::chat::ui::ChatUi;
use crate::commands::chat::ChatAgent;

const PROVIDER_KEY: &str = "memory.background_provider";
const MODEL_KEY: &str = "memory.background_model";

/// `/bgsaver-model` — pick, persist, and live-apply the background saver's provider/model.
///
/// `/bgsaver-model reset` clears both keys and falls back to the session's model.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_bgsaver_model_command(
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &mut Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    toggles: &FeatureToggles,
    current_provider: &str,
    current_model: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    if arg.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "reset" | "default" | "off"
        )
    }) {
        let layers =
            harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
        harness_core::config::unset_in_file(&layers, PROVIDER_KEY)?;
        harness_core::config::unset_in_file(&layers, MODEL_KEY)?;

        config.memory.background_provider = None;
        config.memory.background_model = None;

        if config.memory.mode.is_background() {
            let (memory, notes) = engines(agent).await;
            let saver = build_saver_for(
                config,
                &memory,
                notes,
                current_provider,
                current_model,
                cred_manager.clone(),
                discovery.clone(),
            )?;
            attach(agent, saver, toggles).await;
        }

        ui.clear_box_on_exit();
        ui.draw_initial_box();
        ui.emit_above(&format!(
            "{} Background saver reset to the session model: {} ({})",
            "✔".green().bold(),
            current_model.cyan(),
            current_provider.dimmed()
        ));
        return Ok(());
    }

    let seed_provider = config
        .memory
        .background_provider
        .as_deref()
        .unwrap_or(current_provider);
    let seed_model = config
        .memory
        .background_model
        .as_deref()
        .unwrap_or(current_model);

    let Some((provider, model)) = pick_provider_and_model(
        key_rx,
        ui,
        cred_manager,
        discovery,
        seed_provider,
        seed_model,
        false,
    )
    .await?
    else {
        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(());
    };

    // Build before writing: a bad provider/model pair must not reach the config file.
    let (memory, notes) = engines(agent).await;
    let saver = build_saver_for(
        config,
        &memory,
        notes,
        &provider,
        &model,
        cred_manager.clone(),
        discovery.clone(),
    )?;

    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    if let Err(error) = harness_core::config::set_in_file(&layers, PROVIDER_KEY, &provider) {
        ui.emit_above(&format!(
            "{} Background saver switched for this run, but saving the provider failed: {}",
            "⚠".yellow().bold(),
            error
        ));
    }
    if let Err(error) = harness_core::config::set_in_file(&layers, MODEL_KEY, &model) {
        ui.emit_above(&format!(
            "{} Background saver switched for this run, but saving the model failed: {}",
            "⚠".yellow().bold(),
            error
        ));
    }

    config.memory.background_provider = Some(provider.clone());
    config.memory.background_model = Some(model.clone());

    ui.clear_box_on_exit();
    ui.draw_initial_box();

    if config.memory.mode.is_background() {
        attach(agent, saver, toggles).await;
        ui.emit_above(&format!(
            "{} Background saver → {} ({}) · next pass uses it",
            "✔".green().bold(),
            model.cyan(),
            provider.dimmed()
        ));
    } else {
        ui.emit_above(&format!(
            "{} Background saver model saved: {} ({}). Enable {} to use it.",
            "✔".green().bold(),
            model.cyan(),
            provider.dimmed(),
            "memory.mode: background".yellow()
        ));
    }

    Ok(())
}

/// The engines the live saver writes through. Cloned out of the agent so the lock is not held
/// while the picker or a provider call runs.
async fn engines(
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
) -> (Arc<dyn MemoryEngine>, Arc<dyn NotesEngine>) {
    let agent = agent.lock().await;
    let memory: Arc<dyn MemoryEngine> = agent.memory().clone();
    let notes: Arc<dyn NotesEngine> = agent.notes().clone();
    (memory, notes)
}

/// Replace the live saver and carry the `/memory off` and `/notes off` toggles across the swap.
async fn attach(
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    saver: harness_memory::MemorySaver,
    toggles: &FeatureToggles,
) {
    let mut agent = agent.lock().await;
    agent.set_background_saver(Some(Arc::new(saver)));
    agent.set_memory_enabled(toggles.enabled(Feature::Memory));
    agent.set_notes_enabled(toggles.enabled(Feature::Notes));
}
