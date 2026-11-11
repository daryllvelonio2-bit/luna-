//! Startup wiring for a chat session.
//!
//! One session is opened, its model and compact model resolved, the tool registry filled, and the
//! agent assembled with the chat screen's observer. The result is the [`ChatRuntime`] both loops
//! run against.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use dialoguer::console::Term;

use harness_core::tools::TodoScope;
use harness_core::{
    Agent, ApprovalRequest, BackgroundSaver, CheckpointController, Config, DefaultToolRegistry,
    HarnessError, ModelProvider, SessionBinding, SessionStore, SqliteSessionStore, ToolRegistry,
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

use super::commands::FeatureToggles;
use super::helpers::register_mcp_servers;
use super::memory_writer;
use super::observers::{agent_observer, delegate_completion_notifier, delegate_event_notifier};
use super::prompts::ChatSudoPrompter;
use super::render::TurnProcessItem;
use super::turn;
use super::ui::ChatUi;
use super::ChatAgent;
use crate::commands::approval::{approval_settings, build_approver, ChatApprover};
use crate::commands::context_window::context_window;
use crate::commands::sessions::{open_store, resolve_session, SessionSelection};
use crate::commands::usage::UsageRecorder;

/// Everything the two chat loops need from the startup phase.
///
/// The channels are the input side of the screen: approvals, sudo passwords, clarifications and
/// background job completions. The handles are shared with the agent, so the loop and the turn
/// see the same rows, messages and flags.
pub(super) struct ChatRuntime {
    pub(super) config: Config,
    pub(super) cred_manager: Arc<CredentialManager>,
    pub(super) discovery: Arc<ModelDiscovery>,
    pub(super) store: Arc<SqliteSessionStore>,
    pub(super) usage: Arc<UsageRecorder>,
    pub(super) session_id: String,
    pub(super) todo_scope: TodoScope,
    pub(super) current_provider: String,
    pub(super) current_model: String,
    pub(super) compact_provider: Option<String>,
    pub(super) compact_model: Option<String>,
    pub(super) feature_toggles: FeatureToggles,
    pub(super) skills_arc: Option<Arc<Vec<harness_plugins::Skill>>>,
    pub(super) skills_readout: Option<String>,
    pub(super) bg_queue: Arc<harness_core::tasks::BackgroundTaskQueue>,
    pub(super) delegate_tool: Arc<harness_core::tools::DelegateTaskTool>,
    pub(super) is_attended: bool,
    pub(super) ui: Arc<ChatUi>,
    pub(super) approval_rx: mpsc::UnboundedReceiver<(
        ApprovalRequest,
        oneshot::Sender<harness_core::ApprovalDecision>,
    )>,
    pub(super) sudo_rx: mpsc::UnboundedReceiver<(String, oneshot::Sender<Option<String>>)>,
    pub(super) clarify_rx: mpsc::UnboundedReceiver<(
        crate::commands::clarify::ClarifyRequest,
        oneshot::Sender<crate::commands::clarify::ClarifyResponse>,
    )>,
    pub(super) job_rx: mpsc::UnboundedReceiver<harness_core::tasks::JobCompletion>,
    pub(super) allow_all_session: Arc<AtomicBool>,
    pub(super) plan_mode: Arc<AtomicBool>,
    pub(super) agent: Arc<tokio::sync::Mutex<ChatAgent>>,
    pub(super) steer_handle: Arc<Mutex<std::collections::VecDeque<String>>>,
    pub(super) interrupt_handle: Arc<harness_core::InterruptHandle>,
    pub(super) in_flight_tools: Arc<Mutex<Vec<TurnProcessItem>>>,
    pub(super) interruptions: Arc<Mutex<Vec<turn::InterruptNotice>>>,
    pub(super) compacting: Arc<AtomicBool>,
}

/// The registry and the shared handles it was built with.
struct ToolSetup {
    bg_queue: Arc<harness_core::tasks::BackgroundTaskQueue>,
    delegate_tool: Arc<harness_core::tools::DelegateTaskTool>,
    checkpoints: Arc<CheckpointController>,
    sudo_rx: mpsc::UnboundedReceiver<(String, oneshot::Sender<Option<String>>)>,
    skills_arc: Option<Arc<Vec<harness_plugins::Skill>>>,
    skills_readout: Option<String>,
}

/// The session row, model choice and todo scope resolved before anything is built.
struct SessionSetup {
    session_id: String,
    todo_scope: TodoScope,
    current_provider: String,
    current_model: String,
    record_compact_model: Option<(String, String)>,
    compact_provider: Option<String>,
    compact_model: Option<String>,
}

struct ProviderSetup {
    provider: Arc<GenericProviderClient>,
    compaction: Option<harness_core::CompactionSetup>,
}

struct Tooling {
    memory: Arc<MemoryEngineKind>,
    notes: Arc<FileNotesEngine>,
    memory_saver: Option<Arc<harness_memory::MemorySaver>>,
    feature_toggles: FeatureToggles,
    registry: Arc<DefaultToolRegistry>,
}

struct ApprovalState {
    approval_tx: mpsc::UnboundedSender<(
        ApprovalRequest,
        oneshot::Sender<harness_core::ApprovalDecision>,
    )>,
    approval_rx: mpsc::UnboundedReceiver<(
        ApprovalRequest,
        oneshot::Sender<harness_core::ApprovalDecision>,
    )>,
    allow_all_session: Arc<AtomicBool>,
    plan_mode: Arc<AtomicBool>,
    approver: Arc<dyn harness_core::Approver>,
}

struct AgentHandles {
    agent: Arc<tokio::sync::Mutex<ChatAgent>>,
    steer_handle: Arc<Mutex<std::collections::VecDeque<String>>>,
    interrupt_handle: Arc<harness_core::InterruptHandle>,
    in_flight_tools: Arc<Mutex<Vec<TurnProcessItem>>>,
    interruptions: Arc<Mutex<Vec<turn::InterruptNotice>>>,
    compacting: Arc<AtomicBool>,
}

/// Resolve the session row, its model choice, and the compact-model overrides.
async fn prepare_session(
    config: &mut Config,
    store: &Arc<SqliteSessionStore>,
    discovery: &Arc<ModelDiscovery>,
    selection: SessionSelection,
    explicit_model: bool,
) -> Result<SessionSetup, HarnessError> {
    // Every turn is appended to this session; a resumed one replays its recent transcript.
    // Resolved before the provider client so the session's stored model choice can take effect.
    let session_id = resolve_session(store.as_ref(), selection).await?;
    // The todo list belongs to the session, not to the workspace: a new session starts
    // with an empty list and never sees another session's tasks. The scope is a shared
    // handle, so the tools follow the session when it forks or starts over.
    let todo_scope =
        harness_core::tools::TodoScope::new(config.paths.todos_dir.clone(), &session_id);

    let mut current_provider = config.model.provider.clone();
    let mut current_model = config.model.default.clone();

    // A session carries its own model choice: `/model` stamps it on the session row, so a
    // resumed session returns to the model that conversation was switched to even when the
    // config default has moved on. An explicit CLI `--provider`/`--model` outranks the stored
    // choice for the run but does not overwrite it.
    let record = store.get(&session_id).await?;
    if !explicit_model {
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

    // Fold against the model's own window, not a fixed bar: a 300k threshold on a 256k model only
    // folds after the provider has already refused the request. Keeping the number on the shared
    // config also hands the same ceiling to delegated subagents, which clone this config.
    config.agent.context_window =
        Some(context_window(config, discovery, &current_provider, &current_model) as usize);

    // Stamp the model this session starts on (a fresh session, or a row written before model
    // persistence existed) so the choice stays durable even after the config default changes.
    if record.model.is_none() {
        store
            .set_model(&session_id, &current_provider, &current_model)
            .await?;
    }

    let compact_provider: Option<String> = record
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
    let compact_model: Option<String> = record
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

    Ok(SessionSetup {
        session_id,
        todo_scope,
        current_provider,
        current_model,
        record_compact_model: record.compact_model,
        compact_provider,
        compact_model,
    })
}

fn build_provider_setup(
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    session: &SessionSetup,
) -> Result<ProviderSetup, HarnessError> {
    let provider = Arc::new(GenericProviderClient::new(
        session.current_provider.clone(),
        session.current_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&session.current_provider),
    )?);

    // Compaction model: priority is session-specific override -> config setting -> active model
    let (compaction_provider_id, compaction_model) = match &session.record_compact_model {
        Some((p, m)) => (p.clone(), m.clone()),
        None => {
            let p = config
                .agent
                .compaction_provider
                .clone()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| session.current_provider.clone());
            let m = config
                .agent
                .compaction_model
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| session.current_model.clone());
            (p, m)
        }
    };
    let compaction = match GenericProviderClient::new(
        compaction_provider_id.clone(),
        compaction_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&compaction_provider_id),
    ) {
        Ok(client) => {
            let compaction_provider: Arc<dyn ModelProvider> = Arc::new(client);
            Some(harness_core::CompactionSetup {
                provider: compaction_provider,
                model: compaction_model,
            })
        }
        // An unusable compaction provider is a warning, not a startup failure: the session still
        // runs, and the turn that cannot compact says so.
        Err(error) => {
            eprintln!(
                "warning: context compaction is unavailable ({}): {}",
                compaction_provider_id, error
            );
            None
        }
    };

    Ok(ProviderSetup {
        provider,
        compaction,
    })
}

async fn build_tooling(
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    session: &SessionSetup,
    provider: &Arc<GenericProviderClient>,
    store: &Arc<SqliteSessionStore>,
) -> Result<(Tooling, ToolSetup), HarnessError> {
    let memory = build_memory_engine(config)?;
    let notes = Arc::new(FileNotesEngine::new(
        config.paths.notes_dir.clone(),
        Some(config.paths.notes_db.clone()),
    )?);

    // Who writes durable memory. `session` (the default) keeps the original behaviour: the agent
    // decides mid-turn and calls `save_memory` / `save_note`. `background` hands the pen to an
    // independent saver that reads each finished turn and writes the same records through the same
    // tool, so the write tools are withheld from the session to keep exactly one writer.
    let background_mode = config.memory.mode.is_background();
    let memory_dyn: Arc<dyn harness_core::MemoryEngine> = memory.clone();
    let memory_saver = memory_writer::build_saver(
        config,
        &memory_dyn,
        notes.clone(),
        &session.current_provider,
        &session.current_model,
        cred_manager.clone(),
        discovery.clone(),
    );
    // Runtime feature switches (`/memory off`, `/notes off`, `/skills off`, `/mcp off`). The agent
    // holds the authoritative flags; these atomics let the menu and the piped loop read them
    // without taking the agent lock.
    let feature_toggles = FeatureToggles::default();

    let registry = Arc::new(DefaultToolRegistry::new());
    let tool_setup = register_tools(
        config,
        &registry,
        &memory,
        &notes,
        background_mode,
        memory_saver.is_some(),
        &feature_toggles,
        provider,
        store,
        &session.todo_scope,
    )
    .await?;

    Ok((
        Tooling {
            memory,
            notes,
            memory_saver,
            feature_toggles,
            registry,
        },
        tool_setup,
    ))
}

/// Background shell jobs announce themselves on this channel. Only the interactive loop
/// consumes it; in piped mode the queue stays poll-only, so a channel nobody reads cannot
/// grow with every job the script starts.
fn start_job_notifications(
    bg_queue: &Arc<harness_core::tasks::BackgroundTaskQueue>,
    is_attended: bool,
) -> mpsc::UnboundedReceiver<harness_core::tasks::JobCompletion> {
    let (job_tx, job_rx) = mpsc::unbounded_channel::<harness_core::tasks::JobCompletion>();
    if is_attended {
        bg_queue.set_completion_notifier(Arc::new(move |completion| {
            let _ = job_tx.send(completion);
        }));
    }
    job_rx
}

fn build_approval_state(config: &Config, is_attended: bool) -> ApprovalState {
    let (approval_tx, approval_rx) = mpsc::unbounded_channel::<(
        ApprovalRequest,
        oneshot::Sender<harness_core::ApprovalDecision>,
    )>();

    let allow_all_session = Arc::new(AtomicBool::new(
        config.tools.approval == harness_core::ApprovalMode::Allow,
    ));
    let plan_mode = Arc::new(AtomicBool::new(false));

    let approver: Arc<dyn harness_core::Approver> =
        if is_attended && config.tools.approval == harness_core::ApprovalMode::Ask {
            Arc::new(ChatApprover::new(
                approval_tx.clone(),
                allow_all_session.clone(),
                plan_mode.clone(),
            ))
        } else {
            build_approver(config)
        };

    ApprovalState {
        approval_tx,
        approval_rx,
        allow_all_session,
        plan_mode,
        approver,
    }
}

#[allow(clippy::type_complexity)]
async fn register_clarify_tool(
    registry: &Arc<DefaultToolRegistry>,
) -> Result<
    (
        mpsc::UnboundedSender<(
            crate::commands::clarify::ClarifyRequest,
            oneshot::Sender<crate::commands::clarify::ClarifyResponse>,
        )>,
        mpsc::UnboundedReceiver<(
            crate::commands::clarify::ClarifyRequest,
            oneshot::Sender<crate::commands::clarify::ClarifyResponse>,
        )>,
    ),
    HarnessError,
> {
    let (clarify_tx, clarify_rx) = mpsc::unbounded_channel::<(
        crate::commands::clarify::ClarifyRequest,
        oneshot::Sender<crate::commands::clarify::ClarifyResponse>,
    )>();
    registry
        .register(Arc::new(crate::commands::clarify::ClarifyTool::new(
            clarify_tx.clone(),
        )))
        .await?;
    Ok((clarify_tx, clarify_rx))
}

fn install_ui_bridge(
    ui: &Arc<ChatUi>,
    clarify_tx: &mpsc::UnboundedSender<(
        crate::commands::clarify::ClarifyRequest,
        oneshot::Sender<crate::commands::clarify::ClarifyResponse>,
    )>,
    approval_tx: &mpsc::UnboundedSender<(
        ApprovalRequest,
        oneshot::Sender<harness_core::ApprovalDecision>,
    )>,
    allow_all_session: &Arc<AtomicBool>,
) {
    let ui_for_bridge = ui.clone();
    harness_providers::opencode::set_ui_bridge(harness_providers::opencode::OpencodeUiBridge {
        clarify_tx: clarify_tx.clone(),
        approval_tx: approval_tx.clone(),
        allow_all: allow_all_session.clone(),
        update_context_used_fn: Some(Arc::new(move |tokens| {
            ui_for_bridge.update_context_used(tokens);
        })),
    });
}

#[allow(clippy::too_many_arguments)]
fn assemble_agent(
    config: &Config,
    session: &SessionSetup,
    provider: Arc<GenericProviderClient>,
    memory: Arc<MemoryEngineKind>,
    notes: Arc<FileNotesEngine>,
    registry: Arc<DefaultToolRegistry>,
    approver: Arc<dyn harness_core::Approver>,
    delegate_tool: &Arc<harness_core::tools::DelegateTaskTool>,
    memory_saver: Option<Arc<harness_memory::MemorySaver>>,
    compaction: Option<harness_core::CompactionSetup>,
    ui: Arc<ChatUi>,
    checkpoints: Arc<CheckpointController>,
    store: Arc<SqliteSessionStore>,
) -> Result<AgentHandles, HarnessError> {
    let mut agent_config = config.agent.clone();
    agent_config.workspace_root = Some(config.paths.workspace_root.clone());
    agent_config.memory_mode = config.memory.mode;

    let agent = Agent::new(provider, memory, notes, registry, agent_config.clone())
        .with_permissions(harness_core::PermissionEngine::validate(
            config.tools.permissions.clone(),
        )?)
        .with_approval(approver.clone(), approval_settings(config))
        .with_session(SessionBinding {
            store: store.clone(),
            session_id: session.session_id.clone(),
            resume_max_messages: config.session.resume_max_messages,
        });
    let mut agent = agent;
    // Workers run in the background but must ask the same human as the main turn: hand the chat
    // approver to the delegate tool so `/<name>` edits prompt (and honor Shift+Tab yolo) exactly
    // like a main-model edit.
    delegate_tool.set_worker_approver(approver.clone());
    // Attach the independent writer when the config asked for it; `None` leaves the session the
    // only writer, which is the original behaviour.
    agent.set_background_saver(memory_saver.map(|saver| saver as Arc<dyn BackgroundSaver>));

    let in_flight_tools: Arc<Mutex<Vec<TurnProcessItem>>> = Arc::new(Mutex::new(Vec::new()));
    // Mid-turn messages the agent took, oldest first. The observer fills this so the turn-end
    // handler can split the turn into the same records a resumed transcript would replay.
    let interruptions: Arc<Mutex<Vec<turn::InterruptNotice>>> = Arc::new(Mutex::new(Vec::new()));
    // True only while a fold the *user* asked for is rewriting the session in place. The input
    // path reads it to decide between queueing a message and interrupting a turn: a turn started
    // then would be built on history that is about to change under it.
    //
    // An automatic fold no longer sets it. Its summary lands while the turn that tripped it runs,
    // and blocking the composer for the length of a summarising call is what made auto-compact
    // feel like a stall — the agent has a transcript to work from either way.
    let compacting = Arc::new(AtomicBool::new(false));

    // Lifecycle hooks: opt-in, loaded from the user file and (only when trusted) the project
    // file. Loading is fail-closed, so a missing file means an empty runner, not an error.
    let hooks = Arc::new(harness_core::HookRunner::load(
        &config.paths.workspace_root,
        &config.hooks,
    )?);
    let mut agent = agent
        .with_hooks(hooks.clone())
        .with_checkpoints(checkpoints)
        .with_observer(agent_observer(
            ui,
            in_flight_tools.clone(),
            interruptions.clone(),
            session.todo_scope.clone(),
            compacting.clone(),
        ));
    if let Some(setup) = compaction {
        agent = agent.with_compaction(setup);
    }

    let steer_handle = agent.steer_handle();
    let interrupt_handle = agent.interrupt_handle();
    let agent = Arc::new(tokio::sync::Mutex::new(agent));

    Ok(AgentHandles {
        agent,
        steer_handle,
        interrupt_handle,
        in_flight_tools,
        interruptions,
        compacting,
    })
}

fn wire_delegate_notifications(
    delegate_tool: &Arc<harness_core::tools::DelegateTaskTool>,
    ui: &Arc<ChatUi>,
    store: &Arc<SqliteSessionStore>,
    session_id: &str,
) {
    let delegate_trackers: Arc<
        std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, String)>>,
    > = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    delegate_tool.set_event_notifier(delegate_event_notifier(
        ui.clone(),
        delegate_trackers.clone(),
    ));
    delegate_tool.set_notifier(delegate_completion_notifier(
        ui.clone(),
        delegate_trackers,
        store.clone(),
        session_id.to_string(),
    ));
}

/// Resolve the session, provider and agent, and return the runtime both loops share.
pub(super) async fn prepare(
    mut config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    selection: SessionSelection,
    explicit_model: bool,
) -> Result<ChatRuntime, HarnessError> {
    let store = Arc::new(open_store(&config)?);
    // One recorder for the session: the ledger handle is process-wide, and the prices are resolved
    // from the config once rather than per turn.
    let usage = std::sync::Arc::new(crate::commands::usage::UsageRecorder::from_config(&config));
    let session =
        prepare_session(&mut config, &store, &discovery, selection, explicit_model).await?;

    let ProviderSetup {
        provider,
        compaction,
    } = build_provider_setup(&config, &cred_manager, &discovery, &session)?;

    let (tooling, tool_setup) = build_tooling(
        &config,
        &cred_manager,
        &discovery,
        &session,
        &provider,
        &store,
    )
    .await?;

    let is_attended = detect_attended_terminal();
    let job_rx = start_job_notifications(&tool_setup.bg_queue, is_attended);
    let ui = Arc::new(ChatUi::new(is_attended));
    let approval = build_approval_state(&config, is_attended);

    let (clarify_tx, clarify_rx) = register_clarify_tool(&tooling.registry).await?;
    install_ui_bridge(
        &ui,
        &clarify_tx,
        &approval.approval_tx,
        &approval.allow_all_session,
    );

    let handles = assemble_agent(
        &config,
        &session,
        provider,
        tooling.memory,
        tooling.notes,
        tooling.registry,
        approval.approver,
        &tool_setup.delegate_tool,
        tooling.memory_saver,
        compaction,
        ui.clone(),
        tool_setup.checkpoints,
        store.clone(),
    )?;

    tool_setup
        .delegate_tool
        .set_steer_handle(handles.steer_handle.clone());
    wire_delegate_notifications(&tool_setup.delegate_tool, &ui, &store, &session.session_id);
    emit_session_start_hook(&handles.agent).await;

    Ok(ChatRuntime {
        config,
        cred_manager,
        discovery,
        store,
        usage,
        session_id: session.session_id,
        todo_scope: session.todo_scope,
        current_provider: session.current_provider,
        current_model: session.current_model,
        compact_provider: session.compact_provider,
        compact_model: session.compact_model,
        feature_toggles: tooling.feature_toggles,
        skills_arc: tool_setup.skills_arc,
        skills_readout: tool_setup.skills_readout,
        bg_queue: tool_setup.bg_queue,
        delegate_tool: tool_setup.delegate_tool,
        is_attended,
        ui,
        approval_rx: approval.approval_rx,
        sudo_rx: tool_setup.sudo_rx,
        clarify_rx,
        job_rx,
        allow_all_session: approval.allow_all_session,
        plan_mode: approval.plan_mode,
        agent: handles.agent,
        steer_handle: handles.steer_handle,
        interrupt_handle: handles.interrupt_handle,
        in_flight_tools: handles.in_flight_tools,
        interruptions: handles.interruptions,
        compacting: handles.compacting,
    })
}

/// Whether the terminal can draw an interactive chat (console says so, or we are in kitty).
fn detect_attended_terminal() -> bool {
    let in_kitty = std::env::var("KITTY_WINDOW_ID").is_ok();
    Term::stdout().features().is_attended() || in_kitty
}

/// Fire the session-start hook, warning only if it refuses the run.
async fn emit_session_start_hook(agent: &Arc<tokio::sync::Mutex<ChatAgent>>) {
    let ag = agent.lock().await;
    if let Err(error) = ag
        .emit_hook_event(harness_core::HookEvent::SessionStart, None)
        .await
    {
        tracing::warn!("session_start hook refused the run: {}", error);
    }
}

/// Fill the tool registry and build the shared task queue, delegate tool and checkpoints.
#[allow(clippy::too_many_arguments)]
async fn register_tools(
    config: &Config,
    registry: &Arc<DefaultToolRegistry>,
    memory: &Arc<MemoryEngineKind>,
    notes: &Arc<FileNotesEngine>,
    background_mode: bool,
    memory_saver_present: bool,
    feature_toggles: &FeatureToggles,
    provider: &Arc<GenericProviderClient>,
    store: &Arc<SqliteSessionStore>,
    todo_scope: &TodoScope,
) -> Result<ToolSetup, HarnessError> {
    register_memory_and_note_tools(
        config,
        registry,
        memory,
        notes,
        background_mode,
        memory_saver_present,
        feature_toggles,
    )
    .await?;

    let (skills_arc, skills_readout) = load_skills_and_readout(config);
    register_skill_and_desktop_tools(registry, &skills_arc).await?;

    let (bg_queue, delegate_tool, sudo_rx) =
        register_task_and_core_tools(config, registry, provider, memory, notes, store, todo_scope)
            .await?;

    let checkpoints = register_checkpoint_middleware(config, registry).await;

    Ok(ToolSetup {
        bg_queue,
        delegate_tool,
        checkpoints,
        sudo_rx,
        skills_arc,
        skills_readout,
    })
}

/// Register native memory tools.
///
/// In background saver mode, all memory/note modification tools are withheld from the main agent
/// because the background saver owns memory creation, updating, and persistence completely.
/// In session writer mode, the main agent is the primary writer and receives the full toolset.
#[allow(clippy::too_many_arguments)]
async fn register_memory_and_note_tools(
    config: &Config,
    registry: &Arc<DefaultToolRegistry>,
    memory: &Arc<MemoryEngineKind>,
    notes: &Arc<FileNotesEngine>,
    background_mode: bool,
    memory_saver_present: bool,
    feature_toggles: &FeatureToggles,
) -> Result<(), HarnessError> {
    if !background_mode {
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
    }

    // Read-only note tools are always available so the agent can reference existing knowledge:
    registry
        .register(Arc::new(SearchNotesTool::new(notes.clone())))
        .await?;
    registry
        .register(Arc::new(ListNotesTool::new(notes.clone())))
        .await?;
    registry
        .register(Arc::new(ReadNoteTool::new(notes.clone())))
        .await?;
    // Recall is read-only and available in both writer modes.
    registry
        .register(Arc::new(
            MemorySearchTool::new(memory.clone())
                .with_notes(notes.clone())
                .with_notes_gate(feature_toggles.notes_atomic()),
        ))
        .await?;

    // One line of notice, because a mode that changes *where* records come from is worth stating.
    if memory_saver_present {
        if let Some(line) = memory_writer::notice(config) {
            eprintln!("{line}");
        }
    }
    Ok(())
}

fn load_skills_and_readout(
    config: &Config,
) -> (Option<Arc<Vec<harness_plugins::Skill>>>, Option<String>) {
    // Load skills from skills_dir and workspace .harness/skills
    let mut all_skills = Vec::new();
    let _ = std::fs::create_dir_all(&config.paths.skills_dir);
    if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&config.paths.skills_dir)
    {
        all_skills.extend(skills);
    }
    let workspace_skills = config.paths.workspace_root.join(".harness/skills");
    if workspace_skills.exists() {
        if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&workspace_skills) {
            all_skills.extend(skills);
        }
    }
    // The skills the harness ships with, including the desktop skill.
    all_skills.extend(crate::commands::desktop::builtin_skills());

    // `/skills` prints a name-only readout: a paragraph per skill is a wall of text on a terminal,
    // and the procedure itself is `skill_view`'s job. Skills are not listed in the system prompt;
    // the model discovers them through `skills_list` and loads one only when it invokes it.
    let skills_readout = crate::commands::skills::readout(&all_skills);

    // Skills are shared with the `skill_view` / `skills_list` tools and borrowed as a slice for
    // the command surface, so `/<skill-name>` can dispatch from the slash menu.
    let skills_arc = if all_skills.is_empty() {
        None
    } else {
        Some(Arc::new(all_skills))
    };
    (skills_arc, skills_readout)
}

async fn register_skill_and_desktop_tools(
    registry: &Arc<DefaultToolRegistry>,
    skills_arc: &Option<Arc<Vec<harness_plugins::Skill>>>,
) -> Result<(), HarnessError> {
    if let Some(skills_arc) = skills_arc {
        registry
            .register(Arc::new(harness_plugins::SkillViewTool::new(
                skills_arc.clone(),
            )))
            .await?;
        registry
            .register(Arc::new(harness_plugins::SkillsListTool::new(
                skills_arc.clone(),
            )))
            .await?;
    }
    // The desktop tools, and the skill that documents them.
    crate::commands::desktop::tools::register(registry.as_ref()).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn register_task_and_core_tools(
    config: &Config,
    registry: &Arc<DefaultToolRegistry>,
    provider: &Arc<GenericProviderClient>,
    memory: &Arc<MemoryEngineKind>,
    notes: &Arc<FileNotesEngine>,
    store: &Arc<SqliteSessionStore>,
    todo_scope: &TodoScope,
) -> Result<
    (
        Arc<harness_core::tasks::BackgroundTaskQueue>,
        Arc<harness_core::tools::DelegateTaskTool>,
        mpsc::UnboundedReceiver<(String, oneshot::Sender<Option<String>>)>,
    ),
    HarnessError,
> {
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

    // Register core filesystem and shell tools (§2.2)
    let sudo_cache = Arc::new(std::sync::Mutex::new(std::env::var("SUDO_PASSWORD").ok()));
    let (sudo_tx, sudo_rx) = mpsc::unbounded_channel::<(String, oneshot::Sender<Option<String>>)>();
    let sudo_prompter = Arc::new(ChatSudoPrompter::new(sudo_tx));

    let read_cache = harness_core::tools::ReadCache::new();
    let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
    // `terminal` and `process` must observe the same background-job registry as `/jobs` and the
    // task viewer — hand the registration the queue this host already holds instead of letting it
    // create a second one that only the tools would see.
    harness_core::tools::register_core_tools_shared_queue(
        &reg_dyn,
        config,
        read_cache,
        Some(sudo_cache),
        Some(sudo_prompter),
        bg_queue.clone(),
        todo_scope.clone(),
    )
    .await?;

    // Register MCP servers from config
    register_mcp_servers(registry, config).await?;

    Ok((bg_queue, delegate_tool, sudo_rx))
}

async fn register_checkpoint_middleware(
    config: &Config,
    registry: &Arc<DefaultToolRegistry>,
) -> Arc<CheckpointController> {
    // Per-turn file snapshots: registered as registry middleware so the copy happens immediately
    // before the write, not somewhere near it.
    let checkpoints = Arc::new(harness_core::CheckpointController::new(
        config.paths.checkpoints_dir.clone(),
        config.paths.workspace_root.clone(),
        &config.checkpoints,
    ));
    registry
        .register_middleware(Arc::new(harness_core::CheckpointMiddleware::new(
            checkpoints.clone(),
        )))
        .await;
    checkpoints
}
