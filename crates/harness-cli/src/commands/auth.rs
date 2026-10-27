use clap::Subcommand;
use colored::Colorize;
use dialoguer::{Input, Password, Select};
use std::io::Write;
use std::sync::Arc;

use harness_core::HarnessError;
use harness_onboarding::menu::{panel_header, Panel};
use harness_onboarding::screen::{self, Size};
use harness_onboarding::style::Line;
use harness_providers::{get_provider_catalog, CredentialManager, RotationStrategy};

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Add API key credentials for a provider
    Add {
        #[arg(short, long)]
        provider: Option<String>,
        #[arg(short, long)]
        label: Option<String>,
        #[arg(short, long)]
        key: Option<String>,
        #[arg(long)]
        priority: bool,
    },
    /// List all configured provider credentials
    List {
        #[arg(short, long)]
        provider: Option<String>,
    },
    /// Remove a registered credential by label or ID
    Remove {
        #[arg(short, long)]
        provider: String,
        #[arg(short, long)]
        target: String,
    },
    /// Reset cooldowns / temporary disabled status for a provider (or all providers)
    Reset {
        #[arg(short, long)]
        provider: Option<String>,
    },
    /// Set key rotation strategy (round_robin, fill_first, random, least_used)
    Strategy {
        #[arg(short, long)]
        provider: String,
        #[arg(short, long)]
        strategy: String,
    },
    /// Set a credential as the priority/primary key
    Priority {
        #[arg(short, long)]
        provider: String,
        #[arg(short, long)]
        target: String,
    },
    /// Take the priority flag away from a credential (demote it to a fallback)
    ClearPriority {
        #[arg(short, long)]
        provider: String,
        #[arg(short, long)]
        target: String,
    },
    /// Manage Google Antigravity OAuth credentials
    Antigravity {
        #[command(subcommand)]
        subcommand: AntigravitySubcommand,
    },
    /// Manage OpenCode credentials
    Opencode {
        #[command(subcommand)]
        subcommand: OpencodeSubcommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum AntigravitySubcommand {
    /// Copy logged-in Antigravity credentials from system/Shiina into Luna harness
    Copy,
}

#[derive(Subcommand, Debug)]
pub enum OpencodeSubcommand {
    /// Copy logged-in OpenCode credentials from system/Shiina into Luna harness
    Copy,
}

pub async fn handle_auth_command(
    cmd: AuthCommand,
    cred_manager: Arc<CredentialManager>,
) -> Result<(), HarnessError> {
    match cmd {
        AuthCommand::Add {
            provider,
            label,
            key,
            priority,
        } => {
            let provider_id = match provider {
                Some(p) => p,
                None => {
                    let catalog = get_provider_catalog();
                    let items: Vec<String> = catalog
                        .iter()
                        .map(|c| format!("{} ({})", c.display_name, c.id))
                        .collect();
                    let selection = Select::new()
                        .with_prompt("Select Provider")
                        .items(&items)
                        .default(0)
                        .interact()
                        .map_err(|e| HarnessError::Io(e.to_string()))?;
                    catalog[selection].id.clone()
                }
            };

            let lbl = match label {
                Some(l) => l,
                None => Input::<String>::new()
                    .with_prompt("Credential Label (e.g. 'work-account', 'backup-key')")
                    .interact_text()
                    .map_err(|e| HarnessError::Io(e.to_string()))?,
            };

            let api_key = match key {
                Some(k) => k,
                None => Password::new()
                    .with_prompt("API Key (masked)")
                    .interact()
                    .map_err(|e| HarnessError::Io(e.to_string()))?,
            };

            let id = cred_manager
                .add_credential(&provider_id, &lbl, &api_key, priority)
                .await?;

            println!(
                "{} Credential '{}' registered for provider '{}' (ID: {})",
                "✔".green().bold(),
                lbl.bold(),
                provider_id.cyan(),
                id
            );
        }
        AuthCommand::List { provider } => {
            let catalog = get_provider_catalog();
            let targets: Vec<String> = if let Some(p) = provider {
                vec![p]
            } else {
                catalog.iter().map(|c| c.id.clone()).collect()
            };

            println!("{}", "Luna Auth Credential Catalog:".bold().underline());
            for p_id in targets {
                let creds = cred_manager.get_credentials(&p_id).await;
                if creds.is_empty() {
                    continue;
                }
                println!("\nProvider: {}", p_id.cyan().bold());
                for c in creds {
                    let prio = if c.is_priority {
                        " [PRIORITY]".yellow().bold()
                    } else {
                        "".normal()
                    };
                    let status = if c.is_temporarily_disabled {
                        " [COOLDOWN/DISABLED]".red().bold()
                    } else {
                        "".normal()
                    };
                    println!(
                        "  - Label: {:<16} ID: {:<36} Used: {:<5}{}{}",
                        c.label.green(),
                        c.id,
                        c.usage_count,
                        prio,
                        status
                    );
                }
            }
        }
        AuthCommand::Remove { provider, target } => {
            let removed = cred_manager.remove_credential(&provider, &target).await?;
            if removed {
                println!("{} Credential '{}' removed.", "✔".green(), target);
            } else {
                println!("{} Credential '{}' not found.", "✖".red(), target);
            }
        }
        AuthCommand::Reset { provider } => {
            let count = cred_manager.reset_cooldowns(provider.as_deref()).await?;
            if let Some(ref p) = provider {
                println!(
                    "{} Reset cooldowns for provider '{}' ({} credentials re-enabled)",
                    "✔".green(),
                    p,
                    count
                );
            } else {
                println!(
                    "{} Reset cooldowns across all providers ({} credentials re-enabled)",
                    "✔".green(),
                    count
                );
            }
        }
        AuthCommand::Strategy { provider, strategy } => {
            let strat = match strategy.to_lowercase().as_str() {
                "round_robin" | "roundrobin" => RotationStrategy::RoundRobin,
                "fill_first" | "fillfirst" => RotationStrategy::FillFirst,
                "random" => RotationStrategy::Random,
                "least_used" | "leastused" => RotationStrategy::LeastUsed,
                _ => {
                    eprintln!(
                        "Invalid strategy. Choose: round_robin, fill_first, random, least_used"
                    );
                    return Ok(());
                }
            };
            cred_manager.set_rotation_strategy(&provider, strat).await?;
            println!(
                "{} Rotation strategy for '{}' set to {:?}",
                "✔".green(),
                provider,
                strat
            );
        }
        AuthCommand::Priority { provider, target } => {
            let updated = cred_manager
                .set_priority_credential(&provider, &target)
                .await?;
            if updated {
                println!(
                    "{} Credential '{}' set as priority for '{}'",
                    "✔".green(),
                    target,
                    provider
                );
                // Honesty check: only fill_first consults is_priority, so say so instead of letting
                // the flag sit there looking effective under round_robin/random/least_used.
                println!(
                    "  note: the priority key is used first only under the fill_first strategy \
                     (luna auth strategy -p {} -s fill_first)",
                    provider
                );
            } else {
                println!("{} Credential '{}' not found.", "✖".red(), target);
            }
        }
        AuthCommand::ClearPriority { provider, target } => {
            let updated = cred_manager
                .clear_priority_credential(&provider, &target)
                .await?;
            if updated {
                println!(
                    "{} Credential '{}' is no longer the priority key for '{}'",
                    "✔".green(),
                    target,
                    provider
                );
                // The flag was the only thing keeping a metered fallback in front of a real key.
                println!(
                    "  note: '{}' stays in the pool as a fallback and is handed out when no \
                     other key is left",
                    target
                );
            } else {
                println!("{} Credential '{}' not found.", "✖".red(), target);
            }
        }
        AuthCommand::Antigravity { subcommand } => match subcommand {
            AntigravitySubcommand::Copy => {
                let creds = harness_providers::detect_antigravity_credentials()?;
                let label = creds
                    .email
                    .clone()
                    .unwrap_or_else(|| "antigravity-user".to_string());
                let secret_payload =
                    serde_json::to_string(&creds).unwrap_or_else(|_| creds.access_token.clone());

                let id = cred_manager
                    .add_credential("antigravity", &label, &secret_payload, true)
                    .await?;
                let _ = cred_manager
                    .set_rotation_strategy(
                        "antigravity",
                        harness_providers::RotationStrategy::FillFirst,
                    )
                    .await;

                println!(
                    "{} Successfully copied Antigravity credentials for '{}' into Luna!",
                    "✔".green().bold(),
                    label.bold()
                );
                println!("  Provider:        {}", "antigravity".cyan().bold());
                println!("  Credential ID:   {}", id);
                println!("  Default model:   {}", "gemini-3.8-flash".cyan());
                println!("  Set as priority: {}", "yes".green());
            }
        },
        AuthCommand::Opencode { subcommand } => match subcommand {
            OpencodeSubcommand::Copy => {
                let detected = harness_providers::detect_opencode_credentials()?;
                println!(
                    "{} Found {} OpenCode credential(s) on system:",
                    "✔".green().bold(),
                    detected.len()
                );
                for cred in detected {
                    let id = cred_manager
                        .add_credential(
                            &cred.provider,
                            &cred.label,
                            &cred.api_key,
                            cred.is_priority,
                        )
                        .await?;
                    println!(
                        "  {} Registered credential for provider '{}' ({})",
                        "✔".green().bold(),
                        cred.provider.cyan().bold(),
                        cred.label.bold()
                    );
                    println!("    Credential ID:   {}", id);
                    // The metered free-tier token is registered as a fallback, never as the
                    // key the pool hands out first: a priority free token turns every request
                    // into a rate-limit wait even though the account key sits in the same pool.
                    if cred.is_priority {
                        println!("    Set as priority: {}", "yes".green());
                    } else {
                        println!("    Set as priority: {}", "no (fallback)".yellow());
                    }
                }
            }
        },
    }
    Ok(())
}

fn current_terminal_size() -> Size {
    match crossterm::terminal::size() {
        Ok((cols, rows)) => Size::new(cols, rows),
        _ => Size::fallback(),
    }
}

fn print_styled_line(line: &Line) {
    for span in &line.spans {
        let mut text = span.text.clone();
        if span.bold {
            text = text.bold().to_string();
        }
        if let Some(c) = span.color {
            text = match c {
                crossterm::style::Color::Cyan => text.cyan().to_string(),
                crossterm::style::Color::Yellow => text.yellow().to_string(),
                crossterm::style::Color::DarkGrey => text.dimmed().to_string(),
                crossterm::style::Color::Green => text.green().to_string(),
                crossterm::style::Color::Red => text.red().to_string(),
                crossterm::style::Color::White => text.white().to_string(),
                _ => text,
            };
        }
        print!("{}", text);
    }
    println!();
}

pub const AUTH_PANEL: Panel = Panel {
    title: "🌙  LUNA",
    subtitle: "MODEL PROVIDER SETUP",
    caption: "Connect your AI model providers and API keys",
};

async fn get_configured_summary(cred_manager: Arc<CredentialManager>) -> Vec<String> {
    let catalog = get_provider_catalog();
    let mut configured = Vec::new();
    for p in &catalog {
        let creds = cred_manager.get_credentials(&p.id).await;
        if !creds.is_empty() {
            configured.push(format!(
                "{} ({} key{})",
                p.display_name,
                creds.len(),
                if creds.len() > 1 { "s" } else { "" }
            ));
        }
    }
    configured
}

fn pause_prompt() {
    println!();
    print!("  Press Enter to continue...");
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
}

/// Interactive credential pool management when `luna auth` or `luna setup` is called.
pub async fn handle_interactive_auth(
    cred_manager: Arc<CredentialManager>,
) -> Result<(), HarnessError> {
    loop {
        let size = current_terminal_size();
        let widest = 68;

        print!("\x1b[2J\x1b[H");

        let header_lines = panel_header(size, &AUTH_PANEL, widest);
        for line in &header_lines {
            print_styled_line(line);
        }
        println!();

        let configured = get_configured_summary(cred_manager.clone()).await;
        let status_text = if configured.is_empty() {
            "Status: No providers connected yet. Choose an option below to connect."
                .dimmed()
                .to_string()
        } else {
            format!(
                "{} {}",
                "Status: Connected:".green().bold(),
                configured.join(", ").cyan()
            )
        };
        println!("{}", screen::centered(&status_text, size.columns));
        println!();

        let options = [
            (
                "1",
                "Add Provider API Key",
                "Connect NVIDIA NIM, OpenRouter, Anthropic, OpenAI, etc.",
            ),
            (
                "2",
                "Import Antigravity / Google",
                "Use Google login session from Antigravity",
            ),
            (
                "3",
                "Import OpenCode / Shiina",
                "Use credentials from local OpenCode / Shiina",
            ),
            (
                "4",
                "List Configured Credentials",
                "Inspect active keys, usage counts, and status",
            ),
            (
                "5",
                "Remove a Credential",
                "Delete an API key from the local pool",
            ),
            (
                "6",
                "Advanced Settings",
                "Reset cooldowns or configure rotation strategies",
            ),
            ("7", "Finish Setup", "Save and proceed"),
        ];

        let menu_lines: Vec<String> = options
            .iter()
            .map(|(num, label, desc)| {
                format!(
                    "  {}  {:<26} {}",
                    num.bold().cyan(),
                    label.bold(),
                    desc.dimmed()
                )
            })
            .collect();

        for line in screen::center_group(menu_lines.iter().map(String::as_str), size.columns) {
            println!("{}", line);
        }
        println!();

        let prompt = screen::centered("Select 1-7 [default: 1] ▸ ", size.columns);
        print!("{}", prompt);
        let _ = std::io::stdout().flush();

        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            return Ok(());
        }
        let choice = input.trim();
        let choice = if choice.is_empty() { "1" } else { choice };

        match choice {
            "1" => {
                interactive_add(cred_manager.clone()).await?;
            }
            "2" => {
                println!();
                handle_auth_command(
                    AuthCommand::Antigravity {
                        subcommand: AntigravitySubcommand::Copy,
                    },
                    cred_manager.clone(),
                )
                .await?;
                pause_prompt();
            }
            "3" => {
                println!();
                handle_auth_command(
                    AuthCommand::Opencode {
                        subcommand: OpencodeSubcommand::Copy,
                    },
                    cred_manager.clone(),
                )
                .await?;
                pause_prompt();
            }
            "4" => {
                println!();
                handle_auth_command(AuthCommand::List { provider: None }, cred_manager.clone())
                    .await?;
                pause_prompt();
            }
            "5" => {
                interactive_remove(cred_manager.clone()).await?;
            }
            "6" => {
                println!();
                let adv_choices = [
                    "Reset cooldowns for a provider",
                    "Set rotation strategy for a provider",
                    "Back",
                ];
                let sel = Select::new()
                    .with_prompt("Advanced Action")
                    .items(&adv_choices)
                    .default(0)
                    .interact()
                    .unwrap_or(2);
                if sel == 0 {
                    interactive_reset(cred_manager.clone()).await?;
                } else if sel == 1 {
                    interactive_strategy(cred_manager.clone()).await?;
                }
            }
            "7" | "q" | "exit" => {
                println!();
                println!(
                    "{}",
                    screen::centered(
                        &format!(
                            "{} Setup complete. You are ready to chat with Luna.",
                            "🌙".cyan()
                        ),
                        size.columns
                    )
                );
                println!();
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

async fn interactive_add(cred_manager: Arc<CredentialManager>) -> Result<(), HarnessError> {
    println!();
    let popular_providers = [
        (
            "nvidia",
            "NVIDIA NIM",
            "Fast DeepSeek-V3, DeepSeek-R1, Llama 3.3",
        ),
        (
            "openrouter",
            "OpenRouter",
            "Unified access to Claude, GPT-4o, DeepSeek, etc.",
        ),
        (
            "gemini",
            "Google Gemini",
            "Gemini 2.0 Flash, Gemini 1.5 Pro",
        ),
        (
            "anthropic",
            "Anthropic",
            "Claude 3.5 Sonnet, Claude 3.5 Haiku",
        ),
        ("openai", "OpenAI", "GPT-4o, o3-mini"),
        (
            "deepseek",
            "DeepSeek",
            "Official DeepSeek API (deepseek-chat, deepseek-reasoner)",
        ),
        (
            "groq",
            "Groq",
            "High-speed inference (Llama 3.3 70B, Mixtral)",
        ),
        ("ollama", "Ollama", "Local models (no API key required)"),
        (
            "custom",
            "Custom Endpoint",
            "OpenAI-compatible server (vLLM, LM Studio, local)",
        ),
    ];

    let mut items: Vec<String> = popular_providers
        .iter()
        .map(|(id, name, desc)| {
            format!(
                "{:<14} - {:<16} ({})",
                id.cyan(),
                name.bold(),
                desc.dimmed()
            )
        })
        .collect();
    items.push(format!(
        "{:<14} - {:<16} ({})",
        "other".cyan(),
        "Other Providers".bold(),
        "Pick from full catalog".dimmed()
    ));
    items.push(format!("{:<14} - Back to menu", "cancel".dimmed()));

    let selection = Select::new()
        .with_prompt("Select AI Provider")
        .items(&items)
        .default(0)
        .interact()
        .map_err(|e| HarnessError::Io(e.to_string()))?;

    if selection == items.len() - 1 {
        return Ok(());
    }

    let provider_id = if selection < popular_providers.len() {
        popular_providers[selection].0.to_string()
    } else {
        let catalog = get_provider_catalog();
        let cat_items: Vec<String> = catalog
            .iter()
            .map(|c| format!("{} ({})", c.display_name, c.id))
            .collect();
        let cat_sel = Select::new()
            .with_prompt("Select Provider from Catalog")
            .items(&cat_items)
            .default(0)
            .interact()
            .map_err(|e| HarnessError::Io(e.to_string()))?;
        catalog[cat_sel].id.clone()
    };

    println!();
    let default_label = format!("{}-primary", provider_id);
    let label = Input::<String>::new()
        .with_prompt("Credential Label")
        .default(default_label)
        .interact_text()
        .map_err(|e| HarnessError::Io(e.to_string()))?;

    let api_key = if provider_id == "ollama" {
        "ollama-local".to_string()
    } else {
        Password::new()
            .with_prompt("API Key (masked)")
            .interact()
            .map_err(|e| HarnessError::Io(e.to_string()))?
    };

    let is_priority = dialoguer::Confirm::new()
        .with_prompt("Set as primary credential for this provider?")
        .default(true)
        .interact()
        .unwrap_or(true);

    let id = cred_manager
        .add_credential(&provider_id, &label, &api_key, is_priority)
        .await?;

    println!();
    println!(
        "  {} Credential '{}' registered for provider '{}' (ID: {})",
        "✔".green().bold(),
        label.bold(),
        provider_id.cyan(),
        id
    );
    pause_prompt();
    Ok(())
}

async fn interactive_remove(cred_manager: Arc<CredentialManager>) -> Result<(), HarnessError> {
    let catalog = get_provider_catalog();
    let mut configured_providers = Vec::new();
    for p in &catalog {
        let creds = cred_manager.get_credentials(&p.id).await;
        if !creds.is_empty() {
            configured_providers.push(p.id.clone());
        }
    }

    if configured_providers.is_empty() {
        println!("No credentials registered for any provider.");
        return Ok(());
    }

    let selection = Select::new()
        .with_prompt("Select Provider to remove credential from")
        .items(&configured_providers)
        .default(0)
        .interact()
        .map_err(|e| HarnessError::Io(e.to_string()))?;
    let provider = &configured_providers[selection];

    let creds = cred_manager.get_credentials(provider).await;
    if creds.is_empty() {
        println!("No credentials for {}.", provider);
        return Ok(());
    }

    println!("\nCredentials for {}:", provider.cyan().bold());
    for (i, c) in creds.iter().enumerate() {
        println!(
            "  #{:<2} {:<18} ID: {:<36} (Used: {})",
            i + 1,
            c.label.green(),
            c.id,
            c.usage_count
        );
    }
    println!();

    let raw = Input::<String>::new()
        .with_prompt("Remove #, ID, or label (blank to cancel)")
        .allow_empty(true)
        .interact_text()
        .map_err(|e| HarnessError::Io(e.to_string()))?;

    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(());
    }

    let target = if let Ok(idx) = raw.parse::<usize>() {
        if idx > 0 && idx <= creds.len() {
            creds[idx - 1].id.clone()
        } else {
            raw.to_string()
        }
    } else {
        raw.to_string()
    };

    let removed = cred_manager.remove_credential(provider, &target).await?;
    if removed {
        println!(
            "{} Credential '{}' removed from '{}'.",
            "✔".green(),
            target,
            provider
        );
    } else {
        println!(
            "{} Credential '{}' not found for '{}'.",
            "✖".red(),
            target,
            provider
        );
    }
    Ok(())
}

async fn interactive_reset(cred_manager: Arc<CredentialManager>) -> Result<(), HarnessError> {
    let catalog = get_provider_catalog();
    let mut options = vec!["All providers".to_string()];
    for p in &catalog {
        options.push(format!("{} ({})", p.display_name, p.id));
    }

    let selection = Select::new()
        .with_prompt("Select Provider to reset cooldowns for")
        .items(&options)
        .default(0)
        .interact()
        .map_err(|e| HarnessError::Io(e.to_string()))?;

    let provider = if selection == 0 {
        None
    } else {
        Some(catalog[selection - 1].id.clone())
    };

    let count = cred_manager.reset_cooldowns(provider.as_deref()).await?;
    if let Some(ref p) = provider {
        println!(
            "{} Reset cooldowns for provider '{}' ({} credentials re-enabled)",
            "✔".green(),
            p,
            count
        );
    } else {
        println!(
            "{} Reset cooldowns across all providers ({} credentials re-enabled)",
            "✔".green(),
            count
        );
    }
    Ok(())
}

async fn interactive_strategy(cred_manager: Arc<CredentialManager>) -> Result<(), HarnessError> {
    let catalog = get_provider_catalog();
    let items: Vec<String> = catalog
        .iter()
        .map(|c| format!("{} ({})", c.display_name, c.id))
        .collect();
    let selection = Select::new()
        .with_prompt("Select Provider to configure rotation strategy")
        .items(&items)
        .default(0)
        .interact()
        .map_err(|e| HarnessError::Io(e.to_string()))?;
    let provider_id = &catalog[selection].id;

    let current = cred_manager.get_rotation_strategy(provider_id).await;
    println!(
        "\nCurrent rotation strategy for {}: {:?}",
        provider_id.cyan().bold(),
        current
    );

    let strategies = [
        (
            "round_robin",
            "Cycle through keys evenly",
            RotationStrategy::RoundRobin,
        ),
        (
            "fill_first",
            "Use first/priority key until exhausted, then next",
            RotationStrategy::FillFirst,
        ),
        (
            "least_used",
            "Always pick the least-used key",
            RotationStrategy::LeastUsed,
        ),
        (
            "random",
            "Random selection across active keys",
            RotationStrategy::Random,
        ),
    ];

    let strat_items: Vec<String> = strategies
        .iter()
        .map(|(name, desc, strat)| {
            let mark = if *strat == current { " (current)" } else { "" };
            format!("{:<14} — {}{}", name, desc, mark)
        })
        .collect();

    let strat_idx = Select::new()
        .with_prompt("Select strategy")
        .items(&strat_items)
        .default(match current {
            RotationStrategy::RoundRobin => 0,
            RotationStrategy::FillFirst => 1,
            RotationStrategy::LeastUsed => 2,
            RotationStrategy::Random => 3,
        })
        .interact()
        .map_err(|e| HarnessError::Io(e.to_string()))?;

    let chosen_strategy = strategies[strat_idx].2;
    cred_manager
        .set_rotation_strategy(provider_id, chosen_strategy)
        .await?;
    println!(
        "{} Rotation strategy for '{}' set to {:?}",
        "✔".green().bold(),
        provider_id,
        chosen_strategy
    );
    Ok(())
}
