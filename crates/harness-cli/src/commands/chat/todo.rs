//! The todo list, as the composer shows it.
//!
//! The list belongs to the session: the tool keeps one file per conversation, and this
//! module reads that same file back so the user sees the *tasks* — a checklist, with
//! the finished ones ticked — instead of the raw `todo_list` argument dump the model
//! happened to send.
//!
//! The panel is chrome, not transcript: it is drawn above the composer and stays
//! there while the conversation scrolls under it.

use colored::Colorize;
use harness_core::tools::todo::{read_snapshot, TodoItem, TodoScope, TodoSnapshot, TodoStatus};

use super::ui::ChatUi;

/// The session's list, as it stands right now.
pub fn read_session_snapshot(scope: &TodoScope) -> TodoSnapshot {
    read_snapshot(&scope.path())
}

/// The glyph that carries a task's state.
///
/// The core owns the icons, so the panel and the tool's own output cannot drift
/// apart: a tick for what is finished, a triangle for what is running, an empty
/// circle for what is still open.
pub fn status_icon(status: TodoStatus) -> &'static str {
    status.glyph()
}

/// The icon a `todo_list` call shows in the transcript.
///
/// The call line is one line in a log of tool calls: it says what the call *did*,
/// so the icons differ by action rather than repeating the state glyph.
fn action_icon(action: &str) -> &'static str {
    match action {
        "add" => "○",
        "update" => "▸",
        "remove" => "✖",
        "clear_done" => "⌫",
        _ => "☰",
    }
}

/// Narrowest the panel is drawn. A very narrow terminal would otherwise squeeze the
/// task text down to a single character.
const MIN_PANEL_WIDTH: usize = 40;

/// The checklist panel, as the rows the composer draws above itself.
///
/// The panel is a box: a header carrying the counts, one row per task, and a footer
/// with the fold hint. Collapsed it is a single row.
pub fn render_panel(snapshot: &TodoSnapshot, collapsed: bool, width: usize) -> Vec<String> {
    if snapshot.items.is_empty() {
        return Vec::new();
    }

    let width = width.max(MIN_PANEL_WIDTH);
    let (done, active, pending) = snapshot.counts();
    let counts = format!("{} done · {} active · {} pending", done, active, pending);
    let hint = if collapsed {
        "Ctrl+Shift+O to expand"
    } else {
        "Ctrl+Shift+O to collapse"
    };

    if collapsed {
        return vec![collapsed_row(&counts, hint, width)];
    }

    let mut rows = Vec::with_capacity(snapshot.items.len() + 2);
    rows.push(top_border("Todo", &counts, width));

    for item in &snapshot.items {
        rows.push(task_row(item, width));
    }
    rows.push(bottom_border(hint, width));
    rows
}

/// The single row the collapsed panel shows.
fn collapsed_row(counts: &str, hint: &str, width: usize) -> String {
    let left = format!("Todo · {}", counts);
    let used = left.chars().count() + hint.chars().count() + 8;
    let fill = width.saturating_sub(used);
    format!(
        "╭─ {} {} {} ─╮",
        left.bold().cyan(),
        "─".repeat(fill),
        hint.dimmed()
    )
}

/// The panel's top border, with the title on the left and the counts on the right.
fn top_border(title: &str, right: &str, width: usize) -> String {
    let used = title.chars().count() + right.chars().count() + 8;
    let fill = width.saturating_sub(used);
    format!(
        "╭─ {} {} {} ─╮",
        title.bold().cyan(),
        "─".repeat(fill),
        right.dimmed()
    )
}

/// The panel's bottom border, with the fold hint on the right.
fn bottom_border(hint: &str, width: usize) -> String {
    let fill = width.saturating_sub(hint.chars().count() + 4);
    format!("╰{} {} ╯", "─".repeat(fill), hint.dimmed())
}

/// One task, in a row of the panel.
///
/// The row is padded to the panel's width so the box's right edge stays a straight
/// line whatever the task text says.
fn task_row(item: &TodoItem, width: usize) -> String {
    let icon = status_icon(item.status);
    let inner = width.saturating_sub(4);
    // "icon #id  text": one space after the icon, two after the id.
    let fixed = 1 + 1 + 1 + item.id.to_string().chars().count() + 2;
    // The text is cut to the room the row has, and the cut is marked: a task whose
    // text runs past the edge of the panel is still legible.
    let room = inner.saturating_sub(fixed);
    let text = if item.text.chars().count() > room {
        let cut = room.saturating_sub(1);
        format!("{}…", item.text.chars().take(cut).collect::<String>())
    } else {
        item.text.clone()
    };
    let pad = inner.saturating_sub(fixed + text.chars().count());

    let styled_icon = match item.status {
        TodoStatus::Done => icon.green().bold().to_string(),
        TodoStatus::InProgress => icon.yellow().bold().to_string(),
        TodoStatus::Pending => icon.dimmed().to_string(),
    };
    let styled_text = match item.status {
        TodoStatus::Done => text.dimmed().to_string(),
        _ => text.normal().to_string(),
    };

    format!(
        "│ {} {}  {}{} │",
        styled_icon,
        format!("#{}", item.id).dimmed(),
        styled_text,
        " ".repeat(pad)
    )
}

/// Re-read the session's list and redraw the panel.
///
/// The panel is drawn from the file the tool writes, so it shows the list as it
/// stands now rather than as it stood when the turn started.
pub fn refresh_panel(ui: &ChatUi, scope: &TodoScope) {
    let snapshot = read_session_snapshot(scope);
    let empty = snapshot.items.is_empty();
    ui.set_todo_panel(if empty { None } else { Some(snapshot) });
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
                    format!("{} {} (+{} more)", action_icon("add"), first, rest.len())
                }
                Some((first, _)) => format!("{} {}", action_icon("add"), first),
                None => format!("{} todo", action_icon("add")),
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
                    format!("{} #{} {}", status_icon(status), id, text)
                }
                None => format!("{} todo", action_icon("update")),
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
                    format!("{} #{} {}", action_icon("remove"), id, text)
                }
                None => format!("{} todo", action_icon("remove")),
            }
        }
        "clear_done" => format!("{} cleared done tasks", action_icon("clear_done")),
        _ => {
            let count = read_session_snapshot(scope).items.len();
            format!("{} {} tasks", action_icon("list"), count)
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
    use harness_core::tools::todo::TodoItem;
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

    fn plain(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|r| dialoguer::console::strip_ansi_codes(r).to_string())
            .collect()
    }

    #[test]
    fn the_panel_lists_every_task_with_its_state() {
        let (_dir, scope) = scope_with(json!({}));
        let snapshot = read_session_snapshot(&scope);
        assert_eq!(snapshot.counts(), (1, 1, 1));

        let panel = plain(&render_panel(&snapshot, false, 80));
        let text = panel.join("\n");
        assert!(text.contains("write the parser"), "got: {}", text);
        assert!(text.contains("test it"), "got: {}", text);
        assert!(text.contains("ship it"), "got: {}", text);
        assert!(text.contains("○"), "an open task is an empty circle: {}", text);
        assert!(text.contains("✔"), "a finished task is ticked: {}", text);
        assert!(text.contains("▸"), "a task in flight is a triangle: {}", text);
        assert!(text.contains("Ctrl+Shift+O to collapse"), "got: {}", text);
    }

    #[test]
    fn the_panel_is_a_box_whose_rows_are_all_the_same_width() {
        let (_dir, scope) = scope_with(json!({}));
        let rows = plain(&render_panel(&read_session_snapshot(&scope), false, 80));
        assert_eq!(rows.len(), 5, "got: {:?}", rows);
        for row in &rows {
            assert_eq!(row.chars().count(), 80, "row is not the panel's width: {}", row);
        }
        assert!(rows[0].starts_with('╭') && rows[0].ends_with('╮'), "got: {}", rows[0]);
        assert!(rows[1].starts_with('│') && rows[1].ends_with('│'), "got: {}", rows[1]);
        assert!(rows[4].starts_with('╰') && rows[4].ends_with('╯'), "got: {}", rows[4]);
    }

    #[test]
    fn a_long_task_text_is_cut_to_fit_the_box() {
        let dir = tempfile::tempdir().unwrap();
        let scope = TodoScope::new(dir.path().join("todos"), "s1");
        std::fs::create_dir_all(dir.path().join("todos")).unwrap();
        std::fs::write(
            scope.path(),
            serde_json::to_string_pretty(&json!({
                "next_id": 1,
                "items": [{ "id": 0, "text": "x".repeat(200), "status": "pending" }]
            }))
            .unwrap(),
        )
        .unwrap();

        let rows = plain(&render_panel(&read_session_snapshot(&scope), false, 60));
        for row in &rows {
            assert_eq!(row.chars().count(), 60, "row is not the panel's width: {}", row);
        }
        assert!(rows[1].contains('…'), "the cut is marked: {}", rows[1]);
    }

    /// The panel's exact shape. A golden screen, so a change to the box is a visible
    /// change in the test rather than a surprise in the terminal.
    #[test]
    fn the_panel_has_the_shape_it_is_drawn_in() {
        let snapshot = TodoSnapshot {
            items: vec![
                TodoItem {
                    id: 9,
                    text: "Add a regression test for the panel's erase accounting on resize"
                        .to_string(),
                    status: TodoStatus::InProgress,
                },
                TodoItem {
                    id: 10,
                    text: "Check the pinned panel in a narrow terminal (40 columns)".to_string(),
                    status: TodoStatus::Pending,
                },
                TodoItem {
                    id: 11,
                    text: "Document the pinned todo panel in the README".to_string(),
                    status: TodoStatus::Pending,
                },
            ],
        };

        let rows = plain(&render_panel(&snapshot, false, 72));
        assert_eq!(
            rows,
            vec![
                "╭─ Todo ─────────────────────────────── 0 done · 1 active · 2 pending ─╮",
                "│ ▸ #9  Add a regression test for the panel's erase accounting on res… │",
                "│ ○ #10  Check the pinned panel in a narrow terminal (40 columns)      │",
                "│ ○ #11  Document the pinned todo panel in the README                  │",
                "╰──────────────────────────────────────────── Ctrl+Shift+O to collapse ╯",
            ]
        );
    }

    #[test]
    fn a_collapsed_panel_is_one_row() {
        let (_dir, scope) = scope_with(json!({}));
        let rows = plain(&render_panel(&read_session_snapshot(&scope), true, 80));
        assert_eq!(rows.len(), 1, "got: {:?}", rows);
        assert_eq!(rows[0].chars().count(), 80, "got: {}", rows[0]);
        assert!(rows[0].contains("Ctrl+Shift+O to expand"), "got: {}", rows[0]);
    }

    #[test]
    fn an_empty_list_has_no_panel() {
        let dir = tempfile::tempdir().unwrap();
        let scope = TodoScope::new(dir.path().join("todos"), "s1");
        assert!(render_panel(&read_session_snapshot(&scope), false, 80).is_empty());
    }

    #[test]
    fn an_add_call_names_the_task_it_creates() {
        let dir = tempfile::tempdir().unwrap();
        let scope = TodoScope::new(dir.path().join("todos"), "s1");
        let line = format_call(&json!({ "action": "add", "text": "write the parser" }), &scope);
        assert_eq!(line, "○ write the parser");

        let many = format_call(
            &json!({ "action": "add", "tasks": ["one", "two", "three"] }),
            &scope,
        );
        assert_eq!(many, "○ one (+2 more)");
    }

    #[test]
    fn an_update_call_shows_the_new_state_and_the_task_text() {
        let (_dir, scope) = scope_with(json!({}));
        let line = format_call(
            &json!({ "action": "update", "id": 1, "status": "done" }),
            &scope,
        );
        assert_eq!(line, "✔ #1 test it");
    }

    #[test]
    fn a_remove_call_shows_the_task_it_removes() {
        let (_dir, scope) = scope_with(json!({}));
        let line = format_call(&json!({ "action": "remove", "id": 1 }), &scope);
        assert_eq!(line, "✖ #1 test it");
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


