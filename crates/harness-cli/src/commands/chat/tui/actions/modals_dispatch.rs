//! modals_dispatch

use super::*;

pub async fn run_select_action(
    env: &mut Env<'_>,
    st: &mut TuiState,
    action: SelectAction,
    choice: usize,
) {
    match action {
        SelectAction::SettingsMain => match choice {
            0 => {
                let items = vec!["Dark Mode (default)".to_string(), "Light Mode".to_string()];
                let selected = match env.config.display.theme {
                    harness_core::ThemeMode::Dark => 0,
                    harness_core::ThemeMode::Light => 1,
                };
                st.modal = ActiveModal::SelectModal {
                    title: "Select Theme".to_string(),
                    items,
                    selected,
                    action: SelectAction::SettingsTheme,
                };
            }
            1 => {
                let items: Vec<String> =
                    std::iter::once("Uncapped / Display everything (default)".to_string())
                        .chain((1..=10).map(|n| {
                            if n == 1 {
                                "1 line".to_string()
                            } else {
                                format!("{n} lines")
                            }
                        }))
                        .collect();
                let selected = env.config.display.block_limit.unwrap_or(0);
                st.modal = ActiveModal::SelectModal {
                    title: "Select Display Block Limit".to_string(),
                    items,
                    selected,
                    action: SelectAction::SettingsBlockLimit,
                };
            }
            2..=8 => toggle_settings_main_item(env, st, choice).await,
            9 => open_provider_picker_for(env, st, ModelTarget::Approval).await,
            10 => open_settings_voice_menu(env, st, 0),
            _ => {
                let summary = format_settings_summary(env.config, Some(env.feature_toggles));
                show_text_modal(st, "Current CLI Settings", &summary);
            }
        },
        SelectAction::SettingsVoice => match choice {
            1 => open_settings_voice_list_menu(env, st),
            7 => open_settings_menu(env, st, 10),
            _ => {
                let prev_provider = env.config.voice.provider.clone();
                let arg = match choice {
                    0 => {
                        if env.config.voice.enabled {
                            "voice off"
                        } else {
                            "voice on"
                        }
                    }
                    2 => {
                        if env.config.voice.auto_send {
                            "voice auto-send off"
                        } else {
                            "voice auto-send on"
                        }
                    }
                    3 => "voice gemini",
                    4 => "voice antigravity",
                    5 => "voice openai",
                    6 => "voice groq",
                    7 => {
                        if env.config.voice.enable_barge_in {
                            "voice barge off"
                        } else {
                            "voice barge on"
                        }
                    }
                    _ => "voice show",
                };
                if let Err(error) = handle_settings_command(
                    None,
                    &env.ui,
                    env.config,
                    &env.display_settings,
                    &mut st.process_expanded,
                    Some(arg),
                )
                .await
                {
                    env.ui
                        .set_transient_error(Some(format!("Settings failed: {error}")));
                } else {
                    st.voice_auto_send = env.config.voice.auto_send;
                    let provider_changed = env.config.voice.provider != prev_provider;
                    if env.config.voice.enabled && (!st.voice_active || provider_changed) {
                        activate_voice(env, st).await;
                    } else if !env.config.voice.enabled && st.voice_active {
                        deactivate_voice(env, st).await;
                    }
                }
                open_settings_voice_menu(env, st, choice);
            }
        },
        SelectAction::SettingsVoiceList => {
            let catalog = crate::commands::voice::available_voices();
            if choice == catalog.len() {
                st.modal = ActiveModal::InputModal {
                    title: "Custom Voice ID".to_string(),
                    prompt: "Enter any voice ID (e.g. voice_..., ja-JP-NanamiNeural, Aoede):"
                        .to_string(),
                    value: env.config.voice.voice_name.clone(),
                    secret: false,
                    action: InputAction::VoiceCustomName,
                };
                return;
            }
            if choice > catalog.len() {
                open_settings_voice_menu(env, st, 1);
                return;
            }
            if let Some(entry) = catalog.get(choice) {
                let arg = format!("voice name {}", entry.id);
                if let Err(error) = handle_settings_command(
                    None,
                    &env.ui,
                    env.config,
                    &env.display_settings,
                    &mut st.process_expanded,
                    Some(&arg),
                )
                .await
                {
                    env.ui
                        .set_transient_error(Some(format!("Settings failed: {error}")));
                } else {
                    crate::commands::voice::live_tts::drop_live_tts_session();
                    st.voice_speech_queue.interrupt();
                }
            }
            open_settings_voice_menu(env, st, 1);
        }
        SelectAction::SettingsTheme => {
            let arg = if choice == 1 {
                "theme light"
            } else {
                "theme dark"
            };
            if let Err(error) = handle_settings_command(
                None,
                &env.ui,
                env.config,
                &env.display_settings,
                &mut st.process_expanded,
                Some(arg),
            )
            .await
            {
                env.ui
                    .set_transient_error(Some(format!("Settings failed: {error}")));
            } else {
                crate::commands::chat::tui::theme::set_active_theme(env.config.display.theme);
                st.settled_cache.clear();
                st.live_content_cache = Default::default();
                st.live_reasoning_cache = Default::default();
                open_settings_menu(env, st, 0);
            }
        }
        SelectAction::SettingsBlockLimit => {
            let arg = if choice == 0 {
                "limit all".to_string()
            } else {
                format!("limit {choice}")
            };
            if let Err(error) = handle_settings_command(
                None,
                &env.ui,
                env.config,
                &env.display_settings,
                &mut st.process_expanded,
                Some(&arg),
            )
            .await
            {
                env.ui
                    .set_transient_error(Some(format!("Settings failed: {error}")));
            }
        }
        SelectAction::DirScope { path } => {
            let is_global = choice == 1;
            if let Err(error) =
                apply_dir_choice(&env.ui, env.config, &env.agent, &path, is_global).await
            {
                env.ui
                    .set_transient_error(Some(format!("Workspace change failed: {error}")));
            }
        }
        SelectAction::ModelScope {
            target,
            provider,
            model,
        } => {
            let is_global = choice == 1;
            let result = match target {
                ModelTarget::Default { .. } => {
                    apply_default_target(env, st, &provider, &model, is_global).await
                }
                ModelTarget::Compact { .. } => {
                    apply_compact_target(env, st, &provider, &model, is_global).await
                }
                _ => Ok(()),
            };
            if let Err(error) = result {
                env.ui
                    .set_transient_error(Some(format!("Model switch failed: {error}")));
            }
        }
        SelectAction::ConnectMenu => match choice {
            0 => open_connect_add_provider(env, st).await,
            1 => {
                let text = list_keys_text(&env.cred_manager).await;
                show_text_modal(st, "Connected Providers", &text);
            }
            2 => open_connect_priority_provider(env, st).await,
            3 => open_connect_remove_provider(env, st).await,
            4 => {
                let result =
                    crate::commands::chat::connect::import_antigravity(&env.ui, &env.cred_manager)
                        .await;
                finish_connect_action(env, st, result).await;
            }
            5 => {
                let result =
                    crate::commands::chat::connect::import_opencode(&env.ui, &env.cred_manager)
                        .await;
                finish_connect_action(env, st, result).await;
            }
            6 => {
                let result =
                    crate::commands::chat::connect::import_codex(&env.ui, &env.cred_manager).await;
                finish_connect_action(env, st, result).await;
            }
            7 => open_connect_strategy_provider(env, st).await,
            8 => open_connect_reset_cooldowns(env, st).await,
            _ => {}
        },
        SelectAction::ConnectAddProvider { ids } => {
            if let Some(provider) = ids.get(choice) {
                if provider == "custom" {
                    st.modal = ActiveModal::InputModal {
                        title: "Add Custom Provider — Name".to_string(),
                        prompt: "Custom provider name (e.g. coarena, vllm, local)".to_string(),
                        value: String::new(),
                        secret: false,
                        action: InputAction::CustomProviderName,
                    };
                } else {
                    st.modal = ActiveModal::InputModal {
                        title: "Add Provider — Label".to_string(),
                        prompt: format!("Credential label (blank uses '{provider}-primary')"),
                        value: String::new(),
                        secret: false,
                        action: InputAction::ApiKeyLabel {
                            provider: provider.clone(),
                        },
                    };
                }
            }
        }
        SelectAction::ConnectRemoveProvider { ids } => {
            if let Some(provider) = ids.get(choice) {
                open_connect_remove_credential(env, st, provider.clone()).await;
            }
        }
        SelectAction::ConnectRemoveCredential { provider, ids } => {
            let Some(id) = ids.get(choice) else {
                return;
            };
            match env.cred_manager.remove_credential(&provider, id).await {
                Ok(true) => {
                    env.ui
                        .emit_above(&format!("✔ Credential removed from {provider}"));
                    let ui_bg = env.ui.clone();
                    let prov_bg = env.current_provider.clone();
                    let mod_bg = env.current_model.clone();
                    let cred_bg = env.cred_manager.clone();
                    let sid_bg = env.session_id.clone();
                    tokio::spawn(async move {
                        refresh_antigravity_status_if_needed(
                            &ui_bg,
                            &prov_bg,
                            &mod_bg,
                            &cred_bg,
                            Some(&sid_bg),
                        )
                        .await;
                    });
                }
                Ok(false) => env
                    .ui
                    .set_transient_error(Some("Credential not found".to_string())),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Remove failed: {error}"))),
            }
            open_connect_menu(st);
        }
        SelectAction::ConnectPriorityProvider { ids } => {
            if let Some(provider) = ids.get(choice) {
                open_connect_priority_credential(env, st, provider.clone()).await;
            }
        }
        SelectAction::ConnectPriorityCredential { provider, ids } => {
            let Some(id) = ids.get(choice).cloned() else {
                return;
            };
            let creds = env.cred_manager.get_credentials(&provider).await;
            let label = creds
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.label.clone())
                .unwrap_or_else(|| id.clone());

            let has_override = env
                .cred_manager
                .get_session_priority_credential(env.session_id, &provider)
                .is_some();

            let mut items = vec![
                "This session only".to_string(),
                "Globally (default for all sessions)".to_string(),
            ];
            if has_override {
                items.push("Clear session priority (use global default)".to_string());
            }

            st.modal = ActiveModal::SelectModal {
                title: format!("Prioritize {label} ({provider}) for"),
                items,
                selected: 0,
                action: SelectAction::ConnectPriorityScope {
                    provider,
                    id,
                    has_override,
                },
            };
        }
        SelectAction::ConnectPriorityScope {
            provider,
            id,
            has_override,
        } => {
            match choice {
                0 => {
                    // This session only
                    env.cred_manager.set_session_priority_credential(
                        env.session_id,
                        &provider,
                        &id,
                    );
                    let _ = env
                        .store
                        .set_credential_priority(env.session_id, &provider, &id)
                        .await;
                    let current = env.cred_manager.get_rotation_strategy(&provider).await;
                    if current != RotationStrategy::FillFirst && current != RotationStrategy::Never
                    {
                        let _ = env
                            .cred_manager
                            .set_rotation_strategy(&provider, RotationStrategy::FillFirst)
                            .await;
                    }
                    env.ui.emit_above(&format!(
                        "✔ Priority credential set for this session ({provider})"
                    ));
                    let ui_bg = env.ui.clone();
                    let prov_bg = env.current_provider.clone();
                    let mod_bg = env.current_model.clone();
                    let cred_bg = env.cred_manager.clone();
                    let sid_bg = env.session_id.clone();
                    tokio::spawn(async move {
                        refresh_antigravity_status_if_needed(
                            &ui_bg,
                            &prov_bg,
                            &mod_bg,
                            &cred_bg,
                            Some(&sid_bg),
                        )
                        .await;
                    });
                }
                1 => {
                    // Globally (all sessions)
                    match env
                        .cred_manager
                        .set_priority_credential(&provider, &id)
                        .await
                    {
                        Ok(true) => {
                            env.cred_manager
                                .clear_session_priority_credential(env.session_id, &provider);
                            let _ = env
                                .store
                                .clear_credential_priority(env.session_id, &provider)
                                .await;
                            let current = env.cred_manager.get_rotation_strategy(&provider).await;
                            if current != RotationStrategy::FillFirst
                                && current != RotationStrategy::Never
                            {
                                let _ = env
                                    .cred_manager
                                    .set_rotation_strategy(&provider, RotationStrategy::FillFirst)
                                    .await;
                            }
                            env.ui.emit_above(&format!(
                                "✔ Priority credential set globally for {provider}"
                            ));
                            let ui_bg = env.ui.clone();
                            let prov_bg = env.current_provider.clone();
                            let mod_bg = env.current_model.clone();
                            let cred_bg = env.cred_manager.clone();
                            let sid_bg = env.session_id.clone();
                            tokio::spawn(async move {
                                refresh_antigravity_status_if_needed(
                                    &ui_bg,
                                    &prov_bg,
                                    &mod_bg,
                                    &cred_bg,
                                    Some(&sid_bg),
                                )
                                .await;
                            });
                        }
                        Ok(false) => env.ui.set_transient_error(Some(
                            "Failed to set priority credential".to_string(),
                        )),
                        Err(error) => env
                            .ui
                            .set_transient_error(Some(format!("Priority failed: {error}"))),
                    }
                }
                2 if has_override => {
                    // Clear session priority
                    env.cred_manager
                        .clear_session_priority_credential(env.session_id, &provider);
                    let _ = env
                        .store
                        .clear_credential_priority(env.session_id, &provider)
                        .await;
                    env.ui.emit_above(&format!(
                        "✔ Cleared session priority override for {provider} (following global default)"
                    ));
                    let ui_bg = env.ui.clone();
                    let prov_bg = env.current_provider.clone();
                    let mod_bg = env.current_model.clone();
                    let cred_bg = env.cred_manager.clone();
                    let sid_bg = env.session_id.clone();
                    tokio::spawn(async move {
                        refresh_antigravity_status_if_needed(
                            &ui_bg,
                            &prov_bg,
                            &mod_bg,
                            &cred_bg,
                            Some(&sid_bg),
                        )
                        .await;
                    });
                }
                _ => {}
            }
            open_connect_menu(st);
        }
        SelectAction::ConnectStrategyProvider { ids } => {
            if let Some(provider) = ids.get(choice) {
                open_connect_strategy(st, provider.clone());
            }
        }
        SelectAction::ConnectStrategy { provider } => {
            let strategies = connect_strategies();
            let Some((label, strategy)) = strategies.get(choice) else {
                return;
            };
            match env
                .cred_manager
                .set_rotation_strategy(&provider, *strategy)
                .await
            {
                Ok(()) => {
                    let name = label.split(' ').next().unwrap_or("");
                    env.ui
                        .emit_above(&format!("✔ Rotation strategy for {provider} set to {name}"));
                }
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Strategy failed: {error}"))),
            }
            open_connect_menu(st);
        }
        SelectAction::ConnectResetCooldowns { ids } => {
            let target = if choice == 0 {
                None
            } else {
                let Some(provider) = ids.get(choice - 1) else {
                    return;
                };
                Some(provider.clone())
            };
            match env.cred_manager.reset_cooldowns(target.as_deref()).await {
                Ok(count) => {
                    env.ui
                        .emit_above(&format!("✔ {count} credential(s) re-enabled"));
                    let ui_bg = env.ui.clone();
                    let prov_bg = env.current_provider.clone();
                    let mod_bg = env.current_model.clone();
                    let cred_bg = env.cred_manager.clone();
                    let sid_bg = env.session_id.clone();
                    tokio::spawn(async move {
                        refresh_antigravity_status_if_needed(
                            &ui_bg,
                            &prov_bg,
                            &mod_bg,
                            &cred_bg,
                            Some(&sid_bg),
                        )
                        .await;
                    });
                }
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Reset failed: {error}"))),
            }
            open_connect_menu(st);
        }
        SelectAction::ProfileDeleteConfirm { name } => {
            if choice == 0 {
                match crate::commands::profile::delete_profile(
                    &name,
                    &env.config.paths.profiles_dir,
                ) {
                    Ok(()) => env.ui.emit_above(&format!("✔ Deleted {name}")),
                    Err(error) => env
                        .ui
                        .set_transient_error(Some(format!("Delete failed: {error}"))),
                }
            } else {
                env.ui.emit_above("Cancelled.");
            }
        }
        SelectAction::SkillPinSelect { skills } => {
            if let Some(skill) = skills.get(choice) {
                st.modal = ActiveModal::SelectModal {
                    title: format!("Pin '{}'", skill),
                    items: vec![
                        "Pin to this session only".to_string(),
                        "Pin globally (all sessions)".to_string(),
                    ],
                    selected: 0,
                    action: SelectAction::SkillPinScope {
                        skill: skill.clone(),
                    },
                };
            }
        }
        SelectAction::SkillPinScope { skill } => {
            let is_global = choice == 1;
            if is_global {
                if !env
                    .config
                    .agent
                    .pinned_skills
                    .iter()
                    .any(|s| harness_plugins::names_match(s, &skill))
                {
                    env.config.agent.pinned_skills.push(skill.clone());
                    let path = harness_core::config::config_file_path();
                    if let Ok(yaml) = serde_yaml::to_string(env.config) {
                        let _ = std::fs::write(path, yaml);
                    }
                }
                env.ui
                    .emit_above(&format!("✔ Pinned '{}' globally.", skill));
            } else {
                let mut session_pinned = env
                    .store
                    .get_pinned_skills(env.session_id)
                    .await
                    .unwrap_or_default();
                if !session_pinned
                    .iter()
                    .any(|s| harness_plugins::names_match(s, &skill))
                {
                    session_pinned.push(skill.clone());
                }
                let _ = env
                    .store
                    .set_pinned_skills(env.session_id, &session_pinned)
                    .await;
                st.session_pinned_skills = session_pinned;
                env.ui
                    .emit_above(&format!("✔ Pinned '{}' to this session.", skill));
            }

            // Refresh active agent
            let session_pinned = env
                .store
                .get_pinned_skills(env.session_id)
                .await
                .unwrap_or_default();
            let mut combined = session_pinned;
            for g in &env.config.agent.pinned_skills {
                if !combined.iter().any(|s| harness_plugins::names_match(s, g)) {
                    combined.push(g.clone());
                }
            }
            let skills_slice: &[harness_plugins::Skill] =
                env.skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);
            let resolved = harness_plugins::resolve_pinned_instructions(
                &combined,
                skills_slice,
                Some(&env.config.paths.skills_dir),
            );
            let mut ag = env.agent.lock().await;
            ag.set_pinned_skills(combined);
            ag.set_pinned_skill_instructions(resolved);
        }
        SelectAction::ProfileCreateProviders { name, summary } => {
            let copy_providers = choice == 0;
            match create_profile(env, &name, summary, copy_providers) {
                Ok(dest) => {
                    env.ui.emit_above(&format!("✔ Created {}", dest.display()));
                    env.ui.emit_above(&format!(
                        "Open the new profile with `luna {name}` or `{name}`."
                    ));
                }
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Create failed: {error}"))),
            }
        }
        SelectAction::TaskList { tasks } => {
            let Some((id, log_path)) = tasks.get(choice) else {
                return;
            };
            let body = std::fs::read_to_string(log_path)
                .unwrap_or_else(|_| format!("No log yet for {id}."));
            show_text_modal(st, &format!("Task {id}"), &body);
        }
        SelectAction::MemoryProject { projects } => {
            if choice == 0 {
                st.memory_project_filter = None;
                env.agent.lock().await.set_memory_project_filter(None);
                st.system_notices.push(SystemNotice::new(
                    "✓ Memory scope reset to all projects (default)",
                ));
            } else if choice <= projects.len() {
                let project = projects[choice - 1].clone();
                st.memory_project_filter = Some(project.clone());
                env.agent
                    .lock()
                    .await
                    .set_memory_project_filter(Some(project.clone()));
                st.system_notices.push(SystemNotice::new(format!(
                    "✓ Memory scoped to '{project}' for this session"
                )));
            } else {
                st.modal = ActiveModal::InputModal {
                    title: "Filter Memory by Project".to_string(),
                    prompt: "Project name / slug (blank resets to all projects)".to_string(),
                    value: st.memory_project_filter.clone().unwrap_or_default(),
                    secret: false,
                    action: InputAction::MemoryCustomProject,
                };
            }
        }
    }
}

pub(crate) async fn finish_connect_action(
    env: &mut Env<'_>,
    st: &mut TuiState,
    result: Result<(), HarnessError>,
) {
    if let Err(error) = result {
        env.ui
            .set_transient_error(Some(format!("Provider setup failed: {error}")));
    }
    let ui_bg = env.ui.clone();
    let prov_bg = env.current_provider.clone();
    let mod_bg = env.current_model.clone();
    let cred_bg = env.cred_manager.clone();
    let sid_bg = env.session_id.clone();
    tokio::spawn(async move {
        refresh_antigravity_status_if_needed(&ui_bg, &prov_bg, &mod_bg, &cred_bg, Some(&sid_bg))
            .await;
    });
    open_connect_menu(st);
}

pub(crate) fn create_profile(
    env: &Env<'_>,
    name: &str,
    summary: String,
    copy_providers: bool,
) -> Result<std::path::PathBuf, HarnessError> {
    let seed = harness_core::profiles::default_profile()?;
    crate::commands::profile::create_profile_with(
        &env.config.paths.profiles_dir,
        env.config,
        name,
        summary,
        seed.persona,
        seed.behavior,
        copy_providers,
        &[],
        &[],
    )
}

/// Execute a value submitted from an [`ActiveModal::InputModal`].
pub async fn run_input_action(
    env: &mut Env<'_>,
    st: &mut TuiState,
    action: InputAction,
    value: String,
) {
    match action {
        InputAction::DirPath { scope } => {
            let path = value.trim();
            if path.is_empty() {
                env.ui.emit_above("Workspace unchanged");
                return;
            }
            let expanded = harness_core::config::expand_tilde(path);
            if !std::path::PathBuf::from(expanded).is_dir() {
                env.ui
                    .set_transient_error(Some(format!("Directory not found: {path}")));
                open_dir_input(st, scope);
                return;
            }
            match scope {
                Some(is_global) => {
                    if let Err(error) =
                        apply_dir_choice(&env.ui, env.config, &env.agent, path, is_global).await
                    {
                        env.ui
                            .set_transient_error(Some(format!("Workspace change failed: {error}")));
                    }
                }
                None => open_dir_scope(st, path.to_string()),
            }
        }
        InputAction::ApiKeyLabel { provider } => {
            let label = if value.trim().is_empty() {
                format!("{provider}-primary")
            } else {
                value.trim().to_string()
            };
            st.modal = ActiveModal::InputModal {
                title: "Add Provider — API Key".to_string(),
                prompt: format!("API key for {provider} (masked)"),
                value: String::new(),
                secret: true,
                action: InputAction::ApiKeySecret { provider, label },
            };
        }
        InputAction::ApiKeySecret { provider, label } => {
            if value.is_empty() {
                env.ui.emit_above("No key entered — nothing added");
                open_connect_menu(st);
                return;
            }
            match env
                .cred_manager
                .add_credential(&provider, &label, &value, true)
                .await
            {
                Ok(id) => env.ui.emit_above(&format!(
                    "✔ Credential '{label}' added for {provider} ({id})"
                )),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Add credential failed: {error}"))),
            }
            let ui_bg = env.ui.clone();
            let prov_bg = env.current_provider.clone();
            let mod_bg = env.current_model.clone();
            let cred_bg = env.cred_manager.clone();
            let sid_bg = env.session_id.clone();
            tokio::spawn(async move {
                refresh_antigravity_status_if_needed(
                    &ui_bg,
                    &prov_bg,
                    &mod_bg,
                    &cred_bg,
                    Some(&sid_bg),
                )
                .await;
            });
            st.provider_items = build_provider_items(
                env.cred_manager.clone(),
                Some(&env.config.providers),
                env.current_provider,
            )
            .await;
            open_connect_menu(st);
        }
        InputAction::CustomProviderName => {
            let name = value.trim().to_lowercase();
            if name.is_empty() {
                env.ui.emit_above("No provider name entered — cancelled");
                open_connect_menu(st);
                return;
            }
            if !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                env.ui.set_transient_error(Some(
                    "Provider name may only contain alphanumeric characters, hyphens, and underscores"
                        .to_string(),
                ));
                st.modal = ActiveModal::InputModal {
                    title: "Add Custom Provider — Name".to_string(),
                    prompt: "Invalid characters. Use alphanumeric, hyphen, underscore:".to_string(),
                    value: name,
                    secret: false,
                    action: InputAction::CustomProviderName,
                };
                return;
            }
            st.modal = ActiveModal::InputModal {
                title: format!("Add Custom Provider [{name}] — Endpoint"),
                prompt: format!("Base URL endpoint for {name} (e.g. https://api.example.com/v1)"),
                value: "http://localhost:8000/v1".to_string(),
                secret: false,
                action: InputAction::CustomProviderEndpoint { provider: name },
            };
        }
        InputAction::CustomProviderEndpoint { provider } => {
            let endpoint = value.trim().to_string();
            if endpoint.is_empty() {
                env.ui.emit_above("No endpoint entered — cancelled");
                open_connect_menu(st);
                return;
            }
            st.modal = ActiveModal::InputModal {
                title: format!("Add Custom Provider [{provider}] — API Key"),
                prompt: format!("API key for {provider} (masked, leave blank if unauthenticated)"),
                value: String::new(),
                secret: true,
                action: InputAction::CustomProviderApiKey { provider, endpoint },
            };
        }
        InputAction::CustomProviderApiKey { provider, endpoint } => {
            let key = if value.trim().is_empty() {
                "none".to_string()
            } else {
                value.trim().to_string()
            };
            let label = format!("{provider}-primary");

            let layers =
                harness_core::config::process_layers(crate::commands::config::catalog_defaults());
            if let Ok(ref layers) = layers {
                if let Err(err) = harness_core::config::set_in_file(
                    layers,
                    &format!("providers.{provider}.base_url"),
                    &endpoint,
                ) {
                    tracing::warn!(
                        "Failed to persist custom provider base_url to config file: {err}"
                    );
                }
            }

            let override_cfg = harness_core::config::ProviderOverride {
                base_url: Some(endpoint.clone()),
                models_endpoint: None,
                timeout_secs: None,
                context_window: None,
            };
            env.config
                .providers
                .insert(provider.clone(), override_cfg.clone());
            env.discovery.set_override(&provider, override_cfg);

            match env
                .cred_manager
                .add_credential(&provider, &label, &key, true)
                .await
            {
                Ok(id) => {
                    env.ui.emit_above(&format!(
                        "✔ Custom provider '{provider}' ({endpoint}) created with credential '{label}' ({id})"
                    ));
                }
                Err(error) => {
                    env.ui
                        .set_transient_error(Some(format!("Add credential failed: {error}")));
                }
            }

            st.provider_items = build_provider_items(
                env.cred_manager.clone(),
                Some(&env.config.providers),
                env.current_provider,
            )
            .await;

            open_connect_menu(st);
        }
        InputAction::AutoCompactThreshold => {
            match harness_onboarding::compact_step::parse_threshold(&value) {
                Ok(threshold) => {
                    let (_, _, current_percent) = env.agent.lock().await.auto_compact_settings();
                    open_auto_compact_percent(st, threshold, current_percent);
                }
                Err(message) => {
                    let typed = value;
                    env.ui.set_transient_error(Some(message.clone()));
                    st.modal = ActiveModal::InputModal {
                        title: "Auto-compact — Threshold".to_string(),
                        prompt: format!("{message}. Blank keeps the default"),
                        value: typed,
                        secret: false,
                        action: InputAction::AutoCompactThreshold,
                    };
                }
            }
        }
        InputAction::AutoCompactPercent { threshold } => {
            match harness_onboarding::compact_step::parse_percent(&value) {
                Ok(percent) => {
                    if let Err(error) =
                        apply_auto_compact_values(&env.ui, &env.agent, threshold, percent).await
                    {
                        env.ui.set_transient_error(Some(format!(
                            "Auto-compact update failed: {error}"
                        )));
                    }
                }
                Err(message) => {
                    let typed = value;
                    env.ui.set_transient_error(Some(message.clone()));
                    st.modal = ActiveModal::InputModal {
                        title: "Auto-compact — Percentage".to_string(),
                        prompt: format!("{message}. Enter 1–100"),
                        value: typed,
                        secret: false,
                        action: InputAction::AutoCompactPercent { threshold },
                    };
                }
            }
        }
        InputAction::ProfileCreateName => {
            let name = value.trim().to_lowercase();
            if let Err(error) = harness_core::profiles::validate_name(&name) {
                env.ui
                    .set_transient_error(Some(format!("Invalid name: {error}")));
                st.modal = ActiveModal::InputModal {
                    title: "Create Profile — Name".to_string(),
                    prompt: "Lowercase letters, digits and '-'".to_string(),
                    value,
                    secret: false,
                    action: InputAction::ProfileCreateName,
                };
                return;
            }
            open_profile_create_summary(st, name);
        }
        InputAction::ProfileCreateSummary { name } => {
            let summary = if value.trim().is_empty() {
                "Custom profile".to_string()
            } else {
                value.trim().to_string()
            };
            open_profile_create_providers(st, name, summary);
        }
        InputAction::MemoryCustomProject => {
            let trimmed = value.trim();
            if trimmed.is_empty()
                || matches!(
                    trimmed.to_ascii_lowercase().as_str(),
                    "reset" | "all" | "default" | "clear"
                )
            {
                st.memory_project_filter = None;
                env.agent.lock().await.set_memory_project_filter(None);
                st.system_notices.push(SystemNotice::new(
                    "✓ Memory scope reset to all projects (default)",
                ));
            } else {
                let project = trimmed.to_string();
                st.memory_project_filter = Some(project.clone());
                env.agent
                    .lock()
                    .await
                    .set_memory_project_filter(Some(project.clone()));
                st.system_notices.push(SystemNotice::new(format!(
                    "✓ Memory scoped to '{project}' for this session"
                )));
            }
        }
        InputAction::VoiceCustomName => {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                let arg = format!("voice name {trimmed}");
                if let Err(error) = handle_settings_command(
                    None,
                    &env.ui,
                    env.config,
                    &env.display_settings,
                    &mut st.process_expanded,
                    Some(&arg),
                )
                .await
                {
                    env.ui
                        .set_transient_error(Some(format!("Settings failed: {error}")));
                } else {
                    crate::commands::voice::live_tts::drop_live_tts_session();
                    st.voice_speech_queue.interrupt();
                }
            }
            open_settings_voice_menu(env, st, 1);
        }
    }
}

pub(crate) fn open_settings_voice_menu(env: &Env<'_>, st: &mut TuiState, selected: usize) {
    let active_mark = |p: &str| {
        if env.config.voice.provider.eq_ignore_ascii_case(p) {
            " ✓"
        } else {
            ""
        }
    };
    let send_mode = if env.config.voice.auto_send {
        "Auto-send (default)"
    } else {
        "Verify before send"
    };
    let items = vec![
        if env.config.voice.enabled {
            "Disable Voice Mode".to_string()
        } else {
            "Enable Voice Mode".to_string()
        },
        format!("Voice · [{}]", env.config.voice.voice_name),
        format!("Voice Send Mode · [{send_mode}]"),
        format!(
            "Provider: Gemini (Gemini 3.5 Transcribe + Live Voice){}",
            active_mark("gemini")
        ),
        format!(
            "Provider: Antigravity (Google Live Voice){}",
            active_mark("antigravity")
        ),
        format!(
            "Provider: OpenAI (Whisper STT + TTS){}",
            active_mark("openai")
        ),
        format!(
            "Provider: Groq (Whisper STT + Edge TTS){}",
            active_mark("groq")
        ),
        format!(
            "Toggle Barge-in Interrupt · [{}]",
            if env.config.voice.enable_barge_in {
                "Enabled"
            } else {
                "Disabled"
            }
        ),
        "← Back to CLI Settings".to_string(),
    ];
    st.modal = ActiveModal::SelectModal {
        title: "Voice Mode Settings".to_string(),
        items,
        selected: selected.min(7),
        action: SelectAction::SettingsVoice,
    };
}

pub(crate) fn open_settings_voice_list_menu(env: &Env<'_>, st: &mut TuiState) {
    let catalog = crate::commands::voice::available_voices();
    let current = env.config.voice.voice_name.trim();
    let selected = catalog
        .iter()
        .position(|v| v.id.eq_ignore_ascii_case(current))
        .unwrap_or(0);
    let mut items: Vec<String> = catalog
        .iter()
        .map(|v| {
            let mark = if v.id.eq_ignore_ascii_case(current) {
                " ✓"
            } else {
                ""
            };
            format!("[{}] {} — {}{mark}", v.provider_label, v.id, v.description)
        })
        .collect();
    items.push("Custom voice ID...".to_string());
    items.push("← Back to Voice Settings".to_string());
    st.modal = ActiveModal::SelectModal {
        title: format!("Available Voices ({} built-in)", catalog.len()),
        items,
        selected,
        action: SelectAction::SettingsVoiceList,
    };
}

pub(crate) async fn apply_feature_live(env: &Env<'_>, feature: Feature, enabled: bool) {
    env.feature_toggles.set(feature, enabled);
    let registry = {
        let mut ag = env.agent.lock().await;
        match feature {
            Feature::Memory => ag.set_memory_enabled(enabled),
            Feature::Notes => ag.set_notes_enabled(enabled),
            Feature::Skills => ag.set_skills_enabled(enabled),
            Feature::Mcp => ag.set_mcp_enabled(enabled),
        }
        ag.registry().clone()
    };
    if enabled && feature == Feature::Mcp {
        if let Err(err) =
            crate::commands::chat::helpers::ensure_mcp_servers_registered(&registry, env.config)
                .await
        {
            env.ui
                .set_transient_error(Some(format!("Failed to start MCP servers: {err}")));
        }
    }
}

pub(crate) async fn apply_feature_global(
    env: &mut Env<'_>,
    feature: Feature,
    enabled: bool,
    disabled_until: Option<u64>,
) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    let flag_str = if enabled { "true" } else { "false" };
    let (agent_key, until_key) = match feature {
        Feature::Mcp => ("agent.mcp_enabled", "features.mcp_disabled_until"),
        Feature::Skills => ("agent.skills_enabled", "features.skills_disabled_until"),
        Feature::Memory => ("agent.memory_enabled", "features.memory_disabled_until"),
        Feature::Notes => ("agent.notes_enabled", "features.notes_disabled_until"),
    };

    harness_core::config::set_in_file(&layers, agent_key, flag_str)?;
    if feature == Feature::Mcp {
        harness_core::config::set_in_file(&layers, "mcp.enabled", flag_str)?;
    }
    match disabled_until {
        Some(until) => {
            harness_core::config::set_in_file(&layers, until_key, &until.to_string())?;
        }
        None => {
            let _ = harness_core::config::unset_in_file(&layers, until_key)?;
        }
    }

    match feature {
        Feature::Mcp => {
            env.config.mcp.enabled = enabled;
            env.config.agent.mcp_enabled = enabled;
            env.config.features.mcp_disabled_until = disabled_until;
        }
        Feature::Skills => {
            env.config.agent.skills_enabled = enabled;
            env.config.features.skills_disabled_until = disabled_until;
        }
        Feature::Memory => {
            env.config.agent.memory_enabled = enabled;
            env.config.features.memory_disabled_until = disabled_until;
        }
        Feature::Notes => {
            env.config.agent.notes_enabled = enabled;
            env.config.features.notes_disabled_until = disabled_until;
        }
    }

    let now = harness_core::Config::current_unix_secs();
    let effective = enabled && !disabled_until.is_some_and(|u| now < u);
    apply_feature_live(env, feature, effective).await;
    Ok(())
}

pub(crate) async fn apply_memory_mode_choice(
    env: &mut Env<'_>,
    mode: harness_core::MemoryMode,
) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    if mode.is_background() {
        let (memory_dyn, notes_dyn) = {
            let ag = env.agent.lock().await;
            let mem: Arc<dyn MemoryEngine> = ag.memory().clone();
            let notes: Arc<dyn harness_core::NotesEngine> = ag.notes().clone();
            (mem, notes)
        };
        let provider = env
            .config
            .memory
            .background_provider
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(env.current_provider.as_str());
        let model = env
            .config
            .memory
            .background_model
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(env.current_model.as_str());
        let saver = crate::commands::chat::memory_writer::build_saver_for(
            env.config,
            &memory_dyn,
            notes_dyn,
            provider,
            model,
            env.cred_manager.clone(),
            env.discovery.clone(),
        )?;
        harness_core::config::set_in_file(&layers, "memory.mode", mode.as_str())?;
        env.config.memory.mode = mode;
        let mut ag = env.agent.lock().await;
        ag.set_background_saver(Some(Arc::new(saver)));
        ag.set_memory_enabled(env.feature_toggles.enabled(Feature::Memory));
        ag.set_notes_enabled(env.feature_toggles.enabled(Feature::Notes));
    } else {
        harness_core::config::set_in_file(&layers, "memory.mode", mode.as_str())?;
        env.config.memory.mode = mode;
        env.agent.lock().await.set_background_saver(None);
    }
    Ok(())
}

pub async fn apply_thinking_selection(
    env: &Env<'_>,
    st: &mut TuiState,
    effort: Option<ReasoningEffort>,
) {
    if st.active_turn.is_some() {
        env.ui.set_transient_error(Some(
            "Wait for or interrupt the active turn in this session before changing thinking level."
                .into(),
        ));
        return;
    }
    if let Ok(mut ag) = env.agent.try_lock() {
        ag.set_reasoning_effort(effort);
        st.reasoning_effort = effort;
    }
}

pub fn set_all_process_expanded(st: &mut TuiState, expanded: bool) {
    st.process_expanded = expanded;
    st.expanded_turns.clear();
    st.collapsed_turns.clear();
    if expanded {
        st.expanded_thinking.clear();
        st.expanded_live_thinking.clear();
        st.live_thinking_view = LiveThinkingView::Expanded;
    } else {
        st.expanded_thinking.clear();
        st.expanded_tools.clear();
        st.expanded_live_thinking.clear();
        st.expanded_live_tools.clear();
        st.live_thinking_view = LiveThinkingView::Tail;
    }
}

/// Execute a `ctrl+p` Command Palette action.
pub async fn run_palette_action(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    action: PaletteAction,
) -> Result<Flow, HarnessError> {
    match action {
        PaletteAction::NewSession => {
            create_new_session(env, st).await?;
        }
        PaletteAction::SwitchSession => {
            open_session_picker(env, st).await;
        }
        PaletteAction::SwitchModel => {
            open_provider_picker(env, st).await;
        }
        PaletteAction::CycleAgentMode => {
            apply_agent_mode(env, st, st.mode.cycle());
        }
        PaletteAction::SelectThinking => {
            open_thinking_picker(env, st).await;
        }
        PaletteAction::ToggleProcessDetails => {
            set_all_process_expanded(st, !st.process_expanded);
        }
        PaletteAction::ToggleTodoPanel => {
            env.ui.toggle_todo_collapsed();
        }
        PaletteAction::CompactContext => {
            return run_slash_command(env, st, terminal, "/compact", "/compact").await;
        }
        PaletteAction::ShowDiff => {
            return run_slash_command(env, st, terminal, "/diff", "/diff").await;
        }
        PaletteAction::ShowUsage => {
            return run_slash_command(env, st, terminal, "/usage", "/usage").await;
        }
        PaletteAction::ShowBackgroundJobs => {
            return run_slash_command(env, st, terminal, "/jobs", "/jobs").await;
        }
        PaletteAction::ShowHelp => {
            st.modal = ActiveModal::Help { scroll: 0 };
        }
        PaletteAction::SlashCommand(cmd) => {
            return run_slash_command(env, st, terminal, &cmd, &cmd).await;
        }
        PaletteAction::Quit => return Ok(Flow::Break),
    }
    Ok(Flow::Continue)
}
