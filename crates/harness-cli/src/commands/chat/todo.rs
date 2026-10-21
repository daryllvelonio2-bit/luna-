//! The todo list, as the transcript shows it.
//!
//! The tool itself keeps the list in `<workspace>/.luna-todo.json`. The chat reads that
//! file back so the transcript can show the *tasks* — a checklist, with the finished ones
//! ticked — instead of the raw `todo_list` argument dump the model happened to send.

use std::path::Path;

use colored::Colorize;
use serde::Deserialize;

/// Where the todo tool persists the list. Same name the tool uses.
pub const TODO_FILE_NAME: &str = ".luna-todo.json";

/// One task in the list.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TodoEntry {
    pub id: u64,
    pub text: String,
    pub status: TodoStatus,
}

/// Where a task stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Done,
}

impl TodoStatus {
    /// The glyph that carries the state: an empty box, a half-filled one, a ticked one.
    pub fn glyph(self) -> &'static str {
        match self {
            TodoStatus::Pending => "☐",
            TodoStatus::InProgress => "◐",
            TodoStatus::Done => "☑",
        }
    }
}

/// The whole list, as read from disk.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct TodoSnapshot {
    #[serde(default)]
    pub items: Vec<TodoEntry>,
}

impl TodoSnapshot {
    /// How many tasks are done, in progress and still open.
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut done = 0;
        let mut active = 0;
        let mut pending = 0;
        for item in &self.items {
            match item.status {
                TodoStatus::Done => done += 1,
                TodoStatus::InProgress => active += 1,
                TodoStatus::Pending => pending += 1,
            }
        }
        (done, active, pending)
    }

    /// The task with this id, if the list still has it.
    pub fn find(&self, id: u64) -> Option<&TodoEntry> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// Read the list from the workspace. A missing or unreadable file is an empty list, not
/// an error: the panel is decoration, and decoration must never break the transcript.
pub fn read_snapshot(workspace_root: &Path) -> TodoSnapshot {
    let path = workspace_root.join(TODO_FILE_NAME);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return TodoSnapshot::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// The checklist panel, drawn under the transcript.
///
/// Collapsed it is a single summary line; expanded it is one row per task.
pub fn render_panel(snapshot: &TodoSnapshot, collapsed: bool) -> Option<String> {
    if snapshot.items.is_empty() {
        return None;
    }

    let (done, active, pending) = snapshot.counts();
    let marker = if collapsed { "▶" } else { "▼" };
    let hint = if collapsed {
        "Ctrl+Shift+O to expand"
    } else {
        "Ctrl+Shift+O to collapse"
    };

    let mut out = format!(
        "{} {} · {} done · {} active · {} pending · {}",
        marker.cyan(),
        "Todo".bold(),
        done.to_string().green(),
        active.to_string().yellow(),
        pending,
        hint.dimmed()
    );

    if collapsed {
        return Some(out);
    }

    for item in &snapshot.items {
        let (glyph, text) = match item.status {
            TodoStatus::Done => (item.status.glyph().green(), item.text.dimmed()),
            TodoStatus::InProgress => (item.status.glyph().yellow(), item.text.normal()),
            TodoStatus::Pending => (item.status.glyph().dimmed(), item.text.normal()),
        };
        out.push_str(&format!(
            "\n    {} {}  {}",
            glyph,
            format!("#{}", item.id).dimmed(),
            text
        ));
    }
    Some(out)
}

/// Render one `todo_list` call as the task it acts on, rather than as its arguments.
///
/// An `add` names the task it creates; an `update` or a `remove` names the task it
/// touches, which the list itself knows the text of.
pub fn format_call(arguments: &serde_json::Value, workspace_root: &Path) -> String {
    let action = arguments
        .get("action")
        .and_then(|a| a.as_str())
        .unwrap_or("list");

    match action {
        "add" => {
            let texts = added_texts(arguments);
            match texts.split_first() {
                Some((first, rest)) if !rest.is_empty() => {
                    format!("☐ {} (+{} more)", first, rest.len())
                }
                Some((first, _)) => format!("☐ {}", first),
                None => "☐ todo".to_string(),
            }
        }
        "update" => {
            let id = arguments.get("id").and_then(|i| i.as_u64());
            let status = arguments
                .get("status")
                .and_then(|s| s.as_str())
                .and_then(parse_status);
            match id {
                Some(id) => {
                    let known = read_snapshot(workspace_root).find(id).cloned();
                    let text = known
                        .as_ref()
                        .map(|item| item.text.clone())
                        .unwrap_or_else(|| "todo".to_string());
                    let status = status.or_else(|| known.as_ref().map(|item| item.status));
                    let status = status.unwrap_or(TodoStatus::Pending);
                    format!("{} #{} {}", status.glyph(), id, text)
                }
                None => "☐ todo".to_string(),
            }
        }
        "remove" => {
            let id = arguments.get("id").and_then(|i| i.as_u64());
            match id {
                Some(id) => {
                    let text = read_snapshot(workspace_root)
                        .find(id)
                        .map(|item| item.text.clone())
                        .unwrap_or_else(|| "todo".to_string());
                    format!("☒ #{} {}", id, text)
                }
                None => "☒ todo".to_string(),
            }
        }
        "clear_done" => "☑ cleared done tasks".to_string(),
        _ => {
            let count = read_snapshot(workspace_root).items.len();
            format!("📋 {} tasks", count)
        }
    }
}

/// The task texts an `add` call carries, whether they came as a list or as one string.
fn added_texts(arguments: &serde_json::Value) -> Vec<String> {
    let mut texts = Vec::new();
    if let Some(items) = arguments.get("tasks").and_then(|t| t.as_array()) {
        for item in items {
            if let Some(text) = item.as_str().map(str::trim).filter(|t| !t.is_empty()) {
                texts.push(text.to_string());
            }
        }
    } else if let Some(text) = arguments
        .get("text")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        texts.push(text.to_string());
    }
    texts
}

/// The status names the tool accepts.
fn parse_status(value: &str) -> Option<TodoStatus> {
    match value.trim().to_lowercase().as_str() {
        "pending" | "todo" | "open" => Some(TodoStatus::Pending),
        "in_progress" | "in-progress" | "inprogress" | "doing" | "working" => {
            Some(TodoStatus::InProgress)
        }
        "done" | "completed" | "complete" | "finished" | "closed" => Some(TodoStatus::Done),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workspace_with(tasks: serde_json::Value) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(TODO_FILE_NAME),
            serde_json::to_string_pretty(&tasks).unwrap(),
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_missing_file_is_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_snapshot(dir.path()).items.is_empty());
    }

    #[test]
    fn the_panel_lists_every_task_with_its_state() {
        let dir = workspace_with(json!({
            "next_id": 3,
            "items": [
                { "id": 0, "text": "write the parser", "status": "pending" },
                { "id": 1, "text": "test it", "status": "done" },
                { "id": 2, "text": "ship it", "status": "in_progress" }
            ]
        }));

        let snapshot = read_snapshot(dir.path());
        assert_eq!(snapshot.counts(), (1, 1, 1));

        let panel = render_panel(&snapshot, false).unwrap();
        let plain = dialoguer::console::strip_ansi_codes(&panel).to_string();
        assert!(plain.contains("write the parser"), "got: {}", plain);
        assert!(plain.contains("test it"), "got: {}", plain);
        assert!(plain.contains("ship it"), "got: {}", plain);
        assert!(plain.contains("☐"), "an open task is an empty box: {}", plain);
        assert!(plain.contains("☑"), "a finished task is ticked: {}", plain);
        assert!(plain.contains("◐"), "a task in flight is half done: {}", plain);
        assert!(plain.contains("Ctrl+Shift+O to collapse"), "got: {}", plain);
    }

    #[test]
    fn a_collapsed_panel_is_one_line() {
        let dir = workspace_with(json!({
            "next_id": 1,
            "items": [{ "id": 0, "text": "write the parser", "status": "pending" }]
        }));
        let panel = render_panel(&read_snapshot(dir.path()), true).unwrap();
        assert!(!panel.contains('\n'), "got: {}", panel);
        assert!(panel.contains("Ctrl+Shift+O to expand"), "got: {}", panel);
    }

    #[test]
    fn an_empty_list_has_no_panel() {
        let dir = tempfile::tempdir().unwrap();
        assert!(render_panel(&read_snapshot(dir.path()), false).is_none());
    }

    #[test]
    fn an_add_call_names_the_task_it_creates() {
        let dir = tempfile::tempdir().unwrap();
        let line = format_call(&json!({ "action": "add", "text": "write the parser" }), dir.path());
        assert_eq!(line, "☐ write the parser");

        let many = format_call(
            &json!({ "action": "add", "tasks": ["one", "two", "three"] }),
            dir.path(),
        );
        assert_eq!(many, "☐ one (+2 more)");
    }

    #[test]
    fn an_update_call_shows_the_new_state_and_the_task_text() {
        let dir = workspace_with(json!({
            "next_id": 2,
            "items": [{ "id": 1, "text": "test it", "status": "pending" }]
        }));
        let line = format_call(
            &json!({ "action": "update", "id": 1, "status": "done" }),
            dir.path(),
        );
        assert_eq!(line, "☑ #1 test it");
    }

    #[test]
    fn a_remove_call_shows_the_task_it_removes() {
        let dir = workspace_with(json!({
            "next_id": 2,
            "items": [{ "id": 1, "text": "test it", "status": "pending" }]
        }));
        let line = format_call(&json!({ "action": "remove", "id": 1 }), dir.path());
        assert_eq!(line, "☒ #1 test it");
    }

    #[test]
    fn a_list_call_counts_the_tasks() {
        let dir = workspace_with(json!({
            "next_id": 2,
            "items": [{ "id": 0, "text": "a", "status": "pending" }]
        }));
        let line = format_call(&json!({ "action": "list" }), dir.path());
        assert!(line.contains("1 tasks"), "got: {}", line);
    }
}
