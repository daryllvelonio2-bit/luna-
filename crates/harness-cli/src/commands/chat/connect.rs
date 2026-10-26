//! `/connect` — the provider setup flow, inside the chat.
//!
//! The same job `luna setup` does, reached without leaving the conversation: add a provider's
//! API key, see which providers are connected, drop a key, and pull in a local Antigravity or
//! OpenCode login.
//!
//! It is built from the chat's own key primitives (`select_from_rx`, `input_from_rx`) rather than
//! `dialoguer`: the chat owns stdin in raw mode with a key reader thread attached, and a second
//! reader on the same fd would race it for keystrokes.

use std::io::Write;
use std::sync::Arc;

use colored::Colorize;

use harness_core::HarnessError;
use harness_providers::{get_provider_catalog, CredentialManager, RotationStrategy};

use super::super::chat::AppKey;
use super::ui::ChatUi;

/// The action `/connect` runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectAction {
    AddKey,
    ListKeys,
    RemoveKey,
    ImportAntigravity,
    ImportOpencode,
    RotationStrategy,
    ResetCooldowns,
    Back,
}

impl ConnectAction {
    fn label(self) -> &'static str {
        match self {
            ConnectAction::AddKey => "Add provider API key",
            ConnectAction::ListKeys => "List connected providers",
            ConnectAction::RemoveKey => "Remove a credential",
            ConnectAction::ImportAntigravity => "Import Antigravity / Google",
            ConnectAction::ImportOpencode => "Import OpenCode / Shiina",
            ConnectAction::RotationStrategy => "Set rotation strategy",
            ConnectAction::ResetCooldowns => "Reset cooldowns",
            ConnectAction::Back => "Back to chat",
        }
    }
}

/// Run the connect flow.
///
/// Returns `Err` only when the flow could not be entered at all; a failed sub-action is reported
/// in the chat and the flow continues, so one bad key does not end the session.
pub(crate) async fn handle_connect_command(
    key_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    cred_manager: &Arc<CredentialManager>,
) -> Result<(), HarnessError> {
    // The chat's own keys are read from this channel, so anything already queued belongs to the
    // command line the user just typed, not to this menu.
    while key_rx.try_recv().is_ok() {}

    loop {
        let Some(action) = pick_action(key_rx, ui).await else {
            // Esc from the menu: back to the chat, not out of the session.
            ui.clear_box_on_exit();
            ui.draw_initial_box();
            return Ok(());
        };

        let outcome = match action {
            ConnectAction::AddKey => add_key(key_rx, ui, cred_manager).await,
            ConnectAction::ListKeys => list_keys(ui, cred_manager).await,
            ConnectAction::RemoveKey => remove_key(key_rx, ui, cred_manager).await,
            ConnectAction::ImportAntigravity => import_antigravity(ui, cred_manager).await,
            ConnectAction::ImportOpencode => import_opencode(ui, cred_manager).await,
            ConnectAction::RotationStrategy => set_strategy(key_rx, ui, cred_manager).await,
            ConnectAction::ResetCooldowns => reset_cooldowns(key_rx, ui, cred_manager).await,
            ConnectAction::Back => {
                ui.clear_box_on_exit();
                ui.draw_initial_box();
                return Ok(());
            }
        };

        if let Err(error) = outcome {
            ui.clear_box_on_exit();
            ui.draw_initial_box();
            ui.emit_above(&format!("{} {}", "✖".red().bold(), error.to_string().red()));
            return Ok(());
        }

        // Each action leaves the composer where it was found, so redraw it before the next
        // question. Without this the menu opens over a stale box and the prompt drifts.
        ui.clear_box_on_exit();
        ui.draw_initial_box();
    }
}

/// The menu of connect actions.
async fn pick_action(
    key_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
) -> Option<ConnectAction> {
    const ACTIONS: [ConnectAction; 8] = [
        ConnectAction::AddKey,
        ConnectAction::ListKeys,
        ConnectAction::RemoveKey,
        ConnectAction::ImportAntigravity,
        ConnectAction::ImportOpencode,
        ConnectAction::RotationStrategy,
        ConnectAction::ResetCooldowns,
        ConnectAction::Back,
    ];

    let items: Vec<String> = ACTIONS.iter().map(|a| a.label().to_string()).collect();
    let index =
        super::commands::select_from_rx(key_rx, ui, "Connect a provider", &items, 0, false, false)
            .await?;

    ACTIONS.get(index).copied()
}

/// The providers a key can be added for, with the ones already connected marked.
async fn provider_items(cred_manager: &CredentialManager) -> Vec<(String, bool)> {
    let loaded = cred_manager.get_loaded_providers().await;
    get_provider_catalog()
        .into_iter()
        .map(|p| {
            let connected = loaded.iter().any(|l| l.eq_ignore_ascii_case(&p.id));
            let tag = if connected { " [connected]" } else { "" };
            (format!("{} ({}){}", p.display_name, p.id, tag), connected)
        })
        .collect()
}

/// Add an API key for a provider.
async fn add_key(
    key_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    cred_manager: &Arc<CredentialManager>,
) -> Result<(), HarnessError> {
    let providers = provider_items(cred_manager).await;
    let items: Vec<String> = providers.iter().map(|(label, _)| label.clone()).collect();

    let Some(index) =
        super::commands::select_from_rx(key_rx, ui, "Select provider", &items, 0, false, false)
            .await
    else {
        return Ok(());
    };

    let provider_id = provider_id_of(&providers[index].0).to_string();

    let Some(label) = super::commands::input_from_rx(key_rx, "Credential label").await else {
        return Ok(());
    };
    let label = if label.trim().is_empty() {
        format!("{}-primary", provider_id)
    } else {
        label.trim().to_string()
    };

    // The key is typed into the chat's composer, which echoes what it receives. A secret that
    // echoes is a secret in the scrollback, so the prompt is masked by hand: the characters are
    // taken but only a dot is shown.
    let Some(api_key) = masked_input(key_rx, "API key (masked)").await else {
        return Ok(());
    };
    if api_key.is_empty() {
        ui.emit_above(&format!("{} No key entered — nothing added", "•".dimmed()));
        return Ok(());
    }

    let id = cred_manager
        .add_credential(&provider_id, &label, &api_key, true)
        .await?;

    ui.emit_above(&format!(
        "{} Credential '{}' added for {} ({})",
        "✔".green().bold(),
        label.bold(),
        provider_id.cyan(),
        id.dimmed()
    ));
    Ok(())
}

/// The provider id out of a rendered menu label like `NVIDIA NIM (nvidia)`.
fn provider_id_of(label: &str) -> &str {
    match label.rfind('(') {
        Some(open) => label[open + 1..].trim_end_matches(')'),
        None => label.trim(),
    }
}

/// Type a secret without echoing it.
async fn masked_input(
    key_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppKey>,
    prompt: &str,
) -> Option<String> {
    print!("{}: ", prompt.bold().cyan());
    let _ = std::io::stdout().flush();

    let mut secret = String::new();
    while let Some(key) = key_rx.recv().await {
        match key {
            AppKey::Char(c) if !c.is_control() => {
                secret.push(c);
                print!("*");
                let _ = std::io::stdout().flush();
            }
            AppKey::Backspace => {
                if secret.pop().is_some() {
                    print!("\x08 \x08");
                    let _ = std::io::stdout().flush();
                }
            }
            AppKey::Enter => {
                println!();
                let _ = std::io::stdout().flush();
                return Some(secret);
            }
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => {
                println!();
                let _ = std::io::stdout().flush();
                return None;
            }
            _ => {}
        }
    }
    None
}

/// List what is connected.
async fn list_keys(ui: &ChatUi, cred_manager: &Arc<CredentialManager>) -> Result<(), HarnessError> {
    let mut any = false;
    for provider in get_provider_catalog() {
        let creds = cred_manager.get_credentials(&provider.id).await;
        if creds.is_empty() {
            continue;
        }
        any = true;
        ui.emit_above(&format!(
            "{} {}",
            provider.display_name.bold(),
            format!("({})", provider.id).dimmed()
        ));
        for cred in creds {
            let flags = [
                cred.is_priority.then_some("priority"),
                cred.is_temporarily_disabled.then_some("cooldown"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
            let flags = if flags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", flags)
            };
            ui.emit_above(&format!(
                "    {} — used {} time(s){}",
                cred.label.green(),
                cred.usage_count,
                flags.dimmed()
            ));
        }
    }

    if !any {
        ui.emit_above(&format!(
            "{} No providers connected yet — add one above",
            "•".dimmed()
        ));
    }
    Ok(())
}

/// Remove a credential.
async fn remove_key(
    key_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    cred_manager: &Arc<CredentialManager>,
) -> Result<(), HarnessError> {
    let mut connected: Vec<String> = Vec::new();
    for provider in get_provider_catalog() {
        if !cred_manager.get_credentials(&provider.id).await.is_empty() {
            connected.push(provider.id.clone());
        }
    }

    if connected.is_empty() {
        ui.emit_above(&format!(
            "{} Nothing to remove — no providers connected",
            "•".dimmed()
        ));
        return Ok(());
    }

    let Some(index) = super::commands::select_from_rx(
        key_rx,
        ui,
        "Provider to remove from",
        &connected,
        0,
        false,
        false,
    )
    .await
    else {
        return Ok(());
    };
    let provider = connected[index].clone();

    let creds = cred_manager.get_credentials(&provider).await;
    let items: Vec<String> = creds
        .iter()
        .map(|c| format!("{} (used {})", c.label, c.usage_count))
        .collect();

    let Some(cred_index) = super::commands::select_from_rx(
        key_rx,
        ui,
        &format!("Credential of {}", provider),
        &items,
        0,
        false,
        false,
    )
    .await
    else {
        return Ok(());
    };
    let target = creds[cred_index].id.clone();

    let removed = cred_manager.remove_credential(&provider, &target).await?;
    ui.emit_above(&format!(
        "{} Credential removed from {}",
        if removed {
            "✔".green().bold()
        } else {
            "✖".red().bold()
        },
        provider.cyan()
    ));
    Ok(())
}

/// Copy a local Antigravity login into the credential pool.
async fn import_antigravity(
    ui: &ChatUi,
    cred_manager: &Arc<CredentialManager>,
) -> Result<(), HarnessError> {
    let detected = harness_providers::detect_antigravity_credentials()?;
    let label = detected
        .email
        .clone()
        .unwrap_or_else(|| "antigravity-user".to_string());
    let payload =
        serde_json::to_string(&detected).unwrap_or_else(|_| detected.access_token.clone());

    let id = cred_manager
        .add_credential("antigravity", &label, &payload, true)
        .await?;
    let _ = cred_manager
        .set_rotation_strategy("antigravity", RotationStrategy::FillFirst)
        .await;

    ui.emit_above(&format!(
        "{} Antigravity credentials imported ({})",
        "✔".green().bold(),
        id.dimmed()
    ));
    Ok(())
}

/// Copy local OpenCode credentials into the credential pool.
async fn import_opencode(
    ui: &ChatUi,
    cred_manager: &Arc<CredentialManager>,
) -> Result<(), HarnessError> {
    let detected = harness_providers::detect_opencode_credentials()?;
    if detected.is_empty() {
        ui.emit_above(&format!(
            "{} No OpenCode credentials found on this machine",
            "•".dimmed()
        ));
        return Ok(());
    }

    for cred in detected {
        let id = cred_manager
            .add_credential(&cred.provider, &cred.label, &cred.api_key, cred.is_priority)
            .await?;
        // The metered free-tier token is a fallback: promoting it would make every request
        // wait out the provider's rate limiter while the account key sits unused in the pool.
        if cred.is_priority {
            ui.emit_above(&format!(
                "{} {} imported as the priority key ({})",
                "✔".green().bold(),
                cred.provider.cyan(),
                id.dimmed()
            ));
        } else {
            ui.emit_above(&format!(
                "{} {} imported as a fallback ({})",
                "✔".green().bold(),
                cred.provider.cyan(),
                id.dimmed()
            ));
        }
    }
    Ok(())
}

/// Choose how a provider's keys are picked.
async fn set_strategy(
    key_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    cred_manager: &Arc<CredentialManager>,
) -> Result<(), HarnessError> {
    let mut connected: Vec<String> = Vec::new();
    for provider in get_provider_catalog() {
        if !cred_manager.get_credentials(&provider.id).await.is_empty() {
            connected.push(provider.id.clone());
        }
    }
    if connected.is_empty() {
        ui.emit_above(&format!("{} No providers connected yet", "•".dimmed()));
        return Ok(());
    }

    let Some(index) = super::commands::select_from_rx(
        key_rx,
        ui,
        "Provider to configure",
        &connected,
        0,
        false,
        false,
    )
    .await
    else {
        return Ok(());
    };
    let provider = connected[index].clone();

    const STRATEGIES: [(&str, RotationStrategy); 4] = [
        (
            "round_robin — cycle through keys evenly",
            RotationStrategy::RoundRobin,
        ),
        (
            "fill_first — use the first key until it fails",
            RotationStrategy::FillFirst,
        ),
        (
            "least_used — always pick the least-used key",
            RotationStrategy::LeastUsed,
        ),
        ("random — pick a key at random", RotationStrategy::Random),
    ];
    let items: Vec<String> = STRATEGIES
        .iter()
        .map(|(label, _)| label.to_string())
        .collect();

    let Some(strategy_index) = super::commands::select_from_rx(
        key_rx,
        ui,
        &format!("Strategy for {}", provider),
        &items,
        0,
        false,
        false,
    )
    .await
    else {
        return Ok(());
    };

    cred_manager
        .set_rotation_strategy(&provider, STRATEGIES[strategy_index].1)
        .await?;
    ui.emit_above(&format!(
        "{} Rotation strategy for {} set to {}",
        "✔".green().bold(),
        provider.cyan(),
        STRATEGIES[strategy_index].0.split(' ').next().unwrap_or("")
    ));
    Ok(())
}

/// Re-enable keys that are in cooldown.
async fn reset_cooldowns(
    key_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    cred_manager: &Arc<CredentialManager>,
) -> Result<(), HarnessError> {
    let mut connected: Vec<String> = Vec::new();
    for provider in get_provider_catalog() {
        if !cred_manager.get_credentials(&provider.id).await.is_empty() {
            connected.push(provider.id.clone());
        }
    }

    let items: Vec<String> = std::iter::once("All providers".to_string())
        .chain(connected.iter().cloned())
        .collect();

    let Some(index) =
        super::commands::select_from_rx(key_rx, ui, "Reset cooldowns for", &items, 0, false, false)
            .await
    else {
        return Ok(());
    };

    let target = if index == 0 {
        None
    } else {
        Some(connected[index - 1].clone())
    };
    let count = cred_manager.reset_cooldowns(target.as_deref()).await?;
    ui.emit_above(&format!(
        "{} {} credential(s) re-enabled",
        "✔".green().bold(),
        count
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_id_is_read_out_of_its_menu_label() {
        assert_eq!(provider_id_of("NVIDIA NIM (nvidia)"), "nvidia");
        assert_eq!(
            provider_id_of("Custom (OpenAI-compatible) (custom)"),
            "custom"
        );
        assert_eq!(provider_id_of("plain"), "plain");
    }

    #[test]
    fn every_menu_entry_has_a_label() {
        const ACTIONS: [ConnectAction; 8] = [
            ConnectAction::AddKey,
            ConnectAction::ListKeys,
            ConnectAction::RemoveKey,
            ConnectAction::ImportAntigravity,
            ConnectAction::ImportOpencode,
            ConnectAction::RotationStrategy,
            ConnectAction::ResetCooldowns,
            ConnectAction::Back,
        ];
        for action in ACTIONS {
            assert!(!action.label().is_empty());
        }
    }
}
