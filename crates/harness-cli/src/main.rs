use std::sync::Arc;
use clap::{Parser, Subcommand};
use harness_core::HarnessError;
use harness_providers::{CredentialManager, ModelDiscovery};
use harness_telemetry::init_telemetry;

mod commands;
use commands::auth::{handle_auth_command, AuthCommand};
use commands::chat::run_interactive_chat;
use commands::models::{handle_models_command, ModelsCommand};
use commands::supervisor::run_supervisor_loop;
use commands::voice::run_voice_loop;

#[derive(Parser, Debug)]
#[command(name = "luna", author = "Luna AI", version = "0.1.0", about = "Autonomous AI Agent Harness in Rust")]
struct Cli {
    #[arg(long, global = true, help = "Output telemetry logs in JSON format")]
    json: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Interactive chat session with Luna agent
    Chat {
        #[arg(short, long, default_value = "openai")]
        provider: String,
        #[arg(short, long)]
        model: Option<String>,
    },
    /// Manage multi-provider credentials and key rotation
    Auth {
        #[command(subcommand)]
        subcommand: AuthCommand,
    },
    /// Query and refresh dynamic model catalog
    Models {
        #[command(subcommand)]
        subcommand: ModelsCommand,
    },
    /// Start hands-free voice interface with wake word "Luna"
    Voice {
        #[arg(short, long, default_value = "openai")]
        provider: String,
        #[arg(short, long)]
        model: Option<String>,
    },
    /// Run agent supervised with auto-rebuild and restart
    Supervisor {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<(), HarnessError> {
    let cli = Cli::parse();
    init_telemetry(cli.json);

    let cred_manager = CredentialManager::new(None);
    let discovery = Arc::new(ModelDiscovery::new(None, 24));

    match cli.command {
        Some(Commands::Chat { provider, model }) => {
            run_interactive_chat(provider, model, cred_manager, discovery).await?;
        }
        Some(Commands::Auth { subcommand }) => {
            handle_auth_command(subcommand, cred_manager).await?;
        }
        Some(Commands::Models { subcommand }) => {
            handle_models_command(subcommand, cred_manager, discovery).await?;
        }
        Some(Commands::Voice { provider, model }) => {
            run_voice_loop(provider, model, cred_manager, discovery).await?;
        }
        Some(Commands::Supervisor { args }) => {
            run_supervisor_loop(args).await?;
        }
        None => {
            // Default to chat if no subcommand specified
            run_interactive_chat("openai".to_string(), None, cred_manager, discovery).await?;
        }
    }

    Ok(())
}
