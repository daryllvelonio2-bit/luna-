//! Keyboard, mouse, and paste event handling for the TUI.

use std::io;

use crossterm::event::{Event, KeyEventKind};
use harness_core::HarnessError;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::super::interactive::state::{Env, Flow};
use super::actions::sync_slash_popup;
use super::state::{ActiveModal, TuiState};

pub mod keys;
pub mod modals;
pub mod mouse;

#[cfg(test)]
mod tests;

pub use keys::handle_key;
#[allow(unused_imports)]
pub use modals::{close_modal_with_cancel, handle_modal_key};
#[allow(unused_imports)]
pub use mouse::{clear_selection, handle_mouse};

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
        Event::Resize(_, _) => {
            // Ratatui autoresizes on draw. Ensure buffer geometry matches the new window
            // size without emitting an ANSI clear sequence that causes screen flicker.
            let _ = terminal.autoresize();
            Ok(Flow::Continue)
        }
        Event::FocusGained | Event::FocusLost => Ok(Flow::Continue),
    }
}

pub fn handle_paste(env: &Env<'_>, st: &mut TuiState, text: &str) {
    let cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
    match &mut st.modal {
        ActiveModal::None => {
            if cleaned.is_empty() {
                if let Ok(Some(img)) = crate::commands::clipboard_image::read_clipboard_image() {
                    st.composer
                        .attach_image(img.width, img.height, img.data_url());
                    sync_slash_popup(env, st);
                }
                return;
            }
            st.composer.paste_text(&cleaned);
            sync_slash_popup(env, st);
        }
        ActiveModal::CommandPalette { query, selected } => {
            query.push_str(cleaned.trim());
            *selected = 0;
        }
        ActiveModal::SessionPicker {
            query, selected, ..
        } => {
            query.push_str(cleaned.trim());
            *selected = 0;
        }
        ActiveModal::ProviderPicker {
            query, selected, ..
        } => {
            query.push_str(cleaned.trim());
            *selected = 0;
        }
        ActiveModal::ModelPicker {
            query, selected, ..
        } => {
            query.push_str(cleaned.trim());
            *selected = 0;
        }
        ActiveModal::Approval {
            input,
            editing_input,
            ..
        } if *editing_input => {
            input.push_str(cleaned.trim());
            env.approval_activity.notify_one();
        }
        ActiveModal::Clarify {
            custom,
            editing_custom,
            follow_selection,
            ..
        } if *editing_custom => {
            custom.push_str(cleaned.trim());
            *follow_selection = true;
        }
        ActiveModal::Sudo { input, .. } => {
            input.push_str(cleaned.trim());
        }
        ActiveModal::InputModal { value, .. } => {
            value.push_str(cleaned.trim());
        }
        _ => {}
    }
}
