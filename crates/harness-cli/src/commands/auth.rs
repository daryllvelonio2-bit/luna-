use std::sync::Arc;
use clap::Subcommand;
use colored::Colorize;
use dialoguer::{Input, Password, Select};

use harness_core::HarnessError;
use harness_providers::{
    get_provider_catalog, CredentialManager, RotationStrategy,
};

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
                    let prio = if c.is_priority { "[PRIORITY]".yellow().bold() } else { "".normal() };
                    println!(
                        "  - Label: {:<16} ID: {:<36} Used: {:<5} {}",
                        c.label.green(),
                        c.id,
                        c.usage_count,
                        prio
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
        AuthCommand::Strategy { provider, strategy } => {
            let strat = match strategy.to_lowercase().as_str() {
                "round_robin" | "roundrobin" => RotationStrategy::RoundRobin,
                "fill_first" | "fillfirst" => RotationStrategy::FillFirst,
                "random" => RotationStrategy::Random,
                "least_used" | "leastused" => RotationStrategy::LeastUsed,
                _ => {
                    eprintln!("Invalid strategy. Choose: round_robin, fill_first, random, least_used");
                    return Ok(());
                }
            };
            cred_manager.set_rotation_strategy(&provider, strat).await?;
            println!("{} Rotation strategy for '{}' set to {:?}", "✔".green(), provider, strat);
        }
        AuthCommand::Priority { provider, target } => {
            let updated = cred_manager.set_priority_credential(&provider, &target).await?;
            if updated {
                println!("{} Credential '{}' set as priority for '{}'", "✔".green(), target, provider);
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
    }
    Ok(())
}
