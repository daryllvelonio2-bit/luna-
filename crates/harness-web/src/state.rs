use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::{broadcast, oneshot, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use harness_app::events::{from_agent_event, AppEvent};
use harness_app::runtime::{AppRuntime, AppSessionOptions};
use harness_app::session::{open_store, SessionSelection};
use harness_core::config::Config;
use harness_core::session::SqliteSessionStore;
use harness_core::{ApprovalDecision, ClarifyResponse, HarnessError};
use harness_plugins::PluginManager;
use harness_providers::{CredentialManager, ModelDiscovery};
use harness_scheduler::{ensure_scheduler_plugin, ScheduleStore, SchedulerService};

use crate::auth::WebAuthState;

pub struct ActiveSessionHandle {
    pub runtime: Arc<Mutex<AppRuntime>>,
    pub turn_running: Arc<AtomicBool>,
    pub active_cancel: std::sync::Mutex<Option<CancellationToken>>,
    pub pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>>,
    pub pending_clarifies: Arc<Mutex<HashMap<String, oneshot::Sender<ClarifyResponse>>>>,
}

pub struct WebState {
    pub config: RwLock<Config>,
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
        let plugin_manager = Arc::new(PluginManager::new(plugins_dir, Some(ws_plugins)));
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

        let (event_tx, _) = broadcast::channel(512);

        Ok(Arc::new(Self {
            config: RwLock::new(config),
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
        let mut map = self.sessions.lock().await;
        if let Some(handle) = map.get(session_id) {
            return Ok(handle.clone());
        }

        let agent_entry = if let Some(agent_id) = &plugin_agent_id {
            self.plugin_manager
                .sidebar_agents()
                .into_iter()
                .find(|a| a.id == *agent_id)
        } else {
            None
        };

        let options = AppSessionOptions {
            interactive: true,
            explicit_model: false,
            system_prompt_prefix: agent_entry.as_ref().map(|a| a.system_prompt.clone()),
            extra_tools: None,
        };

        let event_tx = self.event_tx.clone();
        let sid = session_id.to_string();
        let observer: harness_core::AgentObserver = Arc::new(move |event| {
            let app_evt = from_agent_event(&sid, event);
            let _ = event_tx.send(app_evt);
        });

        let cfg_snapshot = self.config.read().await.clone();
        let mut runtime = AppRuntime::prepare(
            cfg_snapshot,
            self.cred_manager.clone(),
            self.discovery.clone(),
            SessionSelection::Resume(session_id.to_string()),
            options,
            Some(observer),
        )
        .await?;

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
        });

        map.insert(session_id.to_string(), handle.clone());
        if let Some(agent_id) = plugin_agent_id {
            self.session_agents
                .write()
                .await
                .insert(session_id.to_string(), agent_id);
        }

        Ok(handle)
    }
}
