use colored::Colorize;
use std::sync::Arc;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use harness_core::config::{self, Config};
use harness_core::HarnessError;
use harness_providers::{CredentialManager, ModelDiscovery};
use harness_telemetry::init_telemetry;

mod commands;

mod eval;
#[cfg(test)]
mod update_tests;
use commands::auth::{handle_auth_command, handle_interactive_auth, AuthCommand};
use commands::chat::{run_inline_chat, run_interactive_chat};
use commands::config::{catalog_defaults, handle_config_command, ConfigCommand};
use commands::mcp::{handle_mcp_command, McpCommand};
use commands::memory::{handle_memory_command, MemoryCommand};
use commands::models::{handle_models_command, ModelsCommand};
use commands::notes::{handle_notes_command, NotesCommand};
use commands::profile::{handle_profile_command, parse_invocation, ProfileCommand};
use commands::sessions::{handle_sessions_command, SessionSelection, SessionsCommand};
use commands::skill::{handle_skill_command, SkillCommand};
use commands::supervisor::run_supervisor_loop;
use commands::voice::run_voice_loop;
use eval::command::{handle_eval_command, EvalCommand};

/// Model-discovery cache lifetime. A cache TTL rather than user intent, so it is not a v1 config
/// key; the cache path itself is `paths.models_cache`.
const MODEL_CACHE_TTL_HOURS: i64 = 24;

/// What `luna --version` reports: the package version plus the commit the binary was built from,
/// stamped by `build.rs`. It is what the launch-time update check compares against.
const VERSION: &str = env!("LUNA_VERSION");

#[derive(Parser, Debug)]
#[command(
    name = "luna",
    author = "Luna AI",
    version = VERSION,
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

    /// Use the classic inline CLI scrollback REPL instead of the full-screen TUI
    #[arg(long = "cli")]
    cli_mode: bool,

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
        /// Resume a stored session by id or name (`luna sessions list` shows both)
        #[arg(long, conflicts_with_all = ["continue_session", "fork"])]
        session: Option<String>,
        /// Fork an existing session and start from the copy
        #[arg(long, conflicts_with_all = ["session", "continue_session"])]
        fork: Option<String>,
        /// Resume the most recently updated session
        #[arg(long = "continue")]
        continue_session: bool,
        /// Use the classic inline CLI scrollback REPL instead of the full-screen TUI
        #[arg(long = "cli")]
        cli_mode: bool,
    },
    /// Launch the full-screen OpenCode-style TUI
    Tui {
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
    /// Run an autonomous multi-turn loop until an objective is achieved or stopped
    Goal {
        /// The objective; `-` or omitted reads it from stdin
        #[arg(value_name = "OBJECTIVE")]
        objective: Option<String>,
        /// Maximum iterations before stopping (default: `goal.max_iterations`)
        #[arg(short = 'n', long)]
        max_iterations: Option<usize>,
        /// Emit NDJSON events on stdout instead of plain output
        #[arg(long)]
        json: bool,
        /// Approve every mutating tool call without asking (headless default is deny)
        #[arg(long)]
        yes: bool,
        /// Exit non-zero if any tool call failed or the goal did not complete
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
        /// How many turns `--all` reads (default 200, newest first)
        #[arg(long)]
        limit: Option<usize>,
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
    /// Install a skill from GitHub, or list the skills the loader picks up
    Skill {
        #[command(subcommand)]
        subcommand: SkillCommand,
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
    /// List, create, show, delete, or locate profiles
    Profile {
        #[command(subcommand)]
        subcommand: ProfileCommand,
    },
    /// Run the setup / onboarding flow, or target a specific subsystem
    #[command(name = "setup", alias = "onboard", alias = "onboarding")]
    Setup {
        /// Subsystem to configure: memory, notes, compact, gateway, web, auth, or all
        #[arg(value_name = "TARGET")]
        target: Option<String>,
        /// Optional direct value to set without interactive prompts (e.g. "background", "session")
        #[arg(value_name = "VALUE")]
        value: Option<String>,
    },
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
    /// Inspect and maintain the Markdown memory store
    Memory {
        #[command(subcommand)]
        subcommand: MemoryCommand,
    },
    /// Work with the per-project notes workspace: notes, docs, and graph
    Notes {
        #[command(subcommand)]
        subcommand: NotesCommand,
    },
    /// Inspect configured Model Context Protocol (MCP) servers and the tools they expose
    Mcp {
        #[command(subcommand)]
        subcommand: Option<McpCommand>,
    },
}

/// Everything the profile selector and clap resolve before config-dependent work begins.
struct Startup {
    cli: Cli,
    layers: harness_core::config::Layers,
    profile_name: String,
}

/// Select the profile and parse the command line against the resolved defaults.
fn parse_cli_invocation() -> Result<Startup, HarnessError> {
    // Profile selection happens before clap, because `luna terra` is not a subcommand and a
    // launcher named `terra` changes argv[0] rather than argv[1]. The selector is removed from
    // the argument list, leaving clap an ordinary command line.
    let invocation = parse_invocation(
        std::env::args_os().collect(),
        &harness_core::profiles::profiles_root(),
    )?;
    let profile_name = invocation.name().to_string();

    // The config resolves *before* clap parses, because the flag help text reports the configured
    // defaults — `luna chat --help` must show the real provider, not a literal.
    let mut layers = config::process_layers(catalog_defaults())?;
    if profile_name != harness_core::profiles::DEFAULT_PROFILE {
        let dir =
            harness_core::profiles::profile_dir(&layers.defaults.paths.profiles_dir, &profile_name);
        layers.profile = Some(dir.join("config.yaml"));
    }
    let help_defaults = config::resolve(&layers)?;

    let matches = build_command(&help_defaults).get_matches_from(invocation.args);
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    Ok(Startup {
        cli,
        layers,
        profile_name,
    })
}

/// Fold the CLI flags into the highest-precedence config layer. Returns whether a model was
/// chosen explicitly for this run.
fn apply_model_flags(cli: &Cli, layers: &mut harness_core::config::Layers) -> bool {
    let (chat_provider, chat_model, chat_yes) = match &cli.command {
        Some(Commands::Chat {
            provider,
            model,
            yes,
            ..
        })
        | Some(Commands::Tui {
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
        Some(Commands::Exec { yes, .. })
        | Some(Commands::Goal { yes, .. })
        | Some(Commands::Gateway { yes, .. }) => {
            (cli.provider.as_ref(), cli.model.as_ref(), *yes || cli.yes)
        }
        None => (cli.provider.as_ref(), cli.model.as_ref(), cli.yes),
        _ => (cli.provider.as_ref(), cli.model.as_ref(), cli.yes),
    };

    if let Some(provider) = chat_provider {
        layers
            .flags
            .insert("model.provider".to_string(), provider.clone());
        if chat_model.is_none() {
            layers.flags.insert(
                "model.default".to_string(),
                harness_providers::catalog::get_default_model(provider).to_string(),
            );
        }
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

    explicit_model
}

fn resolve_chat_config(
    layers: &harness_core::config::Layers,
    profile_name: &str,
) -> Result<Config, HarnessError> {
    let mut config: Config = config::resolve(layers)?.into_config();
    // The profile's persona/behavior and (for a custom profile) its state paths. Forced after
    // resolution so a profile's config.yaml cannot point at another profile's data.
    commands::profile::activate(&mut config, profile_name)?;
    Ok(config)
}

fn interactive_chat(cli: &Cli) -> bool {
    match &cli.command {
        None | Some(Commands::Chat { .. }) | Some(Commands::Tui { .. }) => {
            dialoguer::console::Term::stdout().features().is_attended()
        }
        _ => false,
    }
}

/// First-run onboarding.
///
/// It runs before telemetry, credentials or a session are touched, so a user who has never
/// configured Luna gets the briefing instead of an agent loop that fails on a missing provider. It
/// only ever runs on an attended `luna`/`luna chat`: `exec`, pipes and CI stay headless, and
/// `LUNA_FIRST_RUN=false` or `luna config set first_run false` skips it. It configures the built-in
/// default; a named profile is opened by invocation, never onboarded.
async fn maybe_run_first_run_onboarding(
    config: &mut Config,
    layers: &harness_core::config::Layers,
    profile_name: &str,
    is_interactive_chat: bool,
) -> Result<bool, HarnessError> {
    if is_interactive_chat
        && profile_name == harness_core::profiles::DEFAULT_PROFILE
        && harness_onboarding::needs_onboarding(layers)?
    {
        let outcome = harness_onboarding::run_interactive(layers)?;
        tracing::info!(
            profile = outcome.profile,
            config = %outcome.config_path.display(),
            "first-run onboarding complete"
        );
        // Re-resolve so this same run uses the settings it just wrote.
        *config = config::resolve(layers)?.into_config();
        commands::profile::activate(config, profile_name)?;
        return Ok(true);
    }
    Ok(false)
}

fn setup_telemetry(cli_json: bool, config: &Config, is_interactive_chat: bool) {
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
        cli_json || config.telemetry.json,
        &config.telemetry.level,
        log_file_path.as_deref(),
    );
}

/// Build the credential manager and answer the launch-time update question.
///
/// Returns `None` when an update was installed and the process should exit so the new build is
/// used on the next launch.
async fn prepare_credentials_and_updates(
    config: &mut Config,
    layers: &harness_core::config::Layers,
    is_interactive_chat: bool,
    ran_first_run_onboarding: bool,
) -> Result<Option<Arc<CredentialManager>>, HarnessError> {
    // The credential manager's own error (unreadable/corrupt store) must reach the user: the
    // corrupt-store defect this merge fixes is exactly a `CredentialManager::new` that used to
    // silently fall back to an empty store. It now reads the config-resolved path.
    let cred_manager = CredentialManager::new(Some(config.paths.credentials.clone()))?;

    if is_interactive_chat && ran_first_run_onboarding {
        commands::auth::handle_interactive_auth(cred_manager.clone()).await?;
        *config = config::resolve(layers)?.into_config();
    }

    // The build question comes before the chat UI exists, so the first frame on screen is the
    // answer to something the launch itself asked. It is only asked on an attended chat, and the
    // check itself is silent unless a newer build was actually found.
    if is_interactive_chat {
        if let Some(status) = commands::update::check_at_launch(config).await {
            match commands::update::ask_and_install(&status, config).await {
                commands::update::LaunchOutcome::Installed(path) => {
                    println!(
                        "{} Installed {} → {} at {}. Restart luna to use it.",
                        "✔".green().bold(),
                        status.current_label(),
                        status.latest_label(),
                        path.display()
                    );
                    return Ok(None);
                }
                // The modal already showed the reason. Launching the old build is the safe
                // fallback, and running the same update question again now would only repeat it.
                commands::update::LaunchOutcome::Failed(_) => {}
                commands::update::LaunchOutcome::Remind => {}
                commands::update::LaunchOutcome::Later => {}
            }
        }
    }

    Ok(Some(cred_manager))
}

/// Create the model-discovery cache and fill in the provider/model defaults it implies.
async fn resolve_discovery_and_defaults(
    config: &mut Config,
    cred_manager: &CredentialManager,
) -> Arc<ModelDiscovery> {
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

    discovery
}

fn session_selection(
    session: Option<String>,
    fork: Option<String>,
    continue_session: bool,
) -> SessionSelection {
    match (session, fork, continue_session) {
        (Some(id), _, _) => SessionSelection::Resume(id),
        (None, Some(fork_id), _) => SessionSelection::Fork(fork_id),
        (None, None, true) => SessionSelection::Continue,
        (None, None, false) => SessionSelection::New,
    }
}

#[tokio::main]
async fn main() -> Result<(), HarnessError> {
    let Startup {
        cli,
        mut layers,
        profile_name,
    } = parse_cli_invocation()?;

    // CLI flags are the highest-precedence layer.
    let explicit_model = apply_model_flags(&cli, &mut layers);

    let mut config = resolve_chat_config(&layers, &profile_name)?;
    let is_interactive_chat = interactive_chat(&cli);

    let ran_first_run_onboarding =
        maybe_run_first_run_onboarding(&mut config, &layers, &profile_name, is_interactive_chat)
            .await?;

    setup_telemetry(cli.json, &config, is_interactive_chat);

    let Some(cred_manager) = prepare_credentials_and_updates(
        &mut config,
        &layers,
        is_interactive_chat,
        ran_first_run_onboarding,
    )
    .await?
    else {
        return Ok(());
    };

    let discovery = resolve_discovery_and_defaults(&mut config, &cred_manager).await;

    dispatch_command(
        cli,
        config,
        &layers,
        cred_manager,
        discovery,
        explicit_model,
    )
    .await
}

async fn dispatch_command(
    cli: Cli,
    config: Config,
    layers: &harness_core::config::Layers,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    explicit_model: bool,
) -> Result<(), HarnessError> {
    match cli.command {
        Some(Commands::Chat {
            session,
            fork,
            continue_session,
            cli_mode,
            ..
        }) => {
            let selection = session_selection(
                session.or(cli.session),
                fork.or(cli.fork),
                continue_session || cli.continue_session,
            );
            if cli_mode || cli.cli_mode {
                run_inline_chat(config, cred_manager, discovery, selection, explicit_model).await?;
            } else {
                run_interactive_chat(config, cred_manager, discovery, selection, explicit_model)
                    .await?;
            }
        }
        Some(Commands::Tui {
            session,
            fork,
            continue_session,
            ..
        }) => {
            let selection = session_selection(
                session.or(cli.session),
                fork.or(cli.fork),
                continue_session || cli.continue_session,
            );
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
            run_usage_command(
                config,
                cred_manager,
                discovery,
                since,
                by,
                json,
                limit,
                recent,
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
            run_exec_command(
                config,
                cred_manager,
                discovery,
                prompt,
                json,
                yes,
                strict,
                session.or(cli.session),
                fork.or(cli.fork),
                continue_session || cli.continue_session,
            )
            .await?;
        }
        Some(Commands::Goal {
            objective,
            max_iterations,
            json,
            yes,
            strict,
            session,
            fork,
            continue_session,
        }) => {
            run_goal_command(
                config,
                cred_manager,
                discovery,
                objective,
                max_iterations,
                json,
                yes,
                strict,
                session.or(cli.session),
                fork.or(cli.fork),
                continue_session || cli.continue_session,
            )
            .await?;
        }
        Some(Commands::Auth { subcommand }) => {
            run_auth_selection(subcommand, cred_manager).await?;
        }
        Some(Commands::Models { subcommand }) => {
            run_models_command(config, subcommand, cred_manager, discovery).await?;
        }
        Some(Commands::Fetch { provider, refresh }) => {
            run_fetch_command(&config, cred_manager, discovery, provider, refresh).await?;
        }
        Some(Commands::Diff {
            session,
            turn,
            all,
            limit,
            stat,
            json,
        }) => {
            run_diff_command(&config, session, turn, all, limit, stat, json).await?;
        }
        Some(Commands::Sessions { subcommand }) => {
            run_sessions_command(config, cred_manager, discovery, subcommand).await?;
        }
        Some(Commands::Skill { subcommand }) => {
            handle_skill_command(subcommand, &config).await?;
        }
        Some(Commands::Supervisor { args }) => {
            run_supervisor_loop(args).await?;
        }
        Some(Commands::Config { subcommand }) => {
            handle_config_command(subcommand, layers)?;
        }
        Some(Commands::Profile { subcommand }) => {
            run_profile_command(subcommand, &config).await?;
        }
        // Explicitly re-runnable: the `first_run` gate is bypassed because asking for the flow is
        // itself the intent. Recalibrating a core later is a config edit, not a memory wipe.
        Some(Commands::Setup { target, value }) => {
            handle_setup_command(target, value, layers, cred_manager).await?;
        }
        Some(Commands::Gateway {
            platform,
            yes,
            port,
        }) => {
            run_gateway_command(config, cred_manager, discovery, platform, yes, port).await?;
        }
        Some(Commands::Eval { subcommand }) => {
            handle_eval_command(subcommand, cred_manager, discovery, &config).await?;
        }
        Some(Commands::Heartbeat { target }) => {
            run_heartbeat_command(&config, &cred_manager, &discovery, target).await?;
        }
        Some(Commands::Memory { subcommand }) => {
            handle_memory_command(subcommand, &config).await?;
        }
        Some(Commands::Notes { subcommand }) => {
            handle_notes_command(subcommand, &config).await?;
        }
        Some(Commands::Mcp { subcommand }) => {
            run_mcp_selection(subcommand, &config).await?;
        }
        None => {
            let selection = session_selection(cli.session, cli.fork, cli.continue_session);
            if cli.cli_mode {
                run_inline_chat(config, cred_manager, discovery, selection, explicit_model).await?;
            } else {
                run_interactive_chat(config, cred_manager, discovery, selection, explicit_model)
                    .await?;
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_usage_command(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    since: Option<String>,
    by: Option<String>,
    json: bool,
    limit: usize,
    recent: bool,
) -> Result<(), HarnessError> {
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
    .await
}

/// `--session`/`--continue`/`--fork` behave as they do in `chat`, so a headless run can pick up a
/// conversation a person started.
#[allow(clippy::too_many_arguments)]
async fn run_exec_command(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    prompt: Option<String>,
    json: bool,
    yes: bool,
    strict: bool,
    session: Option<String>,
    fork: Option<String>,
    continue_session: bool,
) -> Result<(), HarnessError> {
    let selection = session_selection(session, fork, continue_session);
    let options = commands::exec::ExecOptions {
        prompt: prompt.clone(),
        json,
        selection,
        yes,
        strict,
    };
    commands::exec::run_exec(config, cred_manager, discovery, options).await
}

#[allow(clippy::too_many_arguments)]
async fn run_goal_command(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    objective: Option<String>,
    max_iterations: Option<usize>,
    json: bool,
    yes: bool,
    strict: bool,
    session: Option<String>,
    fork: Option<String>,
    continue_session: bool,
) -> Result<(), HarnessError> {
    let selection = session_selection(session, fork, continue_session);
    let options = commands::goal::GoalOptions {
        objective,
        max_iterations,
        json,
        selection,
        yes,
        strict,
    };
    commands::goal::run_goal(config, cred_manager, discovery, options).await
}

async fn run_auth_selection(
    subcommand: Option<AuthCommand>,
    cred_manager: Arc<CredentialManager>,
) -> Result<(), HarnessError> {
    if let Some(sub) = subcommand {
        handle_auth_command(sub, cred_manager).await?;
    } else {
        handle_interactive_auth(cred_manager).await?;
    }
    Ok(())
}

async fn run_models_command(
    config: Config,
    subcommand: ModelsCommand,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
) -> Result<(), HarnessError> {
    let default_provider = config.model.provider.clone();
    handle_models_command(subcommand, &default_provider, cred_manager, discovery).await
}

async fn run_fetch_command(
    config: &Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    provider: Option<String>,
    refresh: bool,
) -> Result<(), HarnessError> {
    let default_provider = config.model.provider.clone();
    let provider = provider.unwrap_or(default_provider);
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
    Ok(())
}

async fn run_diff_command(
    config: &Config,
    session: Option<String>,
    turn: Option<u32>,
    all: bool,
    limit: Option<usize>,
    stat: bool,
    json: bool,
) -> Result<(), HarnessError> {
    commands::diff::run_diff(
        config,
        commands::diff::DiffOptions {
            session,
            turn,
            json,
            stat,
            all,
            limit,
        },
    )
    .await
}

async fn run_sessions_command(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    subcommand: Option<SessionsCommand>,
) -> Result<(), HarnessError> {
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
    Ok(())
}

async fn run_profile_command(
    subcommand: ProfileCommand,
    config: &Config,
) -> Result<(), HarnessError> {
    let global = commands::profile::global_config()?;
    handle_profile_command(subcommand, config, &global)
}

async fn run_gateway_command(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    platform: Option<String>,
    yes: bool,
    port: Option<u16>,
) -> Result<(), HarnessError> {
    let options = commands::gateway::GatewayOptions {
        platform,
        yes,
        port,
    };
    commands::gateway::run_gateway(config, cred_manager, discovery, options).await
}

async fn run_heartbeat_command(
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    target: Option<String>,
) -> Result<(), HarnessError> {
    commands::heartbeat::run_cli_heartbeat(target.as_deref(), config, cred_manager, discovery).await
}

async fn run_mcp_selection(
    subcommand: Option<McpCommand>,
    config: &Config,
) -> Result<(), HarnessError> {
    // Bare `luna mcp` reads the config, the cheap and safe default.
    handle_mcp_command(
        subcommand.unwrap_or(McpCommand::List { json: false }),
        config,
    )
    .await
}

async fn handle_setup_command(
    target: Option<String>,
    value: Option<String>,
    layers: &harness_core::config::Layers,
    cred_manager: std::sync::Arc<harness_providers::CredentialManager>,
) -> Result<(), harness_core::HarnessError> {
    let target_norm = target
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    match target_norm.as_deref() {
        None | Some("all") => setup_all(layers, cred_manager).await?,
        Some(
            "memory" | "mem" | "saver" | "memory-saving" | "memory_saving" | "memory-mode"
            | "memory_mode" | "mode",
        ) => setup_memory(layers, value)?,
        Some("notes" | "note" | "notes-dir" | "notes_dir" | "dir") => setup_notes(layers, value)?,
        Some("compact" | "compaction" | "auto-compact" | "auto_compact" | "autocompact") => {
            setup_compact(layers, value)?
        }
        Some("gateway" | "gw") => setup_gateway(layers)?,
        Some("web" | "search" | "fetch" | "browser") => setup_web(layers, value)?,
        Some("auth" | "credentials" | "creds" | "keys" | "provider" | "providers") => {
            handle_interactive_auth(cred_manager).await?;
        }
        Some(other) => {
            return Err(harness_core::HarnessError::ConfigurationError(format!(
                "unknown setup target: '{other}'. Available targets: memory, notes, compact, gateway, web, auth, all"
            )));
        }
    }
    Ok(())
}

async fn setup_all(
    layers: &harness_core::config::Layers,
    cred_manager: std::sync::Arc<harness_providers::CredentialManager>,
) -> Result<(), harness_core::HarnessError> {
    let outcome = harness_onboarding::run_interactive(layers)?;
    println!(
        "profile = {} (built-in) written to {}",
        outcome.profile,
        outcome.config_path.display()
    );
    handle_interactive_auth(cred_manager).await?;
    Ok(())
}

fn setup_memory(
    layers: &harness_core::config::Layers,
    value: Option<String>,
) -> Result<(), harness_core::HarnessError> {
    if let Some(val) = value {
        let trimmed = val.trim().to_ascii_lowercase();
        let mode = match trimmed.as_str() {
            "session" | "1" => harness_core::MemoryMode::Session,
            "background" | "bg" | "2" => harness_core::MemoryMode::Background,
            _ => {
                return Err(harness_core::HarnessError::ConfigurationError(format!(
                    "unknown memory mode: '{val}'. Expected 'session' or 'background'."
                )));
            }
        };
        let path = harness_onboarding::persist_memory(layers, mode)?;
        println!(
            "memory.mode = {} written to {}",
            mode.as_str(),
            path.display()
        );
    } else {
        let (mode, path) = harness_onboarding::run_memory_interactive(layers)?;
        println!(
            "memory.mode = {} written to {}",
            mode.as_str(),
            path.display()
        );
    }
    Ok(())
}

fn setup_notes(
    layers: &harness_core::config::Layers,
    value: Option<String>,
) -> Result<(), harness_core::HarnessError> {
    if let Some(val) = value {
        let p = std::path::PathBuf::from(val.trim());
        let path = harness_onboarding::persist_notes(layers, &p)?;
        println!(
            "paths.notes_dir = {} written to {}",
            p.display(),
            path.display()
        );
    } else {
        let (notes_dir, path) = harness_onboarding::run_notes_interactive(layers)?;
        println!(
            "paths.notes_dir = {} written to {}",
            notes_dir.display(),
            path.display()
        );
    }
    Ok(())
}

fn setup_compact(
    layers: &harness_core::config::Layers,
    value: Option<String>,
) -> Result<(), harness_core::HarnessError> {
    if let Some(val) = value {
        let trimmed = val.trim().to_ascii_lowercase();
        let enabled = match trimmed.as_str() {
            "on" | "true" | "enable" | "enabled" | "1" => true,
            "off" | "false" | "disable" | "disabled" | "0" => false,
            _ => {
                return Err(harness_core::HarnessError::ConfigurationError(format!(
                    "unknown compact value: '{val}'. Expected 'on' or 'off'."
                )));
            }
        };
        let sel = harness_onboarding::AutoCompactSelection {
            enabled,
            ..Default::default()
        };
        let path = harness_onboarding::persist_auto_compact(layers, &sel)?;
        println!(
            "auto_compact_enabled = {} written to {}",
            enabled,
            path.display()
        );
    } else {
        let (compact, path) = harness_onboarding::run_compact_interactive(layers)?;
        println!(
            "auto_compact_enabled = {} written to {}",
            compact.enabled,
            path.display()
        );
    }
    Ok(())
}

fn setup_gateway(layers: &harness_core::config::Layers) -> Result<(), harness_core::HarnessError> {
    let (gateway, path) = harness_onboarding::run_gateway_interactive(layers)?;
    println!(
        "gateway.platform = {} (enabled = {}) written to {}",
        gateway.platform,
        gateway.enabled,
        path.display()
    );
    Ok(())
}

fn setup_web(
    layers: &harness_core::config::Layers,
    value: Option<String>,
) -> Result<(), harness_core::HarnessError> {
    if let Some(val) = value {
        let trimmed = val.trim().to_ascii_lowercase();
        let sel = match trimmed.as_str() {
            "off" | "false" | "disabled" | "none" => harness_onboarding::WebSelection::disabled(),
            "duckduckgo" | "ddg" => harness_onboarding::WebSelection {
                enabled: true,
                backend: harness_core::config::SearchBackend::DuckDuckGo,
                api_key_env: None,
            },
            "brave" => harness_onboarding::WebSelection {
                enabled: true,
                backend: harness_core::config::SearchBackend::Brave,
                api_key_env: Some("BRAVE_SEARCH_API_KEY".to_string()),
            },
            "tavily" => harness_onboarding::WebSelection {
                enabled: true,
                backend: harness_core::config::SearchBackend::Tavily,
                api_key_env: Some("TAVILY_API_KEY".to_string()),
            },
            _ => {
                return Err(harness_core::HarnessError::ConfigurationError(format!(
                    "unknown web option: '{val}'. Expected 'off', 'duckduckgo', 'brave', or 'tavily'."
                )));
            }
        };
        let path = harness_onboarding::persist_web(layers, &sel)?;
        println!(
            "tools.web.enabled = {}, search_backend = {} written to {}",
            sel.enabled,
            sel.backend.as_str(),
            path.display()
        );
    } else {
        let (web, path) = harness_onboarding::run_web_interactive(layers)?;
        println!(
            "tools.web.enabled = {}, search_backend = {} written to {}",
            web.enabled,
            web.backend.as_str(),
            path.display()
        );
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

#[cfg(test)]
mod goal_cli_tests {
    use super::*;

    #[test]
    fn goal_subcommand_parses_flags_and_objective() {
        let cli = Cli::try_parse_from([
            "luna",
            "goal",
            "--max-iterations",
            "8",
            "--yes",
            "--strict",
            "--json",
            "ship the /goal command",
        ])
        .expect("luna goal parses");

        match cli.command {
            Some(Commands::Goal {
                objective,
                max_iterations,
                json,
                yes,
                strict,
                ..
            }) => {
                assert_eq!(objective.as_deref(), Some("ship the /goal command"));
                assert_eq!(max_iterations, Some(8));
                assert!(json);
                assert!(yes);
                assert!(strict);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
