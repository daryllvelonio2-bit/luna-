//! The todo list, as the transcript shows it.
//!
//! The list belongs to the session: the tool keeps one file per conversation, and this
//! module reads that same file back so the transcript shows the *tasks* — a checklist,
//! with the finished ones ticked — instead of the raw `todo_list` argument dump the
//! model happened to send.

use colored::Colorize;
use harness_core::tools::todo::{read_snapshot, TodoScope, TodoSnapshot, TodoStatus};

/// The session's list, as it stands right now.
pub fn read_session_snapshot(scope: &TodoScope) -> TodoSnapshot {
    read_snapshot(&scope.path())
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
pub fn format_call(arguments: &serde_json::Value, scope: &TodoScope) -> String {
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
                    let known = read_session_snapshot(scope).find(id).cloned();
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
                    let text = read_session_snapshot(scope)
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
            let count = read_session_snapshot(scope).items.len();
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

    fn scope_with(_unused: serde_json::Value) -> (tempfile::TempDir, TodoScope) {
        let dir = tempfile::tempdir().unwrap();
        let scope = TodoScope::new(dir.path().join("todos"), "s1");
        let file = json!({
            "next_id": 3,
            "items": [
                { "id": 0, "text": "write the parser", "status": "pending" },
                { "id": 1, "text": "test it", "status": "done" },
                { "id": 2, "text": "ship it", "status": "in_progress" }
            ]
        });
        std::fs::create_dir_all(dir.path().join("todos")).unwrap();
        std::fs::write(scope.path(), serde_json::to_string_pretty(&file).unwrap()).unwrap();
        (dir, scope)
    }

    #[test]
    fn the_panel_lists_every_task_with_its_state() {
        let (_dir, scope) = scope_with(json!({}));
        let snapshot = read_session_snapshot(&scope);
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
        let (_dir, scope) = scope_with(json!({}));
        let panel = render_panel(&read_session_snapshot(&scope), true).unwrap();
        assert!(!panel.contains('\n'), "got: {}", panel);
        assert!(panel.contains("Ctrl+Shift+O to expand"), "got: {}", panel);
    }

    #[test]
    fn an_empty_list_has_no_panel() {
        let dir = tempfile::tempdir().unwrap();
        let scope = TodoScope::new(dir.path().join("todos"), "s1");
        assert!(render_panel(&read_session_snapshot(&scope), false).is_none());
    }

    #[test]
    fn an_add_call_names_the_task_it_creates() {
        let dir = tempfile::tempdir().unwrap();
        let scope = TodoScope::new(dir.path().join("todos"), "s1");
        let line = format_call(&json!({ "action": "add", "text": "write the parser" }), &scope);
        assert_eq!(line, "☐ write the parser");

        let many = format_call(
            &json!({ "action": "add", "tasks": ["one", "two", "three"] }),
            &scope,
        );
        assert_eq!(many, "☐ one (+2 more)");
    }

    #[test]
    fn an_update_call_shows_the_new_state_and_the_task_text() {
        let (_dir, scope) = scope_with(json!({}));
        let line = format_call(
            &json!({ "action": "update", "id": 1, "status": "done" }),
            &scope,
        );
        assert_eq!(line, "☑ #1 test it");
    }

    #[test]
    fn a_remove_call_shows_the_task_it_removes() {
        let (_dir, scope) = scope_with(json!({}));
        let line = format_call(&json!({ "action": "remove", "id": 1 }), &scope);
        assert_eq!(line, "☒ #1 test it");
    }

    #[test]
    fn a_list_call_counts_the_tasks() {
        let (_dir, scope) = scope_with(json!({}));
        let line = format_call(&json!({ "action": "list" }), &scope);
        assert!(line.contains("3 tasks"), "got: {}", line);
    }

    /// Two sessions must not see each other's list.
    #[test]
    fn each_session_sees_only_its_own_list() {
        let dir = tempfile::tempdir().unwrap();
        let first = TodoScope::new(dir.path().join("todos"), "s1");
        let second = TodoScope::new(dir.path().join("todos"), "s2");
        std::fs::create_dir_all(dir.path().join("todos")).unwrap();
        std::fs::write(
            first.path(),
            serde_json::to_string_pretty(&json!({
                "next_id": 1,
                "items": [{ "id": 0, "text": "mine", "status": "pending" }]
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(read_session_snapshot(&first).items.len(), 1);
        assert_eq!(read_session_snapshot(&second).items.len(), 0);
    }
}
