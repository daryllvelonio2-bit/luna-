use colored::Colorize;
use std::io::{self, Write};
use std::sync::atomic::Ordering;

use super::super::render::{activity_line, compaction_line, PROMPT};
use super::metrics::{physical_line_count, reasoning_window, tool_tail_limit, visible_width};
use super::panels::BoxDrawnMetrics;
use super::terminal::{get_terminal_height, get_terminal_width};
use super::view::ChatUi;

impl ChatUi {
    pub fn erase_live_and_box(&self, previous_live_rows: usize) {
        let _paint = self.paint_in_flight();
        if !self.is_attended || self.is_tui_mode() {
            return;
        }
        if self.modal_active() {
            return;
        }
        self.box_drawn.store(false, Ordering::SeqCst);

        let metrics_opt = self
            .drawn_metrics
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let (rows_below, rows_above) = if let Some(m) = metrics_opt {
            let current_width = get_terminal_width().max(1);
            let bottom_rows = physical_line_count(m.divider_len, current_width);
            let prompt_rows = physical_line_count(m.prompt_len, current_width);
            let top_divider_rows = physical_line_count(m.divider_len, current_width);
            let status_rows = if m.status_line_len > 0 {
                physical_line_count(m.status_line_len, current_width)
            } else {
                0
            };
            let queued_rows = if m.queued_line_len > 0 {
                physical_line_count(m.queued_line_len, current_width)
            } else {
                0
            };
            let live_rows: usize = m
                .live_rows_lens
                .iter()
                .map(|&len| physical_line_count(len, current_width))
                .sum();
            // The panel sits above the box, so it is erased with the region above it.
            let panel_rows: usize = m
                .panel_rows_lens
                .iter()
                .map(|&len| physical_line_count(len, current_width))
                .sum();
            let delegate_rows: usize = m
                .delegate_rows_lens
                .iter()
                .map(|&len| physical_line_count(len, current_width))
                .sum();
            let memory_notice_rows: usize = m
                .memory_notice_rows_lens
                .iter()
                .map(|&len| physical_line_count(len, current_width))
                .sum();

            let below = bottom_rows;
            let above = prompt_rows.saturating_sub(1)
                + top_divider_rows
                + status_rows
                + queued_rows
                + live_rows
                + panel_rows
                + delegate_rows
                + memory_notice_rows;
            (below, above)
        } else {
            let total = previous_live_rows + self.box_row_count();
            (1, total.saturating_sub(1))
        };

        if rows_below > 0 {
            print!("\x1b[{}B", rows_below);
        }

        let term_height = get_terminal_height();
        let total_rows = (rows_below + rows_above + 1).min(term_height);
        for i in 0..total_rows {
            print!("\r\x1b[2K");
            if i + 1 < total_rows {
                print!("\x1b[1A");
            }
        }
        print!("\r\x1b[J");
        let _ = io::stdout().flush();
    }
    pub fn emit_above(&self, text: &str) {
        let _paint = self.paint_in_flight();
        // Record before the early returns below: a line held for a modal is still the command's
        // output, and the turn must keep it even though the screen does not show it yet.
        if let Some(capture) = self
            .command_capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            capture.push(text.to_string());
        }
        if self.is_tui_mode() {
            let capturing = self
                .command_capture
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            if !capturing {
                self.tui_notices
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(text.to_string());
                self.notify_tui();
            }
            return;
        }
        if !self.is_attended {
            println!("{}", text);
            return;
        }
        if self.modal_active() {
            // The modal owns the alternate screen: writing here would put the line on that screen
            // and lose it when the screen is given back. Hold it for the transcript instead.
            self.pending_above
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(text.to_string());
            return;
        }
        // A transcript line lands between the mark and the composer, so the animation's row
        // arithmetic no longer holds. The mark keeps whatever frame it last drew.
        self.stop_landing();
        let buf = self.input_buf.lock().unwrap_or_else(|e| e.into_inner());
        let width = get_terminal_width();
        let previous = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let prev = live.rows.len();
            live.rows.clear();
            prev
        };
        self.erase_live_and_box(previous);
        println!("{}", text);
        self.draw_box(width, &buf);
    }
    pub fn set_live(&self, rows: Vec<String>) {
        let _paint = self.paint_in_flight();
        if self.is_tui_mode() {
            self.notify_tui();
            return;
        }
        if !self.is_attended {
            for row in &rows {
                println!("{}", row);
            }
            return;
        }
        if self.modal_active() {
            return;
        }
        let buf = self.input_buf.lock().unwrap_or_else(|e| e.into_inner());
        let width = get_terminal_width();
        let previous = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let prev = live.rows.len();
            live.rows = rows.clone();
            prev
        };
        // The fast path: the block keeps its height, so it is rewritten where it stands and the
        // screen never blanks. This is what stops the status line from flickering on every tick.
        if let Some(sequence) = self.repaint_block_sequence(width, &buf) {
            print!("{sequence}");
            let _ = io::stdout().flush();
            return;
        }
        self.erase_live_and_box(previous);
        for row in &rows {
            println!("{}", row);
        }
        self.draw_box(width, &buf);
    }
    pub fn animate_tick(&self) {
        if !self.is_attended || self.is_tui_mode() {
            return;
        }
        let width = get_terminal_width();
        // Rows the composer below the region draws: the block may fill the rest of the screen, and
        // it is measured before the live lock is taken so the two locks are never held together.
        let chrome = self.box_row_count() + 2;

        let rows = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let mut rows = reasoning_window(&live, width, chrome);

            if let Some(activity) = &mut live.activity {
                activity.frame = activity.frame.wrapping_add(1);
                rows.push(activity_line(
                    activity.frame,
                    &activity.call,
                    activity.detail.as_deref(),
                    activity.started.elapsed(),
                    width,
                ));
                // The newest lines of what the call is printing, under the call itself.
                rows.extend(live.tool_output.rows(width, tool_tail_limit(&live)));
            }

            if let Some(compaction) = &mut live.compaction {
                compaction.frame = compaction.frame.wrapping_add(1);
                rows.push(compaction_line(
                    compaction.frame,
                    compaction.forced,
                    compaction.started.elapsed(),
                    width,
                ));
            }

            rows
        };

        if rows.is_empty()
            && self
                .live
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .rows
                .is_empty()
            && self.delegate_tasks.lock().unwrap().is_empty()
        {
            return;
        }

        self.set_live(rows);
    }
    /// The composer as it stands, top to bottom: the todo panel, the status line, and the two
    /// rules around the prompt.
    ///
    /// The prompt sits between the rules, so the row the rules leave blank is the
    /// prompt's own row.
    pub fn composer_rows(&self, width: usize, buf: &str) -> Vec<String> {
        let divider = "─".repeat(width).dimmed().to_string();
        let mut rows = Vec::new();
        // The panel is chrome: it is drawn above the composer, so it stays put while
        // the conversation scrolls under it.
        rows.extend(self.todo_panel_rows(width));
        // A running child is also chrome: its row stays above the composer after the main turn
        // ends, so the user can still see which delegated task is running.
        rows.extend(self.delegate_task_rows(width));
        // What the detached saver just wrote: visible until the next turn, not part of the
        // transcript.
        rows.extend(self.memory_notice_rows(width));
        if let Some(queued) = self.format_queue_line(width) {
            rows.push(queued);
        }
        if let Some(status) = self.format_status_line(width) {
            rows.push(status);
        }
        rows.push(divider.clone());
        rows.push(self.prompt_row_drawn(buf).0);
        rows.push(divider);
        rows
    }
    pub fn draw_box(&self, width: usize, buf: &str) {
        let _paint = self.paint_in_flight();
        if self.is_tui_mode() {
            self.notify_tui();
            return;
        }
        if self.modal_active() {
            return;
        }
        let rows = self.composer_rows(width, buf);
        let prompt_str = rows[rows.len() - 2].clone();
        let bottom_rule = rows[rows.len() - 1].clone();
        // The hint is drawn after a caret that belongs before it, so the cursor walks back over
        // it: the next keypress has to land right after the prompt, not after the hint.
        let hint_width = self
            .hint_for(width, buf)
            .map(|hint| visible_width(&hint))
            .unwrap_or(0);

        for row in &rows[..rows.len() - 2] {
            println!("{}", row);
        }
        // The rules are printed around a blank line, and the prompt is written over
        // that blank line: the cursor moves back up to it.
        println!();
        println!("{}", bottom_rule);
        print!("\x1b[2A\r{}", prompt_str);
        if hint_width > 0 {
            print!("\x1b[{hint_width}D");
        }
        let _ = io::stdout().flush();

        let live_rows = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rows
            .clone();
        let live_rows_lens = live_rows.iter().map(|r| visible_width(r)).collect();

        let panel = self.todo_panel_rows(width);
        let delegate_rows = self.delegate_task_rows(width);
        let memory_notice_rows = self.memory_notice_rows(width);
        let queue_line = self.format_queue_line(width);
        let status_str = self.format_status_line(width);

        // The block is everything between the transcript and the prompt: the live region, then the
        // composer's own rows, ending with the rule above the prompt. While the fresh-session
        // shower is running, the mark and its blank separator sit directly above the live region,
        // so they are measured here too — that is what lets the animation tick find them from the
        // prompt and rewrite them in place.
        let mut block_rows = self.landing_block_prefix().unwrap_or_default();
        block_rows.extend(live_rows.clone());
        block_rows.extend(rows[..rows.len() - 2].iter().cloned());

        *self.drawn_metrics.lock().unwrap_or_else(|e| e.into_inner()) = Some(BoxDrawnMetrics {
            width,
            queued_line_len: queue_line.as_ref().map(|s| visible_width(s)).unwrap_or(0),
            status_line_len: status_str.as_ref().map(|s| visible_width(s)).unwrap_or(0),
            divider_len: width,
            prompt_len: visible_width(&prompt_str),
            live_rows_lens,
            panel_rows_lens: panel.iter().map(|r| visible_width(r)).collect(),
            delegate_rows_lens: delegate_rows.iter().map(|r| visible_width(r)).collect(),
            memory_notice_rows_lens: memory_notice_rows
                .iter()
                .map(|r| visible_width(r))
                .collect(),
            block_rows,
            prompt_drawn: prompt_str.clone(),
        });

        self.box_drawn.store(true, Ordering::SeqCst);
    }
    pub fn draw_initial_box(&self) {
        if self.is_tui_mode() {
            self.notify_tui();
            return;
        }
        if !self.is_attended {
            print!("{}", PROMPT.bold().yellow());
            let _ = io::stdout().flush();
            return;
        }
        let width = get_terminal_width();
        let buf = self.input_buf.lock().unwrap_or_else(|e| e.into_inner());
        self.draw_box(width, &buf);
    }
    pub fn redraw_box(&self) {
        let _paint = self.paint_in_flight();
        if self.is_tui_mode() {
            self.notify_tui();
            return;
        }
        if !self.is_attended || !self.box_drawn.load(Ordering::SeqCst) {
            return;
        }
        if self.modal_active() {
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
            return;
        }
        let (previous, live_rows) = {
            let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            (live.rows.len(), live.rows.clone())
        };
        self.erase_live_and_box(previous);
        for row in &live_rows {
            println!("{}", row);
        }
        self.draw_box(width, &buf);
    }
    pub fn redraw_status_line(&self) {
        if self.is_tui_mode() {
            self.notify_tui();
            return;
        }
        if !self.is_attended || !self.box_drawn.load(Ordering::SeqCst) {
            return;
        }
        self.redraw_box();
    }
    pub fn redraw_queue_line(&self) {
        if self.is_tui_mode() {
            self.notify_tui();
            return;
        }
        if !self.is_attended || !self.box_drawn.load(Ordering::SeqCst) {
            return;
        }
        self.redraw_box();
    }
    pub fn clear_box_on_exit(&self) {
        let _paint = self.paint_in_flight();
        if !self.is_attended || self.is_tui_mode() {
            return;
        }
        let previous = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let prev = live.rows.len();
            live.rows.clear();
            live.activity = None;
            prev
        };
        self.erase_live_and_box(previous);
        let _ = io::stdout().flush();
    }
}

/// One frame's worth of the region between the transcript and the prompt.
struct RepaintBlock {
    live_rows: Vec<String>,
    panel_rows: Vec<String>,
    delegate_rows: Vec<String>,
    memory_notice_rows: Vec<String>,
    queued_line: Option<String>,
    status_line: Option<String>,
    /// The rows as they are drawn, top to bottom, ending with the rule above the prompt.
    rows: Vec<String>,
}

/// Whether the new block can be rewritten in place over the old one.
///
/// A row wider than the terminal would occupy two physical rows, and the cursor arithmetic counts
/// one row per block row; a different height means the composer moves; reaching above the top of
/// the screen scrolls the terminal and desyncs every row below.
fn block_can_repaint(block: &[String], old_block: &[String], width: usize) -> bool {
    if block
        .iter()
        .chain(old_block.iter())
        .any(|row| visible_width(row) > width)
    {
        return false;
    }
    if block.len() != old_block.len() {
        return false;
    }
    if old_block.len() + 2 > get_terminal_height() {
        return false;
    }
    true
}

/// The cursor moves that rewrite only the rows whose text changed, then seat the caret back on the
/// prompt row. `current_offset` tracks how far above the prompt row the cursor currently sits.
fn block_repaint_sequence(
    old_block: &[String],
    block: &[String],
    prompt: &str,
    hint_width: usize,
    prompt_drawn: &str,
) -> String {
    let mut sequence = String::new();
    let mut current_offset = 0usize;
    for (i, row) in block.iter().enumerate() {
        if old_block.get(i).map(|old| old == row).unwrap_or(false) {
            continue;
        }
        // Block row `i` sits this many rows above the prompt row.
        let target_offset = old_block.len() - i;
        if target_offset > current_offset {
            let up = target_offset - current_offset;
            sequence.push_str(&format!("\r\x1b[{up}A\x1b[2K{row}"));
        } else if target_offset < current_offset {
            let down = current_offset - target_offset;
            sequence.push_str(&format!("\r\x1b[{down}B\x1b[2K{row}"));
        } else {
            sequence.push_str(&format!("\r\x1b[2K{row}"));
        }
        current_offset = target_offset;
    }
    // The cursor belongs back on the prompt row: the caret lives there, and the next keypress
    // is written there.
    if current_offset > 0 {
        sequence.push_str(&format!("\r\x1b[{current_offset}B"));
    }
    if prompt != prompt_drawn {
        sequence.push_str("\r\x1b[2K");
        sequence.push_str(prompt);
    }
    // The caret belongs where typing happens: at the end of the row, less the hint it walks
    // back over. An unchanged prompt row is not rewritten, only re-seated.
    let column = visible_width(prompt).saturating_sub(hint_width);
    if column > 0 {
        sequence.push_str(&format!("\r\x1b[{column}C"));
    } else {
        sequence.push('\r');
    }
    sequence
}

impl ChatUi {
    /// The escape sequence that repaints the region between the transcript and the prompt in place.
    ///
    /// The animation tick changes the spinner frame and the elapsed time twelve times a second.
    /// Erasing that region *and* the composer to redraw it made the status line, the rules and the
    /// prompt flash on every tick, which reads as a broken display rather than as activity.
    ///
    /// When the region keeps its height — the common case: the same rows, a new frame — only the
    /// rows whose text actually changed are rewritten, where they stand. The status line, the
    /// rules and the prompt are not touched at all unless they changed, so nothing blinks. A
    /// height change (the live tail grew, a tool started, the queue line appeared) means the
    /// composer has to move, and the caller falls back to the full erase-and-redraw.
    pub(super) fn repaint_block_sequence(&self, width: usize, buf: &str) -> Option<String> {
        let metrics = self.repaint_metrics(width)?;
        let block = self.repaint_block(width);
        if !block_can_repaint(&block.rows, &metrics.block_rows, width) {
            return None;
        }

        let (prompt, hint_width) = self.prompt_row_drawn(buf);
        let sequence = block_repaint_sequence(
            &metrics.block_rows,
            &block.rows,
            &prompt,
            hint_width,
            &metrics.prompt_drawn,
        );
        self.store_repaint_metrics(&block, prompt);
        Some(sequence)
    }

    /// The measurements of the frame on screen, or `None` when there is no box to repaint in place.
    fn repaint_metrics(&self, width: usize) -> Option<BoxDrawnMetrics> {
        if !self.box_drawn.load(Ordering::SeqCst) {
            return None;
        }
        let metrics = self
            .drawn_metrics
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        if metrics.width != width {
            // A resize redraws everything anyway.
            return None;
        }
        Some(metrics)
    }

    /// The block as it should look now: everything between the transcript and the prompt.
    fn repaint_block(&self, width: usize) -> RepaintBlock {
        let live_rows = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rows
            .clone();
        let panel_rows = self.todo_panel_rows(width);
        let delegate_rows = self.delegate_task_rows(width);
        let memory_notice_rows = self.memory_notice_rows(width);
        let queued_line = self.format_queue_line(width);
        let status_line = self.format_status_line(width);

        // The block is everything between the transcript and the prompt, top to bottom. The
        // landing prefix is only present while the mark is still immediately above the block;
        // once the transcript starts, `stop_landing` has cleared it.
        let mut rows: Vec<String> =
            Vec::with_capacity(live_rows.len() + panel_rows.len() + delegate_rows.len() + 5);
        if let Some(prefix) = self.landing_block_prefix() {
            rows.extend(prefix);
        }
        rows.extend(live_rows.iter().cloned());
        rows.extend(panel_rows.iter().cloned());
        rows.extend(delegate_rows.iter().cloned());
        rows.extend(memory_notice_rows.iter().cloned());
        if let Some(line) = &queued_line {
            rows.push(line.clone());
        }
        if let Some(line) = &status_line {
            rows.push(line.clone());
        }
        rows.push("─".repeat(width).dimmed().to_string());

        RepaintBlock {
            live_rows,
            panel_rows,
            delegate_rows,
            memory_notice_rows,
            queued_line,
            status_line,
            rows,
        }
    }

    /// Keep the measurements honest: the rows changed, the height did not.
    fn store_repaint_metrics(&self, block: &RepaintBlock, prompt: String) {
        let mut guard = self.drawn_metrics.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = guard.as_mut() {
            m.live_rows_lens = block.live_rows.iter().map(|r| visible_width(r)).collect();
            m.panel_rows_lens = block.panel_rows.iter().map(|r| visible_width(r)).collect();
            m.delegate_rows_lens = block
                .delegate_rows
                .iter()
                .map(|r| visible_width(r))
                .collect();
            m.memory_notice_rows_lens = block
                .memory_notice_rows
                .iter()
                .map(|r| visible_width(r))
                .collect();
            m.queued_line_len = block
                .queued_line
                .as_ref()
                .map(|s| visible_width(s))
                .unwrap_or(0);
            m.status_line_len = block
                .status_line
                .as_ref()
                .map(|s| visible_width(s))
                .unwrap_or(0);
            m.prompt_len = visible_width(&prompt);
            m.block_rows = block.rows.clone();
            m.prompt_drawn = prompt;
        }
    }
}
