//! settings

use super::*;

pub async fn toggle_settings_main_item(env: &mut Env<'_>, st: &mut TuiState, choice: usize) {
    match choice {
        0 => {
            let next = match env.config.display.theme {
                harness_core::ThemeMode::Dark => "theme light",
                harness_core::ThemeMode::Light => "theme dark",
            };
            if let Err(error) = handle_settings_command(
                None,
                &env.ui,
                env.config,
                &env.display_settings,
                &mut st.process_expanded,
                Some(next),
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
            }
            open_settings_menu(env, st, 0);
        }
        1 => {
            let next = match env.config.display.block_limit {
                None => Some(1),
                Some(n) if n < 10 => Some(n + 1),
                Some(_) => None,
            };
            let arg = match next {
                None => "limit all".to_string(),
                Some(n) => format!("limit {n}"),
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
            st.block_limit = env.config.display.block_limit;
            open_settings_menu(env, st, 1);
        }
        2 => {
            let arg = if env.config.display.auto_collapse {
                "collapse off"
            } else {
                "collapse on"
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
                set_all_process_expanded(st, !env.config.display.auto_collapse);
            }
            open_settings_menu(env, st, 2);
        }
        3 => {
            let arg = if env.config.display.auto_hide_thinking {
                "thinking off"
            } else {
                "thinking on"
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
            }
            st.auto_hide_thinking = env.config.display.auto_hide_thinking;
            open_settings_menu(env, st, 3);
        }
        4..=7 => {
            let feature = match choice {
                4 => Feature::Mcp,
                5 => Feature::Skills,
                6 => Feature::Memory,
                _ => Feature::Notes,
            };
            let live_on = env.feature_toggles.enabled(feature);
            let (global_on, until) = match feature {
                Feature::Mcp => (
                    env.config.agent.mcp_enabled,
                    env.config.features.mcp_disabled_until,
                ),
                Feature::Skills => (
                    env.config.agent.skills_enabled,
                    env.config.features.skills_disabled_until,
                ),
                Feature::Memory => (
                    env.config.agent.memory_enabled,
                    env.config.features.memory_disabled_until,
                ),
                Feature::Notes => (
                    env.config.agent.notes_enabled,
                    env.config.features.notes_disabled_until,
                ),
            };
            let timed_off = until.is_some_and(|u| u > harness_core::Config::current_unix_secs());

            if live_on && global_on && !timed_off {
                apply_feature_live(env, feature, false).await;
                notify_setting_change(
                    st,
                    format!("{} turned OFF for this session", feature.title()),
                );
            } else if !live_on && global_on && !timed_off {
                if let Err(err) = apply_feature_global(env, feature, false, None).await {
                    env.ui
                        .set_transient_error(Some(format!("Settings failed: {err}")));
                } else {
                    notify_setting_change(st, format!("{} turned OFF globally", feature.title()));
                }
            } else if let Err(err) = apply_feature_global(env, feature, true, None).await {
                env.ui
                    .set_transient_error(Some(format!("Settings failed: {err}")));
            } else {
                notify_setting_change(st, format!("{} turned ON", feature.title()));
            }
            open_settings_menu(env, st, choice);
        }
        8 => {
            let next_mode = if env.config.memory.mode.is_background() {
                harness_core::MemoryMode::Session
            } else {
                harness_core::MemoryMode::Background
            };
            let label = match next_mode {
                harness_core::MemoryMode::Session => "Session writer",
                harness_core::MemoryMode::Background => "Background saver",
            };
            if let Err(err) = apply_memory_mode_choice(env, next_mode).await {
                env.ui
                    .set_transient_error(Some(format!("Memory saver switch failed: {err}")));
            } else {
                notify_setting_change(st, format!("Memory saver set to {label} (saved)"));
            }
            open_settings_menu(env, st, 8);
        }
        9 => {
            if let Err(err) = reset_approval_model_choice(env, st) {
                env.ui
                    .set_transient_error(Some(format!("Approval model reset failed: {err}")));
                open_settings_menu(env, st, 9);
            }
        }
        10 => {
            let next = if env.config.voice.enabled {
                "voice off"
            } else {
                "voice on"
            };
            if let Err(error) = handle_settings_command(
                None,
                &env.ui,
                env.config,
                &env.display_settings,
                &mut st.process_expanded,
                Some(next),
            )
            .await
            {
                env.ui
                    .set_transient_error(Some(format!("Voice settings failed: {error}")));
            }
            open_settings_menu(env, st, 10);
        }
        _ => {
            open_settings_menu(env, st, choice);
        }
    }
}

pub(crate) async fn open_memory_project_picker(st: &mut TuiState) {
    let projects = st
        .template_agent
        .memory_engine()
        .list_projects()
        .await
        .unwrap_or_default();
    let active = st.memory_project_filter.as_deref();
    let mut items = Vec::with_capacity(projects.len() + 2);
    let default_mark = if active.is_none() { "● " } else { "" };
    items.push(format!("{default_mark}★ All projects (default / reset)"));
    for proj in &projects {
        let mark = if active == Some(proj.as_str()) {
            "● "
        } else {
            "  "
        };
        items.push(format!("{mark}{proj}"));
    }
    items.push("✎ Custom project name…".to_string());
    st.modal = ActiveModal::SelectModal {
        title: "Select Project Memory Scope".to_string(),
        items,
        selected: 0,
        action: SelectAction::MemoryProject { projects },
    };
}

pub(crate) fn open_dir_input(st: &mut TuiState, scope: Option<bool>) {
    st.modal = ActiveModal::InputModal {
        title: "Change Workspace".to_string(),
        prompt: "Path to use as the workspace root (blank cancels)".to_string(),
        value: String::new(),
        secret: false,
        action: InputAction::DirPath { scope },
    };
}

pub(crate) fn open_dir_scope(st: &mut TuiState, path: String) {
    st.modal = ActiveModal::SelectModal {
        title: "Workspace Scope".to_string(),
        items: vec![
            "This session only".to_string(),
            "Globally (persist for all sessions)".to_string(),
        ],
        selected: 0,
        action: SelectAction::DirScope { path },
    };
}

pub(crate) fn open_connect_menu(st: &mut TuiState) {
    const ITEMS: [&str; 10] = [
        "Add provider API key",
        "List connected providers",
        "Set priority credential",
        "Remove a credential",
        "Import Antigravity / Google",
        "Import OpenCode / Shiina",
        "Import Codex / ChatGPT",
        "Set rotation strategy",
        "Reset cooldowns",
        "Back to chat",
    ];
    st.modal = ActiveModal::SelectModal {
        title: "Connect a provider".to_string(),
        items: ITEMS.iter().map(|item| item.to_string()).collect(),
        selected: 0,
        action: SelectAction::ConnectMenu,
    };
}

pub(crate) async fn open_connect_add_provider(env: &Env<'_>, st: &mut TuiState) {
    let providers = provider_items(&env.cred_manager, Some(&env.config.providers)).await;
    let ids: Vec<String> = providers
        .iter()
        .map(|(label, _)| provider_id_of(label).to_string())
        .collect();
    st.modal = ActiveModal::SelectModal {
        title: "Select provider".to_string(),
        items: providers.into_iter().map(|(label, _)| label).collect(),
        selected: 0,
        action: SelectAction::ConnectAddProvider { ids },
    };
}

pub(crate) async fn open_connect_remove_provider(env: &Env<'_>, st: &mut TuiState) {
    let ids = connected_providers(&env.cred_manager).await;
    if ids.is_empty() {
        env.ui
            .emit_above("Nothing to remove — no providers connected");
        return;
    }
    st.modal = ActiveModal::SelectModal {
        title: "Provider to remove from".to_string(),
        items: ids.clone(),
        selected: 0,
        action: SelectAction::ConnectRemoveProvider { ids },
    };
}

pub(crate) async fn open_connect_remove_credential(env: &Env<'_>, st: &mut TuiState, provider: String) {
    let creds = env.cred_manager.get_credentials(&provider).await;
    if creds.is_empty() {
        env.ui
            .emit_above(&format!("No credentials found for {provider}"));
        open_connect_menu(st);
        return;
    }
    let ids: Vec<String> = creds.iter().map(|cred| cred.id.clone()).collect();
    let items: Vec<String> = creds
        .iter()
        .map(|cred| format!("{} (used {})", cred.label, cred.usage_count))
        .collect();
    st.modal = ActiveModal::SelectModal {
        title: format!("Credential of {provider}"),
        items,
        selected: 0,
        action: SelectAction::ConnectRemoveCredential { provider, ids },
    };
}

pub(crate) async fn open_connect_priority_provider(env: &Env<'_>, st: &mut TuiState) {
    let ids = connected_providers(&env.cred_manager).await;
    if ids.is_empty() {
        env.ui
            .emit_above("Nothing to prioritize — no providers connected");
        return;
    }
    if ids.len() == 1 {
        let provider = ids[0].clone();
        open_connect_priority_credential(env, st, provider).await;
        return;
    }
    st.modal = ActiveModal::SelectModal {
        title: "Provider to set priority for".to_string(),
        items: ids.clone(),
        selected: 0,
        action: SelectAction::ConnectPriorityProvider { ids },
    };
}

pub(crate) async fn open_connect_priority_credential(
    env: &Env<'_>,
    st: &mut TuiState,
    provider: String,
) {
    let creds = env.cred_manager.get_credentials(&provider).await;
    if creds.is_empty() {
        env.ui
            .emit_above(&format!("No credentials found for {provider}"));
        open_connect_menu(st);
        return;
    }
    let session_override = env
        .cred_manager
        .get_session_priority_credential(env.session_id, &provider);
    let mut selected = 0;
    let ids: Vec<String> = creds.iter().map(|cred| cred.id.clone()).collect();
    let items: Vec<String> = creds
        .iter()
        .enumerate()
        .map(|(index, cred)| {
            let is_session_prio = session_override.as_deref() == Some(&cred.id)
                || session_override.as_deref() == Some(&cred.label);
            if is_session_prio || (session_override.is_none() && cred.is_priority) {
                selected = index;
            }
            let mut tags = Vec::new();
            if is_session_prio {
                tags.push("session priority");
            }
            if cred.is_priority {
                tags.push(if session_override.is_some() {
                    "global priority"
                } else {
                    "current priority"
                });
            }
            if cred.is_temporarily_disabled {
                tags.push("cooldown");
            }
            let tag_str = if tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", tags.join(", "))
            };
            format!("{} (used {}){}", cred.label, cred.usage_count, tag_str)
        })
        .collect();
    st.modal = ActiveModal::SelectModal {
        title: format!("Credential of {provider}"),
        items,
        selected,
        action: SelectAction::ConnectPriorityCredential { provider, ids },
    };
}

pub(crate) async fn open_connect_strategy_provider(env: &Env<'_>, st: &mut TuiState) {
    let ids = connected_providers(&env.cred_manager).await;
    if ids.is_empty() {
        env.ui.emit_above("No providers connected yet");
        return;
    }
    st.modal = ActiveModal::SelectModal {
        title: "Provider to configure".to_string(),
        items: ids.clone(),
        selected: 0,
        action: SelectAction::ConnectStrategyProvider { ids },
    };
}

pub(crate) fn open_connect_strategy(st: &mut TuiState, provider: String) {
    st.modal = ActiveModal::SelectModal {
        title: format!("Strategy for {provider}"),
        items: connect_strategies()
            .iter()
            .map(|(label, _)| label.to_string())
            .collect(),
        selected: 0,
        action: SelectAction::ConnectStrategy { provider },
    };
}

pub(crate) fn connect_strategies() -> [(&'static str, RotationStrategy); 5] {
    [
        (
            "round_robin — cycle through keys evenly",
            RotationStrategy::RoundRobin,
        ),
        (
            "fill_first — use the first key until it fails",
            RotationStrategy::FillFirst,
        ),
        (
            "least_used — always pick the least-used key",
            RotationStrategy::LeastUsed,
        ),
        ("random — pick a key at random", RotationStrategy::Random),
        (
            "never — never rotate, only use priority key",
            RotationStrategy::Never,
        ),
    ]
}

pub(crate) async fn open_connect_reset_cooldowns(env: &Env<'_>, st: &mut TuiState) {
    let ids = connected_providers(&env.cred_manager).await;
    let mut items = vec!["All providers".to_string()];
    items.extend(ids.iter().cloned());
    st.modal = ActiveModal::SelectModal {
        title: "Reset cooldowns for".to_string(),
        items,
        selected: 0,
        action: SelectAction::ConnectResetCooldowns { ids },
    };
}

pub(crate) fn read_subagent_log_tail(log_path: &str, max_lines: usize) -> Vec<String> {
    let Ok(body) = std::fs::read_to_string(log_path) else {
        return Vec::new();
    };
    let all: Vec<String> = body.lines().map(str::to_string).collect();
    if all.len() <= max_lines {
        all
    } else {
        all[all.len() - max_lines..].to_vec()
    }
}

pub(crate) fn worker_turns_from_history(
    mut turns: Vec<TurnRecord>,
    goal: &str,
    status: &str,
) -> Vec<TurnRecord> {
    let first_goal_line = goal.lines().next().unwrap_or(goal).trim();
    if let Some(pos) = turns.iter().rposition(|t| {
        let q = t.user_query.trim();
        q.starts_with("GOAL:")
            && (first_goal_line.is_empty()
                || q.contains(first_goal_line)
                || q.trim_start_matches("GOAL:").trim().is_empty())
    }) {
        turns.split_off(pos)
    } else if status != "running" && !turns.is_empty() {
        let n = turns.len();
        turns.split_off(n.saturating_sub(1))
    } else {
        Vec::new()
    }
}

pub(crate) fn worker_label_from_context(context: Option<&str>) -> Option<String> {
    context
        .and_then(|c| c.strip_prefix("worker profile: worker:"))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| format!("worker:{name}"))
}

pub(crate) async fn sync_delegate_wiring(env: &Env<'_>) {
    crate::commands::chat::setup::wire_delegate_notifications(
        &env.delegate_tool,
        &env.ui,
        &env.store,
        env.session_id.as_str(),
    );
    env.delegate_tool.set_steer_handle(env.steer_handle.clone());
    let records = env.delegate_tool.list_tasks().await;
    for rec in records {
        if matches!(
            rec.status,
            harness_core::tools::delegate::SubagentStatus::Running
        ) {
            let label = match worker_label_from_context(rec.context.as_deref()) {
                Some(worker) => {
                    let name = worker.strip_prefix("worker:").unwrap_or(&worker);
                    format!("[{name}] {}", rec.goal)
                }
                None => rec.goal.clone(),
            };
            env.ui.start_delegate_task(&rec.id, &label);
        } else {
            env.ui.finish_delegate_task(&rec.id);
        }
    }
}

