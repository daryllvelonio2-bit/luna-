use super::super::render::{
    needs_separator, reasoning_rows, tool_activity_detail, OutputBlockKind,
};
use super::metrics::finished_reasoning_text;
use super::panels::{Compaction, PtyPane};
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
        // A call that ends without reporting a screen (an interrupt, say) must not leave the mirror
        // claiming to be busy forever: clearing the flag starts its idle countdown.
        if let Some(pane) = live.pty_pane.as_mut() {
            pane.running = false;
        }
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
    /// A pty tool has started: open the mirror, or mark the one already on screen as busy.
    ///
    /// This is deliberately separate from [`Self::start_activity`]: the activity row lives only as
    /// long as the call, while a pty session outlives it. `close` and `list` are bookkeeping, not
    /// screen updates, so neither opens a pane.
    pub fn begin_pty_activity(&self, name: &str, arguments: &serde_json::Value) {
        let action = pty_action(name, arguments);
        if action == "close" || action == "list" {
            return;
        }
        let now = std::time::Instant::now();
        {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            match live.pty_pane.as_mut() {
                Some(pane) => {
                    pane.running = true;
                    pane.last_activity = now;
                }
                None => {
                    live.pty_pane = Some(PtyPane {
                        title: pty_title(name, arguments),
                        lines: Vec::new(),
                        last_activity: now,
                        running: true,
                    });
                }
            }
        }
        self.notify_tui();
    }
    /// A pty tool finished: show the screen it reported, and close the mirror on a real `close`.
    ///
    /// The pane is not dropped when the call ends — the session it mirrors usually does not. Only a
    /// successful close, or the idle cutoff in [`ChatUi::tui_snapshot`], takes it away.
    pub fn record_pty_output(&self, name: &str, is_error: bool, output: &str) {
        if !is_error && output_closes_pty(name, output) {
            let had = {
                let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
                live.pty_pane.take().is_some()
            };
            if had {
                self.notify_tui();
            }
            return;
        }
        let lines = pty_screen_lines(output, !is_error);
        {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let Some(pane) = live.pty_pane.as_mut() else {
                return;
            };
            pane.running = false;
            pane.last_activity = std::time::Instant::now();
            if let Some(lines) = lines {
                pane.lines = lines;
            }
        }
        self.notify_tui();
    }
}

/// How many screen lines the mirror keeps. A pty viewport is dozens of rows; this only bounds
/// what a scrollback read can hand us, and the pane draws the newest rows that fit.
const PTY_PANE_MAX_LINES: usize = 300;

/// The action a pty call names, lowercased, or the tool name's own tail when it passes none.
fn pty_action(name: &str, arguments: &serde_json::Value) -> String {
    if let Some(action) = arguments.get("action").and_then(|v| v.as_str()) {
        return action.to_ascii_lowercase();
    }
    let tail = name.rsplit("__").next().unwrap_or(name);
    tail.trim_start_matches("pty_").to_ascii_lowercase()
}

/// A short label for the pane's title: `send · chat`, `start · luna --help`.
fn pty_title(name: &str, arguments: &serde_json::Value) -> String {
    let action = pty_action(name, arguments);
    let non_empty = |key: &str| {
        arguments
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let argv = arguments
        .get("argv")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|s| !s.is_empty());
    let target = non_empty("label")
        .or(argv)
        .or_else(|| non_empty("command"))
        .or_else(|| non_empty("session_id"));
    match target {
        Some(target) => format!("{action} · {target}"),
        None => action,
    }
}

/// Whether a finished call closed its pty session, which is the one event that removes the mirror.
fn output_closes_pty(name: &str, output: &str) -> bool {
    if pty_action(name, &serde_json::Value::Null) == "close" {
        return true;
    }
    serde_json::from_str::<serde_json::Value>(output.trim())
        .ok()
        .is_some_and(|value| value.get("closed").is_some())
}

/// The rendered screen inside a pty tool's JSON result, or the raw text when it is not JSON.
///
/// A failed call's raw text is an error message, not a screen, so `allow_raw` is off in that case.
fn pty_screen_lines(output: &str, allow_raw: bool) -> Option<Vec<String>> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    let text = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) => value
            .get("screen")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("text").and_then(|v| v.as_str()))?
            .to_string(),
        Err(_) if !allow_raw => return None,
        Err(_) => trimmed.to_string(),
    };
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.len() > PTY_PANE_MAX_LINES {
        lines.drain(..lines.len() - PTY_PANE_MAX_LINES);
    }
    Some(lines)
}
