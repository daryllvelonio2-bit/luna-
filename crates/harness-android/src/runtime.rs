use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_core::approval::{
    AllowAll, ApprovalDecision, ApprovalRequest, ApprovalSettings, Approver,
};
use harness_core::config::{self, Config, Layers, ProviderOverride};
use harness_core::tools::{ReadCache, TodoScope};
use harness_core::{
    Agent, AgentEvent, AgentObserver, CompactionSetup, DefaultToolRegistry, HarnessError,
    MemoryEngine, NotesEngine, PermissionEngine, Role, SessionBinding, SessionStore,
    SqliteSessionStore, ToolRegistry,
};
use harness_memory::{
    build_memory_engine, ForgetMemoryTool, MemoryEngineKind, MemorySearchTool, SaveMemoryTool,
    UpdateMemoryTool,
};
use harness_notes::{
    DeleteNoteTool, FileNotesEngine, ListNotesTool, ReadNoteTool, SaveNoteTool, SearchNotesTool,
    UpdateNoteTool,
};
use harness_plugins::{SkillLoader, SkillViewTool, SkillsListTool};
use harness_providers::{
    get_default_model, get_provider_catalog, resolve_context_window, CredentialManager,
    GenericProviderClient, ModelDiscovery,
};
use serde::Serialize;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub display_name: String,
    pub default_model: String,
    pub has_key: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub title: String,
    pub updated_at: String,
    pub message_count: usize,
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitResponse {
    pub ok: bool,
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub workspace_dir: String,
    pub auto_approve: bool,
    pub context_window: usize,
    pub providers: Vec<ProviderInfo>,
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageDto {
    pub role: String,
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCallDto>,
    pub tool_result: Option<ToolResultDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallDto {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultDto {
    pub call_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDto {
    pub id: String,
    pub category: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteDto {
    pub id: String,
    pub title: String,
    pub pointer: String,
    pub content: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResponse {
    pub ok: bool,
    pub error: String,
}

pub fn error_json(err: impl ToString) -> String {
    serde_json::to_string(&ErrorResponse {
        ok: false,
        error: err.to_string(),
    })
    .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization error"}"#.to_string())
}

pub fn ok_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(error_json)
}

pub fn catalog_defaults() -> Config {
    let mut cfg = Config::default();
    for provider in get_provider_catalog() {
        cfg.providers.insert(
            provider.id,
            ProviderOverride {
                base_url: Some(provider.default_base_url),
                models_endpoint: Some(provider.models_endpoint),
                timeout_secs: Some(provider.timeout_secs),
                context_window: None,
            },
        );
    }
    cfg
}

pub trait EventSink: Send + Sync {
    fn on_event(&self, json_payload: String);
    fn request_approval(&self, request_json: String) -> i32;
}

struct MobileApprover {
    auto_approve: Arc<AtomicBool>,
    session_allow_all: Arc<AtomicBool>,
    sink: Arc<dyn EventSink>,
}

#[async_trait]
impl Approver for MobileApprover {
    async fn approve(&self, request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        if self.auto_approve.load(Ordering::SeqCst) || self.session_allow_all.load(Ordering::SeqCst)
        {
            return Ok(ApprovalDecision::Run);
        }

        let payload = serde_json::json!({
            "tool": request.tool,
            "callId": request.call_id,
            "target": request.target,
            "diff": request.diff,
            "arguments": request.arguments,
        })
        .to_string();

        let sink = self.sink.clone();
        let allow_all = self.session_allow_all.clone();
        let decision = tokio::task::spawn_blocking(move || sink.request_approval(payload))
            .await
            .map_err(|e| HarnessError::ApprovalRejected(format!("approval failed: {e}")))?;

        match decision {
            1 => Ok(ApprovalDecision::Run),
            2 => {
                allow_all.store(true, Ordering::SeqCst);
                Ok(ApprovalDecision::Run)
            }
            _ => Ok(ApprovalDecision::Deny),
        }
    }
}

struct RuntimeInner {
    files_dir: PathBuf,
    workspace_dir: PathBuf,
    config: Config,
    store: Arc<SqliteSessionStore>,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    memory: Arc<MemoryEngineKind>,
    notes: Arc<FileNotesEngine>,
    session_id: String,
    provider_id: String,
    model_id: String,
    auto_approve: Arc<AtomicBool>,
    session_allow_all: Arc<AtomicBool>,
    active_cancel: Mutex<Option<CancellationToken>>,
    active_interrupt: Mutex<Option<Arc<harness_core::InterruptHandle>>>,
}

pub struct LunaMobileRuntime {
    tokio: tokio::runtime::Runtime,
    inner: AsyncMutex<Option<RuntimeInner>>,
}

impl LunaMobileRuntime {
    pub fn new() -> Result<Self, HarnessError> {
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(4)
            .build()
            .map_err(|e| HarnessError::ConfigurationError(format!("tokio init failed: {e}")))?;
        Ok(Self {
            tokio,
            inner: AsyncMutex::new(None),
        })
    }

    pub fn init(
        &self,
        files_dir: &Path,
        workspace_dir: &Path,
    ) -> Result<InitResponse, HarnessError> {
        self.tokio
            .block_on(self.init_async(files_dir, workspace_dir))
    }

    async fn init_async(
        &self,
        files_dir: &Path,
        workspace_dir: &Path,
    ) -> Result<InitResponse, HarnessError> {
        std::fs::create_dir_all(files_dir)
            .map_err(|e| HarnessError::Io(format!("create files_dir failed: {e}")))?;
        std::fs::create_dir_all(workspace_dir)
            .map_err(|e| HarnessError::Io(format!("create workspace_dir failed: {e}")))?;
        let luna_dir = files_dir.join(".luna");
        std::fs::create_dir_all(&luna_dir)
            .map_err(|e| HarnessError::Io(format!("create .luna dir failed: {e}")))?;

        let config_path = luna_dir.join("config.yaml");
        std::env::set_var("HOME", files_dir);
        std::env::set_var("LUNA_CONFIG", &config_path);
        let _ = std::env::set_current_dir(workspace_dir);

        let config = load_mobile_config(&config_path, workspace_dir)?;
        let store = Arc::new(SqliteSessionStore::new(Some(
            config.paths.sessions_db.clone(),
        ))?);
        let cred_manager = CredentialManager::new(None)?;
        let discovery = Arc::new(ModelDiscovery::new(
            Some(config.paths.models_cache.clone()),
            24,
            config.providers.clone(),
        ));
        let memory = build_memory_engine(&config)?;
        let notes = Arc::new(FileNotesEngine::new(
            config.paths.notes_dir.clone(),
            Some(config.paths.notes_db.clone()),
        )?);

        let session_id = match store.most_recent().await? {
            Some(id) => id,
            None => create_fresh_session(&store).await?,
        };

        let (provider_id, model_id) =
            resolve_active_model(&config, &store, &cred_manager, &session_id).await?;
        let auto_approve = Arc::new(AtomicBool::new(
            config.tools.approval == harness_core::ApprovalMode::Allow,
        ));

        let inner = RuntimeInner {
            files_dir: files_dir.to_path_buf(),
            workspace_dir: workspace_dir.to_path_buf(),
            config,
            store,
            cred_manager,
            discovery,
            memory,
            notes,
            session_id,
            provider_id,
            model_id,
            auto_approve,
            session_allow_all: Arc::new(AtomicBool::new(false)),
            active_cancel: Mutex::new(None),
            active_interrupt: Mutex::new(None),
        };

        let summary = build_init_response(&inner).await?;
        *self.inner.lock().await = Some(inner);
        Ok(summary)
    }

    pub fn set_api_key(
        &self,
        provider_id: &str,
        api_key: &str,
    ) -> Result<InitResponse, HarnessError> {
        self.tokio.block_on(async {
            let mut guard = self.inner.lock().await;
            let inner = guard.as_mut().ok_or_else(not_initialized)?;
            let trimmed_provider = provider_id.trim().to_lowercase();
            let trimmed_key = api_key.trim();
            if trimmed_key.is_empty() {
                return Err(HarnessError::ConfigurationError(
                    "API key cannot be empty".to_string(),
                ));
            }
            inner
                .cred_manager
                .add_credential(&trimmed_provider, "mobile", trimmed_key, true)
                .await?;

            let default_model = get_default_model(&trimmed_provider).to_string();
            let layers = mobile_layers(
                &inner.files_dir.join(".luna").join("config.yaml"),
                &inner.workspace_dir,
            )?;
            config::set_in_file(&layers, "model.provider", &trimmed_provider)?;
            if !default_model.is_empty() {
                config::set_in_file(&layers, "model.default", &default_model)?;
            }
            inner.config = load_mobile_config(
                &inner.files_dir.join(".luna").join("config.yaml"),
                &inner.workspace_dir,
            )?;
            inner.provider_id = trimmed_provider.clone();
            inner.model_id = if default_model.is_empty() {
                inner.config.model.default.clone()
            } else {
                default_model
            };
            inner
                .store
                .set_model(&inner.session_id, &inner.provider_id, &inner.model_id)
                .await?;
            build_init_response(inner).await
        })
    }

    pub fn list_models(
        &self,
        provider_id: &str,
        refresh: bool,
    ) -> Result<Vec<String>, HarnessError> {
        self.tokio.block_on(async {
            let guard = self.inner.lock().await;
            let inner = guard.as_ref().ok_or_else(not_initialized)?;
            let pid = provider_id.trim().to_lowercase();
            match inner
                .discovery
                .get_models(&pid, &inner.cred_manager, refresh)
                .await
            {
                Ok(models) if !models.is_empty() => Ok(models),
                _ => {
                    let mut fallback = Vec::new();
                    if let Some(desc) = harness_providers::find_provider(&pid) {
                        fallback.extend(desc.context_windows.keys().cloned());
                    }
                    let def = get_default_model(&pid);
                    if !def.is_empty() && !fallback.iter().any(|m| m == def) {
                        fallback.insert(0, def.to_string());
                    }
                    fallback.sort();
                    fallback.dedup();
                    Ok(fallback)
                }
            }
        })
    }

    pub fn set_model(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<InitResponse, HarnessError> {
        self.tokio.block_on(async {
            let mut guard = self.inner.lock().await;
            let inner = guard.as_mut().ok_or_else(not_initialized)?;
            let pid = provider_id.trim().to_lowercase();
            let mid = model_id.trim().to_string();
            if pid.is_empty() || mid.is_empty() {
                return Err(HarnessError::ConfigurationError(
                    "provider and model must not be empty".to_string(),
                ));
            }
            let cfg_path = inner.files_dir.join(".luna").join("config.yaml");
            let layers = mobile_layers(&cfg_path, &inner.workspace_dir)?;
            config::set_in_file(&layers, "model.provider", &pid)?;
            config::set_in_file(&layers, "model.default", &mid)?;
            inner.config = load_mobile_config(&cfg_path, &inner.workspace_dir)?;
            inner.provider_id = pid.clone();
            inner.model_id = mid.clone();
            inner.store.set_model(&inner.session_id, &pid, &mid).await?;
            build_init_response(inner).await
        })
    }

    pub fn set_auto_approve(&self, enabled: bool) -> Result<bool, HarnessError> {
        self.tokio.block_on(async {
            let mut guard = self.inner.lock().await;
            let inner = guard.as_mut().ok_or_else(not_initialized)?;
            inner.auto_approve.store(enabled, Ordering::SeqCst);
            let cfg_path = inner.files_dir.join(".luna").join("config.yaml");
            let layers = mobile_layers(&cfg_path, &inner.workspace_dir)?;
            let val = if enabled { "allow" } else { "ask" };
            config::set_in_file(&layers, "tools.approval", val)?;
            inner.config = load_mobile_config(&cfg_path, &inner.workspace_dir)?;
            Ok(enabled)
        })
    }

    pub fn new_session(&self) -> Result<InitResponse, HarnessError> {
        self.tokio.block_on(async {
            let mut guard = self.inner.lock().await;
            let inner = guard.as_mut().ok_or_else(not_initialized)?;
            inner.session_id = create_fresh_session(&inner.store).await?;
            inner.session_allow_all.store(false, Ordering::SeqCst);
            build_init_response(inner).await
        })
    }

    pub fn select_session(&self, session_id: &str) -> Result<InitResponse, HarnessError> {
        self.tokio.block_on(async {
            let mut guard = self.inner.lock().await;
            let inner = guard.as_mut().ok_or_else(not_initialized)?;
            inner.store.ensure_session(session_id).await?;
            inner.session_id = session_id.to_string();
            inner.session_allow_all.store(false, Ordering::SeqCst);
            let (pid, mid) = resolve_active_model(
                &inner.config,
                &inner.store,
                &inner.cred_manager,
                &inner.session_id,
            )
            .await?;
            inner.provider_id = pid;
            inner.model_id = mid;
            build_init_response(inner).await
        })
    }

    pub fn delete_session(&self, session_id: &str) -> Result<InitResponse, HarnessError> {
        self.tokio.block_on(async {
            let mut guard = self.inner.lock().await;
            let inner = guard.as_mut().ok_or_else(not_initialized)?;
            inner.store.delete_session(session_id).await?;
            if inner.session_id == session_id {
                inner.session_id = match inner.store.most_recent().await? {
                    Some(id) => id,
                    None => create_fresh_session(&inner.store).await?,
                };
            }
            build_init_response(inner).await
        })
    }

    pub fn get_messages(
        &self,
        session_id: Option<&str>,
    ) -> Result<Vec<ChatMessageDto>, HarnessError> {
        self.tokio.block_on(async {
            let guard = self.inner.lock().await;
            let inner = guard.as_ref().ok_or_else(not_initialized)?;
            let sid = session_id.unwrap_or(&inner.session_id);
            let outcome = inner.store.replay_transcript(sid, 500).await?;
            Ok(outcome
                .history
                .into_iter()
                .map(|row| {
                    let msg = row.message;
                    ChatMessageDto {
                        role: match msg.role {
                            Role::System => "system",
                            Role::User => "user",
                            Role::Assistant => "assistant",
                            Role::Tool => "tool",
                        }
                        .to_string(),
                        content: msg.content.unwrap_or_default(),
                        reasoning: msg.reasoning,
                        tool_calls: msg
                            .tool_calls
                            .into_iter()
                            .map(|tc| ToolCallDto {
                                id: tc.id,
                                name: tc.name,
                                arguments: tc.arguments.to_string(),
                            })
                            .collect(),
                        tool_result: msg.tool_result.map(|tr| ToolResultDto {
                            call_id: tr.call_id,
                            content: tr.content,
                            is_error: tr.is_error,
                        }),
                    }
                })
                .collect())
        })
    }

    pub fn send_message(
        &self,
        prompt: &str,
        sink: Arc<dyn EventSink>,
    ) -> Result<serde_json::Value, HarnessError> {
        self.tokio.block_on(async {
            let (agent, cancel_token) = {
                let guard = self.inner.lock().await;
                let inner = guard.as_ref().ok_or_else(not_initialized)?;
                let agent = build_mobile_agent(inner, sink.clone()).await?;
                let cancel_token = CancellationToken::new();
                *inner
                    .active_cancel
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(cancel_token.clone());
                *inner
                    .active_interrupt
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(agent.interrupt_handle());
                (agent, cancel_token)
            };

            let outcome = agent.run_turn(prompt, cancel_token).await;

            {
                let guard = self.inner.lock().await;
                if let Some(inner) = guard.as_ref() {
                    *inner
                        .active_cancel
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = None;
                    *inner
                        .active_interrupt
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = None;
                }
            }

            let outcome = outcome?;
            Ok(serde_json::json!({
                "ok": true,
                "reply": outcome.reply,
                "durationMs": outcome.duration_ms,
            }))
        })
    }

    pub fn cancel_turn(&self) -> bool {
        let guard = match self.inner.try_lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if let Some(inner) = guard.as_ref() {
            if let Some(tok) = inner
                .active_cancel
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                tok.cancel();
                return true;
            }
        }
        false
    }

    pub fn compact_session(
        &self,
        sink: Arc<dyn EventSink>,
    ) -> Result<serde_json::Value, HarnessError> {
        self.tokio.block_on(async {
            let guard = self.inner.lock().await;
            let inner = guard.as_ref().ok_or_else(not_initialized)?;
            let agent = build_mobile_agent(inner, sink).await?;
            let notice = agent.compact_current_session().await?;
            Ok(serde_json::json!({
                "ok": true,
                "turns": notice.turns,
                "messages": notice.messages,
                "summary": notice.summary,
            }))
        })
    }

    pub fn list_memories(&self, query: &str) -> Result<Vec<MemoryDto>, HarnessError> {
        self.tokio.block_on(async {
            let guard = self.inner.lock().await;
            let inner = guard.as_ref().ok_or_else(not_initialized)?;
            let rows = inner.memory.retrieve_cards(query, 100).await?;
            Ok(rows
                .into_iter()
                .map(|r| MemoryDto {
                    id: r.id,
                    category: format!("{:?}", r.category).to_lowercase(),
                    subject: r.subject,
                    predicate: r.predicate,
                    object: r.object,
                    updated_at: r.updated_at.to_rfc3339(),
                })
                .collect())
        })
    }

    pub fn list_notes(&self, query: &str) -> Result<Vec<NoteDto>, HarnessError> {
        self.tokio.block_on(async {
            let guard = self.inner.lock().await;
            let inner = guard.as_ref().ok_or_else(not_initialized)?;
            let notes = if query.trim().is_empty() {
                inner.notes.list_notes(100, 0).await?
            } else {
                inner.notes.search_notes(query, 100).await?
            };
            Ok(notes
                .into_iter()
                .map(|n| NoteDto {
                    id: n.id,
                    title: n.title,
                    pointer: n.summary_pointer,
                    content: n.content,
                    updated_at: n.updated_at.to_rfc3339(),
                })
                .collect())
        })
    }
}

fn not_initialized() -> HarnessError {
    HarnessError::ConfigurationError("Luna mobile runtime is not initialized".to_string())
}

fn mobile_layers(config_path: &Path, workspace_dir: &Path) -> Result<Layers, HarnessError> {
    let defaults = catalog_defaults();
    let env = config::env_overrides(&defaults)?;
    let mut flags = BTreeMap::new();
    flags.insert(
        "paths.workspace_root".to_string(),
        workspace_dir.to_string_lossy().to_string(),
    );
    Ok(Layers {
        defaults,
        file: Some(config_path.to_path_buf()),
        profile: None,
        env,
        flags,
    })
}

fn load_mobile_config(config_path: &Path, workspace_dir: &Path) -> Result<Config, HarnessError> {
    let layers = mobile_layers(config_path, workspace_dir)?;
    Ok(config::resolve(&layers)?.into_config())
}

async fn create_fresh_session(store: &SqliteSessionStore) -> Result<String, HarnessError> {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    let id = format!("s{}", &uuid[..8]);
    store.ensure_session(&id).await?;
    Ok(id)
}

async fn resolve_active_model(
    config: &Config,
    store: &SqliteSessionStore,
    cred_manager: &CredentialManager,
    session_id: &str,
) -> Result<(String, String), HarnessError> {
    if let Ok(rec) = store.get(session_id).await {
        if let Some((p, m)) = rec.model {
            if !p.is_empty() && !m.is_empty() {
                return Ok((p, m));
            }
        }
    }

    if !config.model.provider.is_empty() && !config.model.default.is_empty() {
        return Ok((config.model.provider.clone(), config.model.default.clone()));
    }

    if let Some(p) = cred_manager.get_default_loaded_provider().await {
        let m = get_default_model(&p).to_string();
        if !m.is_empty() {
            return Ok((p, m));
        }
    }

    let p = if config.model.provider.is_empty() {
        "openai".to_string()
    } else {
        config.model.provider.clone()
    };
    let m = if config.model.default.is_empty() {
        get_default_model(&p).to_string()
    } else {
        config.model.default.clone()
    };
    Ok((p, m))
}

async fn build_init_response(inner: &RuntimeInner) -> Result<InitResponse, HarnessError> {
    let mut providers = Vec::new();
    for desc in get_provider_catalog() {
        // Skip local desktop CLI-only providers on Android unless configured
        if matches!(
            desc.id.as_str(),
            "opencode-cli" | "opencode-headless" | "kiro" | "kiro-cli" | "codex"
        ) {
            continue;
        }
        let creds = inner.cred_manager.get_credentials(&desc.id).await;
        providers.push(ProviderInfo {
            id: desc.id.clone(),
            display_name: desc.display_name,
            default_model: get_default_model(&desc.id).to_string(),
            has_key: !creds.is_empty(),
        });
    }

    let sessions = inner
        .store
        .list()
        .await?
        .into_iter()
        .map(|s| SessionInfo {
            id: s.id,
            name: s.name,
            title: s.title,
            updated_at: s.updated_at.to_rfc3339(),
            message_count: s.message_count,
            provider: s.model.as_ref().map(|(p, _)| p.clone()),
            model: s.model.map(|(_, m)| m),
        })
        .collect();

    let ctx = resolve_context_window(
        &inner.provider_id,
        &inner.model_id,
        inner
            .discovery
            .context_window(&inner.provider_id, &inner.model_id),
    ) as usize;

    Ok(InitResponse {
        ok: true,
        session_id: inner.session_id.clone(),
        provider: inner.provider_id.clone(),
        model: inner.model_id.clone(),
        workspace_dir: inner.workspace_dir.to_string_lossy().to_string(),
        auto_approve: inner.auto_approve.load(Ordering::SeqCst),
        context_window: ctx,
        providers,
        sessions,
    })
}

async fn build_mobile_agent(
    inner: &RuntimeInner,
    sink: Arc<dyn EventSink>,
) -> Result<
    Agent<GenericProviderClient, MemoryEngineKind, FileNotesEngine, DefaultToolRegistry>,
    HarnessError,
> {
    if inner.provider_id.is_empty() {
        return Err(HarnessError::ConfigurationError(
            "No provider selected. Open Settings and add an API key first.".to_string(),
        ));
    }

    let provider = Arc::new(GenericProviderClient::new(
        inner.provider_id.clone(),
        inner.model_id.clone(),
        inner.cred_manager.clone(),
        inner.discovery.clone(),
        inner.config.providers.get(&inner.provider_id),
    )?);

    let registry = Arc::new(DefaultToolRegistry::new());
    let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();

    registry
        .register(Arc::new(SaveMemoryTool::new(inner.memory.clone())))
        .await?;
    registry
        .register(Arc::new(UpdateMemoryTool::new(inner.memory.clone())))
        .await?;
    registry
        .register(Arc::new(ForgetMemoryTool::new(inner.memory.clone())))
        .await?;
    registry
        .register(Arc::new(
            MemorySearchTool::new(inner.memory.clone()).with_notes(inner.notes.clone()),
        ))
        .await?;
    registry
        .register(Arc::new(SaveNoteTool::new(inner.notes.clone())))
        .await?;
    registry
        .register(Arc::new(UpdateNoteTool::new(inner.notes.clone())))
        .await?;
    registry
        .register(Arc::new(DeleteNoteTool::new(inner.notes.clone())))
        .await?;
    registry
        .register(Arc::new(SearchNotesTool::new(inner.notes.clone())))
        .await?;
    registry
        .register(Arc::new(ListNotesTool::new(inner.notes.clone())))
        .await?;
    registry
        .register(Arc::new(ReadNoteTool::new(inner.notes.clone())))
        .await?;

    let mut all_skills = Vec::new();
    let _ = std::fs::create_dir_all(&inner.config.paths.skills_dir);
    if let Ok(skills) = SkillLoader::load_skills_from_dir(&inner.config.paths.skills_dir) {
        all_skills.extend(skills);
    }
    let workspace_skills = inner.workspace_dir.join(".harness/skills");
    if workspace_skills.exists() {
        if let Ok(skills) = SkillLoader::load_skills_from_dir(&workspace_skills) {
            all_skills.extend(skills);
        }
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

    let bg_queue = Arc::new(
        harness_core::tasks::BackgroundTaskQueue::new(inner.config.paths.jobs_dir.clone())
            .with_network_policy(inner.config.tools.network),
    );
    let todo_scope = TodoScope::new(inner.config.paths.todos_dir.clone(), &inner.session_id);
    harness_core::tools::register_core_tools_shared_queue(
        &reg_dyn,
        &inner.config,
        ReadCache::new(),
        None,
        None,
        bg_queue,
        todo_scope,
    )
    .await?;

    let mut agent_config = inner.config.agent.clone();
    agent_config.workspace_root = Some(inner.workspace_dir.clone());
    agent_config.memory_mode = inner.config.memory.mode;
    agent_config.memory_enabled = inner.config.memory_effective_enabled();
    agent_config.notes_enabled = inner.config.notes_effective_enabled();
    agent_config.skills_enabled = inner.config.skills_effective_enabled();
    agent_config.mcp_enabled = inner.config.mcp_effective_enabled();
    agent_config.context_window = Some(resolve_context_window(
        &inner.provider_id,
        &inner.model_id,
        inner
            .discovery
            .context_window(&inner.provider_id, &inner.model_id),
    ) as usize);

    let approver: Arc<dyn Approver> = if inner.auto_approve.load(Ordering::SeqCst) {
        Arc::new(AllowAll)
    } else {
        Arc::new(MobileApprover {
            auto_approve: inner.auto_approve.clone(),
            session_allow_all: inner.session_allow_all.clone(),
            sink: sink.clone(),
        })
    };

    let observer: AgentObserver = Arc::new(move |event| {
        let json = match event {
            AgentEvent::ReasoningDelta(text) => serde_json::json!({
                "type": "reasoning_delta",
                "text": text,
            }),
            AgentEvent::ContentDelta(text) => serde_json::json!({
                "type": "content_delta",
                "text": text,
            }),
            AgentEvent::ToolExecuting { name, arguments } => serde_json::json!({
                "type": "tool_executing",
                "name": name,
                "arguments": arguments.to_string(),
            }),
            AgentEvent::ToolOutput { name, chunk } => serde_json::json!({
                "type": "tool_output",
                "name": name,
                "chunk": chunk,
            }),
            AgentEvent::ToolExecuted {
                name,
                output,
                is_error,
            } => serde_json::json!({
                "type": "tool_executed",
                "name": name,
                "output": output,
                "isError": is_error,
            }),
            AgentEvent::ContextTokensUpdated { tokens_used, reset } => serde_json::json!({
                "type": "context_tokens",
                "tokensUsed": tokens_used,
                "reset": reset,
            }),
            AgentEvent::CompactionStarted { forced } => serde_json::json!({
                "type": "compaction_started",
                "forced": forced,
            }),
            AgentEvent::CompactionFinished { error, .. } => serde_json::json!({
                "type": "compaction_finished",
                "error": error,
            }),
            AgentEvent::Interrupted {
                text,
                reasoning,
                partial,
            } => serde_json::json!({
                "type": "interrupted",
                "text": text,
                "reasoning": reasoning,
                "partial": partial,
            }),
            AgentEvent::Steered { text } => serde_json::json!({
                "type": "steered",
                "text": text,
            }),
            AgentEvent::MemorySaved { name, detail, .. } => serde_json::json!({
                "type": "memory_saved",
                "name": name,
                "detail": detail,
            }),
            AgentEvent::NoteSaved { name, detail, .. } => serde_json::json!({
                "type": "note_saved",
                "name": name,
                "detail": detail,
            }),
            AgentEvent::MemoryPassStarted { model } => serde_json::json!({
                "type": "memory_pass_started",
                "model": model,
            }),
            AgentEvent::MemoryPassFinished { memories, notes } => serde_json::json!({
                "type": "memory_pass_finished",
                "memories": memories,
                "notes": notes,
            }),
        };
        sink.on_event(json.to_string());
    });

    let mut agent = Agent::new(
        provider.clone(),
        inner.memory.clone(),
        inner.notes.clone(),
        registry,
        agent_config,
    )
    .with_permissions(PermissionEngine::validate(
        inner.config.tools.permissions.clone(),
    )?)
    .with_approval(
        approver,
        ApprovalSettings {
            timeout: std::time::Duration::from_secs(inner.config.tools.approval_timeout_secs),
            mutating_overrides: inner.config.tools.mutating_overrides.clone(),
        },
    )
    .with_session(SessionBinding {
        store: inner.store.clone(),
        session_id: inner.session_id.clone(),
        resume_max_messages: inner.config.session.resume_max_messages,
    })
    .with_observer(observer);

    agent = agent.with_compaction(CompactionSetup {
        provider,
        model: inner.model_id.clone(),
    });

    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct NoopSink;
    impl EventSink for NoopSink {
        fn on_event(&self, _json_payload: String) {}
        fn request_approval(&self, _request_json: String) -> i32 {
            1
        }
    }

    #[test]
    fn mobile_runtime_initializes_creates_session_and_stores_key() {
        let dir = TempDir::new().unwrap();
        let files_dir = dir.path().join("files");
        let workspace_dir = dir.path().join("workspace");

        let rt = LunaMobileRuntime::new().unwrap();
        let init = rt.init(&files_dir, &workspace_dir).unwrap();
        assert!(init.ok);
        assert!(!init.session_id.is_empty());
        assert!(!init.providers.is_empty());
        assert_eq!(init.sessions.len(), 1);

        let updated = rt.set_api_key("openai", "sk-test-mobile-key").unwrap();
        assert_eq!(updated.provider, "openai");
        assert!(
            updated
                .providers
                .iter()
                .find(|p| p.id == "openai")
                .unwrap()
                .has_key
        );

        let switched = rt.new_session().unwrap();
        assert_eq!(switched.sessions.len(), 2);
        assert_ne!(switched.session_id, init.session_id);

        let msgs = rt.get_messages(None).unwrap();
        assert!(msgs.is_empty());

        let memories = rt.list_memories("").unwrap();
        assert!(memories.is_empty());
        let notes = rt.list_notes("").unwrap();
        assert!(notes.is_empty());

        let _sink: Arc<dyn EventSink> = Arc::new(NoopSink);
    }
}
