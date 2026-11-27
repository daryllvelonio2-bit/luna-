//! Modal window keyboard input handling for the TUI.

use std::io;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use harness_core::{ApprovalDecision, ApprovalRequest, HarnessError, ReasoningEffort};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::super::super::interactive::state::{Env, Flow};
use super::super::actions::{
    self, apply_agent_mode, apply_approval_model_choice, apply_model_target,
    apply_thinking_selection, ask_approval_model, open_models_for_target, open_provider_picker_for,
    open_thinking_picker, open_thinking_picker_with_current, run_palette_action, switch_to_session,
};
use super::super::render::{
    filtered_models, filtered_palette_items, filtered_providers, filtered_sessions,
};
use super::super::state::{
    self as state, ActiveModal, AgentMode, ApprovalInputMode, ModelTarget, TuiState,
    APPROVAL_ASK_INDEX, APPROVAL_CHOICE_COUNT, APPROVAL_SUGGEST_INDEX,
};
use super::handle_paste;
use crate::commands::clarify::{ClarifyAnswer, ClarifyResponse};

const ERROR_ASK_NEEDS_TEXT: &str = "Type a question first, or press Esc.";
const ERROR_SUGGEST_NEEDS_TEXT: &str = "Type a suggestion first, or press Esc.";
const ERROR_CTRL_S_NEEDS_TEXT: &str = "Type a suggestion before sending it to the model.";
const ERROR_CTRL_S_NO_INPUT: &str = "Press s to type a suggestion first.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApprovalSubmitOutcome {
    Asked(String),
    Suggested(String),
    NeedText(ApprovalInputMode),
    Pending,
}

/// Single source of truth for the suggestion payload: Ctrl+S and the `[s]` choice both build it
/// here, so the gate always receives the same context. `None` means there is nothing to send.
pub(crate) fn approval_suggestion_payload(req: &ApprovalRequest, input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(actions::build_suggestion_feedback(req, trimmed))
}

pub(crate) fn approval_error_for(mode: ApprovalInputMode) -> &'static str {
    match mode {
        ApprovalInputMode::Ask => ERROR_ASK_NEEDS_TEXT,
        ApprovalInputMode::Suggest => ERROR_SUGGEST_NEEDS_TEXT,
    }
}

/// Decide what Enter in the approval input box does. The caller owns the reply channel and sends
/// the decision itself, so this stays pure enough to test without a terminal or a provider.
pub(crate) fn apply_approval_submit<F>(
    mode: ApprovalInputMode,
    req: &ApprovalRequest,
    input: &str,
    asking: bool,
    ask: F,
) -> ApprovalSubmitOutcome
where
    F: FnOnce(String),
{
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return ApprovalSubmitOutcome::NeedText(mode);
    }
    match mode {
        ApprovalInputMode::Ask if !asking => {
            let question = trimmed.to_string();
            ask(question.clone());
            ApprovalSubmitOutcome::Asked(question)
        }
        ApprovalInputMode::Suggest => match approval_suggestion_payload(req, trimmed) {
            Some(feedback) => ApprovalSubmitOutcome::Suggested(feedback),
            None => ApprovalSubmitOutcome::NeedText(mode),
        },
        ApprovalInputMode::Ask => ApprovalSubmitOutcome::Pending,
    }
}

pub async fn handle_modal_key(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    key: KeyEvent,
) -> Result<Flow, HarnessError> {
    let is_paste = (key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V')))
        || (key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::Insert);
    if is_paste {
        if let Some(text) = crate::commands::clipboard_image::read_clipboard_text() {
            handle_paste(env, st, &text);
        }
        return Ok(Flow::Continue);
    }

    let modal = std::mem::replace(&mut st.modal, ActiveModal::None);
    match modal {
        ActiveModal::None => {}
        ActiveModal::CommandPalette {
            mut query,
            mut selected,
        } => {
            if key.code == KeyCode::Esc
                || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p'))
            {
                return Ok(Flow::Continue);
            }
            let items = filtered_palette_items(&query);
            match key.code {
                KeyCode::Up => {
                    if !items.is_empty() {
                        selected = if selected == 0 {
                            items.len() - 1
                        } else {
                            selected - 1
                        };
                    }
                    st.modal = ActiveModal::CommandPalette { query, selected };
                }
                KeyCode::Down => {
                    if !items.is_empty() {
                        selected = (selected + 1) % items.len();
                    }
                    st.modal = ActiveModal::CommandPalette { query, selected };
                }
                KeyCode::Enter => {
                    if let Some(item) = items.get(selected.min(items.len().saturating_sub(1))) {
                        return run_palette_action(env, st, terminal, item.action.clone()).await;
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    st.modal = ActiveModal::CommandPalette { query, selected: 0 };
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(c);
                    st.modal = ActiveModal::CommandPalette { query, selected: 0 };
                }
                _ => {
                    st.modal = ActiveModal::CommandPalette { query, selected };
                }
            }
        }
        ActiveModal::SessionPicker {
            mut query,
            sessions,
            mut selected,
        } => {
            if key.code == KeyCode::Esc {
                return Ok(Flow::Continue);
            }
            let filtered_ids: Vec<String> = filtered_sessions(&sessions, &query)
                .into_iter()
                .map(|s| s.id.clone())
                .collect();
            match key.code {
                KeyCode::Up => {
                    if !filtered_ids.is_empty() {
                        selected = if selected == 0 {
                            filtered_ids.len() - 1
                        } else {
                            selected - 1
                        };
                    }
                    st.modal = ActiveModal::SessionPicker {
                        query,
                        sessions,
                        selected,
                    };
                }
                KeyCode::Down => {
                    if !filtered_ids.is_empty() {
                        selected = (selected + 1) % filtered_ids.len();
                    }
                    st.modal = ActiveModal::SessionPicker {
                        query,
                        sessions,
                        selected,
                    };
                }
                KeyCode::Enter => {
                    if let Some(id) =
                        filtered_ids.get(selected.min(filtered_ids.len().saturating_sub(1)))
                    {
                        if let Err(e) = switch_to_session(env, st, id).await {
                            env.ui
                                .set_transient_error(Some(format!("Session switch failed: {e}")));
                        }
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    st.modal = ActiveModal::SessionPicker {
                        query,
                        sessions,
                        selected: 0,
                    };
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(c);
                    st.modal = ActiveModal::SessionPicker {
                        query,
                        sessions,
                        selected: 0,
                    };
                }
                _ => {
                    st.modal = ActiveModal::SessionPicker {
                        query,
                        sessions,
                        selected,
                    };
                }
            }
        }
        ActiveModal::ProviderPicker {
            target,
            mut query,
            items,
            mut selected,
        } => {
            if key.code == KeyCode::Esc {
                if matches!(
                    target,
                    ModelTarget::OrchestratorMain | ModelTarget::OrchestratorSubagent(_)
                ) {
                    super::super::actions::orchestrator::restore_orchestrator_editor_from_draft(st);
                }
                return Ok(Flow::Continue);
            }
            let filtered_ids: Vec<String> = filtered_providers(&items, &query)
                .into_iter()
                .map(|p| p.id.clone())
                .collect();
            match key.code {
                KeyCode::Up => {
                    if !filtered_ids.is_empty() {
                        selected = if selected == 0 {
                            filtered_ids.len() - 1
                        } else {
                            selected - 1
                        };
                    }
                    st.modal = ActiveModal::ProviderPicker {
                        target,
                        query,
                        items,
                        selected,
                    };
                }
                KeyCode::Down => {
                    if !filtered_ids.is_empty() {
                        selected = (selected + 1) % filtered_ids.len();
                    }
                    st.modal = ActiveModal::ProviderPicker {
                        target,
                        query,
                        items,
                        selected,
                    };
                }
                KeyCode::Enter | KeyCode::Right => {
                    if let Some(provider_id) =
                        filtered_ids.get(selected.min(filtered_ids.len().saturating_sub(1)))
                    {
                        open_models_for_target(env, st, target.clone(), provider_id, String::new())
                            .await;
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    st.modal = ActiveModal::ProviderPicker {
                        target,
                        query,
                        items,
                        selected: 0,
                    };
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(c);
                    st.modal = ActiveModal::ProviderPicker {
                        target,
                        query,
                        items,
                        selected: 0,
                    };
                }
                _ => {
                    st.modal = ActiveModal::ProviderPicker {
                        target,
                        query,
                        items,
                        selected,
                    };
                }
            }
        }
        ActiveModal::ModelPicker {
            target,
            provider,
            provider_display,
            mut query,
            items,
            mut selected,
        } => {
            if key.code == KeyCode::Esc {
                // Esc steps back one picker instead of closing the whole flow: the
                // first press clears the search, the next returns to the providers.
                if !query.is_empty() {
                    st.modal = ActiveModal::ModelPicker {
                        target,
                        provider,
                        provider_display,
                        query: String::new(),
                        items,
                        selected: 0,
                    };
                } else {
                    open_provider_picker_for(env, st, target.clone()).await;
                }
                return Ok(Flow::Continue);
            }
            let filtered_pairs: Vec<(String, String)> = filtered_models(&items, &query)
                .into_iter()
                .map(|m| (m.provider.clone(), m.model.clone()))
                .collect();
            match key.code {
                KeyCode::Up => {
                    if !filtered_pairs.is_empty() {
                        selected = if selected == 0 {
                            filtered_pairs.len() - 1
                        } else {
                            selected - 1
                        };
                    }
                    st.modal = ActiveModal::ModelPicker {
                        target,
                        provider,
                        provider_display,
                        query,
                        items,
                        selected,
                    };
                }
                KeyCode::Down => {
                    if !filtered_pairs.is_empty() {
                        selected = (selected + 1) % filtered_pairs.len();
                    }
                    st.modal = ActiveModal::ModelPicker {
                        target,
                        provider,
                        provider_display,
                        query,
                        items,
                        selected,
                    };
                }
                KeyCode::Left => {
                    open_provider_picker_for(env, st, target.clone()).await;
                }
                KeyCode::Enter => {
                    if let Some((prov, model)) =
                        filtered_pairs.get(selected.min(filtered_pairs.len().saturating_sub(1)))
                    {
                        if matches!(target, ModelTarget::Session | ModelTarget::Approval) {
                            // Chain into the thinking-level picker: remember the chosen pair and
                            // apply everything once the effort is confirmed.
                            st.pending_model_choice =
                                Some((target.clone(), prov.clone(), model.clone()));
                            if matches!(target, ModelTarget::Approval) {
                                open_thinking_picker_with_current(
                                    st,
                                    env.config.tools.approval_reasoning_effort,
                                );
                            } else {
                                open_thinking_picker(env, st).await;
                            }
                        } else if let Err(e) =
                            apply_model_target(env, st, target.clone(), prov, model).await
                        {
                            env.ui
                                .set_transient_error(Some(format!("Model switch failed: {e}")));
                        }
                    }
                }
                KeyCode::Backspace => {
                    if query.is_empty() {
                        open_provider_picker_for(env, st, target.clone()).await;
                    } else {
                        query.pop();
                        st.modal = ActiveModal::ModelPicker {
                            target,
                            provider,
                            provider_display,
                            query,
                            items,
                            selected: 0,
                        };
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(c);
                    st.modal = ActiveModal::ModelPicker {
                        target,
                        provider,
                        provider_display,
                        query,
                        items,
                        selected: 0,
                    };
                }
                _ => {
                    st.modal = ActiveModal::ModelPicker {
                        target,
                        provider,
                        provider_display,
                        query,
                        items,
                        selected,
                    };
                }
            }
        }
        ActiveModal::SelectModal {
            title,
            items,
            mut selected,
            action,
        } => {
            if key.code == KeyCode::Esc {
                return Ok(Flow::Continue);
            }
            match key.code {
                KeyCode::Up => {
                    if !items.is_empty() {
                        selected = if selected == 0 {
                            items.len() - 1
                        } else {
                            selected - 1
                        };
                    }
                    st.modal = ActiveModal::SelectModal {
                        title,
                        items,
                        selected,
                        action,
                    };
                }
                KeyCode::Down => {
                    if !items.is_empty() {
                        selected = (selected + 1) % items.len();
                    }
                    st.modal = ActiveModal::SelectModal {
                        title,
                        items,
                        selected,
                        action,
                    };
                }
                KeyCode::Char(' ') => {
                    if matches!(action, state::SelectAction::SettingsMain) {
                        let choice = selected.min(items.len().saturating_sub(1));
                        actions::toggle_settings_main_item(env, st, choice).await;
                    } else {
                        st.modal = ActiveModal::SelectModal {
                            title,
                            items,
                            selected,
                            action,
                        };
                    }
                }
                KeyCode::Enter => {
                    let choice = selected.min(items.len().saturating_sub(1));
                    actions::run_select_action(env, st, action, choice).await;
                }
                KeyCode::Backspace => match action {
                    state::SelectAction::SettingsVoiceList => {
                        actions::open_settings_voice_menu(env, st, 1);
                    }
                    state::SelectAction::SettingsVoice => {
                        actions::open_settings_menu(env, st, 10);
                    }
                    _ => {
                        st.modal = ActiveModal::SelectModal {
                            title,
                            items,
                            selected,
                            action,
                        };
                    }
                },
                _ => {
                    st.modal = ActiveModal::SelectModal {
                        title,
                        items,
                        selected,
                        action,
                    };
                }
            }
        }
        ActiveModal::InputModal {
            title,
            prompt,
            mut value,
            secret,
            action,
        } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                actions::run_input_action(env, st, action, value).await;
            }
            KeyCode::Backspace => {
                value.pop();
                st.modal = ActiveModal::InputModal {
                    title,
                    prompt,
                    value,
                    secret,
                    action,
                };
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                value.push(c);
                st.modal = ActiveModal::InputModal {
                    title,
                    prompt,
                    value,
                    secret,
                    action,
                };
            }
            _ => {
                st.modal = ActiveModal::InputModal {
                    title,
                    prompt,
                    value,
                    secret,
                    action,
                };
            }
        },
        ActiveModal::TextModal {
            title,
            lines,
            mut scroll,
        } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                scroll = scroll.saturating_sub(1);
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll,
                };
            }
            KeyCode::Down => {
                scroll = (scroll + 1).min(lines.len().saturating_sub(1));
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll,
                };
            }
            KeyCode::PageUp => {
                scroll = scroll.saturating_sub(10);
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll,
                };
            }
            KeyCode::PageDown => {
                scroll = (scroll + 10).min(lines.len().saturating_sub(1));
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll,
                };
            }
            KeyCode::Home => {
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll: 0,
                };
            }
            KeyCode::End => {
                let last = lines.len().saturating_sub(1);
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll: last,
                };
            }
            _ => {
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll,
                };
            }
        },
        ActiveModal::ThinkingPicker {
            current,
            mut selected,
        } => {
            let options: [Option<ReasoningEffort>; 7] = [
                None,
                Some(ReasoningEffort::Minimal),
                Some(ReasoningEffort::Low),
                Some(ReasoningEffort::Medium),
                Some(ReasoningEffort::High),
                Some(ReasoningEffort::Xhigh),
                Some(ReasoningEffort::Max),
            ];
            match key.code {
                KeyCode::Esc => {
                    // Cancelling the chained thinking picker steps back to the model
                    // list it came from; a standalone thinking picker just closes.
                    if let Some((target, provider, _)) = st.pending_model_choice.take() {
                        open_models_for_target(env, st, target, &provider, String::new()).await;
                    }
                }
                KeyCode::Up => {
                    selected = if selected == 0 { 6 } else { selected - 1 };
                    st.modal = ActiveModal::ThinkingPicker { current, selected };
                }
                KeyCode::Down => {
                    selected = (selected + 1) % 7;
                    st.modal = ActiveModal::ThinkingPicker { current, selected };
                }
                KeyCode::Enter => {
                    let chosen_effort = options[selected];
                    match st.pending_model_choice.take() {
                        Some((ModelTarget::Approval, provider, model)) => {
                            if let Err(e) = apply_approval_model_choice(
                                env,
                                st,
                                &provider,
                                &model,
                                chosen_effort,
                            ) {
                                env.ui.set_transient_error(Some(format!(
                                    "Approval model save failed: {e}"
                                )));
                            }
                        }
                        Some((target, provider, model)) => {
                            apply_thinking_selection(env, st, chosen_effort).await;
                            if let Err(e) =
                                apply_model_target(env, st, target, &provider, &model).await
                            {
                                env.ui
                                    .set_transient_error(Some(format!("Model switch failed: {e}")));
                            }
                        }
                        None => {
                            apply_thinking_selection(env, st, chosen_effort).await;
                        }
                    }
                }
                _ => {
                    st.modal = ActiveModal::ThinkingPicker { current, selected };
                }
            }
        }
        ActiveModal::OrchestratorEditor {
            name,
            mut draft,
            mut selected,
            return_to_list,
        } => {
            use super::super::actions::orchestrator::{
                open_orchestrator_edit_list, open_orchestrator_editor, save_orchestrator,
                step_thinking_effort,
            };

            let add_row = draft.subagents.len() + 1;
            let save_row = draft.subagents.len() + 2;
            let total_rows = save_row + 1;
            selected = selected.min(total_rows.saturating_sub(1));

            match key.code {
                KeyCode::Esc => {
                    st.orchestrator_draft = None;
                    if return_to_list {
                        open_orchestrator_edit_list(env, st);
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = if selected == 0 {
                        total_rows - 1
                    } else {
                        selected - 1
                    };
                    open_orchestrator_editor(st, name, draft, selected, return_to_list);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1) % total_rows;
                    open_orchestrator_editor(st, name, draft, selected, return_to_list);
                }
                KeyCode::Left | KeyCode::Right => {
                    let delta: i8 = if key.code == KeyCode::Left { -1 } else { 1 };
                    if selected == 0 {
                        draft.orchestrator.thinking =
                            step_thinking_effort(draft.orchestrator.thinking, delta);
                    } else if selected >= 1 && selected <= draft.subagents.len() {
                        let idx = selected - 1;
                        draft.subagents[idx].thinking =
                            step_thinking_effort(draft.subagents[idx].thinking, delta);
                    }
                    open_orchestrator_editor(st, name, draft, selected, return_to_list);
                }
                KeyCode::Delete | KeyCode::Backspace | KeyCode::Char('d')
                    if selected >= 1 && selected <= draft.subagents.len() =>
                {
                    let idx = selected - 1;
                    draft.subagents.remove(idx);
                    let new_sel = selected.min(draft.subagents.len() + 2);
                    open_orchestrator_editor(st, name, draft, new_sel, return_to_list);
                }
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Err(e) = save_orchestrator(env, st, &name, draft.clone()).await {
                        env.ui.set_transient_error(Some(e.to_string()));
                        open_orchestrator_editor(st, name, draft, selected, return_to_list);
                    }
                }
                KeyCode::Enter => {
                    if selected == 0 {
                        open_orchestrator_editor(st, name, draft, selected, return_to_list);
                        open_provider_picker_for(env, st, ModelTarget::OrchestratorMain).await;
                    } else if selected <= draft.subagents.len() {
                        let idx = selected - 1;
                        open_orchestrator_editor(st, name, draft, selected, return_to_list);
                        open_provider_picker_for(
                            env,
                            st,
                            ModelTarget::OrchestratorSubagent(Some(idx)),
                        )
                        .await;
                    } else if selected == add_row {
                        open_orchestrator_editor(st, name, draft, selected, return_to_list);
                        open_provider_picker_for(env, st, ModelTarget::OrchestratorSubagent(None))
                            .await;
                    } else if let Err(e) = save_orchestrator(env, st, &name, draft.clone()).await {
                        env.ui.set_transient_error(Some(e.to_string()));
                        open_orchestrator_editor(st, name, draft, selected, return_to_list);
                    }
                }
                _ => {
                    open_orchestrator_editor(st, name, draft, selected, return_to_list);
                }
            }
        }
        ActiveModal::McpList {
            mut entries,
            mut selected,
        } => {
            // Esc closes without rebuilding: the modal was already replaced with `None`.
            if key.code == KeyCode::Esc || key.code == KeyCode::Char('q') {
                return Ok(Flow::Continue);
            }
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if !entries.is_empty() {
                        selected = (selected + 1).min(entries.len() - 1);
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(entry) = entries.get_mut(selected) {
                        entry.cycle();
                        let (name, global, session) =
                            (entry.name.clone(), entry.global, entry.session);
                        // The global state is the config default; the session state is in memory.
                        if let Some(server) =
                            env.config.mcp.servers.iter_mut().find(|s| s.name == name)
                        {
                            server.enabled = global;
                        }
                        if global || session {
                            let registry = {
                                let mut ag = env.agent.lock().await;
                                ag.set_mcp_enabled(true);
                                ag.registry().clone()
                            };
                            if let Err(err) =
                                crate::commands::chat::helpers::ensure_mcp_servers_registered(
                                    &registry, env.config,
                                )
                                .await
                            {
                                env.ui.set_transient_error(Some(format!(
                                    "Failed to start MCP servers: {err}"
                                )));
                            }
                        }
                    }
                }
                _ => {}
            }
            st.modal = ActiveModal::McpList { entries, selected };
        }
        ActiveModal::Approval {
            req,
            resp_tx,
            mut selected,
            mut diff_scroll,
            mut editing_input,
            mut input,
            mut asking,
            mut reply,
            mut reply_scroll,
            mut error,
        } => {
            env.approval_activity.notify_one();
            if editing_input {
                match key.code {
                    KeyCode::Esc => {
                        editing_input = false;
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                    KeyCode::Char('s') | KeyCode::Char('S')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        match approval_suggestion_payload(&req, &input) {
                            // Sent: the sender is consumed and the resolved gate closes the modal
                            // on its own, so there is nothing to put back.
                            Some(feedback) => {
                                let _ = resp_tx.send(ApprovalDecision::DenyWithFeedback(feedback));
                            }
                            None => {
                                error = Some(ERROR_CTRL_S_NEEDS_TEXT.to_string());
                                st.modal = ActiveModal::Approval {
                                    req,
                                    resp_tx,
                                    selected,
                                    diff_scroll,
                                    editing_input,
                                    input,
                                    asking,
                                    reply,
                                    reply_scroll,
                                    error,
                                };
                            }
                        }
                    }
                    KeyCode::Enter => {
                        let mode = ApprovalInputMode::from_selected(selected);
                        let outcome = apply_approval_submit(mode, &req, &input, asking, |text| {
                            ask_approval_model(env, st, req.clone(), text)
                        });
                        // Only the suggest route consumes the reply channel; the other outcomes must
                        // keep the modal open, so hold the sender until we know which happened.
                        let mut resp_tx = Some(resp_tx);
                        match outcome {
                            ApprovalSubmitOutcome::Asked(_) => {
                                asking = true;
                                editing_input = false;
                                error = None;
                                reply = None;
                                reply_scroll = 0;
                            }
                            ApprovalSubmitOutcome::Suggested(feedback) => {
                                if let Some(tx) = resp_tx.take() {
                                    let _ = tx.send(ApprovalDecision::DenyWithFeedback(feedback));
                                }
                            }
                            ApprovalSubmitOutcome::NeedText(mode) => {
                                error = Some(approval_error_for(mode).to_string());
                            }
                            ApprovalSubmitOutcome::Pending => {}
                        }
                        // A sent suggestion leaves `resp_tx` empty: the gate resolves, so the modal
                        // is deliberately not rebuilt.
                        if let Some(resp_tx) = resp_tx {
                            st.modal = ActiveModal::Approval {
                                req,
                                resp_tx,
                                selected,
                                diff_scroll,
                                editing_input,
                                input,
                                asking,
                                reply,
                                reply_scroll,
                                error,
                            };
                        }
                    }
                    KeyCode::Backspace => {
                        input.pop();
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        input.push(c);
                        error = None;
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                    _ => {
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                }
            } else {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                        let _ = resp_tx.send(ApprovalDecision::Deny);
                    }
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('1') => {
                        let _ = resp_tx.send(ApprovalDecision::Run);
                    }
                    KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Char('2') => {
                        apply_agent_mode(env, st, AgentMode::Auto);
                        let _ = resp_tx.send(ApprovalDecision::Run);
                    }
                    KeyCode::Char('3') => {
                        let _ = resp_tx.send(ApprovalDecision::Deny);
                    }
                    KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Char('4') => {
                        editing_input = true;
                        selected = APPROVAL_ASK_INDEX;
                        error = None;
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                    // Bare s/S is free; Ctrl+S is a separate shortcut, hence the guard.
                    KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Char('5')
                        if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        editing_input = true;
                        selected = APPROVAL_SUGGEST_INDEX;
                        error = None;
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                    KeyCode::Char('[') => {
                        diff_scroll = diff_scroll.saturating_sub(3);
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                    KeyCode::Char(']') => {
                        diff_scroll += 3;
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                    KeyCode::Char('s') | KeyCode::Char('S')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        match approval_suggestion_payload(&req, &input) {
                            Some(feedback) => {
                                let _ = resp_tx.send(ApprovalDecision::DenyWithFeedback(feedback));
                            }
                            None => {
                                error = Some(ERROR_CTRL_S_NO_INPUT.to_string());
                                st.modal = ActiveModal::Approval {
                                    req,
                                    resp_tx,
                                    selected,
                                    diff_scroll,
                                    editing_input,
                                    input,
                                    asking,
                                    reply,
                                    reply_scroll,
                                    error,
                                };
                            }
                        }
                    }
                    KeyCode::PageUp => {
                        if reply.is_some() {
                            reply_scroll = reply_scroll.saturating_sub(3);
                        } else {
                            diff_scroll = diff_scroll.saturating_sub(3);
                        }
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                    KeyCode::PageDown => {
                        if reply.is_some() {
                            reply_scroll += 3;
                        } else {
                            diff_scroll += 3;
                        }
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                    KeyCode::Up => {
                        selected = if selected == 0 {
                            APPROVAL_CHOICE_COUNT - 1
                        } else {
                            selected - 1
                        };
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                    KeyCode::Down => {
                        selected = (selected + 1) % APPROVAL_CHOICE_COUNT;
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                    KeyCode::Enter => match selected {
                        0 => {
                            let _ = resp_tx.send(ApprovalDecision::Run);
                        }
                        1 => {
                            apply_agent_mode(env, st, AgentMode::Auto);
                            let _ = resp_tx.send(ApprovalDecision::Run);
                        }
                        2 => {
                            let _ = resp_tx.send(ApprovalDecision::Deny);
                        }
                        _ => {
                            editing_input = true;
                            error = None;
                            st.modal = ActiveModal::Approval {
                                req,
                                resp_tx,
                                selected,
                                diff_scroll,
                                editing_input,
                                input,
                                asking,
                                reply,
                                reply_scroll,
                                error,
                            };
                        }
                    },
                    _ => {
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected,
                            diff_scroll,
                            editing_input,
                            input,
                            asking,
                            reply,
                            reply_scroll,
                            error,
                        };
                    }
                }
            }
        }
        ActiveModal::Clarify {
            req,
            resp_tx,
            mut question_idx,
            mut answers,
            mut selected,
            mut custom,
            editing_custom,
            mut scroll,
            mut follow_selection,
        } => {
            if key.code == KeyCode::Esc {
                if editing_custom {
                    st.modal = ActiveModal::Clarify {
                        req,
                        resp_tx,
                        question_idx,
                        answers,
                        selected,
                        custom,
                        editing_custom: false,
                        scroll,
                        follow_selection: true,
                    };
                } else {
                    let _ = resp_tx.send(ClarifyResponse {
                        answer: None,
                        choice_index: None,
                        answers,
                    });
                }
                return Ok(Flow::Continue);
            }

            let choices_len = req
                .questions
                .get(question_idx)
                .map(|q| q.choices.len())
                .unwrap_or(0);
            let total_opts = choices_len + 1;

            if editing_custom {
                match key.code {
                    KeyCode::Backspace => {
                        custom.pop();
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom: true,
                            scroll,
                            follow_selection: true,
                        };
                    }
                    KeyCode::Enter => {
                        let q_text = req.questions[question_idx].question.clone();
                        let ans = std::mem::take(&mut custom);
                        answers.push(ClarifyAnswer {
                            question: q_text,
                            answer: ans,
                            choice_index: None,
                        });
                        question_idx += 1;
                        if question_idx >= req.questions.len() {
                            let first_ans = answers.first().map(|a| a.answer.clone());
                            let first_idx = answers.first().and_then(|a| a.choice_index);
                            let _ = resp_tx.send(ClarifyResponse {
                                answer: first_ans,
                                choice_index: first_idx,
                                answers,
                            });
                        } else {
                            st.modal = ActiveModal::Clarify {
                                req,
                                resp_tx,
                                question_idx,
                                answers,
                                selected: 0,
                                custom: String::new(),
                                editing_custom: false,
                                scroll: 0,
                                follow_selection: true,
                            };
                        }
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        custom.push(c);
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom: true,
                            scroll,
                            follow_selection: true,
                        };
                    }
                    _ => {
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom: true,
                            scroll,
                            follow_selection,
                        };
                    }
                }
            } else {
                match key.code {
                    KeyCode::Up => {
                        selected = if selected == 0 {
                            total_opts.saturating_sub(1)
                        } else {
                            selected - 1
                        };
                        follow_selection = true;
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom,
                            scroll,
                            follow_selection,
                        };
                    }
                    KeyCode::Down => {
                        selected = (selected + 1) % total_opts.max(1);
                        follow_selection = true;
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom,
                            scroll,
                            follow_selection,
                        };
                    }
                    KeyCode::PageUp => {
                        scroll = scroll.saturating_sub(5);
                        follow_selection = false;
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom,
                            scroll,
                            follow_selection,
                        };
                    }
                    KeyCode::PageDown => {
                        scroll = scroll.saturating_add(5);
                        follow_selection = false;
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom,
                            scroll,
                            follow_selection,
                        };
                    }
                    KeyCode::Enter => {
                        if selected >= choices_len {
                            st.modal = ActiveModal::Clarify {
                                req,
                                resp_tx,
                                question_idx,
                                answers,
                                selected,
                                custom,
                                editing_custom: true,
                                scroll,
                                follow_selection: true,
                            };
                        } else {
                            let q = &req.questions[question_idx];
                            let chosen = q.choices[selected].clone();
                            answers.push(ClarifyAnswer {
                                question: q.question.clone(),
                                answer: chosen,
                                choice_index: Some(selected),
                            });
                            question_idx += 1;
                            if question_idx >= req.questions.len() {
                                let first_ans = answers.first().map(|a| a.answer.clone());
                                let first_idx = answers.first().and_then(|a| a.choice_index);
                                let _ = resp_tx.send(ClarifyResponse {
                                    answer: first_ans,
                                    choice_index: first_idx,
                                    answers,
                                });
                            } else {
                                st.modal = ActiveModal::Clarify {
                                    req,
                                    resp_tx,
                                    question_idx,
                                    answers,
                                    selected: 0,
                                    custom: String::new(),
                                    editing_custom: false,
                                    scroll: 0,
                                    follow_selection: true,
                                };
                            }
                        }
                    }
                    _ => {
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom,
                            scroll,
                            follow_selection,
                        };
                    }
                }
            }
        }
        ActiveModal::Sudo {
            prompt,
            resp_tx,
            mut input,
        } => match key.code {
            KeyCode::Esc => {
                let _ = resp_tx.send(None);
            }
            KeyCode::Enter => {
                let _ = resp_tx.send(Some(input));
            }
            KeyCode::Backspace => {
                input.pop();
                st.modal = ActiveModal::Sudo {
                    prompt,
                    resp_tx,
                    input,
                };
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.push(c);
                st.modal = ActiveModal::Sudo {
                    prompt,
                    resp_tx,
                    input,
                };
            }
            _ => {
                st.modal = ActiveModal::Sudo {
                    prompt,
                    resp_tx,
                    input,
                };
            }
        },
        ActiveModal::Help { mut scroll } => match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {}
            KeyCode::Up => {
                scroll = scroll.saturating_sub(1);
                st.modal = ActiveModal::Help { scroll };
            }
            KeyCode::Down => {
                scroll = scroll.saturating_add(1).min(18);
                st.modal = ActiveModal::Help { scroll };
            }
            KeyCode::PageUp => {
                scroll = scroll.saturating_sub(6);
                st.modal = ActiveModal::Help { scroll };
            }
            KeyCode::PageDown => {
                scroll = scroll.saturating_add(6).min(18);
                st.modal = ActiveModal::Help { scroll };
            }
            _ => {
                st.modal = ActiveModal::Help { scroll };
            }
        },
        ActiveModal::SubagentsModal {
            items,
            mut selected,
            mut active_only,
        } => {
            if key.code == KeyCode::Esc {
                return Ok(Flow::Continue);
            }
            let filtered_indices: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(_, i)| !active_only || i.status == "running")
                .map(|(idx, _)| idx)
                .collect();

            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if !filtered_indices.is_empty() {
                        selected = if selected == 0 {
                            filtered_indices.len().saturating_sub(1)
                        } else {
                            selected - 1
                        };
                    }
                    st.modal = ActiveModal::SubagentsModal {
                        items,
                        selected,
                        active_only,
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if !filtered_indices.is_empty() {
                        selected = (selected + 1) % filtered_indices.len();
                    }
                    st.modal = ActiveModal::SubagentsModal {
                        items,
                        selected,
                        active_only,
                    };
                }
                KeyCode::Tab | KeyCode::Char('a')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.code == KeyCode::Tab =>
                {
                    active_only = !active_only;
                    selected = 0;
                    st.modal = ActiveModal::SubagentsModal {
                        items,
                        selected,
                        active_only,
                    };
                }
                KeyCode::Enter => {
                    if let Some(&orig_idx) = filtered_indices.get(selected) {
                        st.modal = ActiveModal::SubagentInspector {
                            items,
                            current_idx: orig_idx,
                            scroll: 0,
                        };
                    } else {
                        st.modal = ActiveModal::SubagentsModal {
                            items,
                            selected,
                            active_only,
                        };
                    }
                }
                KeyCode::Char('x') | KeyCode::Char('c')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.code == KeyCode::Char('x') =>
                {
                    if let Some(&orig_idx) = filtered_indices.get(selected) {
                        if let Some(target) = items.get(orig_idx) {
                            let target_id = target.id.clone();
                            st.modal = ActiveModal::SubagentsModal {
                                items,
                                selected,
                                active_only,
                            };
                            actions::interrupt_subagent(env, st, &target_id).await;
                        } else {
                            st.modal = ActiveModal::SubagentsModal {
                                items,
                                selected,
                                active_only,
                            };
                        }
                    } else {
                        st.modal = ActiveModal::SubagentsModal {
                            items,
                            selected,
                            active_only,
                        };
                    }
                }
                _ => {
                    st.modal = ActiveModal::SubagentsModal {
                        items,
                        selected,
                        active_only,
                    };
                }
            }
        }
        ActiveModal::SubagentInspector {
            items,
            mut current_idx,
            mut scroll,
        } => {
            if key.code == KeyCode::Esc || key.code == KeyCode::Backspace {
                st.modal = ActiveModal::SubagentsModal {
                    items,
                    selected: current_idx,
                    active_only: false,
                };
                return Ok(Flow::Continue);
            }
            match key.code {
                KeyCode::Tab => {
                    if !items.is_empty() {
                        current_idx = (current_idx + 1) % items.len();
                        scroll = 0;
                    }
                    st.modal = ActiveModal::SubagentInspector {
                        items,
                        current_idx,
                        scroll,
                    };
                }
                KeyCode::BackTab => {
                    if !items.is_empty() {
                        current_idx = (current_idx + items.len().saturating_sub(1)) % items.len();
                        scroll = 0;
                    }
                    st.modal = ActiveModal::SubagentInspector {
                        items,
                        current_idx,
                        scroll,
                    };
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    st.modal = ActiveModal::SubagentInspector {
                        items,
                        current_idx,
                        scroll,
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = scroll.saturating_add(1);
                    st.modal = ActiveModal::SubagentInspector {
                        items,
                        current_idx,
                        scroll,
                    };
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(10);
                    st.modal = ActiveModal::SubagentInspector {
                        items,
                        current_idx,
                        scroll,
                    };
                }
                KeyCode::PageDown => {
                    scroll = scroll.saturating_add(10);
                    st.modal = ActiveModal::SubagentInspector {
                        items,
                        current_idx,
                        scroll,
                    };
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    scroll = 0;
                    st.modal = ActiveModal::SubagentInspector {
                        items,
                        current_idx,
                        scroll,
                    };
                }
                KeyCode::End | KeyCode::Char('G') => {
                    scroll = usize::MAX / 2;
                    st.modal = ActiveModal::SubagentInspector {
                        items,
                        current_idx,
                        scroll,
                    };
                }
                KeyCode::Char('x') | KeyCode::Char('c')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.code == KeyCode::Char('x') =>
                {
                    if let Some(target) = items.get(current_idx) {
                        let target_id = target.id.clone();
                        st.modal = ActiveModal::SubagentInspector {
                            items,
                            current_idx,
                            scroll,
                        };
                        actions::interrupt_subagent(env, st, &target_id).await;
                    } else {
                        st.modal = ActiveModal::SubagentInspector {
                            items,
                            current_idx,
                            scroll,
                        };
                    }
                }
                _ => {
                    st.modal = ActiveModal::SubagentInspector {
                        items,
                        current_idx,
                        scroll,
                    };
                }
            }
        }
    }
    Ok(Flow::Continue)
}

pub fn close_modal_with_cancel(st: &mut TuiState) {
    let modal = std::mem::replace(&mut st.modal, ActiveModal::None);
    match modal {
        ActiveModal::Approval { resp_tx, .. } => {
            let _ = resp_tx.send(ApprovalDecision::Deny);
        }
        ActiveModal::Clarify {
            resp_tx, answers, ..
        } => {
            let _ = resp_tx.send(ClarifyResponse {
                answer: None,
                choice_index: None,
                answers,
            });
        }
        ActiveModal::Sudo { resp_tx, .. } => {
            let _ = resp_tx.send(None);
        }
        _ => {}
    }
}
