use clap::Subcommand;
use colored::Colorize;
use std::sync::Arc;

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
        /// Optional specific provider id; without it, refreshes all providers
        #[arg(short, long)]
        provider: Option<String>,
        /// Explicit flag to refresh all providers
        #[arg(short, long)]
        all: bool,
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
            match discovery
                .get_models(&provider, &cred_manager, refresh)
                .await
            {
                Ok(models) => {
                    println!(
                        "\n{} Available Models for {}:",
                        "✔".green(),
                        provider.bold()
                    );
                    for m in models {
                        println!("  • {}", m);
                    }
                }
                Err(e) => {
                    eprintln!("{} Failed to fetch models: {}", "✖".red(), e);
                }
            }
        }
        ModelsCommand::Refresh { provider, all } => {
            if let Some(provider) = provider {
                if !all {
                    println!(
                        "Force-refreshing model catalog for '{}'...",
                        provider.cyan().bold()
                    );
                    match discovery.get_models(&provider, &cred_manager, true).await {
                        Ok(models) => {
                            println!(
                                "\n{} Cache refreshed for {}! Found {} models.",
                                "✔".green().bold(),
                                provider.bold(),
                                models.len()
                            );
                            for m in &models {
                                println!("  • {}", m);
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "{} Failed to refresh models for {}: {}",
                                "✖".red().bold(),
                                provider.bold(),
                                e
                            );
                        }
                    }
                    return Ok(());
                }
            }

            println!("Refreshing model catalog for all providers...\n");
            let results = discovery.refresh_all_models(&cred_manager).await;
            let mut total_models = 0;
            let mut success_count = 0;

            for (p, res) in results {
                match res {
                    Ok(models) => {
                        println!(
                            "  {} {:<16} {} models discovered",
                            "✔".green(),
                            p.bold(),
                            models.len().to_string().cyan().bold()
                        );
                        total_models += models.len();
                        success_count += 1;
                    }
                    Err(e) => {
                        println!("  {} {:<16} failed: {}", "✖".red(), p.bold(), e);
                    }
                }
            }

            println!(
                "\n{} Successfully refreshed {} providers ({} total models cached).",
                "✔".green().bold(),
                success_count,
                total_models
            );
        }
    }
    Ok(())
}
