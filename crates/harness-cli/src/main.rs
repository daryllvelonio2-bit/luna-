use std::sync::Arc;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use harness_core::config::{self, Config};
use harness_core::HarnessError;
use harness_providers::{CredentialManager, ModelDiscovery};
use harness_telemetry::init_telemetry;

mod commands;
mod eval;
use commands::auth::{handle_auth_command, handle_interactive_auth, AuthCommand};
use commands::chat::run_interactive_chat;
use commands::config::{catalog_defaults, handle_config_command, ConfigCommand};
use commands::models::{handle_models_command, ModelsCommand};
use commands::sessions::{handle_sessions_command, SessionSelection, SessionsCommand};
use commands::supervisor::run_supervisor_loop;
use commands::voice::run_voice_loop;
use eval::command::{handle_eval_command, EvalCommand};

/// Model-discovery cache lifetime. A cache TTL rather than user intent, so it is not a v1 config
/// key; the cache path itself is `paths.models_cache`.
const MODEL_CACHE_TTL_HOURS: i64 = 24;

#[derive(Parser, Debug)]
#[command(name = "luna", author = "Luna AI", version = "0.1.0", about = "Autonomous AI Agent Harness in Rust")]
struct Cli {
    #[arg(long, global = true, help = "Output telemetry logs in JSON format")]
    json: bool,

    /// Provider id; without it, `model.provider` from the config file
    #[arg(short, long)]
    provider: Option<String>,

    /// Model id; without it, `model.default` from the config file
    #[arg(short, long)]
    model: Option<String>,

    /// Approve every mutating tool call without asking (same as `tools.approval=allow`)
    #[arg(long)]
    yes: bool,

    /// Resume a stored session by id (`luna sessions list` shows the ids)
    #[arg(long, conflicts_with = "continue_session")]
    session: Option<String>,

    /// Resume the most recently updated session
    #[arg(long = "continue")]
    continue_session: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Interactive chat session with Luna agent
    Chat {
        /// Provider id; without it, `model.provider` from the config file
        #[arg(short, long)]
        provider: Option<String>,
        /// Model id; without it, `model.default` from the config file
        #[arg(short, long)]
        model: Option<String>,
        /// Approve every mutating tool call without asking (same as `tools.approval=allow`)
        #[arg(long)]
        yes: bool,
        /// Resume a stored session by id (`luna sessions list` shows the ids)
        #[arg(long, conflicts_with = "continue_session")]
        session: Option<String>,
        /// Resume the most recently updated session
        #[arg(long = "continue")]
        continue_session: bool,
    },
    /// List stored chat sessions
    Sessions {
        #[command(subcommand)]
        subcommand: SessionsCommand,
    },
    /// Manage multi-provider credentials and key rotation
    Auth {
        #[command(subcommand)]
        subcommand: Option<AuthCommand>,
    },
    /// Query and refresh dynamic model catalog
    Models {
        #[command(subcommand)]
        subcommand: ModelsCommand,
    },
    /// Start hands-free voice interface with wake word "Luna"
    Voice {
        /// Provider id; without it, `model.provider` from the config file
        #[arg(short, long)]
        provider: Option<String>,
        /// Model id; without it, `model.default` from the config file
        #[arg(short, long)]
        model: Option<String>,
        /// Approve every mutating tool call without asking (same as `tools.approval=allow`)
        #[arg(long)]
        yes: bool,
    },
    /// Run agent supervised with auto-rebuild and restart
    Supervisor {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show the resolved configuration and edit the config file
    Config {
        #[command(subcommand)]
        subcommand: ConfigCommand,
    },
    /// Run the scripted end-to-end scenario suite (offline fake provider, or live NIM)
    Eval {
        #[command(subcommand)]
        subcommand: EvalCommand,
    },
}

#[tokio::main]
async fn main() -> Result<(), HarnessError> {
    // The config resolves *before* clap parses, because the flag help text reports the configured
    // defaults — `luna chat --help` must show the real provider, not a literal.
    let mut layers = config::process_layers(catalog_defaults())?;
    let help_defaults = config::resolve(&layers)?;

    let matches = build_command(&help_defaults).get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };

    // CLI flags are the highest-precedence layer.
    let (chat_provider, chat_model, chat_yes) = match &cli.command {
        Some(Commands::Chat { provider, model, yes, .. })
        | Some(Commands::Voice { provider, model, yes, .. }) => (
            provider.as_ref().or(cli.provider.as_ref()),
            model.as_ref().or(cli.model.as_ref()),
            *yes || cli.yes,
        ),
        None => (
            cli.provider.as_ref(),
            cli.model.as_ref(),
            cli.yes,
        ),
        _ => (cli.provider.as_ref(), cli.model.as_ref(), cli.yes),
    };

    if let Some(provider) = chat_provider {
        layers.flags.insert("model.provider".to_string(), provider.clone());
    }
    if let Some(model) = chat_model {
        layers.flags.insert("model.default".to_string(), model.clone());
    }
    // `--yes` is the flag layer of `tools.approval`, so the effective mode is the same
    // value the gate reads and `luna config get` reports.
    if chat_yes {
        layers.flags.insert("tools.approval".to_string(), "allow".to_string());
    }

    if let Some(Commands::Models { subcommand }) = &cli.command {
        let provider = match subcommand {
            ModelsCommand::List { provider, .. } => provider,
            ModelsCommand::Refresh { provider } => provider,
        };
        if let Some(provider) = provider {
            layers.flags.insert("model.provider".to_string(), provider.clone());
        }
    }

    let config: Config = config::resolve(&layers)?.into_config();

    init_telemetry(cli.json || config.telemetry.json, &config.telemetry.level);

    // The credential manager's own error (unreadable/corrupt store) must reach the user: the
    // corrupt-store defect this merge fixes is exactly a `CredentialManager::new` that used to
    // silently fall back to an empty store. It now reads the config-resolved path.
    let cred_manager = CredentialManager::new(Some(config.paths.credentials.clone()))?;
    let discovery = Arc::new(ModelDiscovery::new(
        Some(config.paths.models_cache.clone()),
        MODEL_CACHE_TTL_HOURS,
        config.providers.clone(),
    ));

    match cli.command {
        Some(Commands::Chat {
            session,
            continue_session,
            ..
        }) => {
            let session = session.or(cli.session);
            let continue_session = continue_session || cli.continue_session;
            let selection = match (session, continue_session) {
                (Some(id), _) => SessionSelection::Resume(id),
                (None, true) => SessionSelection::Continue,
                (None, false) => SessionSelection::New,
            };
            run_interactive_chat(config, cred_manager, discovery, selection).await?;
        }
        Some(Commands::Voice { .. }) => {
            run_voice_loop(config, cred_manager, discovery).await?;
        }
        Some(Commands::Auth { subcommand }) => {
            if let Some(sub) = subcommand {
                handle_auth_command(sub, cred_manager).await?;
            } else {
                handle_interactive_auth(cred_manager).await?;
            }
        }
        Some(Commands::Models { subcommand }) => {
            let default_provider = config.model.provider.clone();
            handle_models_command(subcommand, &default_provider, cred_manager, discovery).await?;
        }
        Some(Commands::Sessions { subcommand }) => {
            handle_sessions_command(subcommand, &config).await?;
        }
        Some(Commands::Supervisor { args }) => {
            run_supervisor_loop(args).await?;
        }
        Some(Commands::Config { subcommand }) => {
            handle_config_command(subcommand, &layers)?;
        }
        Some(Commands::Eval { subcommand }) => {
            handle_eval_command(subcommand, cred_manager, discovery, &config).await?;
        }
        None => {
            let selection = match (cli.session, cli.continue_session) {
                (Some(id), _) => SessionSelection::Resume(id),
                (None, true) => SessionSelection::Continue,
                (None, false) => SessionSelection::New,
            };
            run_interactive_chat(config, cred_manager, discovery, selection).await?;
        }
    }

    Ok(())
}

/// Rebuild the clap command so `--help` reports the *resolved* provider and model defaults.
fn build_command(resolved: &config::Resolved) -> clap::Command {
    let provider = resolved.get("model.provider").unwrap_or("(unset)").to_string();
    let model = resolved.get("model.default").unwrap_or("(unset)").to_string();
    let provider_help = format!("Provider id (config default: {})", provider);
    let model_help = format!("Model id (config default: {})", model);

    Cli::command()
        .mut_arg("provider", |arg| arg.help(provider_help.clone()))
        .mut_arg("model", |arg| arg.help(model_help.clone()))
        .mut_subcommand("chat", |chat| {
            chat.mut_arg("provider", |arg| arg.help(provider_help.clone()))
                .mut_arg("model", |arg| arg.help(model_help.clone()))
        })
        .mut_subcommand("voice", |voice| {
            voice
                .mut_arg("provider", |arg| arg.help(provider_help.clone()))
                .mut_arg("model", |arg| arg.help(model_help.clone()))
        })
        .mut_subcommand("models", |models| {
            models
                .mut_subcommand("list", |list| {
                    list.mut_arg("provider", |arg| arg.help(provider_help.clone()))
                })
                .mut_subcommand("refresh", |refresh| {
                    refresh.mut_arg("provider", |arg| arg.help(provider_help.clone()))
                })
        })
}
