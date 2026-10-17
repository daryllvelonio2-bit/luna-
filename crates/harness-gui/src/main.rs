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
