use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use slint::{Model, VecModel};

use harness_providers::antigravity::ANTIGRAVITY_MODELS;
use harness_providers::auth::CredentialManager;
use harness_providers::catalog::get_provider_catalog;

slint::include_modules!();

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

    // Populate initial models immediately from RAM
    let initial_models = model_cache
        .get(&default_prov)
        .cloned()
        .unwrap_or_else(|| vec![format!("{}-default", default_prov)]);
    if let Some(first_model) = initial_models.first() {
        app.set_model_name(first_model.clone().into());
    }
    let initial_models_slint: Vec<slint::SharedString> = initial_models.iter().map(|s| s.into()).collect();
    app.set_available_models(Rc::new(VecModel::from(initial_models_slint)).into());

    // Provider selection callback: instantaneous synchronous in-memory transition (0ms)
    let app_weak = app.as_weak();
    let cache_clone = model_cache.clone();
    app.on_select_provider(move |provider_id| {
        let prov_str = provider_id.to_string();
        if let Some(app) = app_weak.upgrade() {
            let models = cache_clone
                .get(&prov_str)
                .cloned()
                .unwrap_or_else(|| vec![format!("{}-default", prov_str)]);
            let models_slint: Vec<slint::SharedString> = models.iter().map(|s| s.into()).collect();
            app.set_selected_provider(prov_str.into());
            app.set_available_models(Rc::new(VecModel::from(models_slint)).into());
            app.set_picker_step(1);
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

    // Chat submit callback
    let app_weak = app.as_weak();
    app.on_submit_message(move |text| {
        if let Some(app) = app_weak.upgrade() {
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
                content: format!("[{} / {}] Message received: {}", app.get_current_provider(), app.get_model_name(), text).into(),
                timestamp: "Just now".into(),
            });
            app.set_transcript(Rc::new(VecModel::from(msgs)).into());
        }
    });

    app.run()?;
    Ok(())
}
