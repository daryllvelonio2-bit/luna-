//! Shared embedded backend state for the optional Luna Tauri desktop app.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::Utc;
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use harness_app::events::{from_subagent_event, AppEvent};
use harness_app::runtime::{AppRuntime, AppSessionOptions, ExtraToolRegistrar};
use harness_app::session::{create_session, open_store, SessionSelection};
use harness_core::{
    ApprovalDecision, ClarifyResponse, Config, DefaultToolRegistry, HarnessError, SessionStore,
    SqliteSessionStore, ToolRegistry,
};
use harness_plugins::{PluginAgentEntry, PluginManager};
use harness_providers::{CredentialManager, ModelDiscovery};
use harness_scheduler::{
    ensure_scheduler_plugin, register_schedule_tools, DeliveryReceipt, DeliveryTarget,
    ScheduleDeliveryHandler, ScheduleStore, SchedulerService,
};

pub struct ActiveSessionHandle {
    pub session_id: String,
    pub plugin_agent_id: Option<String>,
    pub runtime: Arc<tokio::sync::Mutex<AppRuntime>>,
    pub active_cancel: Mutex<Option<CancellationToken>>,
    pub turn_running: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
pub struct PendingInteractions {
    pub approvals: HashMap<String, oneshot::Sender<ApprovalDecision>>,
    pub clarifies: HashMap<String, oneshot::Sender<ClarifyResponse>>,
    pub sudos: HashMap<String, oneshot::Sender<Option<String>>>,
}

pub struct GuiState {
    pub config: RwLock<Config>,
    pub store: Arc<SqliteSessionStore>,
    pub cred_manager: Arc<CredentialManager>,
    pub discovery: Arc<ModelDiscovery>,
    pub plugin_manager: Arc<PluginManager>,
    pub schedule_store: Arc<ScheduleStore>,
    pub scheduler: Arc<SchedulerService>,
    pub sessions: tokio::sync::Mutex<HashMap<String, Arc<ActiveSessionHandle>>>,
    pub session_agents: RwLock<BTreeMap<String, String>>,
    pub pending: Arc<Mutex<PendingInteractions>>,
    pub app_handle: RwLock<Option<AppHandle>>,
    pub shutdown_token: CancellationToken,
}

impl GuiState {
    pub async fn initialize() -> Result<Arc<Self>, HarnessError> {
        let layers = harness_core::config::process_layers(Config::default())?;
        let config = harness_core::config::resolve(&layers)?.into_config();
        let store = Arc::new(open_store(&config)?);
        let cred_manager = CredentialManager::new(Some(config.paths.credentials.clone()))?;
        cred_manager.load_from_storage().await?;

        let discovery = Arc::new(ModelDiscovery::new(
            Some(config.paths.models_cache.clone()),
            24,
            config.providers.clone(),
        ));

        let ws_plugins = config.paths.workspace_root.join(".luna/plugins");
        let plugin_manager = Arc::new(PluginManager::new(
            config.paths.plugins_dir.clone(),
            Some(ws_plugins),
        ));
        if let Err(err) = ensure_scheduler_plugin(&plugin_manager) {
            tracing::warn!("failed to ensure builtin scheduler plugin: {}", err);
        }
        if let Err(err) = plugin_manager.discover_and_load() {
            tracing::warn!("failed to load plugins: {}", err);
        }

        let schedule_store = Arc::new(
            ScheduleStore::open(Some(&config.paths.schedules_db))
                .map_err(HarnessError::ConfigurationError)?,
        );
        let scheduler = Arc::new(SchedulerService::new(schedule_store.clone()));
        let decls = plugin_manager.schedule_declarations();
        if let Err(err) = scheduler.sync_plugin_declarations(&decls) {
            tracing::warn!("failed to sync plugin schedules: {}", err);
        }

        let session_agents = load_session_agents(&session_agents_path(&config));

        Ok(Arc::new(Self {
            config: RwLock::new(config),
            store,
            cred_manager,
            discovery,
            plugin_manager,
            schedule_store,
            scheduler,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            session_agents: RwLock::new(session_agents),
            pending: Arc::new(Mutex::new(PendingInteractions::default())),
            app_handle: RwLock::new(None),
            shutdown_token: CancellationToken::new(),
        }))
    }

    pub fn attach_app(self: &Arc<Self>, app: AppHandle) {
        *self.app_handle.write().unwrap_or_else(|e| e.into_inner()) = Some(app);
        self.start_scheduler_loop();
    }

    pub fn emit_event<S: serde::Serialize + Clone>(&self, channel: &str, payload: S) {
        if let Some(app) = self
            .app_handle
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = app.emit(channel, payload);
        }
    }

    pub fn emit_app_event(&self, event: &AppEvent) {
        self.emit_event("luna://event", event);
    }

    pub fn bind_session_agent(&self, session_id: &str, plugin_agent_id: Option<&str>) {
        let cfg = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        let mut map = self
            .session_agents
            .write()
            .unwrap_or_else(|e| e.into_inner());
        match plugin_agent_id {
            Some(pid) if !pid.trim().is_empty() => {
                map.insert(session_id.to_string(), pid.to_string());
            }
            _ => {
                map.remove(session_id);
            }
        }
        save_session_agents(&session_agents_path(&cfg), &map);
    }

    pub fn get_session_agent(&self, session_id: &str) -> Option<String> {
        self.session_agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
    }

    pub fn find_plugin_agent(&self, plugin_id: &str) -> Option<PluginAgentEntry> {
        self.plugin_manager
            .sidebar_agents()
            .into_iter()
            .find(|a| a.plugin_id == plugin_id || a.id == plugin_id)
    }

    /// Open or reuse a live interactive session runtime wired to GUI events and plugin/scheduler tools.
    pub async fn get_or_open_session(
        self: &Arc<Self>,
        session_id: &str,
        plugin_agent_override: Option<String>,
    ) -> Result<Arc<ActiveSessionHandle>, String> {
        if let Some(existing) = self.sessions.lock().await.get(session_id).cloned() {
            return Ok(existing);
        }

        let plugin_agent_id = plugin_agent_override.or_else(|| self.get_session_agent(session_id));
        if let Some(pid) = &plugin_agent_id {
            self.bind_session_agent(session_id, Some(pid));
        }

        let cfg = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        let plugin_agent = plugin_agent_id
            .as_deref()
            .and_then(|pid| self.find_plugin_agent(pid));

        let system_prompt_prefix = plugin_agent.as_ref().map(|a| {
            format!(
                "Active Plugin Agent: {} ({})\nCurrent UTC time: {}\n{}\n",
                a.name,
                a.plugin_id,
                Utc::now().to_rfc3339(),
                a.system_prompt
            )
        });

        let pm = self.plugin_manager.clone();
        let sched_store = self.schedule_store.clone();
        let sid_for_tools = session_id.to_string();
        let extra_tools: ExtraToolRegistrar = Arc::new(move |registry: Arc<DefaultToolRegistry>| {
            let pm = pm.clone();
            let sched_store = sched_store.clone();
            let sid = sid_for_tools.clone();
            Box::pin(async move {
                for tool in pm.registered_tools() {
                    registry.register(tool).await?;
                }
                let reg_dyn: Arc<dyn ToolRegistry> = registry;
                register_schedule_tools(&reg_dyn, sched_store, Some(sid), true).await?;
                Ok(())
            })
        });

        let state_for_obs = self.clone();
        let sid_for_obs = session_id.to_string();
        let observer: harness_core::AgentObserver = Arc::new(move |ev| {
            let app_ev = harness_app::events::from_agent_event(&sid_for_obs, ev);
            state_for_obs.emit_app_event(&app_ev);
        });

        let options = AppSessionOptions {
            interactive: true,
            explicit_model: false,
            system_prompt_prefix,
            extra_tools: Some(extra_tools),
        };

        let mut runtime = AppRuntime::prepare(
            cfg,
            self.cred_manager.clone(),
            self.discovery.clone(),
            SessionSelection::Resume(session_id.to_string()),
            options,
            Some(observer),
        )
        .await
        .map_err(|e| e.to_string())?;

        // Wire subagent live events to GUI event stream
        let state_for_sub = self.clone();
        let sid_for_sub = session_id.to_string();
        runtime
            .delegate_tool
            .set_event_notifier(Arc::new(move |ev| {
                let app_ev = from_subagent_event(&sid_for_sub, ev);
                state_for_sub.emit_app_event(&app_ev);
            }));

        // Take interactive channels out before wrapping runtime in Mutex
        let mut approval_rx = std::mem::replace(
            &mut runtime.channels.approval_rx,
            tokio::sync::mpsc::unbounded_channel().1,
        );
        let mut clarify_rx = std::mem::replace(
            &mut runtime.channels.clarify_rx,
            tokio::sync::mpsc::unbounded_channel().1,
        );
        let mut sudo_rx = std::mem::replace(
            &mut runtime.channels.sudo_rx,
            tokio::sync::mpsc::unbounded_channel().1,
        );
        let mut job_rx = std::mem::replace(
            &mut runtime.channels.job_rx,
            tokio::sync::mpsc::unbounded_channel().1,
        );

        let runtime_arc = Arc::new(tokio::sync::Mutex::new(runtime));
        let handle = Arc::new(ActiveSessionHandle {
            session_id: session_id.to_string(),
            plugin_agent_id,
            runtime: runtime_arc,
            active_cancel: Mutex::new(None),
            turn_running: std::sync::atomic::AtomicBool::new(false),
        });

        // Forward approval / clarify / sudo / job completion prompts to the Tauri webview
        {
            let st = self.clone();
            let sid = session_id.to_string();
            let cancel = self.shutdown_token.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Some((req, reply)) = approval_rx.recv() => {
                            let req_id = format!("apr_{}", uuid::Uuid::new_v4().simple());
                            st.pending.lock().unwrap_or_else(|e| e.into_inner()).approvals.insert(req_id.clone(), reply);
                            st.emit_app_event(&AppEvent::ApprovalRequested {
                                request_id: req_id,
                                session_id: sid.clone(),
                                request: req,
                            });
                        }
                        Some((req, reply)) = clarify_rx.recv() => {
                            let req_id = format!("clr_{}", uuid::Uuid::new_v4().simple());
                            st.pending.lock().unwrap_or_else(|e| e.into_inner()).clarifies.insert(req_id.clone(), reply);
                            st.emit_app_event(&AppEvent::ClarifyRequested {
                                request_id: req_id,
                                session_id: sid.clone(),
                                request: req,
                            });
                        }
                        Some((cmd, reply)) = sudo_rx.recv() => {
                            let req_id = format!("sud_{}", uuid::Uuid::new_v4().simple());
                            st.pending.lock().unwrap_or_else(|e| e.into_inner()).sudos.insert(req_id.clone(), reply);
                            st.emit_app_event(&AppEvent::SudoRequested {
                                request_id: req_id,
                                session_id: sid.clone(),
                                command: cmd,
                            });
                        }
                        Some(job) = job_rx.recv() => {
                            st.emit_app_event(&AppEvent::JobCompleted {
                                session_id: sid.clone(),
                                job_id: job.id,
                                command: job.command,
                                status: job.status,
                                elapsed_secs: job.elapsed_secs,
                            });
                        }
                        else => break,
                    }
                }
            });
        }

        self.sessions
            .lock()
            .await
            .insert(session_id.to_string(), handle.clone());
        Ok(handle)
    }

    /// Refresh tools in already-open sessions after plugins are installed/toggled/reloaded.
    pub async fn sync_plugin_tools_to_open_sessions(&self) {
        let plugin_tools = self.plugin_manager.registered_tools();
        let handles: Vec<Arc<ActiveSessionHandle>> = {
            self.sessions.lock().await.values().cloned().collect()
        };
        for h in handles {
            let rt = h.runtime.lock().await;
            for tool in &plugin_tools {
                let _ = rt.registry.register(tool.clone()).await;
            }
        }
        let decls = self.plugin_manager.schedule_declarations();
        let _ = self.scheduler.sync_plugin_declarations(&decls);
        self.emit_event("luna://plugins-updated", ());
    }

    pub fn build_schedule_handler(self: &Arc<Self>) -> ScheduleDeliveryHandler {
        let st = self.clone();
        Arc::new(move |task| {
            let st = st.clone();
            Box::pin(async move {
                let target_sid = match &task.delivery_target {
                    DeliveryTarget::ExistingSession { session_id } => {
                        let _ = st.store.ensure_session(session_id).await;
                        session_id.clone()
                    }
                    DeliveryTarget::SingleNewSession {
                        session_id: Some(existing),
                    } => {
                        let _ = st.store.ensure_session(existing).await;
                        existing.clone()
                    }
                    DeliveryTarget::SingleNewSession { session_id: None }
                    | DeliveryTarget::NewSessionEachRun => {
                        let id = create_session(&st.store)
                            .await
                            .map_err(|e| e.to_string())?;
                        let _ = st
                            .store
                            .set_name(&id, &format!("Schedule: {}", task.name))
                            .await;
                        id
                    }
                };

                let cfg = st.config.read().unwrap_or_else(|e| e.into_inner()).clone();
                let pm = st.plugin_manager.clone();
                let sched_store = st.schedule_store.clone();
                let sid_copy = target_sid.clone();
                let extra_tools: ExtraToolRegistrar = Arc::new(move |reg| {
                    let pm = pm.clone();
                    let sched_store = sched_store.clone();
                    let sid = sid_copy.clone();
                    Box::pin(async move {
                        for tool in pm.registered_tools() {
                            reg.register(tool).await?;
                        }
                        let reg_dyn: Arc<dyn ToolRegistry> = reg;
                        register_schedule_tools(&reg_dyn, sched_store, Some(sid), true).await?;
                        Ok(())
                    })
                });

                let runtime = AppRuntime::prepare(
                    cfg,
                    st.cred_manager.clone(),
                    st.discovery.clone(),
                    SessionSelection::Resume(target_sid.clone()),
                    AppSessionOptions {
                        interactive: false,
                        explicit_model: false,
                        system_prompt_prefix: Some(format!(
                            "Triggered by scheduled task '{}' ({}).",
                            task.name, task.id
                        )),
                        extra_tools: Some(extra_tools),
                    },
                    None,
                )
                .await
                .map_err(|e| e.to_string())?;

                let outcome = {
                    let ag = runtime.agent.lock().await;
                    ag.run_turn(&task.instruction, CancellationToken::new())
                        .await
                        .map_err(|e| e.to_string())?
                };
                st.emit_event("luna://sessions-updated", ());
                st.emit_event("luna://schedules-updated", ());

                Ok(DeliveryReceipt {
                    delivered_at: Utc::now(),
                    session_id: target_sid,
                    status: "delivered".to_string(),
                    summary: outcome.reply.lines().next().unwrap_or("completed").to_string(),
                })
            })
        })
    }

    fn start_scheduler_loop(self: &Arc<Self>) {
        let handler = self.build_schedule_handler();
        let _ = self.scheduler.clone().spawn_loop(
            handler,
            Duration::from_secs(3),
            self.shutdown_token.clone(),
        );
    }
}

fn session_agents_path(config: &Config) -> PathBuf {
    config
        .paths
        .sessions_db
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("gui_session_agents.json")
}

fn load_session_agents(path: &PathBuf) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_session_agents(path: &PathBuf, map: &BTreeMap<String, String>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(path, json);
    }
}
