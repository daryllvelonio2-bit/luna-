use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use harness_core::{
    Agent, AgentConfig, AgentObserver, AllowAll, ApprovalSettings, DefaultToolRegistry,
    PermissionEngine, SessionBinding, SessionStore, SqliteSessionStore, ToolRegistry, TurnOutcome,
};
use harness_memory::SqliteMemoryEngine;
use harness_notes::FileNotesEngine;
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};
use harness_ui_spec::Screen;
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;

use crate::action::{ActionError, ActionHandler, ActionId, ActionRegistry};
use crate::event::EngineEvent;
use crate::prompt::DEFAULT_SYSTEM_PROMPT;
use crate::screens::UiSpecStore;
use crate::tool::{save_screen, CreateUiTool};
use crate::MobileError;

const CREDENTIALS_FILE: &str = "credentials.json";
const MODELS_CACHE_FILE: &str = "models_cache.json";
const MEMORY_DB_FILE: &str = "memory.db";
const NOTES_DIR: &str = "notes";
const SESSIONS_DB_FILE: &str = "sessions.db";
const SCREENS_DIR: &str = "screens";
const MOBILE_CREDENTIAL_LABEL: &str = "mobile";
const DEFAULT_SESSION_ID: &str = "mobile";
const MODEL_CACHE_TTL_HOURS: i64 = 24;
const EVENT_CHANNEL_CAPACITY: usize = 256;
const DEFAULT_CONTEXT_WINDOW: usize = 128_000;
const DEFAULT_RESUME_MESSAGES: usize = 50;

type MobileAgent =
    Agent<GenericProviderClient, SqliteMemoryEngine, FileNotesEngine, DefaultToolRegistry>;

#[derive(Debug, Clone)]
pub struct MobileConfig {
    pub data_dir: PathBuf,
    pub provider: String,
    pub model: String,
    pub api_key: Option<String>,
    pub session_id: String,
    pub system_prompt_prefix: Option<String>,
    pub context_window: usize,
    pub resume_max_messages: usize,
}

impl MobileConfig {
    pub fn new(
        data_dir: impl Into<PathBuf>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            data_dir: data_dir.into(),
            provider: provider.into(),
            model: model.into(),
            api_key: None,
            session_id: DEFAULT_SESSION_ID.to_string(),
            system_prompt_prefix: None,
            context_window: DEFAULT_CONTEXT_WINDOW,
            resume_max_messages: DEFAULT_RESUME_MESSAGES,
        }
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn with_system_prompt_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.system_prompt_prefix = Some(prefix.into());
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = session_id.into();
        self
    }
}

/// The UI-facing facade over a headless Luna agent. The frontend sends prompts, consumes
/// [`EngineEvent`]s, and reads generated [`Screen`]s from the store.
pub struct LunaEngine {
    agent: Mutex<MobileAgent>,
    screens: Arc<UiSpecStore>,
    events: broadcast::Sender<EngineEvent>,
    actions: ActionRegistry,
    cancel: Mutex<Option<CancellationToken>>,
}

impl LunaEngine {
    pub async fn new(config: MobileConfig) -> Result<Self, MobileError> {
        std::fs::create_dir_all(&config.data_dir)?;

        let credentials = CredentialManager::new(Some(config.data_dir.join(CREDENTIALS_FILE)))?;
        if let Some(api_key) = config.api_key.as_deref() {
            if !api_key.trim().is_empty() {
                credentials
                    .add_credential(&config.provider, MOBILE_CREDENTIAL_LABEL, api_key, true)
                    .await?;
            }
        }

        let discovery = Arc::new(ModelDiscovery::new(
            Some(config.data_dir.join(MODELS_CACHE_FILE)),
            MODEL_CACHE_TTL_HOURS,
            BTreeMap::new(),
        ));
        let provider = Arc::new(GenericProviderClient::new(
            config.provider.clone(),
            config.model.clone(),
            credentials,
            discovery,
            None,
        )?);
        let memory = Arc::new(SqliteMemoryEngine::new(Some(
            config.data_dir.join(MEMORY_DB_FILE),
        ))?);
        let notes = Arc::new(FileNotesEngine::new(config.data_dir.join(NOTES_DIR), None)?);
        let screens = Arc::new(UiSpecStore::new(Some(config.data_dir.join(SCREENS_DIR)))?);

        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let registry = Arc::new(DefaultToolRegistry::new());
        registry
            .register(Arc::new(CreateUiTool::new(screens.clone(), events.clone())))
            .await?;

        let store = Arc::new(SqliteSessionStore::new(Some(
            config.data_dir.join(SESSIONS_DB_FILE),
        ))?);
        store.ensure_session(&config.session_id).await?;
        store
            .set_model(&config.session_id, &config.provider, &config.model)
            .await?;

        let agent_config = AgentConfig {
            require_hitl_for_mutation: false,
            memory_enabled: false,
            notes_enabled: false,
            skills_enabled: false,
            mcp_enabled: false,
            workspace_root: Some(config.data_dir.clone()),
            context_window: Some(config.context_window),
            system_prompt_prefix: Some(
                config
                    .system_prompt_prefix
                    .clone()
                    .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string()),
            ),
            ..AgentConfig::default()
        };

        let observer_sender = events.clone();
        let observer: AgentObserver = Arc::new(move |event| {
            if observer_sender.send(EngineEvent::Agent(event)).is_err() {
                tracing::trace!("no engine event subscribers");
            }
        });

        let session_store: Arc<dyn SessionStore> = store;
        let agent = Agent::new(provider, memory, notes, registry, agent_config)
            .with_permissions(PermissionEngine::default())
            .with_approval(Arc::new(AllowAll), ApprovalSettings::default())
            .with_session(SessionBinding {
                store: session_store,
                session_id: config.session_id,
                resume_max_messages: config.resume_max_messages,
            })
            .with_observer(observer);

        Ok(Self {
            agent: Mutex::new(agent),
            screens,
            events,
            actions: ActionRegistry::new(),
            cancel: Mutex::new(None),
        })
    }

    pub fn register_action(
        &mut self,
        id: ActionId,
        handler: Arc<dyn ActionHandler>,
    ) -> Result<(), ActionError> {
        self.actions.register(id, handler)
    }

    pub fn actions(&self) -> &ActionRegistry {
        &self.actions
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events.subscribe()
    }

    pub fn screens(&self) -> Vec<Screen> {
        self.screens.list()
    }

    pub fn screen(&self, id: &str) -> Option<Screen> {
        self.screens.get(id)
    }

    pub fn screen_store(&self) -> Arc<UiSpecStore> {
        self.screens.clone()
    }

    /// Validate, persist, and announce a screen without going through a model turn. Useful for the
    /// UI shell's own builders and for local previews.
    pub fn create_screen(&self, screen: Screen) -> Result<Screen, MobileError> {
        save_screen(&self.screens, &self.events, screen)
    }

    pub async fn send_prompt(&self, prompt: impl Into<String>) -> Result<TurnOutcome, MobileError> {
        let prompt = prompt.into();
        let agent = self.agent.lock().await;
        let token = CancellationToken::new();
        *self.cancel.lock().await = Some(token.clone());
        let result = agent.run_turn(&prompt, token).await;
        *self.cancel.lock().await = None;
        result.map_err(MobileError::from)
    }

    pub async fn cancel(&self) {
        if let Some(token) = self.cancel.lock().await.take() {
            token.cancel();
        }
    }
}
