use colored::Colorize;
use std::io::{self, Write};
use std::sync::atomic::Ordering;

use super::super::render::PROMPT;
use super::consts::{BUSY_HINT, HINT_MIN_ROOM, IDLE_HINT};
use super::metrics::visible_width;
use super::terminal::get_terminal_width;
use super::view::ChatUi;

impl ChatUi {
    pub fn visible_input(&self, buf: &str) -> String {
        let room = get_terminal_width()
            .saturating_sub(PROMPT.chars().count())
            .saturating_sub(1);
        let len = buf.chars().count();
        if len <= room {
            return buf.to_string();
        }
        let keep = room.saturating_sub(1);
        format!("…{}", buf.chars().skip(len - keep).collect::<String>())
    }
    pub fn prompt_row(&self, buf: &str) -> String {
        format!("{}{}", PROMPT.bold().yellow(), self.visible_input(buf))
    }
    /// Mark whether a turn is in flight, so the empty composer's hint can say what is happening.
    ///
    /// Only the hint reads this: it decides between the idle line and the working one. The chat
    /// loop sets it when it spawns a turn and clears it on every path a turn can end by.
    pub fn set_turn_active(&self, active: bool) {
        self.turn_active.store(active, Ordering::SeqCst);
        if active {
            // The first turn brings the transcript, which scrolls the mark away. Settle the
            // shower before that happens.
            self.stop_landing();
        }
    }
    /// Whether a turn is in flight, as far as the composer's hint is concerned.
    pub fn turn_active(&self) -> bool {
        self.turn_active.load(Ordering::SeqCst)
    }
    /// The hint an empty composer shows in a terminal `width` columns wide, if it fits at all.
    ///
    /// `None` when the composer holds text (the line belongs to the user then) or when there is no
    /// room for it: the hint is drawn on the prompt row, so a hint that made the row wrap would
    /// desync every cursor move the in-place repaint makes.
    pub(super) fn hint_for(&self, width: usize, buf: &str) -> Option<String> {
        if !buf.is_empty() {
            return None;
        }
        let room = width
            .saturating_sub(PROMPT.chars().count())
            .saturating_sub(1);
        if room < HINT_MIN_ROOM {
            return None;
        }
        let text = if self.turn_active() {
            BUSY_HINT
        } else {
            IDLE_HINT
        };
        if text.chars().count() <= room {
            return Some(text.to_string());
        }
        let kept: String = text.chars().take(room.saturating_sub(1)).collect();
        Some(format!("{}…", kept))
    }
    /// The prompt row as the composer paints it, plus the visible width of the hint it carries.
    ///
    /// The hint is drawn *after* the caret on purpose: the caller walks the cursor back over it, so
    /// typing starts right after the prompt and the first character simply overwrites the hint.
    /// The width comes back with the row because only the caller knows what it is about to print.
    pub fn prompt_row_drawn(&self, buf: &str) -> (String, usize) {
        let prompt = self.prompt_row(buf);
        match self.hint_for(get_terminal_width(), buf) {
            Some(hint) => {
                let width = visible_width(&hint);
                (format!("{}{}", prompt, hint.dimmed()), width)
            }
            None => (prompt, 0),
        }
    }
    pub fn redraw_input_line(&self) {
        let _paint = self.paint_in_flight();
        if !self.is_attended || !self.box_drawn.load(Ordering::SeqCst) {
            return;
        }
        if self.modal_active() {
            return;
        }
        let width = get_terminal_width();
        let drawn_width = self
            .drawn_metrics
            .lock()
            .unwrap()
            .as_ref()
            .map(|m| m.width)
            .unwrap_or(0);
        if drawn_width != 0 && drawn_width != width {
            self.redraw_box();
            return;
        }
        let buf = self.input_buf.lock().unwrap_or_else(|e| e.into_inner());
        let (row, hint_width) = self.prompt_row_drawn(&buf);
        print!("\r\x1b[K{}", row);
        if hint_width > 0 {
            print!("\x1b[{hint_width}D");
        }
        let _ = io::stdout().flush();
    }
}
