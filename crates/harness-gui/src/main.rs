use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use slint::{Model, VecModel};

use harness_providers::antigravity::ANTIGRAVITY_MODELS;
use harness_providers::auth::CredentialManager;
use harness_providers::catalog::get_provider_catalog;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = AppWindow::new()?;

    // Load credentials and in-memory model cache once at startup
    let cred_manager = CredentialManager::new(None).ok();
    let model_cache = Arc::new(load_all_cached_models());

    // Query registered providers from credentials
    let mut configured_providers = if let Some(ref cm) = cred_manager {
        cm.get_loaded_providers().await
    } else {
        Vec::new()
    };
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

    // Chat submit callback (strictly sandboxed to active project directory)
    let app_weak = app.as_weak();
    let sandbox_submit = sandbox.clone();
    app.on_submit_message(move |text| {
        if let Some(app) = app_weak.upgrade() {
            let active_root = sandbox_submit.get_root();
            let mut msgs: Vec<ChatMsg> = app.get_transcript().iter().collect();
            msgs.push(ChatMsg {
                role: "user".into(),
                author: "You".into(),
                content: text.clone(),
                timestamp: "Just now".into(),
            });
            msgs.push(ChatMsg {
                role: "assistant".into(),
                author: "Luna".into(),
                content: format!(
                    "[🔒 Sandboxed: {}] [{} / {}] Message received: {}",
                    active_root.display(),
                    app.get_current_provider(),
                    app.get_model_name(),
                    text
                ).into(),
                timestamp: "Just now".into(),
            });
            app.set_transcript(Rc::new(VecModel::from(msgs)).into());
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
        let root = sandbox.get_root();
        assert_eq!(root, project_dir.canonicalize().unwrap());

        // 1. Files inside active directory resolve successfully
        let valid = sandbox.resolve_sandboxed_path("src/main.rs");
        assert!(valid.is_ok());
        let valid_path = valid.unwrap();
        assert!(valid_path.starts_with(&root));

        // 2. Path escapes (e.g. ../../) are strictly rejected
        let escape = sandbox.resolve_sandboxed_path("../outside.rs");
        assert!(escape.is_err());
        assert!(escape.unwrap_err().contains("escapes workspace root"));

        // 3. Absolute path outside the active directory is strictly rejected
        let outside = temp.path().join("outside.rs");
        let abs_escape = sandbox.resolve_sandboxed_path(&outside.to_string_lossy());
        assert!(abs_escape.is_err());
        assert!(abs_escape.unwrap_err().contains("escapes workspace root"));

        // 4. Sandboxed write works inside directory
        let write_res = sandbox.write_file_sandboxed("src/lib.rs", "pub fn hello() {}");
        assert!(write_res.is_ok());
        assert_eq!(
            fs::read_to_string(root.join("src/lib.rs")).unwrap(),
            "pub fn hello() {}"
        );

        // 5. Sandboxed write outside directory is rejected
        let write_escape = sandbox.write_file_sandboxed("../escape.txt", "evil");
        assert!(write_escape.is_err());

        // 6. Sandboxed patch works inside directory
        let patch_res = sandbox.patch_sandboxed("src/lib.rs", "hello()", "world()");
        assert!(patch_res.is_ok());
        assert_eq!(
            fs::read_to_string(root.join("src/lib.rs")).unwrap(),
            "pub fn world() {}"
        );

        // 7. Changing active directory updates sandbox boundary
        let new_dir = temp.path().join("another-project");
        fs::create_dir_all(&new_dir).unwrap();
        let changed = sandbox.set_root(new_dir.clone());
        assert!(changed.is_ok());
        assert_eq!(sandbox.get_root(), new_dir.canonicalize().unwrap());

        // Now writes to previous project are outside new sandbox root and must be rejected!
        let old_project_write =
            sandbox.write_file_sandboxed(&project_dir.join("test.txt").to_string_lossy(), "test");
        assert!(old_project_write.is_err());
    }
}
