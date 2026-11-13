use super::super::render::{
    needs_separator, reasoning_rows, tool_activity_detail, OutputBlockKind,
};
use super::metrics::finished_reasoning_text;
use super::panels::Compaction;
use super::tail::Activity;
use super::terminal::get_terminal_width;
use super::view::ChatUi;

impl ChatUi {
    pub fn begin_block(&self, kind: OutputBlockKind) {
        let mut last = self.last_block.lock().unwrap_or_else(|e| e.into_inner());
        if !self.is_tui_mode() && needs_separator(*last, kind) {
            self.emit_above("");
        }
        *last = kind;
    }
    pub fn push_reasoning(&self, text: &str) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if live.reasoning.is_empty() && live.reasoning_started.is_none() && !text.is_empty() {
            live.reasoning_started = Some(std::time::Instant::now());
        }
        live.reasoning.push_str(text);
        drop(live);
        self.notify_tui();
    }
    pub fn push_content(&self, text: &str) {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .content
            .push_str(text);
        self.notify_tui();
    }
    pub fn clear_live_content(&self) {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .content
            .clear();
        self.notify_tui();
    }
    /// Take and clear the current live reply text buffer if non-empty (used to preserve
    /// intermediate assistant prose emitted before a tool call).
    pub fn take_live_content(&self) -> Option<String> {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let text = std::mem::take(&mut live.content);
        drop(live);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            self.notify_tui();
            Some(trimmed.to_string())
        }
    }
    /// Take and clear the current live reasoning buffer if non-empty.
    #[cfg(test)]
    pub fn take_live_reasoning(&self) -> Option<String> {
        self.take_live_reasoning_with_duration().map(|(t, _)| t)
    }
    /// Take and clear the current live reasoning buffer along with its measured duration.
    pub fn take_live_reasoning_with_duration(&self) -> Option<(String, Option<f32>)> {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let text = std::mem::take(&mut live.reasoning);
        let secs = live
            .reasoning_started
            .take()
            .map(|started| started.elapsed().as_secs_f32());
        drop(live);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            self.notify_tui();
            Some((trimmed.to_string(), secs))
        }
    }
    pub fn finish_reasoning(&self) {
        let rows = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            if live.reasoning.is_empty() {
                return;
            }
            if self.is_tui_mode() {
                if !live.reasoning.ends_with('\n') {
                    live.reasoning.push_str("\n\n");
                }
                drop(live);
                self.notify_tui();
                return;
            }
            let text = std::mem::take(&mut live.reasoning);
            reasoning_rows(&text, get_terminal_width())
        };
        if !rows.is_empty() {
            self.begin_block(OutputBlockKind::Thinking);
            self.emit_above(&finished_reasoning_text(&rows));
        }
    }
    pub fn start_activity(&self, call: &str, name: &str, arguments: &serde_json::Value) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.tool_output.clear();
        live.tool_output_expanded = false;
        live.activity = Some(Activity {
            call: call.to_string(),
            tool_name: name.to_string(),
            arguments: arguments.clone(),
            detail: tool_activity_detail(name, arguments),
            started: std::time::Instant::now(),
            frame: 0,
        });
        drop(live);
        self.notify_tui();
    }
    pub fn activity_elapsed_secs(&self) -> Option<f32> {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .activity
            .as_ref()
            .map(|a| a.started.elapsed().as_secs_f32())
    }
    pub fn stop_activity(&self) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.activity = None;
        live.tool_output.clear();
        live.tool_output_expanded = false;
        drop(live);
        self.notify_tui();
    }
    /// Whether a tool is running and therefore whether Ctrl+O should expand the live tail rather
    /// than the transcript.
    pub fn has_live_activity(&self) -> bool {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .activity
            .is_some()
    }
    /// Toggle the live tail between its default height and the whole buffered output.
    ///
    /// Returns whether the tail is now expanded. Only meaningful while a tool is running; the
    /// caller checks [`Self::has_live_activity`] first so the same key still folds the transcript
    /// when Luna is idle.
    pub fn toggle_live_expanded(&self) -> bool {
        let expanded = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            live.tool_output_expanded = !live.tool_output_expanded;
            live.tool_output_expanded
        };
        self.animate_tick();
        expanded
    }
    /// Update the display block limit for running tool tails and execution blocks.
    pub fn set_display_block_limit(&self, limit: Option<usize>) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.block_limit = limit;
    }
    /// Add a chunk of a running tool's output to the tail under its activity row.
    ///
    /// Called from the agent's observer, which fires on the tool's reader task: this only buffers
    /// text and takes a short lock. The animation tick is what draws it, so a fast stream cannot
    /// turn every chunk into a repaint.
    pub fn push_tool_output(&self, chunk: &str) {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tool_output
            .push(chunk);
        self.notify_tui();
    }
    /// A fold of the oldest turns has started: animate it until [`Self::finish_compaction`].
    ///
    /// Nothing is printed here in either mode. In the TUI the row is drawn by the animation tick,
    /// and an unattended run says what is happening with the summary line that follows — a static
    /// "compacting…" line with no way to ever update it would be worse than silence.
    pub fn start_compaction(&self, forced: bool) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.compaction = Some(Compaction {
            forced,
            started: std::time::Instant::now(),
            frame: 0,
        });
        drop(live);
        self.notify_tui();
    }
    /// The fold is over, whatever it produced: stop animating it. Reporting what was folded is the
    /// caller's job, because the same event carries the failure to report.
    pub fn finish_compaction(&self) {
        let had = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            live.compaction.take().is_some()
        };
        if had {
            // One final draw without the row, so the sweep does not stay frozen on screen while the
            // transcript below it is rewritten.
            self.animate_tick();
            self.notify_tui();
        }
    }
    #[allow(dead_code)]
    pub fn is_compacting(&self) -> bool {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .compaction
            .is_some()
    }
}
