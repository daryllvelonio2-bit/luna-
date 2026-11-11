use std::sync::atomic::Ordering;

use harness_core::tools::todo::TodoSnapshot;

use super::format::{format_delegate_task_row, format_save_notice};
use super::panels::{DelegateTaskView, MemoryNotice, MAX_MEMORY_NOTICES};
use super::terminal::get_terminal_width;
use super::view::ChatUi;

impl ChatUi {
    /// The rows the todo panel is drawn from, empty when the list is.
    pub fn todo_panel_rows(&self, width: usize) -> Vec<String> {
        let snapshot = self.todo_snapshot.lock().unwrap_or_else(|e| e.into_inner());
        match snapshot.as_ref() {
            Some(snapshot) => {
                super::super::todo::render_panel(snapshot, self.todo_collapsed(), width)
            }
            None => Vec::new(),
        }
    }
    /// Replace the session's list and redraw the composer, so the new list shows at
    /// once.
    ///
    /// The panel is drawn above the composer, so a change to the list is a change to
    /// the box rather than a line in the transcript: it appears in place instead of
    /// scrolling up with the conversation.
    pub fn set_todo_panel(&self, snapshot: Option<TodoSnapshot>) {
        let changed = {
            let mut panel = self.todo_snapshot.lock().unwrap_or_else(|e| e.into_inner());
            if *panel == snapshot {
                false
            } else {
                *panel = snapshot;
                true
            }
        };
        if !changed {
            return;
        }
        if !self.is_attended {
            // A headless run has no box to redraw, so the list is simply printed.
            for row in self.todo_panel_rows(get_terminal_width()) {
                println!("{}", row);
            }
            return;
        }
        if self.box_drawn.load(Ordering::SeqCst) {
            self.redraw_box();
        }
    }
    /// Whether a todo panel snapshot is active.
    pub fn has_todo_panel(&self) -> bool {
        self.todo_snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
    /// Whether the todo panel is folded to a single row.
    pub fn todo_collapsed(&self) -> bool {
        self.todo_collapsed.load(Ordering::SeqCst)
    }
    /// Fold or unfold the todo panel, and say which way it went.
    pub fn toggle_todo_collapsed(&self) -> bool {
        let folded = !self.todo_collapsed.load(Ordering::SeqCst);
        self.todo_collapsed.store(folded, Ordering::SeqCst);
        if self.box_drawn.load(Ordering::SeqCst) {
            self.redraw_box();
        }
        folded
    }
    /// Show a delegated child above the composer.
    pub fn start_delegate_task(&self, id: &str, goal: &str) {
        let changed = {
            let mut tasks = self.delegate_tasks.lock().unwrap();
            if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
                let changed = task.goal != goal || task.activity != "starting";
                task.goal = goal.to_string();
                task.activity = "starting".to_string();
                changed
            } else {
                tasks.push(DelegateTaskView {
                    id: id.to_string(),
                    goal: goal.to_string(),
                    activity: "starting".to_string(),
                });
                true
            }
        };
        if changed {
            self.redraw_box();
        }
    }
    /// Update what a running delegated child is doing.
    pub fn update_delegate_task(&self, id: &str, activity: &str) {
        let mut tasks = self.delegate_tasks.lock().unwrap();
        if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
            if task.activity != activity {
                task.activity = activity.to_string();
            }
        }
    }
    /// Remove a delegated child after it finishes.
    pub fn finish_delegate_task(&self, id: &str) {
        let changed = {
            let mut tasks = self.delegate_tasks.lock().unwrap();
            let before = tasks.len();
            tasks.retain(|task| task.id != id);
            before != tasks.len()
        };
        if changed {
            self.redraw_box();
        }
    }
    /// The rows that announce delegated children still running in the background.
    /// Only one is shown in the CLI above the composer; full details for all running subagents
    /// are inspected in the background viewer.
    pub fn delegate_task_rows(&self, width: usize) -> Vec<String> {
        let tasks = self.delegate_tasks.lock().unwrap();
        if let Some(task) = tasks.last() {
            vec![format_delegate_task_row(task, tasks.len() - 1, width)]
        } else {
            Vec::new()
        }
    }
    /// Announce one memory the background saver wrote.
    #[allow(dead_code)]
    pub fn push_memory_notice(&self, name: &str, status: &str) {
        self.push_memory_notice_with_detail(name, None, status);
    }
    /// Announce one memory the background saver wrote, with detail.
    pub fn push_memory_notice_with_detail(&self, name: &str, detail: Option<&str>, status: &str) {
        self.push_save_notice("memory", name, detail, status);
    }
    /// Announce a note the background saver wrote, updated, archived, or merged.
    #[allow(dead_code)]
    pub fn push_note_notice(&self, name: &str, status: &str) {
        self.push_note_notice_with_detail(name, None, status);
    }
    /// Announce a note the background saver wrote, updated, archived, or merged, with detail.
    pub fn push_note_notice_with_detail(&self, name: &str, detail: Option<&str>, status: &str) {
        self.push_save_notice("note", name, detail, status);
    }
    /// The shared notice path: one temporary row per stored item, whatever store it lives in.
    fn push_save_notice(&self, storage: &str, name: &str, detail: Option<&str>, status: &str) {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() || name == "-" {
            return;
        }
        let status = status.trim().to_string();
        let detail = detail
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty());
        let changed = {
            let mut notices = self
                .memory_notices
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // One row per item: a later pass that updates the same record replaces the old
            // status instead of stacking a stale `saved` beside a fresh `updated`.
            if let Some(existing) = notices
                .iter_mut()
                .find(|notice| notice.storage == storage && notice.name == name)
            {
                if existing.status == status && existing.detail == detail {
                    false
                } else {
                    existing.status = status.clone();
                    existing.detail = detail.clone();
                    true
                }
            } else {
                notices.push(MemoryNotice {
                    storage: storage.to_string(),
                    name: name.clone(),
                    detail: detail.clone(),
                    status: status.clone(),
                });
                if notices.len() > MAX_MEMORY_NOTICES {
                    let excess = notices.len() - MAX_MEMORY_NOTICES;
                    notices.drain(..excess);
                }
                true
            }
        };
        if !changed {
            return;
        }
        if self.is_attended {
            self.redraw_box();
        } else {
            // No composer to hold a temporary row, so the run log gets the line instead.
            self.emit_above(&format_save_notice(
                storage,
                &name,
                detail.as_deref(),
                &status,
                get_terminal_width(),
            ));
        }
    }
    /// Drop the background-saver notices, as the start of a new turn does.
    pub fn clear_memory_notices(&self) {
        let had = {
            let mut notices = self
                .memory_notices
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let had = !notices.is_empty();
            notices.clear();
            had
        };
        if had {
            self.redraw_box();
        }
    }
    /// The rows that report what the background saver just wrote.
    pub fn memory_notice_rows(&self, width: usize) -> Vec<String> {
        self.memory_notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|notice| {
                format_save_notice(
                    &notice.storage,
                    &notice.name,
                    notice.detail.as_deref(),
                    &notice.status,
                    width,
                )
            })
            .collect()
    }
    pub fn box_row_count(&self) -> usize {
        4 + if self.queue_depth() > 0 { 1 } else { 0 }
            + self.todo_panel_rows(get_terminal_width()).len()
            + self.delegate_task_rows(get_terminal_width()).len()
            + self.memory_notice_rows(get_terminal_width()).len()
    }
    /// How many submitted messages are waiting to be sent.
    #[allow(dead_code)]
    pub fn set_queue_depth(&self, depth: usize) {
        self.queue_depth.store(depth, Ordering::SeqCst);
        self.redraw_queue_line();
    }
    pub fn queue_depth(&self) -> usize {
        self.queue_depth.load(Ordering::SeqCst)
    }
}
