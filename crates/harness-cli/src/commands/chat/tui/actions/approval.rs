//! approval

use super::*;

pub fn resolve_approval_model(
    env: &Env<'_>,
    st: &TuiState,
) -> (String, String, Option<ReasoningEffort>, bool) {
    if let (Some(p), Some(m)) = (
        env.config
            .tools
            .approval_provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        env.config
            .tools
            .approval_model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        return (
            p.to_string(),
            m.to_string(),
            env.config.tools.approval_reasoning_effort,
            false,
        );
    }
    let default_provider = env.config.model.provider.trim();
    let default_model = env.config.model.default.trim();
    if !default_provider.is_empty() && !default_model.is_empty() {
        let effort = if default_provider.eq_ignore_ascii_case(env.current_provider)
            && default_model == env.current_model.as_str()
        {
            st.reasoning_effort
        } else {
            None
        };
        return (
            default_provider.to_string(),
            default_model.to_string(),
            effort,
            true,
        );
    }
    (
        env.current_provider.clone(),
        env.current_model.clone(),
        st.reasoning_effort,
        true,
    )
}

pub(crate) fn format_approval_model_badge(env: &Env<'_>, st: &TuiState) -> String {
    let (provider, model, effort, is_default) = resolve_approval_model(env, st);
    let model_disp = friendly_model_name(&model);
    if is_default {
        format!("default ({model_disp})")
    } else if let Some(e) = effort {
        format!("{model_disp} ({provider} · {e})")
    } else {
        format!("{model_disp} ({provider})")
    }
}

pub fn apply_approval_model_choice(
    env: &mut Env<'_>,
    st: &mut TuiState,
    provider: &str,
    model: &str,
    effort: Option<ReasoningEffort>,
) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    harness_core::config::set_in_file(&layers, "tools.approval_provider", provider)?;
    harness_core::config::set_in_file(&layers, "tools.approval_model", model)?;
    if let Some(e) = effort {
        harness_core::config::set_in_file(&layers, "tools.approval_reasoning_effort", e.as_str())?;
    } else {
        let _ = harness_core::config::unset_in_file(&layers, "tools.approval_reasoning_effort")?;
    }
    env.config.tools.approval_provider = Some(provider.to_string());
    env.config.tools.approval_model = Some(model.to_string());
    env.config.tools.approval_reasoning_effort = effort;
    let effort_label = effort
        .map(|e| e.to_string())
        .unwrap_or_else(|| "off".to_string());
    notify_setting_change(
        st,
        format!(
            "✔ Approval model set to {} ({}, thinking: {})",
            friendly_model_name(model),
            provider,
            effort_label
        ),
    );
    open_settings_menu(env, st, 9);
    Ok(())
}

pub fn reset_approval_model_choice(
    env: &mut Env<'_>,
    st: &mut TuiState,
) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    let _ = harness_core::config::unset_in_file(&layers, "tools.approval_provider")?;
    let _ = harness_core::config::unset_in_file(&layers, "tools.approval_model")?;
    let _ = harness_core::config::unset_in_file(&layers, "tools.approval_reasoning_effort")?;
    env.config.tools.approval_provider = None;
    env.config.tools.approval_model = None;
    env.config.tools.approval_reasoning_effort = None;
    notify_setting_change(st, "✔ Approval model reset to default".to_string());
    open_settings_menu(env, st, 9);
    Ok(())
}

pub(crate) fn truncate_chars_with_ellipsis(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    format!("{}\n...(truncated)", &text[..end])
}

pub fn build_approval_question_prompt(req: &ApprovalRequest, question: &str) -> String {
    let mut out = format!("Pending tool call: {}\n", req.tool);
    if let Some(ref target) = req.target {
        out.push_str(&format!("Target: {target}\n"));
    }
    if let Some(ref diff) = req.diff {
        out.push_str("\nDiff:\n");
        out.push_str(&truncate_chars_with_ellipsis(diff, 8000));
        out.push('\n');
    } else if !req.arguments.is_null() {
        let pretty = serde_json::to_string_pretty(&req.arguments)
            .unwrap_or_else(|_| req.arguments.to_string());
        out.push_str("\nArguments:\n");
        out.push_str(&truncate_chars_with_ellipsis(&pretty, 4000));
        out.push('\n');
    }
    out.push_str("\nUser question / suggestion:\n");
    out.push_str(question.trim());
    out
}

pub fn ask_approval_model(env: &Env<'_>, st: &TuiState, req: ApprovalRequest, question: String) {
    let (provider, model, reasoning_effort, _) = resolve_approval_model(env, st);
    let cred_manager = env.cred_manager.clone();
    let discovery = env.discovery.clone();
    let provider_override = env.config.providers.get(&provider).cloned();
    let tx = st.approval_reply_tx.clone();
    let activity = env.approval_activity.clone();
    activity.notify_one();

    tokio::spawn(async move {
        let call_id = req.call_id.clone();
        let result = async {
            let client = GenericProviderClient::new(
                provider.clone(),
                model.clone(),
                cred_manager,
                discovery,
                provider_override.as_ref(),
            )
            .map_err(|e| {
                format!("failed to initialize approval model ({provider}/{model}): {e}")
            })?;

            let prompt = build_approval_question_prompt(&req, &question);
            let messages = vec![
                Message::system(
                    "You are assisting a user who is reviewing a pending tool approval request in Luna. \
                     Answer their question or evaluate their suggestion clearly and concisely based strictly \
                     on the tool call details shown. Do not invent file contents that were not provided.",
                ),
                Message::user(prompt),
            ];
            let options = CompletionOptions {
                model: Some(model.clone()),
                reasoning_effort,
                ..Default::default()
            };
            let reply = client
                .complete(&messages, &[], &options)
                .await
                .map_err(|e| format!("approval model error ({provider}/{model}): {e}"))?;
            let text = reply.content.as_deref().unwrap_or("").trim().to_string();
            if text.is_empty() {
                Ok("(model returned an empty response)".to_string())
            } else {
                Ok(text)
            }
        }
        .await;
        activity.notify_one();
        let _ = tx.send(ApprovalReply { call_id, result });
    });
}

pub(crate) async fn apply_default_target(
    env: &mut Env<'_>,
    _st: &mut TuiState,
    provider: &str,
    model: &str,
    is_global: bool,
) -> Result<(), HarnessError> {
    crate::commands::chat::commands::apply_default_model_choice(
        &env.ui,
        env.config,
        &env.cred_manager,
        &env.discovery,
        &env.agent,
        &env.store,
        env.session_id,
        env.current_provider,
        env.current_model,
        env.compact_model,
        provider.to_string(),
        model.to_string(),
        is_global,
    )
    .await
}

pub(crate) async fn apply_compact_target(
    env: &mut Env<'_>,
    _st: &mut TuiState,
    provider: &str,
    model: &str,
    is_global: bool,
) -> Result<(), HarnessError> {
    crate::commands::chat::commands::apply_compact_model_choice(
        &env.ui,
        env.config,
        &env.cred_manager,
        &env.discovery,
        &env.agent,
        &env.store,
        env.session_id,
        env.compact_provider,
        env.compact_model,
        provider.to_string(),
        model.to_string(),
        is_global,
    )
    .await
}

pub(crate) fn open_model_scope(
    _env: &mut Env<'_>,
    st: &mut TuiState,
    target: ModelTarget,
    provider: &str,
    model: &str,
) {
    st.modal = ActiveModal::SelectModal {
        title: "Apply model to".to_string(),
        items: vec![
            "This session only".to_string(),
            "Globally (save as default for all sessions)".to_string(),
        ],
        selected: 0,
        action: SelectAction::ModelScope {
            target,
            provider: provider.to_string(),
            model: model.to_string(),
        },
    };
}

pub(crate) fn apply_worker_target(
    env: &mut Env<'_>,
    st: &mut TuiState,
    name: &str,
    provider: &str,
    model: &str,
) -> Result<(), HarnessError> {
    let profile = harness_core::WorkerProfile {
        provider: provider.to_string(),
        model: model.to_string(),
        reasoning_effort: env
            .config
            .workers
            .get(name)
            .and_then(|w| w.reasoning_effort),
    };
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    harness_core::config::set_worker_in_file(&layers, name, &profile)?;
    env.config.workers.insert(name.to_string(), profile);
    st.system_notices.push(SystemNotice::new(format!(
        "✔ Worker /{name} → {model} ({provider}) · invoke with /{name} <task>"
    )));
    Ok(())
}

/// Put plain text into a scrollable TUI modal. ANSI styling is stripped because ratatui cells
/// render escape sequences literally.
pub(crate) fn show_text_modal(st: &mut TuiState, title: &str, body: &str) {
    let cleaned = dialoguer::console::strip_ansi_codes(body).to_string();
    let lines: Vec<String> = if cleaned.trim().is_empty() {
        vec!["(empty)".to_string()]
    } else {
        cleaned.lines().map(str::to_string).collect()
    };
    st.modal = ActiveModal::TextModal {
        title: title.to_string(),
        lines,
        scroll: 0,
    };
}

pub(crate) fn notify_setting_change(st: &mut TuiState, msg: impl Into<String>) {
    let text = msg.into();
    if st.turns.is_empty() {
        st.selection_notice = Some((text, std::time::Instant::now()));
    } else {
        st.system_notices.push(SystemNotice::new(text));
    }
}

pub(crate) fn feature_status_badge(env: &Env<'_>, feature: Feature) -> String {
    let live_on = env.feature_toggles.enabled(feature);
    let (global_on, until) = match feature {
        Feature::Mcp => (
            env.config.mcp.enabled && env.config.agent.mcp_enabled,
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
    let now = harness_core::Config::current_unix_secs();
    if live_on {
        if !global_on || until.is_some_and(|u| u > now) {
            "ON (this session)".to_string()
        } else {
            "ON".to_string()
        }
    } else if let Some(u) = until.filter(|&u| u > now) {
        let days = (u - now).div_ceil(86_400);
        format!("OFF ({days}d left)")
    } else if !global_on {
        "OFF (global)".to_string()
    } else {
        "OFF (this session)".to_string()
    }
}

pub(crate) fn open_settings_menu(env: &Env<'_>, st: &mut TuiState, selected: usize) {
    let config = &env.config;
    let limit = match config.display.block_limit {
        None => "uncapped".to_string(),
        Some(1) => "1 line".to_string(),
        Some(n) => format!("{n} lines"),
    };
    let collapse = if config.display.auto_collapse {
        "on"
    } else {
        "off"
    };
    let thinking = if config.display.auto_hide_thinking {
        "on"
    } else {
        "off"
    };
    let theme_label = match config.display.theme {
        harness_core::ThemeMode::Dark => "dark mode (default)",
        harness_core::ThemeMode::Light => "light mode",
    };
    let saver = match config.memory.mode {
        harness_core::MemoryMode::Session => "Session writer",
        harness_core::MemoryMode::Background => "Background saver",
    };
    let approval_model = format_approval_model_badge(env, st);
    let voice_label = if config.voice.enabled {
        format!(
            "on ({} · {})",
            config.voice.provider, config.voice.voice_name
        )
    } else {
        format!("off ({})", config.voice.voice_name)
    };
    st.modal = ActiveModal::SelectModal {
        title: "CLI Settings".to_string(),
        items: vec![
            format!("Theme · {theme_label}"),
            format!("Display block limit · {limit}"),
            format!("Auto-collapse on turn finish · {collapse}"),
            format!("Auto-collapse thinking while a tool runs · {thinking}"),
            format!("MCP · {}", feature_status_badge(env, Feature::Mcp)),
            format!("Skills · {}", feature_status_badge(env, Feature::Skills)),
            format!("Memory · {}", feature_status_badge(env, Feature::Memory)),
            format!("Notes · {}", feature_status_badge(env, Feature::Notes)),
            format!("Memory saver · {saver}"),
            format!("Approval model · {approval_model}"),
            format!("Voice mode · {voice_label}"),
            "View current settings summary".to_string(),
        ],
        selected: selected.min(11),
        action: SelectAction::SettingsMain,
    };
}
