//! UI-agnostic session wiring and runtime assembly (`AppRuntime`).
//!
//! Prepares a complete agent session (store, provider, memory, notes, tools, MCP, checkpoints,
//! subagents, approval/clarify/sudo channels) without touching terminal I/O. Both `harness-cli`
//! and `harness-gui` build sessions on top of these primitives.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use harness_core::tools::TodoScope;
use harness_core::{
    Agent, AgentObserver, ApprovalDecision, ApprovalRequest, BackgroundSaver, CheckpointController,
    Config, DefaultToolRegistry, HarnessError, InterruptHandle, ModelProvider, SessionBinding,
    SessionStore, SqliteSessionStore, ToolRegistry,
};
use harness_memory::{
    build_memory_engine, ForgetMemoryTool, MemoryEngineKind, MemorySearchTool, SaveMemoryTool,
    UpdateMemoryTool,
};
use harness_notes::{
    DeleteNoteTool, FileNotesEngine, ListNotesTool, ReadNoteTool, SaveNoteTool, SearchNotesTool,
    UpdateNoteTool,
};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};
use tokio::sync::{mpsc, oneshot};

use crate::approval::{approval_settings, build_headless_approver, ChannelApprover};
use crate::clarify::{ClarifyRequest, ClarifyResponse, ClarifyTool};
use crate::context_window::context_window;
use crate::features::FeatureToggles;
use crate::memory_writer;
use crate::prompts::ChannelSudoPrompter;
use crate::session::{open_store, resolve_session, SessionSelection};
use crate::usage::UsageRecorder;

pub type AppAgent =
    Agent<GenericProviderClient, MemoryEngineKind, FileNotesEngine, DefaultToolRegistry>;

/// Extra tool-registration callback so callers (CLI desktop tools, plugin tools, scheduler tools)
/// can inject domain-specific tools into the shared registry during session preparation.
pub type ExtraToolRegistrar = Arc<
    dyn Fn(
            Arc<DefaultToolRegistry>,
        ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Default)]
pub struct AppSessionOptions {
    pub interactive: bool,
    pub explicit_model: bool,
    pub system_prompt_prefix: Option<String>,
    pub extra_tools: Option<ExtraToolRegistrar>,
    /// Optional per-session approval mode override (e.g. autonomous automations).
    pub approval_mode_override: Option<harness_core::ApprovalMode>,
}

pub struct AppSessionChannels {
    pub approval_tx: mpsc::UnboundedSender<(ApprovalRequest, oneshot::Sender<ApprovalDecision>)>,
    pub approval_rx: mpsc::UnboundedReceiver<(ApprovalRequest, oneshot::Sender<ApprovalDecision>)>,
    pub approval_activity: Arc<tokio::sync::Notify>,
    pub clarify_tx: mpsc::UnboundedSender<(ClarifyRequest, oneshot::Sender<ClarifyResponse>)>,
    pub clarify_rx: mpsc::UnboundedReceiver<(ClarifyRequest, oneshot::Sender<ClarifyResponse>)>,
    pub sudo_rx: mpsc::UnboundedReceiver<(String, oneshot::Sender<Option<String>>)>,
    pub job_rx: mpsc::UnboundedReceiver<harness_core::tasks::JobCompletion>,
}

pub struct AppRuntime {
    pub config: Config,
    pub cred_manager: Arc<CredentialManager>,
    pub discovery: Arc<ModelDiscovery>,
    pub store: Arc<SqliteSessionStore>,
    pub usage: Arc<UsageRecorder>,
    pub session_id: String,
    pub todo_scope: TodoScope,
    pub current_provider: String,
    pub current_model: String,
    pub compact_provider: Option<String>,
    pub compact_model: Option<String>,
    pub feature_toggles: FeatureToggles,
    pub registry: Arc<DefaultToolRegistry>,
    pub memory: Arc<MemoryEngineKind>,
    pub notes: Arc<FileNotesEngine>,
    pub bg_queue: Arc<harness_core::tasks::BackgroundTaskQueue>,
    pub delegate_tool: Arc<harness_core::tools::DelegateTaskTool>,
    pub checkpoints: Arc<CheckpointController>,
    pub allow_all_session: Arc<AtomicBool>,
    pub plan_mode: Arc<AtomicBool>,
    pub agent: Arc<tokio::sync::Mutex<AppAgent>>,
    pub steer_handle: Arc<Mutex<VecDeque<String>>>,
    pub interrupt_handle: Arc<InterruptHandle>,
    pub channels: AppSessionChannels,
}

impl AppRuntime {
    /// Assemble a complete headless session runtime.
    pub async fn prepare(
        mut config: Config,
        cred_manager: Arc<CredentialManager>,
        discovery: Arc<ModelDiscovery>,
        selection: SessionSelection,
        options: AppSessionOptions,
        observer: Option<AgentObserver>,
    ) -> Result<Self, HarnessError> {
        let store = Arc::new(open_store(&config)?);
        let usage = Arc::new(UsageRecorder::from_config(&config));
        let session_id = resolve_session(store.as_ref(), selection).await?;
        let todo_scope = TodoScope::new(config.paths.todos_dir.clone(), &session_id);

        let mut current_provider = config.model.provider.clone();
        let mut current_model = config.model.default.clone();
        let record = store.get(&session_id).await?;
        if !options.explicit_model {
            if let Some((provider, model)) = record.model.clone() {
                current_provider = provider;
                current_model = model;
            }
        }
        if current_provider.is_empty() {
            return Err(HarnessError::ConfigurationError(
                "No LLM provider configured or loaded with credentials. Run 'luna auth' to register a credential or specify '--provider <name>'.".to_string(),
            ));
        }

        config.agent.context_window =
            Some(context_window(&config, &discovery, &current_provider, &current_model) as usize);

        if record.model.is_none() {
            store
                .set_model(&session_id, &current_provider, &current_model)
                .await?;
        }

        let compact_provider = record
            .compact_model
            .as_ref()
            .map(|(p, _)| p.clone())
            .or_else(|| {
                config
                    .agent
                    .compaction_provider
                    .clone()
                    .filter(|p| !p.is_empty())
            });
        let compact_model = record
            .compact_model
            .as_ref()
            .map(|(_, m)| m.clone())
            .or_else(|| {
                config
                    .agent
                    .compaction_model
                    .clone()
                    .filter(|m| !m.is_empty())
            });

        for (prov, cred_id) in &record.credential_priorities {
            cred_manager.set_session_priority_credential(&session_id, prov, cred_id);
        }

        let provider = Arc::new(
            GenericProviderClient::new(
                current_provider.clone(),
                current_model.clone(),
                cred_manager.clone(),
                discovery.clone(),
                config.providers.get(&current_provider),
            )?
            .with_session_id(&session_id)
            .with_image_fallback(
                config.model.image_fallback_provider.clone(),
                config.model.image_fallback_model.clone(),
            ),
        );

        let (comp_p, comp_m) = match &record.compact_model {
            Some((p, m)) => (p.clone(), m.clone()),
            None => (
                compact_provider
                    .clone()
                    .unwrap_or_else(|| current_provider.clone()),
                compact_model
                    .clone()
                    .unwrap_or_else(|| current_model.clone()),
            ),
        };
        let compaction = GenericProviderClient::new(
            comp_p.clone(),
            comp_m.clone(),
            cred_manager.clone(),
            discovery.clone(),
            config.providers.get(&comp_p),
        )
        .ok()
        .map(|client| harness_core::CompactionSetup {
            provider: Arc::new(client.with_session_id(&session_id)) as Arc<dyn ModelProvider>,
            model: comp_m,
        });

        let memory = build_memory_engine(&config)?;
        let notes = Arc::new(FileNotesEngine::new(
            config.paths.notes_dir.clone(),
            Some(config.paths.notes_db.clone()),
        )?);
        let memory_dyn: Arc<dyn harness_core::MemoryEngine> = memory.clone();
        let memory_saver = memory_writer::build_saver(
            &config,
            &memory_dyn,
            notes.clone(),
            &current_provider,
            &current_model,
            &session_id,
            cred_manager.clone(),
            discovery.clone(),
        );
        let feature_toggles = FeatureToggles::from_config(&config);
        let registry = Arc::new(DefaultToolRegistry::new());

        registry
            .register(Arc::new(SaveMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(UpdateMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(ForgetMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(SaveNoteTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(UpdateNoteTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(DeleteNoteTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(SearchNotesTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(ListNotesTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(ReadNoteTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(
                MemorySearchTool::new(memory.clone())
                    .with_notes(notes.clone())
                    .with_notes_gate(feature_toggles.notes_atomic()),
            ))
            .await?;

        let bg_queue = Arc::new(
            harness_core::tasks::BackgroundTaskQueue::new(config.paths.jobs_dir.clone())
                .with_network_policy(config.tools.network),
        );
        let delegate_tool = Arc::new(harness_core::tools::DelegateTaskTool::new(
            provider.clone(),
            memory.clone(),
            notes.clone(),
            Some(store.clone()),
            config.clone(),
        ));
        registry.register(delegate_tool.clone()).await?;

        let sudo_cache = Arc::new(Mutex::new(std::env::var("SUDO_PASSWORD").ok()));
        let (sudo_tx, sudo_rx) =
            mpsc::unbounded_channel::<(String, oneshot::Sender<Option<String>>)>();
        let sudo_prompter = Arc::new(ChannelSudoPrompter::new(sudo_tx));

        let read_cache = harness_core::tools::ReadCache::new();
        let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
        harness_core::tools::register_core_tools_shared_queue(
            &reg_dyn,
            &config,
            read_cache,
            Some(sudo_cache),
            Some(sudo_prompter),
            bg_queue.clone(),
            todo_scope.clone(),
        )
        .await?;

        // MCP servers are not spawned at startup: opening the app must not block on a server
        // whose host is not running. They register lazily when MCP is enabled for the session.

        if let Some(extra) = options.extra_tools {
            extra(registry.clone()).await?;
        }

        let checkpoints = Arc::new(CheckpointController::new(
            config.paths.checkpoints_dir.clone(),
            config.paths.workspace_root.clone(),
            &config.checkpoints,
        ));
        registry
            .register_middleware(Arc::new(harness_core::CheckpointMiddleware::new(
                checkpoints.clone(),
            )))
            .await;

        let (job_tx, job_rx) = mpsc::unbounded_channel::<harness_core::tasks::JobCompletion>();
        if options.interactive {
            bg_queue.set_completion_notifier(Arc::new(move |completion| {
                let _ = job_tx.send(completion);
            }));
        }

        let effective_approval = options
            .approval_mode_override
            .unwrap_or(config.tools.approval);
        let (approval_tx, approval_rx) =
            mpsc::unbounded_channel::<(ApprovalRequest, oneshot::Sender<ApprovalDecision>)>();
        let allow_all_session = Arc::new(AtomicBool::new(
            effective_approval == harness_core::ApprovalMode::Allow,
        ));
        let plan_mode = Arc::new(AtomicBool::new(false));
        let (approver, approval_activity): (
            Arc<dyn harness_core::Approver>,
            Arc<tokio::sync::Notify>,
        ) = if options.interactive && effective_approval != harness_core::ApprovalMode::Deny {
            let ch = Arc::new(ChannelApprover::new(
                approval_tx.clone(),
                allow_all_session.clone(),
                plan_mode.clone(),
            ));
            let act = ch.activity_handle();
            (ch, act)
        } else {
            (
                build_headless_approver(&config),
                Arc::new(tokio::sync::Notify::new()),
            )
        };

        let (clarify_tx, clarify_rx) =
            mpsc::unbounded_channel::<(ClarifyRequest, oneshot::Sender<ClarifyResponse>)>();
        registry
            .register(Arc::new(ClarifyTool::new(clarify_tx.clone())))
            .await?;

        let mut agent_config = config.agent.clone();
        if let Some(prefix) = options.system_prompt_prefix {
            agent_config.system_prompt_prefix = Some(match agent_config.system_prompt_prefix {
                Some(existing) if !existing.is_empty() => format!("{existing}\n\n{prefix}"),
                _ => prefix,
            });
        }
        agent_config.workspace_root = Some(config.paths.workspace_root.clone());
        agent_config.memory_mode = config.memory.mode;
        agent_config.memory_enabled = config.memory_effective_enabled();
        agent_config.notes_enabled = config.notes_effective_enabled();
        agent_config.skills_enabled = config.skills_effective_enabled();
        agent_config.mcp_enabled = config.mcp_effective_enabled();

        // Resolve pinned skills (session-specific + global)
        let session_pinned = store
            .get_pinned_skills(&session_id)
            .await
            .unwrap_or_default();
        let mut combined_pinned = config.agent.pinned_skills.clone();
        for sp in session_pinned {
            if !combined_pinned
                .iter()
                .any(|p| harness_plugins::names_match(p, &sp))
            {
                combined_pinned.push(sp);
            }
        }
        if !combined_pinned.is_empty() {
            let mut available_skills = Vec::new();
            if let Ok(loaded) =
                harness_plugins::SkillLoader::load_skills_from_dir(&config.paths.skills_dir)
            {
                available_skills.extend(loaded);
            }
            let ws_skills = config.paths.workspace_root.join(".harness/skills");
            if ws_skills.exists() {
                if let Ok(loaded) = harness_plugins::SkillLoader::load_skills_from_dir(&ws_skills) {
                    available_skills.extend(loaded);
                }
            }
            available_skills.push(harness_plugins::plugin_skill());
            agent_config.pinned_skills = combined_pinned.clone();
            agent_config.pinned_skill_instructions = harness_plugins::resolve_pinned_instructions(
                &combined_pinned,
                &available_skills,
                Some(&config.paths.skills_dir),
            );
        }

        let mut agent = Agent::new(
            provider,
            memory.clone(),
            notes.clone(),
            registry.clone(),
            agent_config,
        )
        .with_permissions(harness_core::PermissionEngine::validate(
            config.tools.permissions.clone(),
        )?)
        .with_approval(approver.clone(), approval_settings(&config))
        .with_session(SessionBinding {
            store: store.clone(),
            session_id: session_id.clone(),
            resume_max_messages: config.session.resume_max_messages,
        });

        delegate_tool.set_worker_approver(approver);
        agent.set_background_saver(memory_saver.map(|s| s as Arc<dyn BackgroundSaver>));

        let hooks = Arc::new(harness_core::HookRunner::load(
            &config.paths.workspace_root,
            &config.hooks,
        )?);
        agent = agent
            .with_hooks(hooks)
            .with_checkpoints(checkpoints.clone());
        if let Some(obs) = observer {
            agent = agent.with_observer(obs);
        }
        if let Some(setup) = compaction {
            agent = agent.with_compaction(setup);
        }

        let steer_handle = agent.steer_handle();
        let interrupt_handle = agent.interrupt_handle();
        delegate_tool.set_steer_handle(steer_handle.clone());
        let agent = Arc::new(tokio::sync::Mutex::new(agent));

        {
            let ag = agent.lock().await;
            if let Err(error) = ag
                .emit_hook_event(harness_core::HookEvent::SessionStart, None)
                .await
            {
                tracing::warn!("session_start hook refused the run: {}", error);
            }
        }

        Ok(Self {
            config,
            cred_manager,
            discovery,
            store,
            usage,
            session_id,
            todo_scope,
            current_provider,
            current_model,
            compact_provider,
            compact_model,
            feature_toggles,
            registry,
            memory,
            notes,
            bg_queue,
            delegate_tool,
            checkpoints,
            allow_all_session,
            plan_mode,
            agent,
            steer_handle,
            interrupt_handle,
            channels: AppSessionChannels {
                approval_tx,
                approval_rx,
                approval_activity,
                clarify_tx,
                clarify_rx,
                sudo_rx,
                job_rx,
            },
        })
    }
}
