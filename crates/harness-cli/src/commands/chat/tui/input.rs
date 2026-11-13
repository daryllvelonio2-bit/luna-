//! Keyboard, mouse, and paste event handling for the TUI.

use std::io;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use harness_core::{ApprovalDecision, HarnessError, ReasoningEffort};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::super::interactive::state::{Env, Flow};
use super::actions::{
    apply_agent_mode, apply_model_selection, apply_thinking_selection, create_new_session,
    interrupt_turn, open_session_picker, run_palette_action, run_slash_command, submit_composer,
    switch_to_session, sync_slash_popup,
};
use super::render::{filtered_models, filtered_palette_items, filtered_sessions};
use super::state::{ActiveModal, AgentMode, TabHitTarget, TuiState};
use crate::commands::clarify::{ClarifyAnswer, ClarifyResponse};

/// Dispatch one terminal event (`Key`, `Mouse`, `Paste`, `Resize`).
pub async fn handle_event(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    event: Event,
) -> Result<Flow, HarnessError> {
    match event {
        Event::Key(key) => {
            // Ignore key-release events on terminals with enhanced keyboard reporting.
            if key.kind == KeyEventKind::Release {
                return Ok(Flow::Continue);
            }
            handle_key(env, st, terminal, key).await
        }
        Event::Mouse(mouse) => handle_mouse(env, st, mouse).await,
        Event::Paste(text) => {
            handle_paste(env, st, &text);
            Ok(Flow::Continue)
        }
        Event::Resize(_, _) => Ok(Flow::Continue),
        Event::FocusGained | Event::FocusLost => Ok(Flow::Continue),
    }
}

fn handle_paste(env: &Env<'_>, st: &mut TuiState, text: &str) {
    let cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
    match &mut st.modal {
        ActiveModal::None => {
            st.composer.insert_str(&cleaned);
            sync_slash_popup(env, st);
        }
        ActiveModal::CommandPalette { query, selected } => {
            query.push_str(cleaned.trim());
            *selected = 0;
        }
        ActiveModal::SessionPicker { query, selected, .. } => {
            query.push_str(cleaned.trim());
            *selected = 0;
        }
        ActiveModal::ModelPicker { query, selected, .. } => {
            query.push_str(cleaned.trim());
            *selected = 0;
        }
        ActiveModal::Clarify {
            custom,
            editing_custom,
            ..
        } if *editing_custom => {
            custom.push_str(cleaned.trim());
        }
        ActiveModal::Sudo { input, .. } => {
            input.push_str(cleaned.trim());
        }
        _ => {}
    }
}

async fn handle_mouse(
    env: &mut Env<'_>,
    st: &mut TuiState,
    mouse: MouseEvent,
) -> Result<Flow, HarnessError> {
    if !matches!(st.modal, ActiveModal::None) {
        return Ok(Flow::Continue);
    }
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            st.scroll_from_bottom = (st.scroll_from_bottom + 3).min(st.max_scroll);
        }
        MouseEventKind::ScrollDown => {
            st.scroll_from_bottom = st.scroll_from_bottom.saturating_sub(3);
        }
        MouseEventKind::Down(MouseButton::Left) if mouse.row == 0 => {
            let hit = st
                .hit_boxes
                .iter()
                .find(|hb| mouse.column >= hb.x_start && mouse.column < hb.x_end)
                .map(|hb| hb.target.clone());
            if let Some(target) = hit {
                match target {
                    TabHitTarget::OverflowButton => {
                        open_session_picker(env, st).await;
                    }
                    TabHitTarget::Session(id) => {
                        if let Err(e) = switch_to_session(env, st, &id).await {
                            env.ui.set_transient_error(Some(format!("Session switch failed: {e}")));
                        }
                    }
                    TabHitTarget::NewSession => {
                        if let Err(e) = create_new_session(env, st).await {
                            env.ui.set_transient_error(Some(format!("New session failed: {e}")));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(Flow::Continue)
}

async fn handle_key(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    key: KeyEvent,
) -> Result<Flow, HarnessError> {
    // Ctrl+C always works (interrupt active turn, or double-tap to exit).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        if st.active_turn.is_some() {
            interrupt_turn(env, st).await;
            st.ctrl_c_armed = None;
            return Ok(Flow::Continue);
        }
        if !matches!(st.modal, ActiveModal::None) {
            close_modal_with_cancel(st);
            return Ok(Flow::Continue);
        }
        if let Some(armed) = st.ctrl_c_armed {
            if armed.elapsed().as_secs() < 2 {
                return Ok(Flow::Break);
            }
        }
        st.ctrl_c_armed = Some(std::time::Instant::now());
        return Ok(Flow::Continue);
    }

    // Disarm Ctrl+C exit prompt on any other key.
    st.ctrl_c_armed = None;

    // If a modal is active, it owns all keyboard input.
    if !matches!(st.modal, ActiveModal::None) {
        return handle_modal_key(env, st, terminal, key).await;
    }

    // Ctrl+P opens the Command Palette.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p') {
        st.modal = ActiveModal::CommandPalette {
            query: String::new(),
            selected: 0,
        };
        return Ok(Flow::Continue);
    }

    // Shift+Tab cycles Agent Mode (`Build` -> `Auto` -> `Plan`).
    if key.code == KeyCode::BackTab {
        apply_agent_mode(env, st, st.mode.cycle());
        return Ok(Flow::Continue);
    }

    // Ctrl+N opens a new session tab.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('n') {
        if let Err(e) = create_new_session(env, st).await {
            env.ui.set_transient_error(Some(format!("New session failed: {e}")));
        }
        return Ok(Flow::Continue);
    }

    // Ctrl+O toggles thinking/tool expansion (or live tool output tail while running).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('o') {
        if env.ui.has_live_activity() {
            env.ui.toggle_live_expanded();
        } else {
            st.process_expanded = !st.process_expanded;
        }
        return Ok(Flow::Continue);
    }

    // Ctrl+T toggles todo panel collapse.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('t') {
        env.ui.toggle_todo_collapsed();
        return Ok(Flow::Continue);
    }

    // Ctrl+V pastes clipboard image (if present) or clipboard text.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('v') {
        handle_ctrl_v(env, st);
        return Ok(Flow::Continue);
    }

    // Slash autocomplete popup intercepts navigation & Tab/Enter/Esc.
    let slash_active = !st.slash_popup.dismissed
        && !st.slash_popup.matches.is_empty()
        && st.composer.text.starts_with('/')
        && !st.composer.text.contains('\n')
        && !st.composer.text[1..].contains(' ');

    if slash_active {
        match key.code {
            KeyCode::Up => {
                if st.slash_popup.selected > 0 {
                    st.slash_popup.selected -= 1;
                } else {
                    st.slash_popup.selected = st.slash_popup.matches.len().saturating_sub(1);
                }
                return Ok(Flow::Continue);
            }
            KeyCode::Down => {
                st.slash_popup.selected =
                    (st.slash_popup.selected + 1) % st.slash_popup.matches.len();
                return Ok(Flow::Continue);
            }
            KeyCode::Tab => {
                if let Some(entry) = st.slash_popup.matches.get(st.slash_popup.selected) {
                    st.composer.set_text(format!("{} ", entry.name));
                    sync_slash_popup(env, st);
                }
                return Ok(Flow::Continue);
            }
            KeyCode::Esc => {
                st.slash_popup.dismissed = true;
                return Ok(Flow::Continue);
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(entry) = st.slash_popup.matches.get(st.slash_popup.selected).cloned() {
                    // Commands that expect an argument complete into composer if user hasn't typed exact match.
                    let needs_arg = matches!(entry.name.as_str(), "/name" | "/goal" | "/fork");
                    if needs_arg && st.composer.text != entry.name {
                        st.composer.set_text(format!("{} ", entry.name));
                        sync_slash_popup(env, st);
                        return Ok(Flow::Continue);
                    }
                    st.composer.clear();
                    sync_slash_popup(env, st);
                    return run_slash_command(env, st, terminal, &entry.name, &entry.name).await;
                }
            }
            _ => {}
        }
    }

    // Escape interrupts active turn or clears transient error.
    if key.code == KeyCode::Esc {
        if st.active_turn.is_some() {
            interrupt_turn(env, st).await;
        } else if env.ui.transient_error().is_some() {
            env.ui.clear_transient_error();
        }
        return Ok(Flow::Continue);
    }

    // Transcript scrolling keys
    match key.code {
        KeyCode::PageUp => {
            let step = (st.last_rendered_rect.1 as usize / 2).max(4);
            st.scroll_from_bottom = (st.scroll_from_bottom + step).min(st.max_scroll);
            return Ok(Flow::Continue);
        }
        KeyCode::PageDown => {
            let step = (st.last_rendered_rect.1 as usize / 2).max(4);
            st.scroll_from_bottom = st.scroll_from_bottom.saturating_sub(step);
            return Ok(Flow::Continue);
        }
        _ => {}
    }

    // Composer editing & submission
    match key.code {
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT)
            {
                st.composer.insert_char('\n');
                sync_slash_popup(env, st);
            } else {
                return submit_composer(env, st, terminal).await;
            }
        }
        KeyCode::Backspace => {
            if key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT)
            {
                st.composer.delete_word_back();
            } else {
                st.composer.backspace();
            }
            sync_slash_popup(env, st);
        }
        KeyCode::Delete => {
            st.composer.delete();
            sync_slash_popup(env, st);
        }
        KeyCode::Left => st.composer.move_left(),
        KeyCode::Right => st.composer.move_right(),
        KeyCode::Home => st.composer.move_home(),
        KeyCode::End => st.composer.move_end(),
        KeyCode::Up => {
            if st.composer.is_on_first_line() {
                st.composer.history_prev();
                sync_slash_popup(env, st);
            } else {
                st.composer.move_up_line();
            }
        }
        KeyCode::Down => {
            if st.composer.is_on_last_line() {
                st.composer.history_next();
                sync_slash_popup(env, st);
            } else {
                st.composer.move_down_line();
            }
        }
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match c {
                    'a' => st.composer.move_home(),
                    'e' => st.composer.move_end(),
                    'w' => {
                        st.composer.delete_word_back();
                        sync_slash_popup(env, st);
                    }
                    'u' => {
                        st.composer.clear();
                        sync_slash_popup(env, st);
                    }
                    _ => {}
                }
            } else if !key.modifiers.contains(KeyModifiers::ALT) {
                st.composer.insert_char(c);
                sync_slash_popup(env, st);
            }
        }
        _ => {}
    }

    Ok(Flow::Continue)
}

fn handle_ctrl_v(env: &Env<'_>, st: &mut TuiState) {
    if let Ok(Some(img)) = crate::commands::clipboard_image::read_clipboard_image() {
        let data_url = img.data_url();
        st.pending_images
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(data_url);
        let count = st
            .pending_images
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        st.system_notices
            .push(format!("Attached clipboard image ({}x{}, {} pending)", img.width, img.height, count));
        return;
    }
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if let Ok(text) = cb.get_text() {
            let cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
            st.composer.insert_str(&cleaned);
            sync_slash_popup(env, st);
        }
    }
}

async fn handle_modal_key(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    key: KeyEvent,
) -> Result<Flow, HarnessError> {
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
                    if let Some(id) = filtered_ids
                        .get(selected.min(filtered_ids.len().saturating_sub(1)))
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
        ActiveModal::ModelPicker {
            mut query,
            items,
            mut selected,
        } => {
            if key.code == KeyCode::Esc {
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
                        query,
                        items,
                        selected,
                    };
                }
                KeyCode::Enter => {
                    if let Some((prov, model)) = filtered_pairs
                        .get(selected.min(filtered_pairs.len().saturating_sub(1)))
                    {
                        if let Err(e) = apply_model_selection(env, st, prov, model, true).await {
                            env.ui
                                .set_transient_error(Some(format!("Model switch failed: {e}")));
                        }
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    st.modal = ActiveModal::ModelPicker {
                        query,
                        items,
                        selected: 0,
                    };
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(c);
                    st.modal = ActiveModal::ModelPicker {
                        query,
                        items,
                        selected: 0,
                    };
                }
                _ => {
                    st.modal = ActiveModal::ModelPicker {
                        query,
                        items,
                        selected,
                    };
                }
            }
        }
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
                KeyCode::Esc => {}
                KeyCode::Up => {
                    selected = if selected == 0 { 6 } else { selected - 1 };
                    st.modal = ActiveModal::ThinkingPicker { current, selected };
                }
                KeyCode::Down => {
                    selected = (selected + 1) % 7;
                    st.modal = ActiveModal::ThinkingPicker { current, selected };
                }
                KeyCode::Enter => {
                    apply_thinking_selection(env, options[selected]).await;
                }
                _ => {
                    st.modal = ActiveModal::ThinkingPicker { current, selected };
                }
            }
        }
        ActiveModal::Approval {
            req,
            resp_tx,
            mut selected,
        } => match key.code {
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
            KeyCode::Up => {
                selected = if selected == 0 { 2 } else { selected - 1 };
                st.modal = ActiveModal::Approval {
                    req,
                    resp_tx,
                    selected,
                };
            }
            KeyCode::Down => {
                selected = (selected + 1) % 3;
                st.modal = ActiveModal::Approval {
                    req,
                    resp_tx,
                    selected,
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
                _ => {
                    let _ = resp_tx.send(ApprovalDecision::Deny);
                }
            },
            _ => {
                st.modal = ActiveModal::Approval {
                    req,
                    resp_tx,
                    selected,
                };
            }
        },
        ActiveModal::Clarify {
            req,
            resp_tx,
            mut question_idx,
            mut answers,
            mut selected,
            mut custom,
            editing_custom,
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
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom,
                        };
                    }
                    KeyCode::Down => {
                        selected = (selected + 1) % total_opts.max(1);
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom,
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
                scroll = scroll.saturating_add(1).min(12);
                st.modal = ActiveModal::Help { scroll };
            }
            _ => {
                st.modal = ActiveModal::Help { scroll };
            }
        },
    }
    Ok(Flow::Continue)
}

fn close_modal_with_cancel(st: &mut TuiState) {
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
