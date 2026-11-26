//! Orchestrator preset creation, editing, thinking-slider control, and runtime activation.

use std::sync::Arc;

use harness_core::config::{set_orchestrator_in_file, validate_orchestrator_name};
use harness_core::{
    HarnessError, ModelProvider, OrchestratorConfig, OrchestratorModelSpec, ReasoningEffort,
};
use harness_providers::GenericProviderClient;

use super::pickers::apply_model_selection;
use super::*;
use crate::commands::chat::tui::state::{InputAction, OrchestratorDraftState, SelectAction};

/// Ordered thinking/effort stops shown on the horizontal slider inside the orchestrator editor.
pub const EFFORT_STOPS: [(Option<ReasoningEffort>, &str); 5] = [
    (None, "off"),
    (Some(ReasoningEffort::Low), "low"),
    (Some(ReasoningEffort::Medium), "medium"),
    (Some(ReasoningEffort::High), "high"),
    (Some(ReasoningEffort::Xhigh), "xhigh"),
];

pub fn effort_stop_index(effort: Option<ReasoningEffort>) -> usize {
    match effort {
        None | Some(ReasoningEffort::Minimal) => 0,
        Some(ReasoningEffort::Low) => 1,
        Some(ReasoningEffort::Medium) => 2,
        Some(ReasoningEffort::High) => 3,
        Some(ReasoningEffort::Xhigh | ReasoningEffort::Max) => 4,
    }
}

pub fn step_thinking_effort(
    current: Option<ReasoningEffort>,
    delta: i8,
) -> Option<ReasoningEffort> {
    let idx = effort_stop_index(current) as i8;
    let next = (idx + delta).clamp(0, (EFFORT_STOPS.len() - 1) as i8) as usize;
    EFFORT_STOPS[next].0
}

pub async fn open_orchestrator_create(env: &Env<'_>, st: &mut TuiState, name: Option<String>) {
    let Some(raw_name) = name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        st.modal = ActiveModal::InputModal {
            title: "Create Orchestrator".to_string(),
            prompt: "Orchestrator name (lowercase letters, digits, '-' or '_'):".to_string(),
            value: String::new(),
            secret: false,
            action: InputAction::CreateOrchestrator,
        };
        return;
    };

    if let Err(e) = validate_orchestrator_name(&raw_name) {
        env.ui.set_transient_error(Some(e.to_string()));
        return;
    }

    let draft = env
        .config
        .orchestrators
        .get(&raw_name)
        .cloned()
        .unwrap_or_else(|| OrchestratorConfig {
            orchestrator: OrchestratorModelSpec {
                provider: env.current_provider.clone(),
                model: env.current_model.clone(),
                thinking: st.reasoning_effort,
            },
            subagents: Vec::new(),
        });
    open_orchestrator_editor(st, raw_name, draft, 0, false);
}

pub fn open_orchestrator_edit_list(env: &Env<'_>, st: &mut TuiState) {
    if env.config.orchestrators.is_empty() {
        env.ui.set_transient_error(Some(
            "No orchestrators configured yet. Use /orchestrator create <name>.".to_string(),
        ));
        return;
    }
    let names: Vec<String> = env.config.orchestrators.keys().cloned().collect();
    let items: Vec<String> = names
        .iter()
        .map(|name| {
            let orch = &env.config.orchestrators[name];
            let sub_count = orch.subagents.len();
            let sub_word = if sub_count == 1 {
                "subagent"
            } else {
                "subagents"
            };
            format!(
                "{}  ({}/{} · {} {})",
                name, orch.orchestrator.provider, orch.orchestrator.model, sub_count, sub_word
            )
        })
        .collect();
    st.modal = ActiveModal::SelectModal {
        title: "Edit Orchestrator".to_string(),
        items,
        selected: 0,
        action: SelectAction::EditOrchestrator { names },
    };
}

pub fn open_orchestrator_use_list(env: &Env<'_>, st: &mut TuiState) {
    if env.config.orchestrators.is_empty() {
        env.ui.set_transient_error(Some(
            "No orchestrators configured yet. Use /orchestrator create <name>.".to_string(),
        ));
        return;
    }
    let names: Vec<String> = env.config.orchestrators.keys().cloned().collect();
    let items: Vec<String> = names
        .iter()
        .map(|name| {
            let orch = &env.config.orchestrators[name];
            let marker = if st.active_orchestrator.as_deref() == Some(name.as_str()) {
                "● "
            } else {
                "  "
            };
            let sub_count = orch.subagents.len();
            let sub_word = if sub_count == 1 {
                "subagent"
            } else {
                "subagents"
            };
            format!(
                "{}{}  ({}/{} · {} {})",
                marker,
                name,
                orch.orchestrator.provider,
                orch.orchestrator.model,
                sub_count,
                sub_word
            )
        })
        .collect();
    st.modal = ActiveModal::SelectModal {
        title: "Select Orchestrator".to_string(),
        items,
        selected: names
            .iter()
            .position(|n| st.active_orchestrator.as_deref() == Some(n.as_str()))
            .unwrap_or(0),
        action: SelectAction::UseOrchestrator { names },
    };
}

pub fn open_orchestrator_editor(
    st: &mut TuiState,
    name: String,
    draft: OrchestratorConfig,
    selected: usize,
    return_to_list: bool,
) {
    st.orchestrator_draft = Some(OrchestratorDraftState {
        name: name.clone(),
        config: draft.clone(),
        selected,
        return_to_list,
    });
    st.modal = ActiveModal::OrchestratorEditor {
        name,
        draft,
        selected,
        return_to_list,
    };
}

pub fn restore_orchestrator_editor_from_draft(st: &mut TuiState) -> bool {
    if let Some(draft) = st.orchestrator_draft.clone() {
        st.modal = ActiveModal::OrchestratorEditor {
            name: draft.name,
            draft: draft.config,
            selected: draft.selected,
            return_to_list: draft.return_to_list,
        };
        true
    } else {
        false
    }
}

pub async fn save_orchestrator(
    env: &mut Env<'_>,
    st: &mut TuiState,
    name: &str,
    draft: OrchestratorConfig,
) -> Result<(), HarnessError> {
    validate_orchestrator_name(name)?;
    if draft.orchestrator.provider.trim().is_empty() || draft.orchestrator.model.trim().is_empty() {
        return Err(HarnessError::ConfigurationError(
            "Orchestrator model must be selected before saving.".to_string(),
        ));
    }
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    set_orchestrator_in_file(&layers, name, &draft)?;
    env.config
        .orchestrators
        .insert(name.to_string(), draft.clone());
    st.orchestrator_draft = None;

    if st.active_orchestrator.as_deref() == Some(name) {
        apply_orchestrator(env, st, name).await?;
    } else {
        st.system_notices.push(SystemNotice::new(format!(
            "Saved orchestrator '{name}' ({}/{} · {} subagents)",
            draft.orchestrator.provider,
            draft.orchestrator.model,
            draft.subagents.len()
        )));
    }
    Ok(())
}

pub async fn apply_orchestrator(
    env: &mut Env<'_>,
    st: &mut TuiState,
    name: &str,
) -> Result<(), HarnessError> {
    let Some(orch) = env.config.orchestrators.get(name).cloned() else {
        return Err(HarnessError::ConfigurationError(format!(
            "Unknown orchestrator '{name}'"
        )));
    };
    if st.active_turn.is_some() {
        env.ui.set_transient_error(Some(
            "Wait for or interrupt the active turn before switching orchestrator.".into(),
        ));
        return Ok(());
    }

    apply_model_selection(
        env,
        st,
        &orch.orchestrator.provider,
        &orch.orchestrator.model,
        true,
    )
    .await?;
    apply_thinking_selection(env, st, orch.orchestrator.thinking).await;

    let mut pool: Vec<(Arc<dyn ModelProvider>, Option<ReasoningEffort>)> =
        Vec::with_capacity(orch.subagents.len());
    for sub in &orch.subagents {
        let client = Arc::new(
            GenericProviderClient::new(
                sub.provider.clone(),
                sub.model.clone(),
                env.cred_manager.clone(),
                env.discovery.clone(),
                env.config.providers.get(&sub.provider),
            )?
            .with_session_id(env.session_id.as_str()),
        );
        pool.push((client, sub.thinking));
    }
    env.delegate_tool.set_subagent_pool(pool);
    st.active_orchestrator = Some(name.to_string());
    st.system_notices.push(SystemNotice::new(format!(
        "Active orchestrator: {name} ({}/{} · {} subagents)",
        orch.orchestrator.provider,
        orch.orchestrator.model,
        orch.subagents.len()
    )));
    Ok(())
}
