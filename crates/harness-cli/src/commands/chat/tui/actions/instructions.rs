//! `/instructions` — user-defined priority instructions included in the system prompt.
//!
//! Instructions are persisted to `instructions.json` next to the profile/config directory so they
//! survive restarts and can be toggled on/off, edited, deleted, or drafted via the unified
//! `bg-model`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use harness_core::{CompletionOptions, Config, HarnessError, Message, ModelProvider, Role};
use harness_providers::GenericProviderClient;

use super::super::super::interactive::state::Env;
use super::super::state::{ActiveModal, InputAction, SelectAction, TuiState};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PriorityInstruction {
    pub id: String,
    pub body: String,
    pub active: bool,
}

pub fn instructions_path(config: &Config) -> PathBuf {
    let base = config
        .paths
        .profiles_dir
        .parent()
        .unwrap_or(&config.paths.profiles_dir);
    base.join("instructions.json")
}

pub fn load_instructions(config: &Config) -> Vec<PriorityInstruction> {
    let path = instructions_path(config);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save_instructions(
    config: &Config,
    items: &[PriorityInstruction],
) -> Result<(), HarnessError> {
    let path = instructions_path(config);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(items)
        .map_err(|e| HarnessError::ConfigurationError(format!("serialize instructions: {e}")))?;
    std::fs::write(&path, json)?;
    Ok(())
}

pub fn active_instruction_bodies(config: &Config) -> Vec<String> {
    load_instructions(config)
        .into_iter()
        .filter(|item| item.active && !item.body.trim().is_empty())
        .map(|item| item.body)
        .collect()
}

pub async fn sync_agent_instructions(env: &Env<'_>) {
    let active = active_instruction_bodies(env.config);
    env.agent.lock().await.set_priority_instructions(active);
}

fn preview_label(item: &PriorityInstruction) -> String {
    let marker = if item.active { "●" } else { "○" };
    let first_line = item.body.lines().next().unwrap_or("").trim();
    let mut short: String = first_line.chars().take(52).collect();
    if first_line.chars().count() > 52 {
        short.push('…');
    }
    if short.is_empty() {
        short = "(empty)".to_string();
    }
    format!("{marker} {short}")
}

pub fn open_instructions_list(config: &Config, st: &mut TuiState) {
    let items = load_instructions(config);
    let mut rows: Vec<String> = items.iter().map(preview_label).collect();
    rows.push("+ Add new instruction".to_string());
    let ids = items.into_iter().map(|i| i.id).collect();
    st.modal = ActiveModal::SelectModal {
        title: "Priority Instructions".to_string(),
        items: rows,
        selected: 0,
        action: SelectAction::InstructionsList { ids },
    };
}

pub fn open_instruction_add_method(st: &mut TuiState) {
    st.modal = ActiveModal::SelectModal {
        title: "Add Priority Instruction".to_string(),
        items: vec![
            "Write or paste instruction".to_string(),
            "Ask bg-model for a prompt".to_string(),
        ],
        selected: 0,
        action: SelectAction::InstructionsAddMethod,
    };
}

pub fn open_instruction_item_menu(config: &Config, st: &mut TuiState, id: &str) {
    let items = load_instructions(config);
    let Some(item) = items.into_iter().find(|i| i.id == id) else {
        open_instructions_list(config, st);
        return;
    };
    let toggle_label = if item.active {
        "Disable in system prompt".to_string()
    } else {
        "Use in system prompt".to_string()
    };
    st.modal = ActiveModal::SelectModal {
        title: "Priority Instruction".to_string(),
        items: vec![
            toggle_label,
            "Edit instruction".to_string(),
            "Ask bg-model to improve".to_string(),
            "Delete instruction".to_string(),
        ],
        selected: 0,
        action: SelectAction::InstructionItemMenu { id: item.id },
    };
}

pub fn open_instruction_create_input(st: &mut TuiState, initial: String) {
    st.modal = ActiveModal::InputModal {
        title: "New Priority Instruction".to_string(),
        prompt: "Enter or edit the priority instruction (Enter to save, Esc to cancel):"
            .to_string(),
        value: initial,
        secret: false,
        action: InputAction::InstructionCreate,
    };
}

pub fn open_instruction_edit_input(st: &mut TuiState, id: String, initial: String) {
    st.modal = ActiveModal::InputModal {
        title: "Edit Priority Instruction".to_string(),
        prompt: "Edit the priority instruction (Enter to save, Esc to cancel):".to_string(),
        value: initial,
        secret: false,
        action: InputAction::InstructionEdit { id },
    };
}

pub fn open_instruction_ask_bg_model_input(st: &mut TuiState) {
    st.modal = ActiveModal::InputModal {
        title: "Ask bg-model for a Prompt".to_string(),
        prompt: "Describe what you want the instruction to achieve:".to_string(),
        value: String::new(),
        secret: false,
        action: InputAction::InstructionAskBgModel,
    };
}

fn next_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("inst_{nanos:x}")
}

pub async fn upsert_instruction(
    env: &Env<'_>,
    id: Option<String>,
    body: String,
) -> Result<(), HarnessError> {
    let trimmed = body.trim().to_string();
    if trimmed.is_empty() {
        return Ok(());
    }
    let mut items = load_instructions(env.config);
    match id {
        Some(target_id) => {
            if let Some(existing) = items.iter_mut().find(|i| i.id == target_id) {
                existing.body = trimmed;
            }
        }
        None => {
            items.push(PriorityInstruction {
                id: next_id(),
                body: trimmed,
                active: true,
            });
        }
    }
    save_instructions(env.config, &items)?;
    sync_agent_instructions(env).await;
    Ok(())
}

pub async fn toggle_instruction(env: &Env<'_>, id: &str) -> Result<(), HarnessError> {
    let mut items = load_instructions(env.config);
    if let Some(item) = items.iter_mut().find(|i| i.id == id) {
        item.active = !item.active;
    }
    save_instructions(env.config, &items)?;
    sync_agent_instructions(env).await;
    Ok(())
}

pub async fn delete_instruction(env: &Env<'_>, id: &str) -> Result<(), HarnessError> {
    let mut items = load_instructions(env.config);
    items.retain(|i| i.id != id);
    save_instructions(env.config, &items)?;
    sync_agent_instructions(env).await;
    Ok(())
}

/// Resolve the unified `bg-model` provider/model (compaction -> background saver -> current session)
/// and ask it to draft or refine a concise, high-signal priority system instruction.
pub async fn suggest_instruction_with_bg_model(
    env: &Env<'_>,
    draft_or_goal: &str,
) -> Result<String, HarnessError> {
    let (provider, model) = if let (Some(p), Some(m)) = (
        env.config.agent.compaction_provider.as_deref(),
        env.config.agent.compaction_model.as_deref(),
    ) {
        (p.to_string(), m.to_string())
    } else if let (Some(p), Some(m)) = (
        env.config.memory.background_provider.as_deref(),
        env.config.memory.background_model.as_deref(),
    ) {
        (p.to_string(), m.to_string())
    } else if let (Some(p), Some(m)) = (
        env.compact_provider.as_deref(),
        env.compact_model.as_deref(),
    ) {
        (p.to_string(), m.to_string())
    } else {
        (env.current_provider.clone(), env.current_model.clone())
    };

    let client = GenericProviderClient::new(
        provider.clone(),
        model,
        env.cred_manager.clone(),
        env.discovery.clone(),
        env.config.providers.get(&provider),
    )?
    .with_session_id(env.session_id.as_str());

    let messages = vec![
        Message {
            role: Role::System,
            content: Some(
                "You are a prompt engineer. Rewrite the user's request or draft into a single, \
                 concise, direct, high-signal priority instruction for an AI coding assistant's \
                 system prompt. Output ONLY the instruction text with no preamble, markdown fence, \
                 or surrounding quotes."
                    .to_string(),
            ),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_result: None,
            usage: None,
        },
        Message {
            role: Role::User,
            content: Some(draft_or_goal.to_string()),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_result: None,
            usage: None,
        },
    ];

    let response = client
        .complete(&messages, &[], &CompletionOptions::default())
        .await?;
    let text = response.content.unwrap_or_default().trim().to_string();
    if text.is_empty() {
        return Err(HarnessError::ProviderError {
            provider,
            message: "bg-model returned an empty instruction".to_string(),
        });
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn instructions_round_trip_and_filter_active() {
        let dir = tempdir().unwrap();
        let mut config = Config::default();
        config.paths.profiles_dir = dir.path().join("profiles");

        let items = vec![
            PriorityInstruction {
                id: "1".to_string(),
                body: "Always reply in bullet points.".to_string(),
                active: true,
            },
            PriorityInstruction {
                id: "2".to_string(),
                body: "Inactive rule".to_string(),
                active: false,
            },
        ];
        save_instructions(&config, &items).unwrap();

        let loaded = load_instructions(&config);
        assert_eq!(loaded, items);
        assert_eq!(
            active_instruction_bodies(&config),
            vec!["Always reply in bullet points.".to_string()]
        );
    }
}
