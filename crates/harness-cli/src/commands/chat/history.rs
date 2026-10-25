//! What the chat remembers between sessions: prompt history, and the terminal state to restore.
use super::keys;
use super::render::TurnRecord;

/// Puts the terminal's key reporting back the way it was found, however the chat ends.
pub(super) struct KittyKeyboardGuard;

impl Drop for KittyKeyboardGuard {
    fn drop(&mut self) {
        keys::disable_kitty_keyboard();
    }
}

/// Wipe the terminal on session exit, so only the shell prompt remains.
/// Cosmetic: a failure to write is ignored on purpose (see `commands::clear_screen`).
pub(crate) fn clear_on_exit() {
    crate::commands::clear_screen();
}

pub(super) fn load_prompt_history(
    path: Option<&std::path::Path>,
    initial_turns: &[TurnRecord],
) -> Vec<String> {
    let mut prompts = Vec::new();
    if let Some(p) = path {
        if let Ok(content) = std::fs::read_to_string(p) {
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && prompts.last().map(|s| s != trimmed).unwrap_or(true) {
                    prompts.push(trimmed.to_string());
                }
            }
        }
    }
    for turn in initial_turns {
        let q = turn.user_query.trim();
        if !q.is_empty() && !q.starts_with('•') && prompts.last().map(|s| s != q).unwrap_or(true)
        {
            prompts.push(q.to_string());
        }
    }
    prompts
}

pub(super) fn append_prompt_history(path: Option<&std::path::Path>, prompt: &str) {
    if let Some(p) = path {
        use std::io::Write;
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
        {
            let _ = writeln!(file, "{}", prompt);
        }
    }
}
