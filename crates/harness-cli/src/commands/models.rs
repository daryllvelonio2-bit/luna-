use std::sync::Arc;
use clap::Subcommand;
use colored::Colorize;

use harness_core::HarnessError;
use harness_providers::{CredentialManager, ModelDiscovery};

#[derive(Subcommand, Debug)]
pub enum ModelsCommand {
    /// List dynamically discovered models for a provider
    List {
        /// Provider id; without it, `model.provider` from the config file
        #[arg(short, long)]
        provider: Option<String>,
        #[arg(short, long)]
        refresh: bool,
    },
    /// Force refresh dynamic model discovery cache
    Refresh {
        /// Provider id; without it, `model.provider` from the config file
        #[arg(short, long)]
        provider: Option<String>,
    },
}

pub async fn handle_models_command(
    cmd: ModelsCommand,
    default_provider: &str,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
) -> Result<(), HarnessError> {
    match cmd {
        ModelsCommand::List { provider, refresh } => {
            let provider = provider.unwrap_or_else(|| default_provider.to_string());
            println!("Querying models for provider '{}'...", provider.cyan());
            match discovery.get_models(&provider, &cred_manager, refresh).await {
                Ok(models) => {
                    println!("\n{} Available Models for {}:", "✔".green(), provider.bold());
                    for m in models {
                        println!("  • {}", m);
                    }
                }
                Err(e) => {
                    eprintln!("{} Failed to fetch models: {}", "✖".red(), e);
                }
            }
        }
        ModelsCommand::Refresh { provider } => {
            let provider = provider.unwrap_or_else(|| default_provider.to_string());
            println!("Force-refreshing model catalog for '{}'...", provider.cyan());
            match discovery.get_models(&provider, &cred_manager, true).await {
                Ok(models) => {
                    println!("{} Cache refreshed! Found {} models.", "✔".green(), models.len());
                }
                Err(e) => {
                    eprintln!("{} Failed to refresh models: {}", "✖".red(), e);
                }
            }
        }
    }
    Ok(())
}
