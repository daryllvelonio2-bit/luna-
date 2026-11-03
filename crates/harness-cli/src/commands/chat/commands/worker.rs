//! Named background workers: `/worker` configures a saved provider/model pair, and the worker's
//! own name invokes it on a fork of the current conversation.
//!
//! The profile is deliberately small — a provider, a model, and an optional reasoning effort — so a
//! worker is a cheaper second model for a narrow task, not a second agent configuration surface.

use std::sync::Arc;

use colored::Colorize;
use tokio::sync::mpsc;

use harness_core::{Config, HarnessError, ReasoningEffort, WorkerProfile};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

use super::picker::pick_provider_and_model;
use super::ChatAgent;
use crate::commands::chat::keys::AppKey;
use crate::commands::chat::ui::ChatUi;
use crate::commands::prompt_images::inline_image_paths;

/// `/worker` — list workers, or configure one with the provider/model picker.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_worker_command(
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &mut Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    current_provider: &str,
    current_model: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let arg = arg.unwrap_or("").trim();

    if arg.eq_ignore_ascii_case("delete") || arg.to_ascii_lowercase().starts_with("delete ") {
        let name = arg["delete".len()..].trim().to_lowercase();
        return delete_worker(ui, config, &name).await;
    }

    if arg.is_empty() || arg.eq_ignore_ascii_case("list") {
        list_workers(ui, config);
        return Ok(());
    }

    let name = arg.to_lowercase();
    harness_core::config::validate_worker_name(&name)?;
    if RESERVED_COMMAND_NAMES
        .iter()
        .any(|reserved| reserved.trim_start_matches('/').eq_ignore_ascii_case(&name))
    {
        return Err(HarnessError::ConfigurationError(format!(
            "worker name '{name}' is a built-in command; pick another name"
        )));
    }

    // Reuse the existing picker. A reconfiguration starts from the worker's saved model so
    // pressing Enter twice keeps the current choice instead of jumping back to the main model.
    let existing = config.workers.get(&name).cloned();
    let (seed_provider, seed_model) = existing
        .as_ref()
        .map(|w| (w.provider.clone(), w.model.clone()))
        .unwrap_or_else(|| (current_provider.to_string(), current_model.to_string()));

    let picked = pick_provider_and_model(
        key_rx,
        ui,
        cred_manager,
        discovery,
        &seed_provider,
        &seed_model,
        true,
    )
    .await?;

    let Some((provider, model)) = picked else {
        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(());
    };

    let profile = WorkerProfile {
        provider,
        model,
        reasoning_effort: existing.and_then(|worker| worker.reasoning_effort),
    };

    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    harness_core::config::set_worker_in_file(&layers, &name, &profile)?;
    config.workers.insert(name.clone(), profile.clone());

    ui.emit_above(&format!(
        "{} Worker {} → {} ({}) · invoke with {}",
        "✔".green().bold(),
        name.cyan().bold(),
        profile.model.cyan(),
        profile.provider.dimmed(),
        format!("/{name} <task>").yellow()
    ));
    Ok(())
}

fn list_workers(ui: &ChatUi, config: &Config) {
    if config.workers.is_empty() {
        ui.emit_above(&format!(
            "{} No workers configured. Create one with {}",
            "•".cyan(),
            "/worker <name>".cyan().bold()
        ));
        return;
    }

    let mut text = String::from("\nConfigured workers:\n");
    for (name, worker) in &config.workers {
        let effort = worker
            .reasoning_effort
            .map(|effort| format!(" · effort {}", effort.as_str()))
            .unwrap_or_default();
        text.push_str(&format!(
            "  {} {} ({}){}\n      invoke: {}\n",
            format!("/{name}").cyan().bold(),
            worker.model,
            worker.provider.dimmed(),
            effort.dimmed(),
            format!("/{name} <task>").yellow(),
        ));
    }
    text.push_str(&format!(
        "\nChange one with {}, remove one with {}.\n",
        "/worker <name>".cyan(),
        "/worker delete <name>".cyan()
    ));
    ui.emit_above(&text);
}

async fn delete_worker(ui: &ChatUi, config: &mut Config, name: &str) -> Result<(), HarnessError> {
    if name.is_empty() {
        return Err(HarnessError::ConfigurationError(
            "usage: /worker delete <name>".to_string(),
        ));
    }
    harness_core::config::validate_worker_name(name)?;

    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    let removed_file = harness_core::config::remove_worker_from_file(&layers, name)?;
    let removed_memory = config.workers.remove(name).is_some();

    if !removed_memory && removed_file.is_none() {
        ui.emit_above(&format!(
            "{} No worker named {}",
            "•".cyan(),
            name.cyan().bold()
        ));
    } else {
        ui.emit_above(&format!(
            "{} Removed worker {}",
            "✔".green().bold(),
            name.cyan().bold()
        ));
    }
    Ok(())
}

/// `/<name> <task>` — run a configured worker on a fork of the current conversation.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_worker_invocation(
    name: &str,
    task: &str,
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &str,
    delegate_tool: Option<&harness_core::tools::DelegateTaskTool>,
    pending_images: Option<&Arc<std::sync::Mutex<Vec<String>>>>,
) -> Result<bool, HarnessError> {
    let Some(delegate_tool) = delegate_tool else {
        ui.emit_above(&format!(
            "{} Workers are unavailable in this chat (no delegate tool registered).",
            "✖".red().bold()
        ));
        return Ok(false);
    };

    let Some(profile) = config.workers.get(name).cloned() else {
        return Ok(false);
    };

    let provider = Arc::new(GenericProviderClient::new(
        profile.provider.clone(),
        profile.model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&profile.provider),
    )?);

    // An unset effort inherits the conversation's current thinking level, so a worker is not
    // silently less (or more) deliberate than the session that invoked it.
    let effort: Option<ReasoningEffort> = match profile.reasoning_effort {
        Some(effort) => Some(effort),
        None => agent.lock().await.reasoning_effort(),
    };

    // A worker invocation is a prompt like any other: typed image paths become data URLs and
    // screenshots pasted before the command ride along with it. The goal stored for the subagent
    // list stays the clean text the user typed, so the viewer is not filled with base64.
    let clean_task = crate::commands::prompt_images::remove_image_markers(task);
    let inlined = inline_image_paths(&clean_task);
    let images = match pending_images {
        Some(pending) => std::mem::take(&mut *pending.lock().unwrap_or_else(|e| e.into_inner())),
        None => Vec::new(),
    };
    let mut prompt = format!("GOAL: {inlined}");
    if !images.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&images.join("\n\n"));
    }

    let child_id = match delegate_tool
        .spawn_worker(
            name,
            &clean_task,
            Some(&prompt),
            session_id,
            provider,
            &WorkerProfile {
                reasoning_effort: effort,
                ..profile.clone()
            },
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            // The images were taken from the composer for this invocation; if the worker never
            // started, put them back so the next turn (or a retry) still carries them.
            if let Some(pending) = pending_images {
                let mut guard = pending.lock().unwrap_or_else(|err| err.into_inner());
                for image in images.into_iter().rev() {
                    guard.insert(0, image);
                }
            }
            return Err(e);
        }
    };

    ui.emit_above(&format!(
        "{} Worker {} started on a fork of this conversation ({}). Use {} to watch it.",
        "✔".green().bold(),
        name.cyan().bold(),
        child_id.dimmed(),
        "/subagents".yellow()
    ));
    Ok(true)
}

/// Names reserved by built-in commands, so a worker cannot shadow a command the user relies on.
pub(crate) const RESERVED_COMMAND_NAMES: &[&str] = &[
    "/model",
    "/models",
    "/provider",
    "/compact-model",
    "/thinking",
    "/think",
    "/reasoning",
    "/name",
    "/sessions",
    "/session",
    "/new",
    "/summarize",
    "/rewind",
    "/diff",
    "/compact",
    "/auto-compact",
    "/expand",
    "/collapse",
    "/skills",
    "/mcp",
    "/plan",
    "/fork",
    "/tasks",
    "/jobs",
    "/subagents",
    "/usage",
    "/todo",
    "/clear",
    "/steer",
    "/queue",
    "/interrupt",
    "/heartbeat",
    "/hb",
    "/connect",
    "/dir",
    "/workspace",
    "/delete",
    "/exit",
    "/quit",
    "/help",
    "/commands",
    "/?",
    "/worker",
];
