//! `luna gateway`: Connect Luna to external chat platforms (Telegram, Discord, WhatsApp).
//!
//! Bridges inbound messages from messaging platforms to Luna agent turns and routes replies back.
//! Manages persistent session per channel/user conversation.

pub mod adapter;
pub mod discord;
pub mod telegram;
#[cfg(test)]
mod tests;
pub mod whatsapp;

use colored::Colorize;
use harness_core::approval::{AllowAll, ApprovalMode, Approver};
use harness_core::{
    approval, Agent, AgentConfig, Config, DefaultToolRegistry, HarnessError, PermissionEngine,
    SessionBinding, ToolRegistry,
};
use harness_evolver::{
    CheckImprovementTool, CodebaseEvolver, DelegateImprovementTool, ListImprovementJobsTool,
};
use harness_memory::{ForgetMemoryTool, SaveMemoryTool, SqliteMemoryEngine, UpdateMemoryTool};
use harness_notes::{
    DeleteNoteTool, ListNotesTool, ReadNoteTool, SaveNoteTool, SearchNotesTool, SqliteNotesEngine,
    UpdateNoteTool,
};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::approval::approval_settings;
use super::chat::ChatAgent;
use super::sessions::open_store;
use adapter::PlatformAdapter;
use discord::DiscordAdapter;
use telegram::TelegramAdapter;
use whatsapp::WhatsAppAdapter;

/// CLI options for `luna gateway`.
#[derive(Debug, Clone)]
pub struct GatewayOptions {
    /// Platform to run: "telegram", "discord", "whatsapp", or "all".
    pub platform: Option<String>,
    /// Approve mutating tool calls without asking.
    pub yes: bool,
    /// Custom port for webhook servers (e.g. WhatsApp).
    pub port: Option<u16>,
}

fn headless_approver(config: &Config, yes: bool) -> Arc<dyn Approver> {
    if yes || config.tools.approval == ApprovalMode::Allow {
        Arc::new(AllowAll)
    } else {
        Arc::new(approval::DenyAll)
    }
}

pub async fn run_gateway(
    mut config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    options: GatewayOptions,
) -> Result<(), HarnessError> {
    // Apply persona prompt prefix
    harness_onboarding::persona::apply_selected(&mut config)?;

    let store = Arc::new(open_store(&config)?);
    let provider_id = config.model.provider.clone();
    let model_id = config.model.default.clone();

    if provider_id.is_empty() {
        return Err(HarnessError::ConfigurationError(
            "No LLM provider configured. Run 'luna auth' or pass a provider in config.".to_string(),
        ));
    }

    let provider = Arc::new(GenericProviderClient::new(
        provider_id.clone(),
        model_id.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&provider_id),
    )?);

    let memory = Arc::new(SqliteMemoryEngine::new(Some(
        config.paths.memory_db.clone(),
    ))?);
    let notes = Arc::new(SqliteNotesEngine::new(Some(config.paths.notes_db.clone()))?);
    let evolver = Arc::new(
        CodebaseEvolver::new(
            config.paths.workspace_root.clone(),
            config.paths.jobs_dir.clone(),
        )
        .with_provider(provider.clone()),
    );

    // Build platform adapters
    let requested_platform =
        options
            .platform
            .as_deref()
            .unwrap_or(if config.gateway.platform.is_empty() {
                "all"
            } else {
                &config.gateway.platform
            });

    let mut adapters: Vec<Box<dyn PlatformAdapter>> = Vec::new();

    // 1. Telegram
    let telegram_token = std::env::var("TELEGRAM_BOT_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(|| {
            if !config.gateway.telegram_bot_token.is_empty() {
                Some(config.gateway.telegram_bot_token.clone())
            } else {
                None
            }
        });

    if (requested_platform == "telegram" || requested_platform == "all") && telegram_token.is_some()
    {
        let token = telegram_token.unwrap();
        let allowed = if !config.gateway.telegram_allowed_users.is_empty() {
            config.gateway.telegram_allowed_users.clone()
        } else if let Ok(val) = std::env::var("TELEGRAM_ALLOWED_USERS") {
            val.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else {
            Vec::new()
        };
        adapters.push(Box::new(TelegramAdapter::new(token, allowed)));
        info!("Telegram platform adapter enabled");
    }

    // 2. Discord
    let discord_token = std::env::var("DISCORD_BOT_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(|| {
            if !config.gateway.discord_bot_token.is_empty() {
                Some(config.gateway.discord_bot_token.clone())
            } else {
                None
            }
        });
    let discord_channel = std::env::var("DISCORD_CHANNEL_ID")
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(|| {
            if !config.gateway.discord_channel_id.is_empty() {
                Some(config.gateway.discord_channel_id.clone())
            } else {
                None
            }
        });

    if (requested_platform == "discord" || requested_platform == "all") && discord_token.is_some() {
        let token = discord_token.unwrap();
        let channel = discord_channel.unwrap_or_default();
        let allowed = if !config.gateway.discord_allowed_users.is_empty() {
            config.gateway.discord_allowed_users.clone()
        } else if let Ok(val) = std::env::var("DISCORD_ALLOWED_USERS") {
            val.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else {
            Vec::new()
        };
        adapters.push(Box::new(DiscordAdapter::new(token, channel, allowed)));
        info!("Discord platform adapter enabled");
    }

    // 3. WhatsApp Cloud
    let wa_token = std::env::var("WHATSAPP_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(|| {
            if !config.gateway.whatsapp_token.is_empty() {
                Some(config.gateway.whatsapp_token.clone())
            } else {
                None
            }
        });
    let wa_phone = std::env::var("WHATSAPP_PHONE_NUMBER_ID")
        .ok()
        .filter(|p| !p.is_empty())
        .or_else(|| {
            if !config.gateway.whatsapp_phone_number_id.is_empty() {
                Some(config.gateway.whatsapp_phone_number_id.clone())
            } else {
                None
            }
        });
    let wa_verify = std::env::var("WHATSAPP_VERIFY_TOKEN")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            if !config.gateway.whatsapp_verify_token.is_empty() {
                Some(config.gateway.whatsapp_verify_token.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "luna-verify".to_string());

    let port = options.port.unwrap_or(config.gateway.port);

    if (requested_platform == "whatsapp" || requested_platform == "all")
        && wa_token.is_some()
        && wa_phone.is_some()
    {
        let wa =
            WhatsAppAdapter::new(wa_token.unwrap(), wa_phone.unwrap(), wa_verify, port).await?;
        adapters.push(Box::new(wa));
        info!("WhatsApp Cloud platform adapter enabled on port {port}");
    }

    if adapters.is_empty() {
        eprintln!(
            "{}",
            "No messaging platforms could be started. Configure bot tokens via `luna config` or env vars:".yellow()
        );
        eprintln!("  Telegram: TELEGRAM_BOT_TOKEN or `luna config set gateway.telegram_bot_token <token>`");
        eprintln!("  Discord:  DISCORD_BOT_TOKEN, DISCORD_CHANNEL_ID");
        eprintln!("  WhatsApp: WHATSAPP_TOKEN, WHATSAPP_PHONE_NUMBER_ID");
        return Err(HarnessError::ConfigurationError(
            "no active gateway platforms configured".to_string(),
        ));
    }

    println!("{}", "🌙 Luna Messaging Gateway active".green().bold());
    println!(
        "Platforms: {}",
        adapters
            .iter()
            .map(|a| a.name())
            .collect::<Vec<_>>()
            .join(", ")
            .cyan()
    );
    println!("Model:     {} / {}", provider_id.cyan(), model_id.cyan());
    println!("Listening for messages... Press Ctrl+C to stop.\n");

    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n{}", "Shutting down gateway...".yellow());
                break;
            }
            _ = interval.tick() => {
                for adapter in &mut adapters {
                    let messages = match adapter.poll_messages().await {
                        Ok(msgs) => msgs,
                        Err(e) => {
                            warn!("Adapter {} poll error: {e}", adapter.name());
                            continue;
                        }
                    };

                    for msg in messages {
                        let session_id = format!("gateway-{}-{}", msg.platform, msg.chat_id.replace(':', "-"));

                        // Build agent for this turn
                        let registry = Arc::new(DefaultToolRegistry::new());
                        if !config.memory.mode.is_background() {
                            let _ = registry.register(Arc::new(SaveMemoryTool::new(memory.clone()))).await;
                            let _ = registry.register(Arc::new(UpdateMemoryTool::new(memory.clone()))).await;
                            let _ = registry.register(Arc::new(ForgetMemoryTool::new(memory.clone()))).await;
                            let _ = registry.register(Arc::new(SaveNoteTool::new(notes.clone()))).await;
                            let _ = registry.register(Arc::new(UpdateNoteTool::new(notes.clone()))).await;
                            let _ = registry.register(Arc::new(DeleteNoteTool::new(notes.clone()))).await;
                        }
                        let _ = registry.register(Arc::new(SearchNotesTool::new(notes.clone()))).await;
                        let _ = registry.register(Arc::new(ListNotesTool::new(notes.clone()))).await;
                        let _ = registry.register(Arc::new(ReadNoteTool::new(notes.clone()))).await;
                        let _ = registry.register(Arc::new(DelegateImprovementTool::new(evolver.clone()))).await;
                        let _ = registry.register(Arc::new(CheckImprovementTool::new(evolver.clone()))).await;
                        let _ = registry.register(Arc::new(ListImprovementJobsTool::new(evolver.clone()))).await;

                        let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
                        let bg_queue = Arc::new(
                            harness_core::tasks::BackgroundTaskQueue::new(config.paths.jobs_dir.clone())
                                .with_network_policy(config.tools.network),
                        );
                        let todo_scope = harness_core::tools::TodoScope::new(config.paths.todos_dir.clone(), &session_id);
                        let _ = harness_core::tools::register_core_tools_shared_queue(
                            &reg_dyn,
                            &config,
                            harness_core::tools::ReadCache::new(),
                            None,
                            None,
                            bg_queue,
                            todo_scope,
                        ).await;

                        let checkpoints = Arc::new(harness_core::CheckpointController::new(
                            config.paths.checkpoints_dir.clone(),
                            config.paths.workspace_root.clone(),
                            &config.checkpoints,
                        ));

                        let permissions = PermissionEngine::validate(config.tools.permissions.clone()).unwrap_or_default();
                        let approver = headless_approver(&config, options.yes || config.gateway.allow_mutating_tools);

                        let mut agent_cfg = AgentConfig {
                            require_hitl_for_mutation: config.agent.require_hitl_for_mutation,
                            ..AgentConfig::default()
                        };
                        agent_cfg.system_prompt_prefix = config.agent.system_prompt_prefix.clone();
                        agent_cfg.workspace_root = Some(config.paths.workspace_root.clone());
                        agent_cfg.memory_mode = config.memory.mode;

                        let hooks = Arc::new(harness_core::HookRunner::load(
                            &config.paths.workspace_root,
                            &config.hooks,
                        )?);

                        let agent: ChatAgent = Agent::new(
                            provider.clone(),
                            memory.clone(),
                            notes.clone(),
                            evolver.clone(),
                            registry,
                            agent_cfg,
                        )
                        .with_permissions(permissions)
                        .with_hooks(hooks)
                        .with_checkpoints(checkpoints)
                        .with_approval(approver, approval_settings(&config))
                        .with_session(SessionBinding {
                            store: store.clone(),
                            session_id: session_id.clone(),
                            resume_max_messages: config.session.resume_max_messages,
                        });

                        println!(
                            "[{}] Inbound from {} (chat: {}): {}",
                            adapter.name().cyan(),
                            msg.sender_name.as_deref().unwrap_or(&msg.sender_id),
                            msg.chat_id,
                            msg.text
                        );

                        let outcome = agent.run_turn(&msg.text, CancellationToken::new()).await;
                        match outcome {
                            Ok(res) => {
                                println!(
                                    "[{}] Reply to {}: {}",
                                    adapter.name().cyan(),
                                    msg.chat_id,
                                    res.reply.lines().next().unwrap_or("")
                                );
                                if let Err(e) = adapter.send_message(&msg.chat_id, &res.reply).await {
                                    error!("Failed to send reply to {}: {e}", msg.chat_id);
                                }
                            }
                            Err(e) => {
                                error!("Agent turn error for session {session_id}: {e}");
                                let _ = adapter.send_message(&msg.chat_id, "I encountered an error processing your request.").await;
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
