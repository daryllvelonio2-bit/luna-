use colored::Colorize;
use std::sync::Arc;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use harness_core::config::{self, Config};
use harness_core::HarnessError;
use harness_providers::{CredentialManager, ModelDiscovery};
use harness_telemetry::init_telemetry;

mod commands;
mod eval;
mod tui;
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
#[command(
    name = "luna",
    author = "Luna AI",
    version = "0.1.0",
    about = "Autonomous AI Agent Harness in Rust"
)]
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

    /// Resume a stored session by id or name (`luna sessions list` shows both)
    #[arg(long, conflicts_with_all = ["continue_session", "fork"])]
    session: Option<String>,

    /// Fork an existing session and start from the copy
    #[arg(long, conflicts_with_all = ["session", "continue_session"])]
    fork: Option<String>,

    /// Resume the most recently updated session
    #[arg(long = "continue")]
    continue_session: bool,

    /// Launch full interactive Antigravity-style desktop TUI
    #[arg(long)]
    tui: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Launch full interactive Antigravity-style desktop TUI
    App,
    /// Alias for App
    Tui,
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
        /// Resume a stored session by id or name (`luna sessions list` shows both)
        #[arg(long, conflicts_with_all = ["continue_session", "fork"])]
        session: Option<String>,
        /// Fork an existing session and start from the copy
        #[arg(long, conflicts_with_all = ["session", "continue_session"])]
        fork: Option<String>,
        /// Resume the most recently updated session
        #[arg(long = "continue")]
        continue_session: bool,
    },
    /// Run one prompt headlessly and print the result (`--json` for an event stream)
    Exec {
        /// The prompt; `-` or omitted reads it from stdin
        #[arg(value_name = "PROMPT")]
        prompt: Option<String>,
        /// Emit NDJSON events on stdout instead of the plain answer
        #[arg(long)]
        json: bool,
        /// Approve every mutating tool call without asking (headless default is deny)
        #[arg(long)]
        yes: bool,
        /// Exit non-zero if any tool call failed, even when the turn completed
        #[arg(long)]
        strict: bool,
        /// Resume a stored session by id or name; without it each run starts a fresh session
        #[arg(long, conflicts_with_all = ["fork", "continue_session"])]
        session: Option<String>,
        /// Fork an existing session and run in the copy
        #[arg(long, conflicts_with_all = ["session", "continue_session"])]
        fork: Option<String>,
        /// Continue the most recently updated session
        #[arg(long = "continue")]
        continue_session: bool,
    },
    /// Show which lifecycle hooks are loaded (and which are ignored)
    Hooks {
        /// One JSON object on stdout
        #[arg(long)]
        json: bool,
    },
    /// Show turn usage: tokens, models, cost and time (from the usage ledger)
    Usage {
        /// Only count turns newer than this: 30m, 12h, 7d, 2w or an RFC 3339 timestamp
        #[arg(long)]
        since: Option<String>,
        /// Group the report: model, session, source or day
        #[arg(long, value_name = "FIELD")]
        by: Option<String>,
        /// One JSON object on stdout instead of tables
        #[arg(long)]
        json: bool,
        /// With --recent: how many rows
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// List the newest individual turns instead of totals
        #[arg(long)]
        recent: bool,
    },
    /// Review what a turn changed on disk: per-file diffs, from its checkpoint
    Diff {
        /// Session id or name (default: the most recently updated session)
        #[arg(long)]
        session: Option<String>,
        /// Turn number (default: the newest checkpointed turn)
        #[arg(long)]
        turn: Option<u32>,
        /// Every checkpointed turn of the session
        #[arg(long)]
        all: bool,
        /// Counts only, no hunks
        #[arg(long)]
        stat: bool,
        /// One JSON object on stdout
        #[arg(long)]
        json: bool,
    },
    /// List or interactively select stored chat sessions
    Sessions {
        #[command(subcommand)]
        subcommand: Option<SessionsCommand>,
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
    /// Fetch models dynamically for a provider (e.g. `luna fetch opencode`, `luna fetch antigravity`)
    Fetch {
        /// Provider name (opencode, antigravity, nvidia, etc.)
        provider: Option<String>,
        /// Force refresh cache from provider
        #[arg(short, long)]
        refresh: bool,
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
    /// Run the setup / onboarding flow: choose a personality core and rewrite the config
    #[command(name = "setup", alias = "onboard", alias = "onboarding")]
    Setup,
    /// Connect Luna to external chat platforms (Telegram, Discord, WhatsApp)
    Gateway {
        /// Platform to run: telegram, discord, whatsapp, or all
        #[arg(short, long)]
        platform: Option<String>,
        /// Approve mutating tool calls without asking (headless default is deny)
        #[arg(long)]
        yes: bool,
        /// Port for webhook servers (e.g. WhatsApp)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Run the scripted end-to-end scenario suite (offline fake provider, or live NIM)
    Eval {
        #[command(subcommand)]
        subcommand: EvalCommand,
    },
    /// Check harness liveness, system health, and background status (or hb)
    #[command(alias = "hb")]
    Heartbeat {
        /// Optional target (e.g. "ping")
        target: Option<String>,
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
        Some(Commands::Chat {
            provider,
            model,
            yes,
            ..
        })
        | Some(Commands::Voice {
            provider,
            model,
            yes,
            ..
        }) => (
            provider.as_ref().or(cli.provider.as_ref()),
            model.as_ref().or(cli.model.as_ref()),
            *yes || cli.yes,
        ),
        Some(Commands::Exec { yes, .. }) | Some(Commands::Gateway { yes, .. }) => {
            (cli.provider.as_ref(), cli.model.as_ref(), *yes || cli.yes)
        }
        None => (cli.provider.as_ref(), cli.model.as_ref(), cli.yes),
        _ => (cli.provider.as_ref(), cli.model.as_ref(), cli.yes),
    };

    if let Some(provider) = chat_provider {
        layers
            .flags
            .insert("model.provider".to_string(), provider.clone());
    }
    if let Some(model) = chat_model {
        layers
            .flags
            .insert("model.default".to_string(), model.clone());
    }
    // `--yes` is the flag layer of `tools.approval`, so the effective mode is the same
    // value the gate reads and `luna config get` reports.
    if chat_yes {
        layers
            .flags
            .insert("tools.approval".to_string(), "allow".to_string());
    }

    // A CLI `--provider`/`--model` is a deliberate choice for the run; the chat then knows not
    // to let the session's stored model choice shadow it.
    let explicit_model = chat_provider.is_some() || chat_model.is_some();

    if let Some(Commands::Models { subcommand }) = &cli.command {
        let provider = match subcommand {
            ModelsCommand::List { provider, .. } => provider,
            ModelsCommand::Refresh { provider, .. } => provider,
        };
        if let Some(provider) = provider {
            layers
                .flags
                .insert("model.provider".to_string(), provider.clone());
        }
    }

    let mut config: Config = config::resolve(&layers)?.into_config();

    let is_interactive_chat = match &cli.command {
        None | Some(Commands::Chat { .. }) => {
            dialoguer::console::Term::stdout().features().is_attended()
        }
        _ => false,
    };

    // First-run onboarding. It runs before telemetry, credentials or a session are touched, so a
    // user who has never configured Luna gets the briefing instead of an agent loop that fails on a
    // missing provider. It only ever runs on an attended `luna`/`luna chat`: `exec`, pipes and CI
    // stay headless, and `LUNA_FIRST_RUN=false` or `luna config set first_run false` skips it.
    let mut ran_first_run_onboarding = false;
    if is_interactive_chat && harness_onboarding::needs_onboarding(&layers)? {
        let outcome = harness_onboarding::run_interactive(&layers)?;
        tracing::info!(
            persona = outcome.persona.name,
            config = %outcome.config_path.display(),
            "first-run onboarding complete"
        );
        // Re-resolve so this same run uses the persona it just wrote.
        config = config::resolve(&layers)?.into_config();
        ran_first_run_onboarding = true;
    }

    // The persona's voice is prefixed to the system prompt. An explicit
    // `agent.system_prompt_prefix` in the config file outranks the menu answer, so this never
    // replaces a prefix the user wrote by hand.
    harness_onboarding::persona::apply_selected(&mut config)?;

    let log_file_path = if is_interactive_chat {
        let luna_dir = config
            .paths
            .sessions_db
            .parent()
            .unwrap_or(std::path::Path::new("."));
        Some(luna_dir.join("luna.log"))
    } else {
        None
    };

    init_telemetry(
        cli.json || config.telemetry.json,
        &config.telemetry.level,
        log_file_path.as_deref(),
    );

    // The credential manager's own error (unreadable/corrupt store) must reach the user: the
    // corrupt-store defect this merge fixes is exactly a `CredentialManager::new` that used to
    // silently fall back to an empty store. It now reads the config-resolved path.
    let cred_manager = CredentialManager::new(Some(config.paths.credentials.clone()))?;

    if is_interactive_chat && ran_first_run_onboarding {
        commands::auth::handle_interactive_auth(cred_manager.clone()).await?;
        config = config::resolve(&layers)?.into_config();
    }
    let discovery = Arc::new(ModelDiscovery::new(
        Some(config.paths.models_cache.clone()),
        MODEL_CACHE_TTL_HOURS,
        config.providers.clone(),
    ));

    // If no provider was explicitly requested, always use the loaded provider as default for that session.
    if config.model.provider.is_empty() {
        if let Some(loaded_provider) = cred_manager.get_default_loaded_provider().await {
            config.model.provider = loaded_provider;
        }
    }

    // Resolve default model for the effective provider if not explicitly set.
    if config.model.default.is_empty() && !config.model.provider.is_empty() {
        config.model.default =
            harness_providers::get_default_model(&config.model.provider).to_string();
    }

    match cli.command {
        Some(Commands::Chat {
            session,
            fork,
            continue_session,
            ..
        }) => {
            let session = session.or(cli.session);
            let fork = fork.or(cli.fork);
            let continue_session = continue_session || cli.continue_session;
            let selection = match (session, fork, continue_session) {
                (Some(id), _, _) => SessionSelection::Resume(id),
                (None, Some(fork_id), _) => SessionSelection::Fork(fork_id),
                (None, None, true) => SessionSelection::Continue,
                (None, None, false) => SessionSelection::New,
            };
            run_interactive_chat(config, cred_manager, discovery, selection, explicit_model)
                .await?;
        }
        Some(Commands::Voice { .. }) => {
            run_voice_loop(config, cred_manager, discovery).await?;
        }
        Some(Commands::Hooks { json }) => {
            commands::hooks::run_hooks(config, commands::hooks::HooksOptions { json })?;
        }
        Some(Commands::Usage {
            since,
            by,
            json,
            limit,
            recent,
        }) => {
            commands::usage::run_usage(
                config,
                cred_manager,
                discovery,
                commands::usage::UsageOptions {
                    since: since.clone(),
                    by: by.clone(),
                    json,
                    limit,
                    recent,
                },
            )
            .await?;
        }
        Some(Commands::Exec {
            prompt,
            json,
            yes,
            strict,
            session,
            fork,
            continue_session,
        }) => {
            // `--session`/`--continue`/`--fork` behave as they do in `chat`, so a headless run can
            // pick up a conversation a person started.
            let selection = match (
                session.or(cli.session),
                fork.or(cli.fork),
                continue_session || cli.continue_session,
            ) {
                (Some(id), _, _) => SessionSelection::Resume(id),
                (None, Some(fork_id), _) => SessionSelection::Fork(fork_id),
                (None, None, true) => SessionSelection::Continue,
                (None, None, false) => SessionSelection::New,
            };
            let options = commands::exec::ExecOptions {
                prompt: prompt.clone(),
                json,
                selection,
                yes,
                strict,
            };
            commands::exec::run_exec(config, cred_manager, discovery, options).await?;
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
        Some(Commands::Fetch { provider, refresh }) => {
            let default_provider = config.model.provider.clone();
            let provider = provider.unwrap_or_else(|| default_provider);
            println!(
                "Fetching models for provider '{}'...",
                provider.cyan().bold()
            );
            match discovery
                .get_models(&provider, &cred_manager, refresh)
                .await
            {
                Ok(models) => {
                    println!(
                        "\n{} Available Models for {}:",
                        "✔".green().bold(),
                        provider.cyan().bold()
                    );
                    for m in &models {
                        println!("  • {}", m);
                    }
                    println!("\n  Total: {} models", models.len());
                }
                Err(e) => {
                    eprintln!("{} Failed to fetch models: {}", "✖".red().bold(), e);
                }
            }
        }
        Some(Commands::Diff {
            session,
            turn,
            all,
            stat,
            json,
        }) => {
            commands::diff::run_diff(
                &config,
                commands::diff::DiffOptions {
                    session,
                    turn,
                    json,
                    stat,
                    all,
                },
            )
            .await?;
        }
        Some(Commands::Sessions { subcommand }) => {
            let cmd = subcommand.unwrap_or_else(|| {
                if dialoguer::console::Term::stdout().features().is_attended() {
                    SessionsCommand::Select
                } else {
                    SessionsCommand::List
                }
            });
            if let Some(selected_session) = handle_sessions_command(cmd, &config).await? {
                run_interactive_chat(
                    config,
                    cred_manager,
                    discovery,
                    SessionSelection::Resume(selected_session),
                    false,
                )
                .await?;
            }
        }
        Some(Commands::Supervisor { args }) => {
            run_supervisor_loop(args).await?;
        }
        Some(Commands::Config { subcommand }) => {
            handle_config_command(subcommand, &layers)?;
        }
        // Explicitly re-runnable: the `first_run` gate is bypassed because asking for the flow is
        // itself the intent. Recalibrating a core later is a config edit, not a memory wipe.
        Some(Commands::Setup) => {
            let outcome = harness_onboarding::run_interactive(&layers)?;
            println!(
                "persona.id = {} ({}) written to {}",
                outcome.persona.id,
                outcome.persona.name,
                outcome.config_path.display()
            );
            handle_interactive_auth(cred_manager).await?;
        }
        Some(Commands::Gateway {
            platform,
            yes,
            port,
        }) => {
            let options = commands::gateway::GatewayOptions {
                platform,
                yes,
                port,
            };
            commands::gateway::run_gateway(config, cred_manager, discovery, options).await?;
        }
        Some(Commands::Eval { subcommand }) => {
            handle_eval_command(subcommand, cred_manager, discovery, &config).await?;
        }
        Some(Commands::Heartbeat { target }) => {
            commands::heartbeat::run_cli_heartbeat(
                target.as_deref(),
                &config,
                &cred_manager,
                &discovery,
            )
            .await?;
        }
        Some(Commands::App) | Some(Commands::Tui) => {
            let model_name = config.model.default.clone();
            let cwd = std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| "rust-agent-harness".to_string());
            let app = tui::app::AppState::new(model_name, cwd);
            tui::run_app_tui(app).await?;
        }
        None if cli.tui => {
            let model_name = config.model.default.clone();
            let cwd = std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| "rust-agent-harness".to_string());
            let app = tui::app::AppState::new(model_name, cwd);
            tui::run_app_tui(app).await?;
        }
        None => {
            let selection = match (cli.session, cli.fork, cli.continue_session) {
                (Some(id), _, _) => SessionSelection::Resume(id),
                (None, Some(fork_id), _) => SessionSelection::Fork(fork_id),
                (None, None, true) => SessionSelection::Continue,
                (None, None, false) => SessionSelection::New,
            };
            run_interactive_chat(config, cred_manager, discovery, selection, explicit_model)
                .await?;
        }
    }

    Ok(())
}

/// Rebuild the clap command so `--help` reports the *resolved* provider and model defaults.
fn build_command(resolved: &config::Resolved) -> clap::Command {
    let provider = resolved
        .get("model.provider")
        .unwrap_or("(unset)")
        .to_string();
    let model = resolved
        .get("model.default")
        .unwrap_or("(unset)")
        .to_string();
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
