//! Normal-mode keyboard input handling, shortcuts, and composer editing for the TUI.

use std::io;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use harness_core::HarnessError;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::super::super::interactive::state::{Env, Flow};
use super::super::actions::{
    apply_agent_mode, create_new_session, interrupt_turn, run_slash_command,
    set_all_process_expanded, submit_composer, sync_slash_popup,
};
use super::super::state::{ActiveModal, TuiState};
use super::modals::{close_modal_with_cancel, handle_modal_key};
use super::mouse::clear_selection;

pub async fn handle_key(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    key: KeyEvent,
) -> Result<Flow, HarnessError> {
    // Copy selected text to clipboard on Ctrl+C or Ctrl+Insert when a selection exists
    if (key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C')))
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Insert)
    {
        if let Some(selection) = st.selection {
            if selection.is_range() || st.click_count >= 2 {
                super::mouse::copy_selection_to_clipboard(env, st);
                return Ok(Flow::Continue);
            }
        }
    }

    // Ctrl+C always works (interrupt active turn, or double-tap to exit).
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
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

    // Disarm Ctrl+C exit prompt on any other key. The Esc arm must survive Esc itself,
    // or the second press that confirms the interrupt is never seen.
    st.ctrl_c_armed = None;
    if key.code != KeyCode::Esc {
        st.esc_armed = None;
    }

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

    // Ctrl+B (or Ctrl+Shift+B) opens the Subagents manager.
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && (key.code == KeyCode::Char('b') || key.code == KeyCode::Char('B'))
    {
        super::super::actions::open_subagents_list(env, st).await;
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
            env.ui
                .set_transient_error(Some(format!("New session failed: {e}")));
        }
        return Ok(Flow::Continue);
    }

    // Ctrl+O toggles thinking/tool expansion (or live tool output tail while running).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('o') {
        if env.ui.has_live_activity() {
            env.ui.toggle_live_expanded();
        } else {
            set_all_process_expanded(st, !st.process_expanded);
        }
        return Ok(Flow::Continue);
    }

    // Ctrl+T toggles todo panel collapse.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('t') {
        env.ui.toggle_todo_collapsed();
        return Ok(Flow::Continue);
    }

    // Ctrl+V, Ctrl+Shift+V, or Shift+Insert pastes clipboard image (if present) or clipboard text.
    let is_paste = (key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V')))
        || (key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::Insert);
    if is_paste {
        st.voice_pending_autosend_at = None;
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
                    if entry.name == "/automation" {
                        st.composer
                            .set_text(harness_scheduler::AUTOMATION_PREFILL.to_string());
                    } else if entry.name == "/plugin" {
                        st.composer
                            .set_text(harness_plugins::PLUGIN_PREFILL.to_string());
                    } else {
                        st.composer.set_text(format!("{} ", entry.name));
                    }
                    sync_slash_popup(env, st);
                }
                return Ok(Flow::Continue);
            }
            KeyCode::Esc => {
                st.slash_popup.dismissed = true;
                return Ok(Flow::Continue);
            }
            KeyCode::Enter
                if !key.modifiers.contains(KeyModifiers::SHIFT)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(entry) = st.slash_popup.matches.get(st.slash_popup.selected).cloned() {
                    if entry.name == "/automation" {
                        st.composer
                            .set_text(harness_scheduler::AUTOMATION_PREFILL.to_string());
                        sync_slash_popup(env, st);
                        return Ok(Flow::Continue);
                    }
                    if entry.name == "/plugin" {
                        st.composer
                            .set_text(harness_plugins::PLUGIN_PREFILL.to_string());
                        sync_slash_popup(env, st);
                        return Ok(Flow::Continue);
                    }
                    let is_worker = entry.details.starts_with("Worker · ");
                    let needs_arg =
                        is_worker || matches!(entry.name.as_str(), "/name" | "/goal" | "/fork");
                    if needs_arg && (is_worker || st.composer.text != entry.name) {
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

    // Escape clears a text selection first, then interrupts or clears the transient error.
    if key.code == KeyCode::Esc {
        st.voice_pending_autosend_at = None;
        if st.selection.is_some() {
            clear_selection(st);
            return Ok(Flow::Continue);
        }
        if st.active_turn.is_some() {
            // A single Esc only arms the interrupt: a turn is expensive to lose, so the
            // second Esc within two seconds is the one that stops it.
            if st.esc_armed.is_some_and(|at| at.elapsed().as_secs() < 2) {
                st.esc_armed = None;
                interrupt_turn(env, st).await;
            } else {
                st.esc_armed = Some(std::time::Instant::now());
            }
        } else if env.ui.transient_error().is_some() {
            env.ui.clear_transient_error();
        }
        return Ok(Flow::Continue);
    }

    // Transcript scrolling keys
    match key.code {
        KeyCode::PageUp => {
            let step = (st.last_rendered_rect.1 as usize / 2).max(4);
            st.scroll_transcript_up(step);
            clear_selection(st);
            return Ok(Flow::Continue);
        }
        KeyCode::PageDown => {
            let step = (st.last_rendered_rect.1 as usize / 2).max(4);
            st.scroll_transcript_down(step);
            clear_selection(st);
            return Ok(Flow::Continue);
        }
        _ => {}
    }

    // Composer editing & submission
    match key.code {
        KeyCode::Enter => {
            st.voice_pending_autosend_at = None;
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
            st.voice_pending_autosend_at = None;
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
            st.voice_pending_autosend_at = None;
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
            st.voice_pending_autosend_at = None;
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
        st.composer
            .attach_image(img.width, img.height, img.data_url());
        sync_slash_popup(env, st);
        return;
    }
    if let Some(text) = crate::commands::clipboard_image::read_clipboard_text() {
        st.composer.paste_text(&text);
        sync_slash_popup(env, st);
    }
}
