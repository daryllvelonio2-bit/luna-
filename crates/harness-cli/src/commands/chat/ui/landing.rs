use std::io::{self, Write};
use std::sync::atomic::Ordering;

use super::super::render::{activity_line, landing_rows, landing_star_rows};
use super::metrics::reasoning_window;
use super::panels::LandingAnimation;
use super::terminal::{get_terminal_height, get_terminal_width};
use super::view::ChatUi;

impl ChatUi {
    /// Draw the mark for a session that has no transcript yet.
    ///
    /// The first frame is printed here and the mark is registered with the UI; the animation
    /// tick then keeps rewriting those same rows in place, so the shower carries on after the
    /// composer has appeared below it. The mark belongs to the scrollback and leaves with the
    /// first turn, which is also when [`Self::stop_landing`] settles it.
    ///
    /// The height check leaves room for the composer and a blank line on either side of the mark,
    /// so a short terminal collapses to the compact wordmark instead of pushing the prompt off
    /// the screen. Every frame keeps the same height, and the block is followed by exactly one
    /// blank line before the composer, so the tick can walk the cursor between the two by count.
    pub fn draw_landing(&self) {
        if !self.is_attended {
            return;
        }
        let width = get_terminal_width();
        let height = get_terminal_height();
        let available = height.saturating_sub(self.box_row_count() + 2);
        let rows = landing_rows(width, available);
        if rows.is_empty() {
            self.stop_landing();
            return;
        }

        println!();
        // Always the same shape: one leading blank line, the mark, and the blank line that keeps
        // the composer clear of it. The count is what the repaint walks back up, so a one-row
        // mark and a ten-row moon must agree on it.
        let frame_rows = landing_star_rows(&rows, 0);
        for (index, row) in frame_rows.iter().enumerate() {
            print!("{}", row);
            if index + 1 < frame_rows.len() {
                println!();
            }
        }
        println!();
        println!();
        let _ = io::stdout().flush();

        *self.landing.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(LandingAnimation { rows, frame: 0 });
    }
    /// Stop advancing the landing shower.
    ///
    /// The rows already on screen stay exactly as they were; only future frames stop. Called when
    /// the mark is about to move (a transcript line, a turn, a screen clear) or the terminal
    /// changes width, because the repaint finds the mark by counting rows above the prompt.
    pub fn stop_landing(&self) {
        self.landing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
    /// The landing rows as they belong at the top of the drawn block: the current frame, then the
    /// blank line that separates the mark from the composer.
    ///
    /// `None` when no shower is running. The caller decides whether the prefix belongs in the
    /// block it is measuring; it only does while the mark is still directly above the composer.
    pub(super) fn landing_block_prefix(&self) -> Option<Vec<String>> {
        let landing = self.landing.lock().unwrap_or_else(|e| e.into_inner());
        let landing = landing.as_ref()?;
        let mut prefix = landing_star_rows(&landing.rows, landing.frame);
        prefix.push(String::new());
        Some(prefix)
    }
    /// Advance the shower by one frame. Returns false when there is nothing to animate.
    fn advance_landing_frame(&self) -> bool {
        let mut landing = self.landing.lock().unwrap_or_else(|e| e.into_inner());
        match landing.as_mut() {
            Some(landing) => {
                landing.frame = landing.frame.wrapping_add(1);
                true
            }
            None => false,
        }
    }
    /// Repaint the landing at its next frame.
    ///
    /// This runs on its own, faster tick than the live region. It reuses the composer's in-place
    /// repaint: the mark's rows are part of the block measured from the prompt, so the same
    /// sequence walks up to them, rewrites only the rows whose stars moved, and returns the
    /// cursor to the prompt. If the layout no longer matches (the composer grew a panel, a
    /// resize raced the tick), the composer is redrawn and the shower keeps running from the new
    /// row positions instead of freezing.
    pub fn animate_landing(&self) {
        if !self.is_attended || !self.box_drawn.load(Ordering::SeqCst) || self.modal_active() {
            return;
        }
        if !self.advance_landing_frame() {
            return;
        }

        let width = get_terminal_width();
        let buf = self
            .input_buf
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(sequence) = self.repaint_block_sequence(width, &buf) {
            print!("{sequence}");
            let _ = io::stdout().flush();
        } else {
            self.redraw_box();
        }
    }
    pub fn handle_resize(&self) {
        let _paint = self.paint_in_flight();
        if !self.is_attended {
            return;
        }
        if self.modal_active() {
            return;
        }
        // The screen is cleared and the transcript redrawn at the new width: the old mark is gone
        // with it, so there is no block for the shower to rewrite.
        self.stop_landing();
        let width = get_terminal_width();
        let buf = self
            .input_buf
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let chrome = self.box_row_count() + 2;

        let rows = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let mut rows = reasoning_window(&live, width, chrome);

            if let Some(activity) = &mut live.activity {
                rows.push(activity_line(
                    activity.frame,
                    &activity.call,
                    activity.detail.as_deref(),
                    activity.started.elapsed(),
                    width,
                ));
            }
            live.rows = rows.clone();
            rows
        };

        // The screen has already been cleared and the transcript redrawn at the new width.
        // There is no old box on screen to erase — calling erase_live_and_box here would
        // move upward from the end of the redrawn transcript and erase the latest conversation lines!
        for row in &rows {
            println!("{}", row);
        }
        self.draw_box(width, &buf);
    }
}
