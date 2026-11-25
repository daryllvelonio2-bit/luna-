//! pickers

use super::*;

pub async fn open_provider_picker(env: &Env<'_>, st: &mut TuiState) {
    open_provider_picker_with_query(env, st, String::new()).await;
}

pub async fn open_provider_picker_for(env: &Env<'_>, st: &mut TuiState, target: ModelTarget) {
    open_provider_picker_with_target(env, st, target, String::new()).await;
}

pub async fn open_provider_picker_with_query(env: &Env<'_>, st: &mut TuiState, query: String) {
    open_provider_picker_with_target(env, st, ModelTarget::Session, query).await;
}

/// Snapshot of the provider rows for the first `/models` step.
///
/// Kept separate from the modal so it can run on a background task: it reads the credential file
/// and probes the OpenCode daemon/binary, neither of which belongs on the render loop.
pub async fn build_provider_items(
    cred_manager: Arc<harness_providers::CredentialManager>,
    config_providers: Option<
        &std::collections::BTreeMap<String, harness_core::config::ProviderOverride>,
    >,
    current_provider: &str,
) -> Vec<ProviderPickerItem> {
    let loaded = cred_manager.get_loaded_providers().await;
    let (has_opencode_daemon, has_opencode_cli) = tokio::task::spawn_blocking(|| {
        (
            harness_providers::read_opencode_service_info().is_some(),
            harness_providers::find_opencode_binary().is_ok(),
        )
    })
    .await
    .unwrap_or((false, false));

    let agy_creds = cred_manager.get_credentials("antigravity").await;
    let agy_primary = agy_creds
        .iter()
        .find(|c| c.is_priority && !c.is_temporarily_disabled)
        .or_else(|| agy_creds.iter().find(|c| !c.is_temporarily_disabled));
    let antigravity_account: Option<String> = agy_primary.and_then(|primary| {
        if !primary.label.is_empty() {
            return Some(primary.label.clone());
        }
        serde_json::from_str::<serde_json::Value>(&primary.api_key)
            .ok()
            .and_then(|val| {
                val.get("email")
                    .and_then(|e| e.as_str())
                    .map(str::to_string)
            })
            .filter(|email| !email.is_empty())
    });

    let catalog = harness_providers::get_provider_catalog();
    let mut configured: Vec<_> = catalog
        .iter()
        .filter(|p| {
            loaded.iter().any(|l| l.eq_ignore_ascii_case(&p.id))
                || p.id.eq_ignore_ascii_case(current_provider)
                || (p.id == "opencode" && has_opencode_daemon)
                || (p.id == "opencode-cli" && has_opencode_cli)
        })
        .cloned()
        .collect();

    let mut custom_ids = Vec::new();
    if let Some(cfgs) = config_providers {
        for id in cfgs.keys() {
            if id != "custom"
                && !custom_ids.contains(id)
                && !configured.iter().any(|p| p.id.eq_ignore_ascii_case(id))
            {
                custom_ids.push(id.clone());
            }
        }
    }
    for id in &loaded {
        let clean = crate::commands::chat::connect::provider_id_of(id);
        if clean != "custom"
            && !custom_ids.contains(&clean.to_string())
            && !configured.iter().any(|p| p.id.eq_ignore_ascii_case(clean))
        {
            custom_ids.push(clean.to_string());
        }
    }
    for id in custom_ids {
        configured.push(harness_providers::catalog::custom_provider_descriptor(
            &id, "",
        ));
    }

    let available = if configured.is_empty() {
        catalog
    } else {
        configured
    };

    let mut items = Vec::with_capacity(available.len());
    for p in available {
        let is_current = p.id.eq_ignore_ascii_case(current_provider);
        let mut tags = Vec::new();
        if is_current {
            tags.push("active".to_string());
        }
        if loaded.iter().any(|l| l.eq_ignore_ascii_case(&p.id)) {
            tags.push("configured".to_string());
        }
        if p.id.eq_ignore_ascii_case("opencode") && has_opencode_daemon {
            tags.push("daemon".to_string());
        } else if p.id.eq_ignore_ascii_case("opencode-cli") && has_opencode_cli {
            tags.push("cli".to_string());
        } else if p.id.eq_ignore_ascii_case("antigravity") || p.id.eq_ignore_ascii_case("agy") {
            if let Some(account) = antigravity_account.clone() {
                tags.push(account);
            }
        }
        items.push(ProviderPickerItem {
            id: p.id,
            display_name: p.display_name,
            tag: tags.join(" · "),
            is_current,
        });
    }
    items
}

/// Open the provider picker immediately from the cached rows, then refresh them off-loop.
pub async fn open_provider_picker_with_target(
    env: &Env<'_>,
    st: &mut TuiState,
    target: ModelTarget,
    query: String,
) {
    let highlight_provider = if matches!(target, ModelTarget::Approval) {
        resolve_approval_model(env, st).0
    } else {
        env.current_provider.clone()
    };
    let items = if highlight_provider.eq_ignore_ascii_case(env.current_provider) {
        st.provider_items.clone()
    } else {
        build_provider_items(
            env.cred_manager.clone(),
            Some(&env.config.providers),
            &highlight_provider,
        )
        .await
    };
    let selected = items.iter().position(|i| i.is_current).unwrap_or(0);
    st.picker_loading = None;
    st.modal = ActiveModal::ProviderPicker {
        target,
        query,
        items,
        selected,
    };
    super::trace_event("provider picker shown");

    let tx = st.picker_tx.clone();
    let cred_manager = env.cred_manager.clone();
    let config_providers = env.config.providers.clone();
    tokio::spawn(async move {
        let items =
            build_provider_items(cred_manager, Some(&config_providers), &highlight_provider).await;
        let _ = tx.send(crate::commands::chat::tui::state::PickerLoad::Providers(
            items,
        ));
    });
}

pub async fn open_models_for_provider(
    env: &Env<'_>,
    st: &mut TuiState,
    provider: &str,
    query: String,
) {
    open_models_for_target(env, st, ModelTarget::Session, provider, query).await;
}

/// Load the model rows for one provider. Pure data, so it can run off the render loop.
pub async fn load_model_items(
    discovery: &harness_providers::ModelDiscovery,
    cred_manager: &harness_providers::CredentialManager,
    provider: &str,
    current_provider: &str,
    current_model: &str,
) -> Vec<ModelPickerItem> {
    let mut models = discovery
        .get_models(provider, cred_manager, false)
        .await
        .unwrap_or_default();
    let def = harness_providers::catalog::get_default_model(provider).to_string();
    if !def.is_empty() && def != "default" && !models.contains(&def) {
        models.insert(0, def);
    }
    if provider.eq_ignore_ascii_case(current_provider) && !models.iter().any(|m| m == current_model)
    {
        models.insert(0, current_model.to_string());
    }

    models
        .into_iter()
        .map(|m| {
            let is_current = provider.eq_ignore_ascii_case(current_provider) && m == current_model;
            ModelPickerItem {
                display_name: friendly_model_name(&m),
                provider: provider.to_string(),
                model: m,
                is_current,
            }
        })
        .collect()
}

/// Open the model picker on the first keystroke and fill it in when discovery answers.
pub async fn open_models_for_target(
    env: &Env<'_>,
    st: &mut TuiState,
    target: ModelTarget,
    provider: &str,
    query: String,
) {
    let provider_owned = provider.to_string();
    st.picker_loading = Some(provider_owned.clone());
    let is_approval_target = matches!(target, ModelTarget::Approval);
    st.modal = ActiveModal::ModelPicker {
        target,
        provider: provider_owned.clone(),
        provider_display: friendly_provider_name(provider),
        query,
        items: Vec::new(),
        selected: 0,
    };

    let tx = st.picker_tx.clone();
    let discovery = env.discovery.clone();
    let cred_manager = env.cred_manager.clone();
    let (current_provider, current_model) = if is_approval_target {
        let (p, m, _, _) = resolve_approval_model(env, st);
        (p, m)
    } else {
        (env.current_provider.clone(), env.current_model.clone())
    };
    tokio::spawn(async move {
        let items = load_model_items(
            discovery.as_ref(),
            cred_manager.as_ref(),
            &provider_owned,
            &current_provider,
            &current_model,
        )
        .await;
        let _ = tx.send(crate::commands::chat::tui::state::PickerLoad::Models {
            provider: provider_owned,
            items,
        });
    });
}

/// Fold a background picker load into the modal that asked for it, preserving the user's query.
pub fn apply_picker_load(st: &mut TuiState, load: crate::commands::chat::tui::state::PickerLoad) {
    use crate::commands::chat::tui::state::PickerLoad;

    match load {
        PickerLoad::Providers(items) => {
            st.provider_items = items.clone();
            if let ActiveModal::ProviderPicker {
                items: current,
                selected,
                ..
            } = &mut st.modal
            {
                let keep = *selected;
                *current = items;
                *selected = keep.min(current.len().saturating_sub(1));
            }
        }
        PickerLoad::Models { provider, items } => {
            if st.picker_loading.as_deref() == Some(provider.as_str()) {
                st.picker_loading = None;
            }
            if let ActiveModal::ModelPicker {
                provider: open_provider,
                items: current,
                selected,
                ..
            } = &mut st.modal
            {
                if open_provider == &provider {
                    let keep = *selected;
                    *current = items;
                    *selected = if keep == 0 {
                        current.iter().position(|i| i.is_current).unwrap_or(0)
                    } else {
                        keep.min(current.len().saturating_sub(1))
                    };
                }
            }
        }
    }
}

pub async fn open_thinking_picker(_env: &Env<'_>, st: &mut TuiState) {
    open_thinking_picker_with_current(st, st.reasoning_effort);
}

pub fn open_thinking_picker_with_current(st: &mut TuiState, cur: Option<ReasoningEffort>) {
    let selected = match cur {
        None => 0,
        Some(ReasoningEffort::Minimal) => 1,
        Some(ReasoningEffort::Low) => 2,
        Some(ReasoningEffort::Medium) => 3,
        Some(ReasoningEffort::High) => 4,
        Some(ReasoningEffort::Xhigh) => 5,
        Some(ReasoningEffort::Max) => 6,
    };
    st.modal = ActiveModal::ThinkingPicker {
        current: cur,
        selected,
    };
}

pub async fn apply_model_selection(
    env: &mut Env<'_>,
    st: &mut TuiState,
    provider: &str,
    model: &str,
    persist: bool,
) -> Result<(), HarnessError> {
    if st.active_turn.is_some() {
        env.ui.set_transient_error(Some(
            "Wait for or interrupt the active turn in this session before changing models.".into(),
        ));
        return Ok(());
    }
    let client = Arc::new(
        GenericProviderClient::new(
            provider.to_string(),
            model.to_string(),
            env.cred_manager.clone(),
            env.discovery.clone(),
            env.config.providers.get(provider),
        )?
        .with_session_id(env.session_id.as_str()),
    );
    let ctx_max = context_window(env.config, env.discovery.as_ref(), provider, model);
    let Ok(mut ag) = env.agent.try_lock() else {
        env.ui.set_transient_error(Some(
            "Session agent is busy; wait for the turn to finish before changing models.".into(),
        ));
        return Ok(());
    };
    ag.set_provider(client.clone());
    ag.set_context_window(Some(ctx_max as usize));
    if env.compact_model.is_none() {
        if let Ok(comp_client) = GenericProviderClient::new(
            provider.to_string(),
            model.to_string(),
            env.cred_manager.clone(),
            env.discovery.clone(),
            env.config.providers.get(provider),
        ) {
            ag.set_compaction(Some(CompactionSetup {
                provider: Arc::new(comp_client.with_session_id(env.session_id.as_str())),
                model: model.to_string(),
            }));
        }
    }
    drop(ag);
    // Children spawned after the switch must run on the new model, not the startup one.
    env.delegate_tool.set_provider(client);
    *env.current_provider = provider.to_string();
    *env.current_model = model.to_string();
    let used = env.ui.current_context_used();
    env.ui.set_status(model, used, ctx_max);

    let ui_tokens = env.ui.clone();
    let agent_tokens = env.agent.clone();
    tokio::spawn(async move {
        let est = agent_tokens
            .lock()
            .await
            .estimate_session_tokens()
            .await
            .unwrap_or(0);
        ui_tokens.update_status(est, ctx_max);
    });

    let ui_bg = env.ui.clone();
    let prov_bg = provider.to_string();
    let mod_bg = model.to_string();
    let cred_bg = env.cred_manager.clone();
    let sid_bg = env.session_id.clone();
    tokio::spawn(async move {
        refresh_antigravity_status_if_needed(&ui_bg, &prov_bg, &mod_bg, &cred_bg, Some(&sid_bg))
            .await;
    });

    if persist {
        let store = env.store.clone();
        let session_id = env.session_id.clone();
        let prov = provider.to_string();
        let mod_ = model.to_string();
        tokio::spawn(async move {
            let _ = store.set_model(&session_id, &prov, &mod_).await;
        });
    }
    st.scroll_from_bottom = 0;
    Ok(())
}

/// Apply a provider → model choice to the slot the picker was opened for.
pub async fn apply_model_target(
    env: &mut Env<'_>,
    st: &mut TuiState,
    target: ModelTarget,
    provider: &str,
    model: &str,
) -> Result<(), HarnessError> {
    if st.active_turn.is_some() && !matches!(target, ModelTarget::Worker(_) | ModelTarget::Approval)
    {
        env.ui.set_transient_error(Some(
            "Wait for or interrupt the active turn in this session before changing models.".into(),
        ));
        return Ok(());
    }

    match target {
        ModelTarget::Session => apply_model_selection(env, st, provider, model, true).await,
        ModelTarget::Default { global } => match global {
            Some(is_global) => apply_default_target(env, st, provider, model, is_global).await,
            None => {
                open_model_scope(
                    env,
                    st,
                    ModelTarget::Default { global: None },
                    provider,
                    model,
                );
                Ok(())
            }
        },
        ModelTarget::Compact { global } => match global {
            Some(is_global) => apply_compact_target(env, st, provider, model, is_global).await,
            None => {
                open_model_scope(
                    env,
                    st,
                    ModelTarget::Compact { global: None },
                    provider,
                    model,
                );
                Ok(())
            }
        },
        ModelTarget::BgSaver => {
            crate::commands::chat::commands::apply_bgsaver_model_choice(
                &env.ui,
                env.config,
                &env.cred_manager,
                &env.discovery,
                &env.agent,
                env.feature_toggles,
                env.current_provider,
                env.current_model,
                env.session_id,
                provider.to_string(),
                model.to_string(),
            )
            .await
        }
        ModelTarget::Approval => apply_approval_model_choice(
            env,
            st,
            provider,
            model,
            env.config.tools.approval_reasoning_effort,
        ),
        ModelTarget::Worker(name) => apply_worker_target(env, st, &name, provider, model),
    }
    .map(|_| ())
}
