use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::{broadcast, oneshot, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use harness_app::events::{from_agent_event, from_subagent_event, AppEvent};
use harness_app::runtime::{AppRuntime, AppSessionOptions, ExtraToolRegistrar};
use harness_app::session::{create_session as create_store_session, open_store, SessionSelection};
use harness_core::config::Config;
use harness_core::session::{SessionStore, SqliteSessionStore};
use harness_core::{ApprovalDecision, ClarifyResponse, HarnessError, ToolRegistry};
use harness_plugins::{
    plugin_skill, register_plugin_tools, PluginManager, SkillLoader, SkillViewTool, SkillsListTool,
    PLUGIN_SKILL_NAME,
};
use harness_providers::{CredentialManager, ModelDiscovery};
use harness_scheduler::{
    automation_skill, ensure_scheduler_plugin, register_schedule_tools, DeliveryReceipt,
    DeliveryTarget, ScheduleDeliveryHandler, ScheduleStore, ScheduleTask, SchedulerService,
    AUTOMATION_SKILL_NAME,
};

use crate::auth::WebAuthState;

#[derive(Clone, Debug, serde::Serialize)]
pub struct WebGoalStatus {
    pub objective: String,
    pub iteration: usize,
    pub max_iterations: usize,
    pub running: bool,
}

pub struct ActiveGoalHandle {
    pub status: WebGoalStatus,
    pub cancel: CancellationToken,
}

pub struct ActiveSessionHandle {
    pub runtime: Arc<Mutex<AppRuntime>>,
    pub turn_running: Arc<AtomicBool>,
    pub active_cancel: std::sync::Mutex<Option<CancellationToken>>,
    pub pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>>,
    pub pending_clarifies: Arc<Mutex<HashMap<String, oneshot::Sender<ClarifyResponse>>>>,
    pub queued_messages: Arc<Mutex<VecDeque<String>>>,
    pub active_goal: Arc<Mutex<Option<ActiveGoalHandle>>>,
}

pub struct WebState {
    pub config: RwLock<Config>,
    pub launch_workspace_root: PathBuf,
    pub store: Arc<SqliteSessionStore>,
    pub cred_manager: Arc<CredentialManager>,
    pub discovery: Arc<ModelDiscovery>,
    pub plugin_manager: Arc<PluginManager>,
    pub scheduler: Arc<SchedulerService>,
    pub auth: Arc<WebAuthState>,
    pub sessions: Mutex<HashMap<String, Arc<ActiveSessionHandle>>>,
    pub session_agents: RwLock<BTreeMap<String, String>>,
    pub event_tx: broadcast::Sender<AppEvent>,
    pub shutdown_token: CancellationToken,
}

impl WebState {
    pub async fn new(
        config: Config,
        bound_host: String,
        trusted_hosts: Vec<String>,
        static_token: Option<String>,
    ) -> Result<Arc<Self>, HarnessError> {
        let store = Arc::new(open_store(&config)?);
        let cred_manager = CredentialManager::new(Some(config.paths.credentials.clone()))?;

        let discovery = Arc::new(ModelDiscovery::new(
            Some(config.paths.models_cache.clone()),
            24,
            config.providers.clone(),
        ));

        let luna_dir = config
            .paths
            .sessions_db
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let plugins_dir = luna_dir.join("plugins");
        let ws_plugins = config.paths.workspace_root.join(".luna/plugins");
        let plugin_manager = Arc::new(
            PluginManager::new(plugins_dir, Some(ws_plugins))
                .with_workspace_root(config.paths.workspace_root.clone()),
        );
        if let Err(err) = ensure_scheduler_plugin(&plugin_manager) {
            tracing::warn!("failed to ensure builtin scheduler plugin: {}", err);
        }
        if let Err(err) = plugin_manager.discover_and_load() {
            tracing::warn!("failed to discover plugins: {}", err);
        }

        let schedules_db = luna_dir.join("schedules.db");
        let sched_store = Arc::new(
            ScheduleStore::open(Some(&schedules_db))
                .map_err(|e| HarnessError::Io(format!("failed to open schedules store: {}", e)))?,
        );
        let scheduler = Arc::new(SchedulerService::new(sched_store));
        if let Err(err) =
            scheduler.sync_plugin_declarations(&plugin_manager.schedule_declarations())
        {
            tracing::warn!("failed to sync plugin schedules: {}", err);
        }

        let (event_tx, _) = broadcast::channel(512);
        let launch_workspace_root = config.paths.workspace_root.clone();

        Ok(Arc::new(Self {
            config: RwLock::new(config),
            launch_workspace_root,
            store,
            cred_manager,
            discovery,
            plugin_manager,
            scheduler,
            auth: Arc::new(WebAuthState::new(bound_host, trusted_hosts, static_token)),
            sessions: Mutex::new(HashMap::new()),
            session_agents: RwLock::new(BTreeMap::new()),
            event_tx,
            shutdown_token: CancellationToken::new(),
        }))
    }

    pub fn emit_event(&self, event: AppEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Remove cached session runtimes that are not currently running a turn so they
    /// pick up updated workspace root or default model configuration on next turn.
    pub async fn invalidate_idle_sessions(&self) {
        let mut map = self.sessions.lock().await;
        map.retain(|_, handle| {
            handle
                .turn_running
                .load(std::sync::atomic::Ordering::SeqCst)
        });
    }

    /// Re-sync plugin-declared schedules and drop idle cached sessions so newly created,
    /// updated, toggled, or deleted plugins take effect immediately across chat sessions.
    pub async fn sync_plugins_after_change(&self) {
        let _ = self
            .scheduler
            .sync_plugin_declarations(&self.plugin_manager.schedule_declarations());
        self.invalidate_idle_sessions().await;
    }

    pub async fn drop_session_if_idle(&self, session_id: &str) {
        let mut map = self.sessions.lock().await;
        if let Some(handle) = map.get(session_id) {
            if !handle
                .turn_running
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                map.remove(session_id);
            }
        }
    }

    pub async fn get_or_open_session(
        &self,
        session_id: &str,
        plugin_agent_id: Option<String>,
    ) -> Result<Arc<ActiveSessionHandle>, HarnessError> {
        self.get_or_open_session_with_approval(session_id, plugin_agent_id, None)
            .await
    }

    pub async fn get_or_open_session_with_approval(
        &self,
        session_id: &str,
        plugin_agent_id: Option<String>,
        approval_mode_override: Option<harness_core::ApprovalMode>,
    ) -> Result<Arc<ActiveSessionHandle>, HarnessError> {
        let mut map = self.sessions.lock().await;
        if let Some(handle) = map.get(session_id) {
            return Ok(handle.clone());
        }

        let resolved_agent_id = match &plugin_agent_id {
            Some(id) => Some(id.clone()),
            None => self.session_agents.read().await.get(session_id).cloned(),
        };
        let agent_entry = if let Some(agent_id) = &resolved_agent_id {
            self.plugin_manager
                .sidebar_agents()
                .into_iter()
                .find(|a| a.id == *agent_id)
        } else {
            None
        };

        let cfg_snapshot = self.config.read().await.clone();
        let skills_dir = cfg_snapshot.paths.skills_dir.clone();
        let ws_skills_dir = cfg_snapshot.paths.workspace_root.join(".harness/skills");
        let sched_store = self.scheduler.store().clone();
        let plugin_mgr = self.plugin_manager.clone();
        let sid_for_tools = session_id.to_string();
        let agent_tool_filter: Option<Vec<String>> = agent_entry
            .as_ref()
            .filter(|a| !a.registered_tools.is_empty())
            .map(|a| a.registered_tools.clone());
        let extra_tools: Option<ExtraToolRegistrar> = Some(Arc::new(move |registry| {
            let skills_dir = skills_dir.clone();
            let ws_skills_dir = ws_skills_dir.clone();
            let sched_store = sched_store.clone();
            let plugin_mgr = plugin_mgr.clone();
            let sid_for_tools = sid_for_tools.clone();
            let agent_tool_filter = agent_tool_filter.clone();
            Box::pin(async move {
                let mut all_skills = Vec::new();
                if let Ok(skills) = SkillLoader::load_skills_from_dir(&skills_dir) {
                    all_skills.extend(skills);
                }
                if ws_skills_dir.exists() {
                    if let Ok(skills) = SkillLoader::load_skills_from_dir(&ws_skills_dir) {
                        all_skills.extend(skills);
                    }
                }
                if !all_skills
                    .iter()
                    .any(|s| harness_plugins::names_match(&s.name, AUTOMATION_SKILL_NAME))
                {
                    all_skills.push(automation_skill());
                }
                if !all_skills
                    .iter()
                    .any(|s| harness_plugins::names_match(&s.name, PLUGIN_SKILL_NAME))
                {
                    all_skills.push(plugin_skill());
                }
                if !all_skills.is_empty() {
                    let skills_arc = Arc::new(all_skills);
                    registry
                        .register(Arc::new(SkillViewTool::new(skills_arc.clone())))
                        .await?;
                    registry
                        .register(Arc::new(SkillsListTool::new(skills_arc)))
                        .await?;
                }
                let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
                register_schedule_tools(&reg_dyn, sched_store, Some(sid_for_tools), true).await?;
                register_plugin_tools(&reg_dyn, plugin_mgr.clone()).await?;
                for tool in plugin_mgr.registered_tools() {
                    if let Some(allowed) = &agent_tool_filter {
                        if !allowed.iter().any(|n| n == tool.name()) {
                            continue;
                        }
                    }
                    if registry.get(tool.name()).await.is_none() {
                        registry.register(tool).await?;
                    }
                }
                Ok(())
            })
        }));

        let options = AppSessionOptions {
            interactive: true,
            explicit_model: false,
            system_prompt_prefix: agent_entry.as_ref().map(|a| a.system_prompt.clone()),
            extra_tools,
            approval_mode_override,
        };

        let event_tx = self.event_tx.clone();
        let sid = session_id.to_string();
        let plugin_mgr_for_obs = self.plugin_manager.clone();
        let observer: harness_core::AgentObserver = Arc::new(move |event| {
            let app_evt = from_agent_event(&sid, event);
            if let AppEvent::ToolExecuted {
                session_id: ref esid,
                ref name,
                ref output,
                ..
            } = app_evt
            {
                let pm = plugin_mgr_for_obs.clone();
                let payload = serde_json::json!({
                    "sessionId": esid,
                    "name": name,
                    "output": output,
                });
                tokio::spawn(async move {
                    pm.dispatch_event("tool_executed", payload).await;
                });
            }
            let _ = event_tx.send(app_evt);
        });

        let mut runtime = AppRuntime::prepare(
            cfg_snapshot,
            self.cred_manager.clone(),
            self.discovery.clone(),
            SessionSelection::Resume(session_id.to_string()),
            options,
            Some(observer),
        )
        .await?;

        // Forward delegated subagent events onto the WebSocket stream
        {
            let sub_tx = self.event_tx.clone();
            let sub_sid = session_id.to_string();
            runtime
                .delegate_tool
                .set_event_notifier(Arc::new(move |evt| {
                    let _ = sub_tx.send(from_subagent_event(&sub_sid, evt));
                }));
        }

        // Forward background job completions onto the WebSocket stream
        let mut job_rx = std::mem::replace(
            &mut runtime.channels.job_rx,
            tokio::sync::mpsc::unbounded_channel().1,
        );
        {
            let job_tx = self.event_tx.clone();
            let job_sid = session_id.to_string();
            tokio::spawn(async move {
                while let Some(comp) = job_rx.recv().await {
                    let _ = job_tx.send(AppEvent::JobCompleted {
                        session_id: job_sid.clone(),
                        job_id: comp.id,
                        command: comp.command,
                        status: comp.status,
                        elapsed_secs: comp.elapsed_secs,
                    });
                }
            });
        }

        let pending_approvals = Arc::new(Mutex::new(HashMap::new()));
        let pending_clarifies = Arc::new(Mutex::new(HashMap::new()));

        // Spawn background task for approval requests
        let mut approval_rx = std::mem::replace(
            &mut runtime.channels.approval_rx,
            tokio::sync::mpsc::unbounded_channel().1,
        );
        let app_map = pending_approvals.clone();
        let evt_tx = self.event_tx.clone();
        let s_id = session_id.to_string();
        tokio::spawn(async move {
            while let Some((req, reply_tx)) = approval_rx.recv().await {
                let req_id = uuid::Uuid::new_v4().to_string();
                app_map.lock().await.insert(req_id.clone(), reply_tx);
                let _ = evt_tx.send(AppEvent::ApprovalRequested {
                    request_id: req_id,
                    session_id: s_id.clone(),
                    request: req,
                });
            }
        });

        // Spawn background task for clarify requests
        let mut clarify_rx = std::mem::replace(
            &mut runtime.channels.clarify_rx,
            tokio::sync::mpsc::unbounded_channel().1,
        );
        let clar_map = pending_clarifies.clone();
        let evt_tx = self.event_tx.clone();
        let s_id = session_id.to_string();
        tokio::spawn(async move {
            while let Some((req, reply_tx)) = clarify_rx.recv().await {
                let req_id = uuid::Uuid::new_v4().to_string();
                clar_map.lock().await.insert(req_id.clone(), reply_tx);
                let _ = evt_tx.send(AppEvent::ClarifyRequested {
                    request_id: req_id,
                    session_id: s_id.clone(),
                    request: req,
                });
            }
        });

        let handle = Arc::new(ActiveSessionHandle {
            runtime: Arc::new(Mutex::new(runtime)),
            turn_running: Arc::new(AtomicBool::new(false)),
            active_cancel: std::sync::Mutex::new(None),
            pending_approvals,
            pending_clarifies,
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            active_goal: Arc::new(Mutex::new(None)),
        });

        map.insert(session_id.to_string(), handle.clone());
        if let Some(agent_id) = plugin_agent_id {
            self.session_agents
                .write()
                .await
                .insert(session_id.to_string(), agent_id);
        }

        let pm = self.plugin_manager.clone();
        let sid_evt = session_id.to_string();
        tokio::spawn(async move {
            pm.dispatch_event(
                "session_started",
                serde_json::json!({ "sessionId": sid_evt }),
            )
            .await;
        });

        Ok(handle)
    }

    /// Resolve or create the target session ID for a scheduled automation run.
    pub async fn resolve_schedule_session(&self, task: &ScheduleTask) -> Result<String, String> {
        match &task.delivery_target {
            DeliveryTarget::ExistingSession { session_id } if !session_id.trim().is_empty() => {
                Ok(session_id.clone())
            }
            DeliveryTarget::SingleNewSession {
                session_id: Some(sid),
            } if !sid.trim().is_empty() => Ok(sid.clone()),
            _ => {
                let sid = create_store_session(&self.store)
                    .await
                    .map_err(|e| format!("failed to create automation session: {e}"))?;
                let title = format!("⚡ {}", task.name);
                let _ = self.store.set_name(&sid, &title).await;
                Ok(sid)
            }
        }
    }

    /// Execute a single scheduled task inside `session_id` and return a `DeliveryReceipt`.
    pub async fn execute_schedule_in_session(
        self: &Arc<Self>,
        task: &ScheduleTask,
        session_id: String,
    ) -> Result<DeliveryReceipt, String> {
        let desired_approval = if task.require_approval {
            harness_core::ApprovalMode::Ask
        } else {
            harness_core::ApprovalMode::Allow
        };
        let handle = self
            .get_or_open_session_with_approval(&session_id, None, Some(desired_approval))
            .await
            .map_err(|e| format!("failed to open automation session: {e}"))?;

        let cancel = CancellationToken::new();
        *handle
            .active_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(cancel.clone());
        handle.turn_running.store(true, Ordering::SeqCst);

        self.emit_event(AppEvent::TurnStarted {
            session_id: session_id.clone(),
            query: task.instruction.clone(),
        });
        let started = std::time::Instant::now();
        let plugin_gen_before = self.plugin_manager.generation();

        let is_existing_target =
            matches!(task.delivery_target, DeliveryTarget::ExistingSession { .. });
        let (result, provider, model) = {
            let rt = handle.runtime.lock().await;
            let prev_allow_all = rt
                .allow_all_session
                .swap(!task.require_approval, Ordering::SeqCst);
            let ag = rt.agent.lock().await;
            let res = ag.run_turn(&task.instruction, cancel).await;
            if is_existing_target {
                rt.allow_all_session.store(prev_allow_all, Ordering::SeqCst);
            }
            (res, rt.current_provider.clone(), rt.current_model.clone())
        };

        handle.turn_running.store(false, Ordering::SeqCst);
        if self.plugin_manager.generation() != plugin_gen_before {
            self.sync_plugins_after_change().await;
        }
        let elapsed = started.elapsed().as_millis() as u64;

        match result {
            Ok(turn_outcome) => {
                {
                    let rt = handle.runtime.lock().await;
                    rt.usage
                        .record_chat(&provider, &model, Some(&session_id), &turn_outcome)
                        .await;
                }
                let usage = turn_outcome.usage;
                let summary_raw = turn_outcome.reply.trim();
                let summary: String = summary_raw.chars().take(280).collect();
                self.emit_event(AppEvent::TurnFinished {
                    session_id: session_id.clone(),
                    reply: turn_outcome.reply,
                    duration_ms: elapsed,
                    prompt_tokens: usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                    completion_tokens: usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
                });
                Ok(DeliveryReceipt {
                    delivered_at: Utc::now(),
                    session_id,
                    status: "delivered".to_string(),
                    summary: if summary.is_empty() {
                        "Completed with no text output.".to_string()
                    } else {
                        summary
                    },
                })
            }
            Err(err) => {
                let err_str = err.to_string();
                self.emit_event(AppEvent::TurnFailed {
                    session_id: session_id.clone(),
                    error: err_str.clone(),
                });
                Ok(DeliveryReceipt {
                    delivered_at: Utc::now(),
                    session_id,
                    status: "failed".to_string(),
                    summary: err_str,
                })
            }
        }
    }

    /// Build the background delivery handler consumed by `SchedulerService::spawn_loop`.
    pub fn schedule_delivery_handler(self: &Arc<Self>) -> ScheduleDeliveryHandler {
        let state = Arc::clone(self);
        Arc::new(move |task: ScheduleTask| {
            let state = Arc::clone(&state);
            Box::pin(async move {
                let session_id = state.resolve_schedule_session(&task).await?;
                state.execute_schedule_in_session(&task, session_id).await
            })
        })
    }
}
