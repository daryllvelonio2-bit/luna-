//! What the agent and its delegates report back to the chat screen.
//!
//! These closures capture only what they draw with, so a running turn can emit events without
//! borrowing anything the interactive loop needs.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use colored::Colorize;
use harness_core::tools::delegate::SubagentTaskEventNotifier;
use harness_core::{SessionStore, SqliteSessionStore};

use super::render::{
    bounded_tool_output, clean_user_query_for_transcript, compaction_report, extract_patch_diff,
    format_delegate_duration, format_delegate_notification, format_delegate_title,
    format_live_tool_output, format_tool_call, format_tool_result, format_tool_result_for,
    has_verdict, is_non_tool_event, is_pty_tool, right_aligned_user_message, OutputBlockKind,
    TurnProcessItem, LUNA_LABEL,
};
use super::todo;
use super::turn;
use super::ui::{get_terminal_width, ChatUi};

/// The observer the agent calls while a turn runs.
///
/// The handles are the same ones the loop holds: the in-flight tool rows, the mid-turn messages
/// the turn-end handler splits the transcript with, the todo panel's scope, and the flag that
/// tells the composer whether a user-requested fold is rewriting history.
pub(super) fn agent_observer(
    ui: Arc<ChatUi>,
    in_flight_tools: Arc<Mutex<Vec<TurnProcessItem>>>,
    interruptions: Arc<Mutex<Vec<turn::InterruptNotice>>>,
    todo_scope: harness_core::tools::TodoScope,
    compacting: Arc<AtomicBool>,
    display_settings: Arc<std::sync::RwLock<harness_core::DisplaySettings>>,
) -> Arc<dyn Fn(harness_core::AgentEvent) + Send + Sync> {
    let state = ObserverState {
        ui,
        in_flight_tools,
        interruptions,
        todo_scope,
        compacting,
        display_settings,
    };
    Arc::new(move |event| state.handle(event))
}

/// The captured handles one agent event is drawn with.
struct ObserverState {
    ui: Arc<ChatUi>,
    in_flight_tools: Arc<Mutex<Vec<TurnProcessItem>>>,
    interruptions: Arc<Mutex<Vec<turn::InterruptNotice>>>,
    todo_scope: harness_core::tools::TodoScope,
    compacting: Arc<AtomicBool>,
    display_settings: Arc<std::sync::RwLock<harness_core::DisplaySettings>>,
}

impl ObserverState {
    fn handle(&self, event: harness_core::AgentEvent) {
        match event {
            harness_core::AgentEvent::ReasoningDelta(text) => {
                self.ui.push_reasoning(&text);
            }
            harness_core::AgentEvent::ContentDelta(text) => {
                self.ui.push_content(&text);
            }
            harness_core::AgentEvent::ContextTokensUpdated { tokens_used, reset } => {
                if reset {
                    self.ui.reset_context_used(tokens_used);
                } else {
                    self.ui.update_context_used(tokens_used);
                }
            }
            harness_core::AgentEvent::MemorySaved {
                name,
                detail,
                status,
            } => {
                // The detached saver lands after the turn; show what it wrote above the
                // composer until the next turn clears it.
                self.ui
                    .push_memory_notice_with_detail(&name, detail.as_deref(), status.as_str());
            }
            harness_core::AgentEvent::NoteSaved {
                name,
                detail,
                status,
            } => {
                self.ui
                    .push_note_notice_with_detail(&name, detail.as_deref(), status.as_str());
            }
            harness_core::AgentEvent::ToolExecuting { name, arguments } => {
                self.handle_tool_executing(name, arguments);
            }
            harness_core::AgentEvent::ToolOutput { name, chunk } => {
                // The tail under the running call. `todo_list` is invisible here for the same
                // reason its call line is: the panel is its result, not a transcript entry.
                if name != "todo_list" {
                    self.ui.push_tool_output(&chunk);
                }
            }
            harness_core::AgentEvent::ToolExecuted {
                name,
                is_error,
                output,
            } => {
                self.handle_tool_executed(name, is_error, output);
            }
            harness_core::AgentEvent::Interrupted {
                text,
                reasoning,
                partial,
            } => {
                self.handle_interrupted(text, reasoning, partial);
            }
            harness_core::AgentEvent::Steered { text } => {
                self.handle_steered(&text);
            }
            harness_core::AgentEvent::CompactionStarted { forced } => {
                // Only a fold that rewrites what the composer would send holds the composer.
                self.compacting
                    .store(forced, std::sync::atomic::Ordering::SeqCst);
                self.ui.start_compaction(forced);
            }
            harness_core::AgentEvent::CompactionFinished { folded, error } => {
                self.handle_compaction_finished(folded, error);
            }
        }
    }

    fn settle_reasoning_for_next_step(&self) {
        if self.ui.is_tui_mode() {
            if let Some((reasoning, secs)) = self.ui.take_live_reasoning_with_duration() {
                self.in_flight_tools
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(TurnProcessItem::thinking_with_duration(reasoning, secs));
            }
        } else {
            self.ui.finish_reasoning();
        }
    }

    fn handle_tool_executing(&self, name: String, arguments: serde_json::Value) {
        self.settle_reasoning_for_next_step();
        if self.ui.is_tui_mode() {
            if let Some(prose) = self.ui.take_live_content() {
                self.in_flight_tools
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(TurnProcessItem::assistant_text(prose));
            }
        } else {
            self.ui.clear_live_content();
        }
        // A todo call reads as the task it acts on, not as the arguments it was sent.
        let call_fmt = if name == "todo_list" {
            todo::format_call(&arguments, &self.todo_scope)
        } else {
            format_tool_call(&name, &arguments)
        };
        let diff_lines = if name == "patch" || name == "edit" || name == "apply_patch" {
            extract_patch_diff(&arguments)
        } else {
            Vec::new()
        };
        if name != "todo_list" {
            self.ui.begin_block(OutputBlockKind::Tool);
            // The call goes into the live region the instant it is invoked, so a slow tool is
            // visible while it runs; `ToolExecuted` below finalizes the same line into the
            // transcript with its verdict.
            self.ui.start_activity(&call_fmt, &name, &arguments);
            if is_pty_tool(&name) {
                self.ui.begin_pty_activity(&name, &arguments);
            }
            self.in_flight_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(TurnProcessItem {
                    tool_name: name.clone(),
                    call_formatted: call_fmt,
                    result_formatted: String::new(),
                    diff_lines,
                    output: String::new(),
                    output_dropped: 0,
                    arguments: Some(arguments),
                    duration_secs: None,
                    is_error: false,
                });
        }
    }

    fn handle_tool_executed(&self, name: String, is_error: bool, output: String) {
        if name == "todo_list" {
            self.ui.stop_activity();
            // The list changed: refresh before drawing so the panel displays the new state immediately
            todo::refresh_panel(&self.ui, &self.todo_scope);
            if is_error {
                let line = format_tool_result(true, &output);
                self.ui.emit_above(&line);
            }
            return;
        }
        let elapsed_secs = self.ui.activity_elapsed_secs();
        self.ui.stop_activity();
        if is_pty_tool(&name) {
            self.ui.record_pty_output(&name, is_error, &output);
        }
        let status = if is_error {
            " (failed)".red().to_string()
        } else {
            " (success)".green().to_string()
        };
        let (line, diff_to_show) = {
            let mut tools = self
                .in_flight_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // When multiple tool calls run in parallel, in_flight_tools holds all of them.
            // Match the first in-flight tool call that hasn't received a verdict yet,
            // preserving call order. Using tools.last_mut() repeatedly updated the last
            // tool in the batch, appending duplicate verdicts and printing it multiple times.
            let target = tools.iter_mut().find(|it| {
                !is_non_tool_event(&it.call_formatted) && !has_verdict(&it.call_formatted)
            });
            if let Some(item) = target {
                item.call_formatted.push_str(&status);
                item.result_formatted = format_tool_result_for(Some(&name), is_error, &output);
                let (bounded, dropped) = bounded_tool_output(&output);
                item.output = bounded;
                item.output_dropped = dropped;
                item.is_error = is_error;
                item.duration_secs = elapsed_secs;
                let diff =
                    if !is_error && (name == "patch" || name == "edit" || name == "apply_patch") {
                        item.diff_lines.clone()
                    } else {
                        Vec::new()
                    };
                (item.call_formatted.clone(), diff)
            } else {
                (
                    format_tool_result_for(Some(&name), is_error, &output),
                    Vec::new(),
                )
            }
        };
        if self.ui.is_tui_mode() {
            self.ui.notify_tui();
            return;
        }
        self.ui.emit_above(&line);
        let block_limit = self
            .display_settings
            .read()
            .ok()
            .and_then(|s| s.block_limit);
        let live_output = format_live_tool_output(Some(&name), is_error, &output, block_limit);
        for out_line in live_output {
            self.ui.emit_above(&out_line);
        }
        if !diff_to_show.is_empty() {
            self.ui.emit_above(&diff_to_show.join("\n"));
        }
    }

    fn handle_interrupted(&self, text: String, reasoning: String, partial: String) {
        let shown = clean_user_query_for_transcript(&text);
        // The reasoning produced before the cut is already in the live region; settle it so
        // it stays on screen instead of disappearing when the next call starts.
        self.settle_reasoning_for_next_step();
        if !self.ui.is_tui_mode() {
            // Reply text is not streamed live, so the part the model had written would go
            // unseen. Show it now, before the user's line that cut it off.
            if !partial.trim().is_empty() {
                self.ui.begin_block(OutputBlockKind::Reply);
                self.ui.emit_above(&format!(
                    "{}\n{}",
                    LUNA_LABEL.bold().green(),
                    partial.trim_end()
                ));
            }
            // The agent has already appended this as a user turn; echo it in the same place a
            // normally submitted line appears.
            self.ui.begin_block(OutputBlockKind::User);
            self.ui
                .emit_above(&right_aligned_user_message(&shown, get_terminal_width()));
        } else {
            self.ui.notify_tui();
        }
        let tool_count = self
            .in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        self.interruptions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(turn::InterruptNotice {
                text: shown,
                reasoning,
                partial,
                tool_count,
                at: std::time::Instant::now(),
            });
    }

    fn handle_steered(&self, text: &str) {
        self.settle_reasoning_for_next_step();
        let line = format!("{} Delivered steer into turn: {}", "⏩".cyan().bold(), text);
        self.in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(TurnProcessItem {
                tool_name: "steer".to_string(),
                call_formatted: line.clone(),
                result_formatted: String::new(),
                diff_lines: Vec::new(),
                output: String::new(),
                output_dropped: 0,
                arguments: None,
                duration_secs: None,
                is_error: false,
            });
        self.ui.emit_above(&line);
    }

    fn handle_compaction_finished(
        &self,
        folded: Option<harness_core::CompactionNotice>,
        error: Option<String>,
    ) {
        self.ui.finish_compaction();
        self.compacting
            .store(false, std::sync::atomic::Ordering::SeqCst);
        if let Some(err) = error {
            self.ui
                .emit_above(&format!("{} Compaction failed: {}", "✖".red(), err));
        } else if let Some(notice) = folded {
            // A fold really retired history, so the gauge may fall — but not while this
            // turn is still on the transcript it replaced. Defer the drop to the boundary,
            // where it lands if the turn never adopts the fold. A turn that does adopt it
            // reports the folded size with `reset: true` and the gauge drops immediately.
            self.ui.defer_context_drop();
            self.ui.emit_above(&compaction_report(&notice));
        }
    }
}

/// Live subagent rows: start, progress, finish.
pub(super) fn delegate_event_notifier(
    ui: Arc<ChatUi>,
    trackers: Arc<Mutex<HashMap<String, (std::time::Instant, String)>>>,
) -> SubagentTaskEventNotifier {
    Arc::new(move |event| {
        use harness_core::tools::delegate::{SubagentActivity, SubagentTaskEvent};
        match event {
            SubagentTaskEvent::Started { id, goal } => {
                // The live row is the notification: `start_delegate_task` draws it above the
                // composer the moment the child starts, and every event after this one repaints
                // that same row. Printing a line here as well put the child's state on screen
                // twice from the first instant.
                trackers.lock().unwrap_or_else(|e| e.into_inner()).insert(
                    id.clone(),
                    (std::time::Instant::now(), "starting".to_string()),
                );
                ui.start_delegate_task(&id, &goal);
            }
            SubagentTaskEvent::Progress { id, activity, .. } => {
                // Progress belongs to the live row and nowhere else. A transcript line per tool
                // event stacked one copy of the same notification per event — a long child buried
                // the conversation under its own progress. `update_delegate_task` repaints the
                // single running row in place instead, which is what the row is for.
                let label = match &activity {
                    SubagentActivity::ModelStreaming => "streaming".to_string(),
                    SubagentActivity::ToolStarted { name } => format!("{} running", name),
                    SubagentActivity::ToolFinished { name, is_error } => {
                        format!("{} {}", name, if *is_error { "failed" } else { "done" })
                    }
                    // The child's newest output line, so a running build is visible in the parent
                    // without opening the viewer. The chunk may end mid-line; the next one wins.
                    SubagentActivity::ToolOutput { name, chunk } => {
                        let newest = chunk
                            .lines()
                            .rev()
                            .find(|line| !line.trim().is_empty())
                            .map(str::trim);
                        match newest {
                            Some(line) if !line.is_empty() => {
                                let short: String = line.chars().take(60).collect();
                                format!("{} · {}", name, short)
                            }
                            _ => format!("{} running", name),
                        }
                    }
                };
                {
                    let mut trackers = trackers.lock().unwrap_or_else(|e| e.into_inner());
                    let entry = trackers
                        .entry(id.clone())
                        .or_insert_with(|| (std::time::Instant::now(), "starting".to_string()));
                    entry.1 = label.clone();
                }

                ui.update_delegate_task(&id, &label);
            }
            SubagentTaskEvent::Finished { record } => {
                ui.finish_delegate_task(&record.id);
            }
        }
    })
}

/// The completion notice a finished subagent leaves above the composer.
pub(super) fn delegate_completion_notifier(
    ui: Arc<ChatUi>,
    trackers: Arc<Mutex<HashMap<String, (std::time::Instant, String)>>>,
    store: Arc<SqliteSessionStore>,
    session_id: String,
) -> harness_core::tools::delegate::SubagentCompletionNotifier {
    Arc::new(move |record| {
        // Safety net: the live row is normally removed by the Finished event. If that event was
        // missed for any reason, the completion callback still clears it, so a finished child
        // never leaves its last action sitting above the composer.
        ui.finish_delegate_task(&record.id);
        let is_ok = matches!(
            record.status,
            harness_core::tools::delegate::SubagentStatus::Completed { .. }
        );
        let summary = match &record.status {
            harness_core::tools::delegate::SubagentStatus::Completed { reply, .. } => {
                reply.lines().next().unwrap_or("completed").to_string()
            }
            harness_core::tools::delegate::SubagentStatus::Failed { error, .. } => error.clone(),
            _ => "finished".to_string(),
        };
        let (started_at, _last_tool) = trackers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&record.id)
            .unwrap_or_else(|| (std::time::Instant::now(), "done".to_string()));
        let total_time = format_delegate_duration(started_at.elapsed());
        let title = format_delegate_title(&record.goal);
        let glyph = if is_ok {
            "✔".green().bold().to_string()
        } else {
            "✖".red().bold().to_string()
        };
        let status = if is_ok { "finished" } else { "failed" };
        let notice = format_delegate_notification(&glyph, &title, status, &total_time);
        ui.emit_above(&notice);
        if !summary.is_empty() && summary != "completed" {
            ui.emit_above(&format!("   {}", summary.dimmed()));
        }
        let s_id = session_id.clone();
        let s_store = store.clone();
        tokio::spawn(async move {
            let _ = s_store
                .append(&s_id, &harness_core::Message::system(&notice), false)
                .await;
        });
    })
}
