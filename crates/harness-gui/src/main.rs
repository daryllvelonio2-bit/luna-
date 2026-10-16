use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use slint::{Model, VecModel};

use harness_providers::antigravity::ANTIGRAVITY_MODELS;
use harness_providers::auth::CredentialManager;
use harness_providers::catalog::resolve_descriptor;
use harness_providers::discovery::ModelDiscovery;

slint::include_modules!();

async fn fetch_models_for_provider(
    provider: &str,
    cred_manager: Option<&CredentialManager>,
    discovery: &ModelDiscovery,
) -> Vec<String> {
    if provider == "antigravity" || provider == "agy" {
        return ANTIGRAVITY_MODELS.iter().map(|&s| s.to_string()).collect();
    }
    if let Some(cm) = cred_manager {
        if let Ok(models) = discovery.get_models(provider, cm, false).await {
            if !models.is_empty() {
                return models;
            }
        }
    }
    if let Ok(desc) = resolve_descriptor(provider, None) {
        let mut list: Vec<String> = desc.context_windows.keys().cloned().collect();
        list.sort();
        if !list.is_empty() {
            return list;
        }
    }
    vec![format!("{}-default", provider)]
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = AppWindow::new()?;

    // Load credentials and model discovery
    let cred_manager = CredentialManager::new(None).ok();
    let discovery = Arc::new(ModelDiscovery::new(None, 24, BTreeMap::new()));

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

    // Fetch initial models for default provider
    let initial_models = fetch_models_for_provider(&default_prov, cred_manager.as_deref(), &discovery).await;
    if let Some(first_model) = initial_models.first() {
        app.set_model_name(first_model.clone().into());
    }
    let initial_models_slint: Vec<slint::SharedString> = initial_models.iter().map(|s| s.into()).collect();
    app.set_available_models(Rc::new(VecModel::from(initial_models_slint)).into());

    // Provider selection callback: populates available models and moves to step 1
    let app_weak = app.as_weak();
    let cred_clone = cred_manager.clone();
    let disc_clone = discovery.clone();
    app.on_select_provider(move |provider_id| {
        let app_weak = app_weak.clone();
        let cred_clone = cred_clone.clone();
        let disc_clone = disc_clone.clone();
        let prov_str = provider_id.to_string();

        tokio::spawn(async move {
            let models = fetch_models_for_provider(&prov_str, cred_clone.as_deref(), &disc_clone).await;
            let models_slint: Vec<slint::SharedString> = models.iter().map(|s| s.into()).collect();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = app_weak.upgrade() {
                    app.set_selected_provider(prov_str.into());
                    app.set_available_models(Rc::new(VecModel::from(models_slint)).into());
                    app.set_picker_step(1);
                }
            });
        });
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
