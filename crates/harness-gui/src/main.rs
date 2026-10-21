use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use slint::{Model, VecModel};
use tokio_util::sync::CancellationToken;

use harness_core::{
    Agent, Config, DefaultToolRegistry, ProviderOverride, ReasoningEffort,
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
                title: s.title.into(),
                active: s.id == active_id,
            });
        }
    }
    if items.is_empty() {
        items.push(SessionItem {
            id: active_id.into(),
            name: "Default Session".into(),
            title: String::new().into(),
            active: true,
        });
    }
    items
}


fn format_tool_call_detail(name: &str, arguments: &serde_json::Value) -> String {
    let arg_str = |key: &str| arguments.get(key).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "read_file" => {
            let path = arg_str("path");
            let offset = arguments.get("offset").and_then(|v| v.as_u64());
            let limit = arguments.get("limit").and_then(|v| v.as_u64());
            let range = match (offset, limit) {
                (Some(o), Some(l)) => format!(" (L{}-{})", o, o + l.saturating_sub(1)),
                (Some(o), None) => format!(" (L{}+)", o),
                (None, Some(l)) => format!(" (L1-{})", l),
                _ => String::new(),
            };
            format!("📖 read_file: {path}{range}")
        }
        "write_file" => {
            let path = arg_str("path");
            let lines = arg_str("content").lines().count();
            format!("✍ write_file: {path} ({lines} lines)")
        }
        "patch" => {
            let path = arg_str("path");
            format!("🩹 patch: {path}")
        }
        "terminal" | "shell" | "bash" => {
            let cmd = arg_str("command");
            let first_line = cmd.lines().next().unwrap_or(cmd).trim();
            format!("💻 terminal: $ {first_line}")
        }
        "search_files" => {
            let pat = arg_str("pattern");
            let path = arg_str("path");
            let target = arg_str("target");
            format!("🔍 search_files: \"{pat}\" in {path} ({target})")
        }
        "list_files" => {
            let path = arg_str("path");
            format!("📂 list_files: {path}")
        }
        _ => {
            let short_args = arguments.to_string();
            let truncated: String = short_args.chars().take(80).collect();
            format!("🔧 {name}: {truncated}")
        }
    }
}


fn format_process_summary(tool_count: usize, tool_names: &[String], duration: std::time::Duration, has_reasoning: bool) -> String {
    let mut parts = Vec::new();
    if tool_count > 0 {
        let mut unique: Vec<String> = Vec::new();
        for n in tool_names {
            if !unique.contains(n) {
                unique.push(n.clone());
            }
        }
        let label = if tool_count == 1 {
            format!("1 tool: {}", unique.first().cloned().unwrap_or_else(|| "tool".to_string()))
        } else if unique.len() <= 2 {
            format!("{} tools: {}", tool_count, unique.join(", "))
        } else {
            format!("{} tools: {}, {}, +{}", tool_count, unique[0], unique[1], unique.len() - 2)
        };
        parts.push(label);
    }
    if duration.as_millis() >= 100 {
        parts.push(format!("thought for {:.1}s", duration.as_secs_f32()));
    } else if has_reasoning {
        parts.push("thinking".to_string());
    }
    parts.join(" • ")
}

fn format_tool_result_summary(output: &str, is_error: bool) -> String {
    let icon = if is_error { "✗" } else { "✓" };
    let first_line = output.lines().find(|l| !l.trim().is_empty()).unwrap_or(output).trim();
    let truncated: String = first_line.chars().take(100).collect();
    format!("{icon} {truncated}")
}


/// Strips DeepSeek DSML tool call tokens (<｜tool calls｜>...<｜tool calls end｜>),
/// stray delimiters, and extracts embedded <think>...</think> blocks.
fn strip_dsml_and_extract_thinking(raw_content: &str) -> (String, Option<String>) {
    let mut text = raw_content.to_string();
    let mut extracted_thinking = None;

    // 1. Extract <think>...</think> or <thought>...</thought>
    for (open_tag, close_tag) in &[("<think>", "</think>"), ("<thought>", "</thought>")] {
        if let Some(start) = text.find(open_tag) {
            if let Some(end) = text[start + open_tag.len()..].find(close_tag) {
                let real_end = start + open_tag.len() + end;
                let thought = text[start + open_tag.len()..real_end].trim().to_string();
                if !thought.is_empty() {
                    extracted_thinking = Some(thought);
                }
                text = format!("{}{}", &text[..start], &text[real_end + close_tag.len()..]);
            } else {
                let thought = text[start + open_tag.len()..].trim().to_string();
                if !thought.is_empty() {
                    extracted_thinking = Some(thought);
                }
                text = text[..start].to_string();
            }
        }
    }

    // 2. Strip DSML tool call blocks
    let block_pairs = [
        ("<｜tool calls｜>", "<｜tool calls end｜>"),
        ("<｜tool_calls｜>", "<｜tool_calls_end｜>"),
        ("<tool_calls>", "</tool_calls>"),
        ("<tool_call>", "</tool_call>"),
        ("<｜tool call begin｜>", "<｜tool call end｜>"),
        ("<｜tool_call_begin｜>", "<｜tool_call_end｜>"),
    ];

    for (start_tag, end_tag) in &block_pairs {
        while let Some(start) = text.find(start_tag) {
            if let Some(end) = text[start + start_tag.len()..].find(end_tag) {
                let full_end = start + start_tag.len() + end + end_tag.len();
                text = format!("{}{}", &text[..start], &text[full_end..]);
            } else {
                text = text[..start].to_string();
                break;
            }
        }
    }

    // 3. Remove any remaining stray DSML tokens <｜...｜>
    let open_tok = "<｜";
    let close_tok = "｜>";
    while let Some(start) = text.find(open_tok) {
        if let Some(end) = text[start + open_tok.len()..].find(close_tok) {
            let full_end = start + open_tok.len() + end + close_tok.len();
            text = format!("{}{}", &text[..start], &text[full_end..]);
        } else {
            text = text[..start].to_string();
            break;
        }
    }

    // 4. Remove stray XML tool tags
    for tag in &["<tool_calls>", "</tool_calls>", "<tool_call>", "</tool_call>"] {
        text = text.replace(tag, "");
    }

    (text.trim().to_string(), extracted_thinking)
}

async fn load_session_msgs(store: &SqliteSessionStore, session_id: &str) -> Vec<ChatMsg> {
    let mut msgs: Vec<ChatMsg> = Vec::new();
    if let Ok(outcome) = store.replay(session_id, 100).await {
        for s in outcome.history {
            let role = match s.message.role {
                harness_core::Role::User => "user",
                harness_core::Role::Assistant => "assistant",
                harness_core::Role::System | harness_core::Role::Tool => continue,
            };
            let author = if role == "user" { "You" } else { "Luna" };
            let raw_content = s.message.content.unwrap_or_default();
            let (content, extracted_think) = if role == "assistant" {
                strip_dsml_and_extract_thinking(&raw_content)
            } else {
                (raw_content, None)
            };
            let reasoning = s.message.reasoning.or(extracted_think).unwrap_or_default();

            let mut tool_strs = Vec::new();
            let mut tool_names = Vec::new();
            for tc in &s.message.tool_calls {
                tool_names.push(tc.name.clone());
                tool_strs.push(format_tool_call_detail(&tc.name, &tc.arguments));
            }
            let tool_calls = tool_strs.join("\n");
            let process_summary = if !tool_strs.is_empty() || !reasoning.is_empty() {
                format_process_summary(tool_strs.len(), &tool_names, std::time::Duration::ZERO, !reasoning.is_empty())
            } else {
                String::new()
            };

            if role == "assistant" {
                if let Some(last) = msgs.last_mut().filter(|m| m.role == "assistant") {
                    // Merge consecutive assistant steps in a single turn so "Luna" headers never repeat
                    if !tool_calls.is_empty() {
                        if last.tool_calls.is_empty() {
                            last.tool_calls = tool_calls.into();
                        } else {
                            let mut cur = last.tool_calls.to_string();
                            cur.push('\n');
                            cur.push_str(&tool_calls);
                            last.tool_calls = cur.into();
                        }
                    }
                    if !reasoning.is_empty() {
                        if last.reasoning.is_empty() {
                            last.reasoning = reasoning.into();
                        } else {
                            let mut cur = last.reasoning.to_string();
                            cur.push('\n');
                            cur.push_str(&reasoning);
                            last.reasoning = cur.into();
                        }
                    }
                    if !content.trim().is_empty() {
                        if last.content.is_empty() {
                            last.content = content.into();
                        } else {
                            let mut cur = last.content.to_string();
                            cur.push_str("\n\n");
                            cur.push_str(&content);
                            last.content = cur.into();
                        }
                    }
                    if !process_summary.is_empty() {
                        last.process_summary = process_summary.into();
                    }
                    continue;
                }
            }

            if !content.trim().is_empty() || !tool_calls.is_empty() || !reasoning.is_empty() {
                msgs.push(ChatMsg {
                    role: role.into(),
                    author: author.into(),
                    content: content.into(),
                    reasoning: reasoning.into(),
                    tool_calls: tool_calls.into(),
                    process_summary: process_summary.into(),
                    status: String::new().into(),
                    timestamp: "Earlier".into(),
                });
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
    let active_reasoning_effort = Arc::new(RwLock::new(Some(ReasoningEffort::High)));
    app.set_reasoning_effort("high".into());

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

    let prov_slint: Vec<slint::SharedString> = configured_providers.iter().map(|s| s.into()).collect();
    app.set_available_providers(Rc::new(VecModel::from(prov_slint)).into());

    // Pre-build ModelRc for each provider so switching providers takes 0ns (pointer copy)
    let mut model_rc_cache: HashMap<String, slint::ModelRc<slint::SharedString>> = HashMap::new();
    for (prov, models) in model_cache.iter() {
        let slint_models: Vec<slint::SharedString> = models.iter().map(|s| s.into()).collect();
        model_rc_cache.insert(prov.clone(), slint::ModelRc::new(VecModel::from(slint_models)));
    }
    let prebuilt_models = Arc::new(model_rc_cache);

    // Restore the persisted provider & model for the active session, or resolve fallback
    let saved_session_model = store.get(&initial_session).await.ok().and_then(|r| r.model);
    let (active_prov, active_model) = if let Some((p, m)) = saved_session_model {
        (p, m)
    } else {
        let p = if !config.model.provider.is_empty() {
            config.model.provider.clone()
        } else {
            configured_providers.first().cloned().unwrap_or_else(|| "antigravity".to_string())
        };
        let m = if !config.model.default.is_empty() {
            config.model.default.clone()
        } else {
            model_cache.get(&p).and_then(|list| list.first()).cloned().unwrap_or_else(|| "default".to_string())
        };
        let _ = store.set_model(&initial_session, &p, &m).await;
        (p, m)
    };

    app.set_current_provider(active_prov.clone().into());
    app.set_selected_provider(active_prov.clone().into());
    app.set_model_name(active_model.into());

    if let Some(rc) = prebuilt_models.get(&active_prov) {
        app.set_available_models(rc.clone());
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

    // Reasoning effort selection callback: updates active reasoning effort level (low/medium/high/xhigh)
    let effort_lock = active_reasoning_effort.clone();
    app.on_select_reasoning_effort(move |effort_str| {
        let effort = ReasoningEffort::from_str_loose(&effort_str.to_string());
        *effort_lock.write().unwrap() = effort;
    });

    // Model selection callback: sets active model, updates provider, closes dropdown, and persists into session
    let app_weak = app.as_weak();
    let store_model = store.clone();
    let current_session_model = current_session.clone();
    app.on_select_model(move |model_id| {
        if let Some(app) = app_weak.upgrade() {
            let prov = app.get_selected_provider();
            let prov_str = prov.to_string();
            let model_str = model_id.to_string();
            app.set_current_provider(prov);
            app.set_model_name(model_id);
            app.set_model_picker_open(false);

            let sid = current_session_model.read().unwrap().clone();
            let store = store_model.clone();
            tokio::spawn(async move {
                if let Err(e) = store.set_model(&sid, &prov_str, &model_str).await {
                    tracing::warn!("failed to persist model for session '{sid}': {e}");
                }
            });
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
    let model_cache_sel = model_cache.clone();
    app.on_select_session(move |sess_id| {
        let sid = sess_id.to_string();
        *current_session_sel.write().unwrap() = sid.clone();
        let app_weak = app_weak.clone();
        let store = store_sel.clone();
        let model_cache = model_cache_sel.clone();
        tokio::spawn(async move {
            let msgs = load_session_msgs(&store, &sid).await;
            let list = load_session_items(&store, &sid).await;
            let saved_model = store.get(&sid).await.ok().and_then(|r| r.model);

            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = app_weak.upgrade() {
                    app.set_transcript(Rc::new(VecModel::from(msgs)).into());
                    app.set_sessions(Rc::new(VecModel::from(list)).into());

                    if let Some((prov, modl)) = saved_model {
                        app.set_current_provider(prov.clone().into());
                        app.set_selected_provider(prov.clone().into());
                        app.set_model_name(modl.into());
                        if let Some(list) = model_cache.get(&prov) {
                            let slint_models: Vec<slint::SharedString> = list.iter().map(|s| s.into()).collect();
                            app.set_available_models(Rc::new(VecModel::from(slint_models)).into());
                        }
                    }
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

        let current_prov = if let Some(app) = app_weak.upgrade() {
            app.get_current_provider().to_string()
        } else {
            String::new()
        };
        let current_modl = if let Some(app) = app_weak.upgrade() {
            app.get_model_name().to_string()
        } else {
            String::new()
        };

        tokio::spawn(async move {
            let _ = store.ensure_session(&new_id).await;
            if !current_prov.is_empty() && !current_modl.is_empty() {
                let _ = store.set_model(&new_id, &current_prov, &current_modl).await;
            }
            let list = load_session_items(&store, &new_id).await;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = app_weak.upgrade() {
                    app.set_transcript(Rc::new(VecModel::default()).into());
                    app.set_sessions(Rc::new(VecModel::from(list)).into());
                }
            });
        });
    });

    // Delete session callback: drops the chat and its stored messages, then refreshes the sidebar.
    let app_weak = app.as_weak();
    let current_session_del = current_session.clone();
    let store_del = store.clone();
    app.on_delete_session(move |sess_id| {
        let sid = sess_id.to_string();
        let app_weak = app_weak.clone();
        let current_session = current_session_del.clone();
        let store = store_del.clone();
        tokio::spawn(async move {
            if let Err(error) = store.delete_session(&sid).await {
                tracing::warn!("failed to delete session '{sid}': {error}");
                return;
            }

            // Deleting the chat on screen has to leave *some* chat behind: the most recent
            // survivor, or a fresh empty one when it was the last.
            let active = current_session.read().unwrap().clone();
            let deleted_active = active == sid;
            let next_active = if deleted_active {
                match store.most_recent().await.ok().flatten() {
                    Some(id) => id,
                    None => {
                        let uuid = uuid::Uuid::new_v4().simple().to_string();
                        let id = format!("s{}", &uuid[..8]);
                        let _ = store.ensure_session(&id).await;
                        id
                    }
                }
            } else {
                active
            };
            *current_session.write().unwrap() = next_active.clone();

            let msgs = if deleted_active {
                Some(load_session_msgs(&store, &next_active).await)
            } else {
                None
            };
            let list = load_session_items(&store, &next_active).await;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = app_weak.upgrade() {
                    if let Some(msgs) = msgs {
                        app.set_transcript(Rc::new(VecModel::from(msgs)).into());
                    }
                    app.set_sessions(Rc::new(VecModel::from(list)).into());
                }
            });
        });
    });

    // Delete-all callback: empties the session store, then opens a fresh chat so the window
    // is never left pointing at a transcript that no longer exists on disk.
    let app_weak = app.as_weak();
    let current_session_clear = current_session.clone();
    let store_clear = store.clone();
    app.on_delete_all_sessions(move || {
        let app_weak = app_weak.clone();
        let current_session = current_session_clear.clone();
        let store = store_clear.clone();
        tokio::spawn(async move {
            match store.delete_all_sessions().await {
                Ok(removed) => tracing::debug!("deleted {removed} sessions"),
                Err(error) => {
                    tracing::warn!("failed to delete all sessions: {error}");
                    return;
                }
            }

            let uuid = uuid::Uuid::new_v4().simple().to_string();
            let new_id = format!("s{}", &uuid[..8]);
            if let Err(error) = store.ensure_session(&new_id).await {
                tracing::warn!("failed to open a session after clearing the store: {error}");
                return;
            }
            *current_session.write().unwrap() = new_id.clone();

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
    let reasoning_effort_submit = active_reasoning_effort.clone();
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
        let active_effort = *reasoning_effort_submit.read().unwrap();
        let is_generating = is_generating_submit.clone();

        if let Some(app) = app_weak.upgrade() {
            let now = chrono::Local::now().format("%H:%M").to_string();
            let mut msgs: Vec<ChatMsg> = app.get_transcript().iter().collect();
            msgs.push(ChatMsg {
                role: "user".into(),
                author: "You".into(),
                content: user_text.clone().into(),
                reasoning: String::new().into(),
                tool_calls: String::new().into(),
                process_summary: String::new().into(),
                status: String::new().into(),
                timestamp: now.clone().into(),
            });
            msgs.push(ChatMsg {
                role: "assistant".into(),
                author: "Luna".into(),
                content: String::new().into(),
                reasoning: String::new().into(),
                tool_calls: String::new().into(),
                process_summary: String::new().into(),
                status: "Thinking…".into(),
                timestamp: now.into(),
            });
            app.set_transcript(Rc::new(VecModel::from(msgs)).into());
            app.set_is_generating(true);

            let provider_id = app.get_current_provider().to_string();
            let model_id = app.get_model_name().to_string();

            tokio::spawn(async move {
                let start_time = std::time::Instant::now();
                let turn_tools: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
                let turn_tool_names: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
                let turn_reasoning: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
                let current_tool_call: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));

                let finish = {
                    let app_weak = app_weak.clone();
                    let is_generating = is_generating.clone();
                    let turn_tools = turn_tools.clone();
                    let turn_tool_names = turn_tool_names.clone();
                    let turn_reasoning = turn_reasoning.clone();

                    move |raw_reply: String, model_reasoning: Option<String>| {
                        let app_weak = app_weak.clone();
                        let is_generating = is_generating.clone();
                        let duration = start_time.elapsed();
                        let (cleaned_reply, extracted_think) = strip_dsml_and_extract_thinking(&raw_reply);

                        let mut final_think = model_reasoning.or(extracted_think);
                        if final_think.is_none() {
                            let streamed_r = turn_reasoning.lock().unwrap().trim().to_string();
                            if !streamed_r.is_empty() {
                                final_think = Some(streamed_r);
                            }
                        }

                        let tools_str = turn_tools.lock().unwrap().join("\n");
                        let tool_count = turn_tools.lock().unwrap().len();
                        let tool_names = turn_tool_names.lock().unwrap().clone();
                        let has_reasoning = final_think.is_some();
                        let summary = format_process_summary(tool_count, &tool_names, duration, has_reasoning);

                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(app) = app_weak.upgrade() {
                                let mut msgs: Vec<ChatMsg> = app.get_transcript().iter().collect();
                                if let Some(last) = msgs.last_mut() {
                                    last.content = cleaned_reply.into();
                                    last.status = String::new().into();
                                    last.process_summary = summary.into();
                                    if !tools_str.is_empty() {
                                        last.tool_calls = tools_str.into();
                                    }
                                    if let Some(th) = final_think {
                                        last.reasoning = th.into();
                                    }
                                }
                                app.set_transcript(Rc::new(VecModel::from(msgs)).into());
                                app.set_is_generating(false);
                            }
                            is_generating.store(false, std::sync::atomic::Ordering::SeqCst);
                        });
                    }
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
                        finish(format!("Provider error: {}", err), None);
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

                // Load skills from skills_dir and workspace .harness/skills
                let mut all_skills = Vec::new();
                let _ = std::fs::create_dir_all(&config.paths.skills_dir);
                if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&config.paths.skills_dir) {
                    all_skills.extend(skills);
                }
                let workspace_skills = active_root.join(".harness/skills");
                if workspace_skills.exists() {
                    if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&workspace_skills) {
                        all_skills.extend(skills);
                    }
                }

                let skills_summary = if all_skills.is_empty() {
                    None
                } else {
                    let lines: Vec<String> = all_skills
                        .iter()
                        .map(|s| format!("- {}: {}", s.name, s.description))
                        .collect();
                    Some(lines.join("\n"))
                };

                if !all_skills.is_empty() {
                    let skills_arc = Arc::new(all_skills);
                    let _ = registry.register(Arc::new(harness_plugins::SkillViewTool::new(skills_arc.clone()))).await;
                    let _ = registry.register(Arc::new(harness_plugins::SkillsListTool::new(skills_arc))).await;
                }

                // Register core sandboxed tools (strictly bounded to active workspace root)
                let mut run_config = (*config).clone();
                run_config.paths.workspace_root = active_root.clone();
                let read_cache = harness_core::tools::ReadCache::new();
                let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
                let _ = harness_core::tools::register_core_tools(&reg_dyn, &run_config, read_cache).await;

                let mut agent_cfg = config.agent.clone();
                agent_cfg.skills_summary = skills_summary;

                let mut agent = Agent::new(
                    provider,
                    memory,
                    notes,
                    evolver,
                    registry,
                    agent_cfg,
                )
                .with_permissions(
                    // Startup already validated this section; if it is somehow invalid now, the
                    // rules are dropped rather than half-applied, and the reason is on stderr.
                    harness_core::PermissionEngine::validate(config.tools.permissions.clone())
                        .map_err(|error| {
                            eprintln!("tools.permissions ignored: {error}");
                            error
                        })
                        .unwrap_or_default(),
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
                agent.set_reasoning_effort(active_effort);

                // Attach live observer to report tool execution or reasoning progress in the UI
                let app_obs = app_weak.clone();
                let tools_obs = turn_tools.clone();
                let tool_names_obs = turn_tool_names.clone();
                let reasoning_obs = turn_reasoning.clone();
                let call_obs = current_tool_call.clone();

                let agent = agent.with_observer(Arc::new(move |event| {
                    let app_obs = app_obs.clone();
                    let tools_obs = tools_obs.clone();
                    let tool_names_obs = tool_names_obs.clone();
                    let reasoning_obs = reasoning_obs.clone();
                    let call_obs = call_obs.clone();

                    match event {
                        harness_core::AgentEvent::ReasoningDelta(delta) => {
                            reasoning_obs.lock().unwrap().push_str(&delta);
                            let current_r = reasoning_obs.lock().unwrap().clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(app) = app_obs.upgrade() {
                                    let mut msgs: Vec<ChatMsg> = app.get_transcript().iter().collect();
                                    if let Some(last) = msgs.last_mut() {
                                        last.reasoning = current_r.into();
                                        last.status = "Thinking…".into();
                                    }
                                    app.set_transcript(Rc::new(VecModel::from(msgs)).into());
                                }
                            });
                        }
                        harness_core::AgentEvent::ContextTokensUpdated { .. } => {}
                        harness_core::AgentEvent::ToolExecuting { name, arguments } => {
                            tool_names_obs.lock().unwrap().push(name.clone());
                            let detail = format_tool_call_detail(&name, &arguments);
                            *call_obs.lock().unwrap() = detail.clone();
                            let status = format!("⚡ Executing {detail}…");
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(app) = app_obs.upgrade() {
                                    let mut msgs: Vec<ChatMsg> = app.get_transcript().iter().collect();
                                    if let Some(last) = msgs.last_mut() {
                                        last.status = status.into();
                                    }
                                    app.set_transcript(Rc::new(VecModel::from(msgs)).into());
                                }
                            });
                        }
                        harness_core::AgentEvent::ToolExecuted { name: _, output, is_error } => {
                            let call_label = call_obs.lock().unwrap().clone();
                            let summary = format_tool_result_summary(&output, is_error);
                            let entry = if call_label.is_empty() {
                                summary
                            } else {
                                format!("{call_label}\n   ↳ {summary}")
                            };
                            tools_obs.lock().unwrap().push(entry);
                            let current_tools = tools_obs.lock().unwrap().join("\n");
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(app) = app_obs.upgrade() {
                                    let mut msgs: Vec<ChatMsg> = app.get_transcript().iter().collect();
                                    if let Some(last) = msgs.last_mut() {
                                        last.tool_calls = current_tools.into();
                                    }
                                    app.set_transcript(Rc::new(VecModel::from(msgs)).into());
                                }
                            });
                        }
                        harness_core::AgentEvent::Steered { .. } => {}
                    }
                }));

                let cancel_token = CancellationToken::new();
                match agent.run_turn(&user_text, cancel_token).await {
                    Ok(outcome) => {
                        finish(outcome.reply, outcome.reasoning);
                    }
                    Err(err) => {
                        finish(format!("Execution error: {}", err), None);
                    }
                }

                // The turn is what titles a chat (the first user line is stored as its title), so
                // refresh the sidebar here rather than leaving the entry on its raw session id.
                let list = load_session_items(&store, &session_id).await;
                let app_sessions = app_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = app_sessions.upgrade() {
                        app.set_sessions(Rc::new(VecModel::from(list)).into());
                    }
                });
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

    #[test]
    fn test_strip_dsml_and_extract_thinking() {
        let input = "<｜tool calls｜><｜tool call begin｜>function<｜tool sep｜>read_file\n{\"path\": \"src/lib.rs\"}<｜tool call end｜><｜tool calls end｜><think>Inspecting codebase structure</think>Here are the findings.<｜end of sentence｜>";
        let (cleaned, thinking) = strip_dsml_and_extract_thinking(input);
        assert_eq!(cleaned, "Here are the findings.");
        assert_eq!(thinking.as_deref(), Some("Inspecting codebase structure"));

        let standard = "Normal message without any DSML tags.";
        let (cleaned2, thinking2) = strip_dsml_and_extract_thinking(standard);
        assert_eq!(cleaned2, "Normal message without any DSML tags.");
        assert_eq!(thinking2, None);
    }

    #[test]
    fn test_format_process_summary() {
        let tools = vec!["read_file".to_string(), "terminal".to_string()];
        let summary = format_process_summary(2, &tools, std::time::Duration::from_millis(3200), true);
        assert_eq!(summary, "2 tools: read_file, terminal • thought for 3.2s");

        let summary_no_tools = format_process_summary(0, &[], std::time::Duration::from_millis(1500), true);
        assert_eq!(summary_no_tools, "thought for 1.5s");

        let summary_instant = format_process_summary(1, &["search_files".to_string()], std::time::Duration::ZERO, false);
        assert_eq!(summary_instant, "1 tool: search_files");
    }
}
