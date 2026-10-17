use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use slint::{Model, VecModel};
use tokio_util::sync::CancellationToken;

use harness_core::{
    Agent, Config, DefaultToolRegistry, ProviderOverride,
    SessionBinding, SessionStore, SqliteSessionStore, ToolRegistry,
};
use harness_evolver::{
    CheckImprovementTool, CodebaseEvolver, DelegateImprovementTool, ListImprovementJobsTool,
};
use harness_memory::{
    ForgetMemoryTool, SaveMemoryTool, SqliteMemoryEngine, UpdateMemoryTool,
};
use harness_notes::{
    DeleteNoteTool, ReadNoteTool, SaveNoteTool, SearchNotesTool, SqliteNotesEngine, UpdateNoteTool,
};
use harness_providers::antigravity::ANTIGRAVITY_MODELS;
use harness_providers::auth::CredentialManager;
use harness_providers::catalog::get_provider_catalog;
use harness_providers::discovery::ModelDiscovery;
use harness_providers::GenericProviderClient;

slint::include_modules!();

/// Manages the active workspace directory and enforces that code edits can ONLY occur
/// within the active project directory sandbox.
#[derive(Debug)]
pub struct ActiveSandbox {
    root: RwLock<PathBuf>,
}

impl ActiveSandbox {
    pub fn new(initial: PathBuf) -> Self {
        let canonical = initial.canonicalize().unwrap_or(initial);
        Self {
            root: RwLock::new(canonical),
        }
    }

    pub fn get_root(&self) -> PathBuf {
        self.root.read().unwrap().clone()
    }

    pub fn set_root(&self, new_root: PathBuf) -> Result<PathBuf, String> {
        if !new_root.is_dir() {
            return Err(format!("'{}' is not a valid directory", new_root.display()));
        }
        let canonical = new_root.canonicalize().map_err(|e| e.to_string())?;
        *self.root.write().unwrap() = canonical.clone();
        Ok(canonical)
    }

    /// Resolves a path relative to the active directory and asserts it does not escape the sandbox.
    /// Only code within the active directory can be read, written, or patched.
    pub fn resolve_sandboxed_path(&self, user_path: &str) -> Result<PathBuf, String> {
        let root = self.get_root();
        harness_core::tools::fs::resolve_path(&root, user_path)
    }

    /// Sandboxed code edit: writes content to a file only if within the active directory
    pub fn write_file_sandboxed(&self, rel_path: &str, content: &str) -> Result<PathBuf, String> {
        let target = self.resolve_sandboxed_path(rel_path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create directories: {e}"))?;
        }
        fs::write(&target, content).map_err(|e| format!("Failed to write file: {e}"))?;
        Ok(target)
    }

    /// Sandboxed code edit: applies find-and-replace only if within the active directory
    pub fn patch_sandboxed(
        &self,
        rel_path: &str,
        old_string: &str,
        new_string: &str,
    ) -> Result<PathBuf, String> {
        let target = self.resolve_sandboxed_path(rel_path)?;
        let data = fs::read_to_string(&target).map_err(|e| format!("Failed to read file: {e}"))?;
        if !data.contains(old_string) {
            return Err("Target string not found in file".to_string());
        }
        let patched = data.replacen(old_string, new_string, 1);
        fs::write(&target, patched).map_err(|e| format!("Failed to save patched file: {e}"))?;
        Ok(target)
    }
}

/// Pre-loads all models for all known and cached providers directly into an in-memory map
/// at application startup. This avoids any runtime disk I/O or network delays during UI interaction.
fn load_all_cached_models() -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    // 1. Antigravity static models (instant)
    map.insert(
        "antigravity".to_string(),
        ANTIGRAVITY_MODELS.iter().map(|&s| s.to_string()).collect(),
    );
    map.insert(
        "agy".to_string(),
        ANTIGRAVITY_MODELS.iter().map(|&s| s.to_string()).collect(),
    );

    // 2. Read ~/.luna/models_cache.json if present (sub-millisecond load at startup)
    if let Ok(home) = std::env::var("HOME") {
        let cache_path = PathBuf::from(home).join(".luna").join("models_cache.json");
        if let Ok(data) = fs::read_to_string(&cache_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(providers) = json.get("providers").and_then(|p| p.as_object()) {
                    for (prov_id, entry) in providers {
                        if let Some(models_val) = entry.get("models").and_then(|m| m.as_array()) {
                            let list: Vec<String> = models_val
                                .iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect();
                            if !list.is_empty() {
                                map.insert(prov_id.clone(), list);
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Fallback to catalog known models for every standard provider
    for desc in get_provider_catalog() {
        map.entry(desc.id.clone()).or_insert_with(|| {
            let mut list: Vec<String> = desc.context_windows.keys().cloned().collect();
            list.sort();
            list
        });
    }

    map
}

/// Resolves runtime configuration and provider overrides
fn load_runtime_config() -> Config {
    let mut config = Config::default();
    for provider in get_provider_catalog() {
        config.providers.insert(
            provider.id,
            ProviderOverride {
                base_url: Some(provider.default_base_url),
                models_endpoint: Some(provider.models_endpoint),
                timeout_secs: Some(provider.timeout_secs),
            },
        );
    }
    match harness_core::config::process_layers(config) {
        Ok(layers) => harness_core::config::resolve(&layers)
            .map(|r| r.into_config())
            .unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

async fn load_session_items(store: &SqliteSessionStore, active_id: &str) -> Vec<SessionItem> {
    let mut items = Vec::new();
    if let Ok(list) = store.list().await {
        for s in list {
            items.push(SessionItem {
                id: s.id.clone().into(),
                name: s.name.into(),
                active: s.id == active_id,
            });
        }
    }
    if items.is_empty() {
        items.push(SessionItem {
            id: active_id.into(),
            name: "Default Session".into(),
            active: true,
        });
    }
    items
}

async fn load_session_msgs(store: &SqliteSessionStore, session_id: &str) -> Vec<ChatMsg> {
    let mut msgs = Vec::new();
    if let Ok(outcome) = store.replay(session_id, 100).await {
        for s in outcome.history {
            let role = match s.message.role {
                harness_core::Role::User => "user",
                harness_core::Role::Assistant => "assistant",
                harness_core::Role::System | harness_core::Role::Tool => continue,
            };
            let author = if role == "user" { "You" } else { "Luna" };
            if let Some(content) = s.message.content {
                if !content.trim().is_empty() {
                    msgs.push(ChatMsg {
                        role: role.into(),
                        author: author.into(),
                        content: content.into(),
                        timestamp: "Earlier".into(),
                    });
                }
            }
        }
    }
    msgs
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = AppWindow::new()?;

    // Load configuration, credentials, and persistent session store
    let config = Arc::new(load_runtime_config());
    let cred_manager = CredentialManager::new(Some(config.paths.credentials.clone()))
        .unwrap_or_else(|_| CredentialManager::new(None).unwrap());
    let discovery = Arc::new(ModelDiscovery::new(
        Some(config.paths.models_cache.clone()),
        24,
        config.providers.clone(),
    ));
    let store = Arc::new(
        SqliteSessionStore::new(Some(config.paths.sessions_db.clone()))
            .unwrap_or_else(|_| SqliteSessionStore::new(None).unwrap()),
    );

    let initial_session = store
        .most_recent()
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            let uuid = uuid::Uuid::new_v4().simple().to_string();
            format!("s{}", &uuid[..8])
        });
    let _ = store.ensure_session(&initial_session).await;
    let current_session = Arc::new(RwLock::new(initial_session.clone()));

    // Populate initial sessions and transcript
    let initial_msgs = load_session_msgs(&store, &initial_session).await;
    let initial_items = load_session_items(&store, &initial_session).await;
    app.set_transcript(Rc::new(VecModel::from(initial_msgs)).into());
    app.set_sessions(Rc::new(VecModel::from(initial_items)).into());

    let model_cache = Arc::new(load_all_cached_models());

    // Query registered providers from credentials
    let mut configured_providers = cred_manager.get_loaded_providers().await;
    if configured_providers.is_empty() {
        configured_providers = vec!["antigravity".to_string(), "nvidia".to_string()];
    }

    let default_prov = configured_providers.first().cloned().unwrap_or_else(|| "antigravity".to_string());
    app.set_current_provider(default_prov.clone().into());
    app.set_selected_provider(default_prov.clone().into());

    let prov_slint: Vec<slint::SharedString> = configured_providers.iter().map(|s| s.into()).collect();
    app.set_available_providers(Rc::new(VecModel::from(prov_slint)).into());

    // Pre-build ModelRc for each provider so switching providers takes 0ns (pointer copy)
    let mut model_rc_cache: HashMap<String, slint::ModelRc<slint::SharedString>> = HashMap::new();
    for (prov, models) in model_cache.iter() {
        let slint_models: Vec<slint::SharedString> = models.iter().map(|s| s.into()).collect();
        model_rc_cache.insert(prov.clone(), slint::ModelRc::new(VecModel::from(slint_models)));
    }
    let prebuilt_models = Arc::new(model_rc_cache);

    // Populate initial models immediately from prebuilt RAM cache
    if let Some(rc) = prebuilt_models.get(&default_prov) {
        app.set_available_models(rc.clone());
    }
    if let Some(first_model) = model_cache.get(&default_prov).and_then(|m| m.first()) {
        app.set_model_name(first_model.clone().into());
    }

    // Provider selection callback: instantaneous synchronous in-memory transition (0ms)
    let app_weak = app.as_weak();
    let prebuilt_clone_prov = prebuilt_models.clone();
    let cache_clone_prov = model_cache.clone();
    app.on_select_provider(move |provider_id| {
        let prov_str = provider_id.to_string();
        if let Some(app) = app_weak.upgrade() {
            app.set_selected_provider(prov_str.clone().into());
            let query = app.get_model_filter().to_string();
            let query_lower = query.trim().to_lowercase();
            if query_lower.is_empty() {
                if let Some(rc) = prebuilt_clone_prov.get(&prov_str) {
                    app.set_available_models(rc.clone());
                } else {
                    let fallback = vec![slint::SharedString::from(format!("{}-default", prov_str))];
                    app.set_available_models(Rc::new(VecModel::from(fallback)).into());
                }
            } else if let Some(models) = cache_clone_prov.get(&prov_str) {
                let filtered: Vec<slint::SharedString> = models
                    .iter()
                    .filter(|m| m.to_lowercase().contains(&query_lower))
                    .map(|s| s.into())
                    .collect();
                app.set_available_models(Rc::new(VecModel::from(filtered)).into());
            }
            app.set_model_scroll_index(0);
        }
    });

    // Instant in-memory search filter callback (sub-microsecond)
    let app_weak = app.as_weak();
    let prebuilt_clone_filter = prebuilt_models.clone();
    let cache_clone_filter = model_cache.clone();
    app.on_filter_models(move |query| {
        if let Some(app) = app_weak.upgrade() {
            let prov_str = app.get_selected_provider().to_string();
            let query_lower = query.trim().to_lowercase();
            if query_lower.is_empty() {
                if let Some(rc) = prebuilt_clone_filter.get(&prov_str) {
                    app.set_available_models(rc.clone());
                }
            } else if let Some(models) = cache_clone_filter.get(&prov_str) {
                let filtered: Vec<slint::SharedString> = models
                    .iter()
                    .filter(|m| m.to_lowercase().contains(&query_lower))
                    .map(|s| s.into())
                    .collect();
                app.set_available_models(Rc::new(VecModel::from(filtered)).into());
            }
            app.set_model_scroll_index(0);
        }
    });

    // Model selection callback: sets active model, updates provider, closes dropdown
    let app_weak = app.as_weak();
    app.on_select_model(move |model_id| {
        if let Some(app) = app_weak.upgrade() {
            app.set_current_provider(app.get_selected_provider());
            app.set_model_name(model_id);
            app.set_model_picker_open(false);
        }
    });

    // Active sandbox directory tracking (defaults to cwd)
    let initial_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let sandbox = Arc::new(ActiveSandbox::new(initial_dir));
    let current_root = sandbox.get_root();
    let dir_name = current_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();
    let dir_path_str = current_root.to_string_lossy().to_string();

    app.set_workspace_name(dir_name.into());
    app.set_workspace_path(dir_path_str.into());
    app.set_is_sandboxed(true);

    // Directory selection callback: opens native directory chooser asynchronously
    let app_weak = app.as_weak();
    let sandbox_clone = sandbox.clone();
    app.on_select_directory(move || {
        let app_weak = app_weak.clone();
        let sandbox = sandbox_clone.clone();
        let current_path = sandbox.get_root();

        tokio::task::spawn_blocking(move || {
            // Use zenity native directory chooser on Linux
            let output = std::process::Command::new("zenity")
                .args([
                    "--file-selection",
                    "--directory",
                    "--title=Select Project Location",
                    &format!("--filename={}/", current_path.display()),
                ])
                .output();

            if let Ok(out) = output {
                if out.status.success() {
                    let chosen = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !chosen.is_empty() {
                        let path = PathBuf::from(chosen);
                        if let Ok(canonical) = sandbox.set_root(path) {
                            let name = canonical
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("project")
                                .to_string();
                            let full_path = canonical.to_string_lossy().to_string();

                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(app) = app_weak.upgrade() {
                                    app.set_workspace_name(name.into());
                                    app.set_workspace_path(full_path.into());
                                }
                            });
                        }
                    }
                }
            }
        });
    });

    // Session selection callback
    let app_weak = app.as_weak();
    let current_session_sel = current_session.clone();
    let store_sel = store.clone();
    app.on_select_session(move |sess_id| {
        let sid = sess_id.to_string();
        *current_session_sel.write().unwrap() = sid.clone();
        let app_weak = app_weak.clone();
        let store = store_sel.clone();
        tokio::spawn(async move {
            let msgs = load_session_msgs(&store, &sid).await;
            let list = load_session_items(&store, &sid).await;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = app_weak.upgrade() {
                    app.set_transcript(Rc::new(VecModel::from(msgs)).into());
                    app.set_sessions(Rc::new(VecModel::from(list)).into());
                }
            });
        });
    });

    // New session callback
    let app_weak = app.as_weak();
    let current_session_new = current_session.clone();
    let store_new = store.clone();
    app.on_new_session(move || {
        let uuid = uuid::Uuid::new_v4().simple().to_string();
        let new_id = format!("s{}", &uuid[..8]);
        *current_session_new.write().unwrap() = new_id.clone();
        let app_weak = app_weak.clone();
        let store = store_new.clone();
        tokio::spawn(async move {
            let _ = store.ensure_session(&new_id).await;
            let list = load_session_items(&store, &new_id).await;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = app_weak.upgrade() {
                    app.set_transcript(Rc::new(VecModel::default()).into());
                    app.set_sessions(Rc::new(VecModel::from(list)).into());
                }
            });
        });
    });

    // Chat submit callback (strictly sandboxed to active project directory)
    let app_weak = app.as_weak();
    let sandbox_submit = sandbox.clone();
    let config_submit = config.clone();
    let cred_manager_submit = cred_manager.clone();
    let discovery_submit = discovery.clone();
    let store_submit = store.clone();
    let current_session_submit = current_session.clone();
    let is_generating = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let is_generating_submit = is_generating.clone();
    app.on_submit_message(move |text| {
        let user_text = text.to_string();
        if user_text.trim().is_empty() {
            return;
        }

        if is_generating_submit.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }

        let app_weak = app_weak.clone();
        let active_root = sandbox_submit.get_root();
        let config = config_submit.clone();
        let cred_manager = cred_manager_submit.clone();
        let discovery = discovery_submit.clone();
        let store = store_submit.clone();
        let session_id = current_session_submit.read().unwrap().clone();
        let is_generating = is_generating_submit.clone();

        if let Some(app) = app_weak.upgrade() {
            let now = chrono::Local::now().format("%H:%M").to_string();
            let mut msgs: Vec<ChatMsg> = app.get_transcript().iter().collect();
            msgs.push(ChatMsg {
                role: "user".into(),
                author: "You".into(),
                content: user_text.clone().into(),
                timestamp: now.clone().into(),
            });
            msgs.push(ChatMsg {
                role: "assistant".into(),
                author: "Luna".into(),
                content: "Thinking...".into(),
                timestamp: now.into(),
            });
            app.set_transcript(Rc::new(VecModel::from(msgs)).into());
            app.set_is_generating(true);

            let provider_id = app.get_current_provider().to_string();
            let model_id = app.get_model_name().to_string();

            tokio::spawn(async move {
                let finish = |reply: String| {
                    let app_weak = app_weak.clone();
                    let is_generating = is_generating.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(app) = app_weak.upgrade() {
                            let mut msgs: Vec<ChatMsg> = app.get_transcript().iter().collect();
                            if let Some(last) = msgs.last_mut() {
                                last.content = reply.into();
                            }
                            app.set_transcript(Rc::new(VecModel::from(msgs)).into());
                            app.set_is_generating(false);
                        }
                        is_generating.store(false, std::sync::atomic::Ordering::SeqCst);
                    });
                };

                let provider_client = match GenericProviderClient::new(
                    provider_id.clone(),
                    model_id.clone(),
                    cred_manager.clone(),
                    discovery.clone(),
                    config.providers.get(&provider_id),
                ) {
                    Ok(client) => client,
                    Err(err) => {
                        finish(format!("Provider error: {}", err));
                        return;
                    }
                };

                let provider = Arc::new(provider_client);
                let memory = Arc::new(
                    SqliteMemoryEngine::new(Some(config.paths.memory_db.clone()))
                        .unwrap_or_else(|_| SqliteMemoryEngine::new(None).unwrap()),
                );
                let notes = Arc::new(
                    SqliteNotesEngine::new(Some(config.paths.notes_db.clone()))
                        .unwrap_or_else(|_| SqliteNotesEngine::new(None).unwrap()),
                );
                let evolver = Arc::new(
                    CodebaseEvolver::new(active_root.clone(), config.paths.jobs_dir.clone())
                        .with_provider(provider.clone()),
                );
                let registry = Arc::new(DefaultToolRegistry::new());

                // Register native memory tools
                let _ = registry.register(Arc::new(SaveMemoryTool::new(memory.clone()))).await;
                let _ = registry.register(Arc::new(UpdateMemoryTool::new(memory.clone()))).await;
                let _ = registry.register(Arc::new(ForgetMemoryTool::new(memory.clone()))).await;

                // Register native note tools
                let _ = registry.register(Arc::new(SaveNoteTool::new(notes.clone()))).await;
                let _ = registry.register(Arc::new(SearchNotesTool::new(notes.clone()))).await;
                let _ = registry.register(Arc::new(ReadNoteTool::new(notes.clone()))).await;
                let _ = registry.register(Arc::new(UpdateNoteTool::new(notes.clone()))).await;
                let _ = registry.register(Arc::new(DeleteNoteTool::new(notes.clone()))).await;

                // Register evolver tools
                let _ = registry.register(Arc::new(DelegateImprovementTool::new(evolver.clone()))).await;
                let _ = registry.register(Arc::new(CheckImprovementTool::new(evolver.clone()))).await;
                let _ = registry.register(Arc::new(ListImprovementJobsTool::new(evolver.clone()))).await;

                // Register skills from skills_dir
                let _ = std::fs::create_dir_all(&config.paths.skills_dir);
                if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&config.paths.skills_dir) {
                    for skill in skills {
                        let _ = registry.register(Arc::new(harness_plugins::SkillToolAdapter::new(skill))).await;
                    }
                }

                // Register core sandboxed tools (strictly bounded to active workspace root)
                let mut run_config = (*config).clone();
                run_config.paths.workspace_root = active_root.clone();
                let read_cache = harness_core::tools::ReadCache::new();
                let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
                let _ = harness_core::tools::register_core_tools(&reg_dyn, &run_config, read_cache).await;

                let agent_cfg = config.agent.clone();

                let agent = Agent::new(
                    provider,
                    memory,
                    notes,
                    evolver,
                    registry,
                    agent_cfg,
                )
                .with_approval(
                    Arc::new(harness_core::approval::AllowAll),
                    harness_core::approval::ApprovalSettings::default(),
                )
                .with_session(SessionBinding {
                    store: store.clone(),
                    session_id: session_id.clone(),
                    resume_max_messages: config.session.resume_max_messages,
                });

                // Attach live observer to report tool execution or reasoning progress in the UI
                let app_obs = app_weak.clone();
                let agent = agent.with_observer(Arc::new(move |event| {
                    let app_obs = app_obs.clone();
                    match event {
                        harness_core::AgentEvent::ReasoningDelta(delta) => {
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(app) = app_obs.upgrade() {
                                    let mut msgs: Vec<ChatMsg> = app.get_transcript().iter().collect();
                                    if let Some(last) = msgs.last_mut() {
                                        if last.content == "Thinking..." {
                                            last.content = delta.into();
                                        } else {
                                            let mut s = last.content.to_string();
                                            s.push_str(&delta);
                                            last.content = s.into();
                                        }
                                    }
                                    app.set_transcript(Rc::new(VecModel::from(msgs)).into());
                                }
                            });
                        }
                        harness_core::AgentEvent::ToolExecuting { name, .. } => {
                            let status = format!("Executing tool: {}...", name);
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(app) = app_obs.upgrade() {
                                    let mut msgs: Vec<ChatMsg> = app.get_transcript().iter().collect();
                                    if let Some(last) = msgs.last_mut() {
                                        last.content = status.into();
                                    }
                                    app.set_transcript(Rc::new(VecModel::from(msgs)).into());
                                }
                            });
                        }
                        harness_core::AgentEvent::ToolExecuted { .. } => {}
                    }
                }));

                let cancel_token = CancellationToken::new();
                match agent.run_turn(&user_text, cancel_token).await {
                    Ok(outcome) => {
                        finish(outcome.reply);
                    }
                    Err(err) => {
                        finish(format!("Execution error: {}", err));
                    }
                }
            });
        }
    });

    app.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_active_sandbox_enforces_directory_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let project_dir = temp.path().join("my-project");
        fs::create_dir_all(&project_dir).unwrap();

        let sandbox = ActiveSandbox::new(project_dir.clone());
        assert_eq!(sandbox.get_root(), project_dir.canonicalize().unwrap());

        // File inside directory is allowed
        let ok_file = sandbox.write_file_sandboxed("src/main.rs", "fn main() {}");
        assert!(ok_file.is_ok());

        // File trying to escape is strictly rejected
        let escape_attempt = sandbox.write_file_sandboxed("../evil.rs", "malicious");
        assert!(escape_attempt.is_err());
    }
}
