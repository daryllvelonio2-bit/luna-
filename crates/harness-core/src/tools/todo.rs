use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::approval::ToolPreview;
use crate::error::HarnessError;
use crate::traits::Tool;
use crate::types::ToolResult;

/// File the todo list persists to, inside the workspace: survives sessions, is readable by a
/// human in any editor, and stays per-worktree like every other workspace artifact.
const TODO_FILE_NAME: &str = ".luna-todo.json";

/// On-disk shape. `next_id` is the max id ever issued plus one, so ids are stable across
/// removals and never reused (a stale model reference cannot point at a different task).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct TodoFile {
    next_id: u64,
    items: Vec<TodoItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct TodoItem {
    id: u64,
    text: String,
    status: TodoStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

impl TodoStatus {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "pending" | "todo" | "open" => Some(Self::Pending),
            "in_progress" | "in-progress" | "inprogress" | "doing" | "working" => Some(Self::InProgress),
            "done" | "completed" | "complete" | "finished" | "closed" => Some(Self::Done),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }
}

pub struct TodoListTool {
    todo_path: PathBuf,
    /// Serializes read-modify-write cycles so two concurrent calls cannot lose an update.
    lock: Arc<Mutex<()>>,
}

impl TodoListTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            todo_path: workspace_root.join(TODO_FILE_NAME),
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn load(&self) -> Result<TodoFile, HarnessError> {
        let raw = match std::fs::read_to_string(&self.todo_path) {
            Ok(raw) => raw,
            // A missing file is an empty list, not an error — the first `add` creates it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TodoFile::default()),
            Err(e) => {
                return Err(HarnessError::ToolExecutionFailed {
                    tool: self.name().to_string(),
                    reason: format!("cannot read {}: {}", self.todo_path.display(), e),
                });
            }
        };
        serde_json::from_str(&raw).map_err(|e| HarnessError::ToolExecutionFailed {
            tool: self.name().to_string(),
            reason: format!("{} is corrupt: {}", self.todo_path.display(), e),
        })
    }

    /// Persists via temp-file + rename, so a crash mid-write cannot truncate the list.
    fn store(&self, file: &TodoFile) -> Result<(), HarnessError> {
        let tmp = self.todo_path.with_extension("json.tmp");
        let raw = serde_json::to_string_pretty(file).map_err(|e| HarnessError::ToolExecutionFailed {
            tool: self.name().to_string(),
            reason: format!("cannot serialize todo list: {}", e),
        })?;
        std::fs::write(&tmp, raw).map_err(|e| HarnessError::ToolExecutionFailed {
            tool: self.name().to_string(),
            reason: format!("cannot write {}: {}", tmp.display(), e),
        })?;
        std::fs::rename(&tmp, &self.todo_path).map_err(|e| HarnessError::ToolExecutionFailed {
            tool: self.name().to_string(),
            reason: format!("cannot replace {}: {}", self.todo_path.display(), e),
        })?;
        Ok(())
    }

    fn render(file: &TodoFile) -> String {
        if file.items.is_empty() {
            return "[todo list is empty]".to_string();
        }
        let mut lines = Vec::with_capacity(file.items.len());
        for item in &file.items {
            lines.push(format!("[{}] {} {}", item.id, item.status.as_str(), item.text));
        }
        lines.join("\n")
    }
}

#[async_trait]
impl Tool for TodoListTool {
    fn name(&self) -> &str {
        "todo_list"
    }

    fn description(&self) -> &str {
        "Maintain the session's shared todo task list. Actions: 'list' (no args), 'add' (text), \
         'update' (id + optional text/status), 'remove' (id), 'clear_done'. Ids are short numbers \
         that are never reused. Use it to plan multi-step work and keep the user informed: add a \
         task before starting it, mark it in_progress while working, done when verified."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "add", "update", "remove", "clear_done"],
                    "description": "Operation to perform (default: list)"
                },
                "id": { "type": "integer", "description": "Task id for 'update' and 'remove'" },
                "text": { "type": "string", "description": "Task text for 'add' or the replacement text for 'update'" },
                "tasks": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of task texts for bulk 'add'"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "done"],
                    "description": "Status for 'update'"
                }
            },
            "required": []
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    fn is_mutating_call(&self, arguments: &serde_json::Value) -> bool {
        let action = arguments.get("action").and_then(|a| a.as_str()).unwrap_or("list");
        action != "list"
    }

    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list");
        let target = match action {
            "add" => {
                if let Some(tasks) = args.get("tasks").and_then(|t| t.as_array()) {
                    Some(format!("add {} tasks", tasks.len()))
                } else {
                    args.get("text")
                        .and_then(|t| t.as_str())
                        .map(|t| format!("add \"{}\"", t))
                }
            }
            "update" => Some(format!(
                "update #{}",
                args.get("id").and_then(|i| i.as_u64()).unwrap_or(0)
            )),
            "remove" => Some(format!(
                "remove #{}",
                args.get("id").and_then(|i| i.as_u64()).unwrap_or(0)
            )),
            _ => None,
        };
        Some(ToolPreview {
            target,
            diff: None,
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list");

        // The whole read-modify-write runs under one lock; SQLite-style per-statement locking
        // would let two interleaved calls drop each other's items.
        let _guard = self.lock.lock().await;
        let mut file = self.load()?;

        // Validation failures are tool errors the model can correct; only storage failures
        // (unwritable file) surface as `Err(HarnessError)`.
        let bad = |reason: String| ToolResult::error(call_id, reason);

        let reply = match action {
            "list" => Self::render(&file),
            "add" => {
                let mut texts_to_add = Vec::new();
                if let Some(arr) = args.get("tasks").or_else(|| args.get("items")).and_then(|v| v.as_array()) {
                    for v in arr {
                        if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                            texts_to_add.push(s.to_string());
                        }
                    }
                } else if let Some(t) = args.get("text").and_then(|t| t.as_str()).map(str::trim).filter(|t| !t.is_empty()) {
                    if t.contains('\n') {
                        for line in t.lines() {
                            let clean = line.trim()
                                .trim_start_matches(|c: char| c == '-' || c == '*' || c == '+' || c == '•')
                                .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')')
                                .trim();
                            if !clean.is_empty() {
                                texts_to_add.push(clean.to_string());
                            }
                        }
                    } else {
                        texts_to_add.push(t.to_string());
                    }
                }

                if texts_to_add.is_empty() {
                    return Ok(bad("'add' requires non-empty 'text' or 'tasks'".to_string()));
                }

                let mut added_msgs = Vec::new();
                for text in texts_to_add {
                    let id = file.next_id;
                    file.next_id += 1;
                    file.items.push(TodoItem {
                        id,
                        text: text.clone(),
                        status: TodoStatus::Pending,
                    });
                    added_msgs.push(format!("[{}] (pending): {}", id, text));
                }
                self.store(&file)?;
                if added_msgs.len() == 1 {
                    format!("added {}", added_msgs[0])
                } else {
                    format!("added {} tasks:\n{}", added_msgs.len(), added_msgs.join("\n"))
                }
            }
            "update" => {
                let Some(id) = args.get("id").and_then(|i| i.as_u64()) else {
                    return Ok(bad("'update' requires 'id'".to_string()));
                };
                let Some(item) = file.items.iter_mut().find(|item| item.id == id) else {
                    return Ok(bad(format!("no task with id {}", id)));
                };
                if let Some(text) = args.get("text").and_then(|t| t.as_str()) {
                    let text = text.trim();
                    if !text.is_empty() {
                        item.text = text.to_string();
                    }
                }
                if let Some(status) = args.get("status").and_then(|s| s.as_str()) {
                    let Some(status) = TodoStatus::parse(status) else {
                        return Ok(bad(format!(
                            "unknown status '{}': use pending, in_progress or done",
                            status
                        )));
                    };
                    item.status = status;
                }
                let rendered = format!("[{}] {} {}", item.id, item.status.as_str(), item.text);
                self.store(&file)?;
                rendered
            }
            "remove" => {
                let Some(id) = args.get("id").and_then(|i| i.as_u64()) else {
                    return Ok(bad("'remove' requires 'id'".to_string()));
                };
                let before = file.items.len();
                file.items.retain(|item| item.id != id);
                if file.items.len() == before {
                    return Ok(bad(format!("no task with id {}", id)));
                }
                self.store(&file)?;
                format!("removed [{}]", id)
            }
            "clear_done" => {
                let removed = file.items.len();
                file.items.retain(|item| item.status != TodoStatus::Done);
                let removed = removed - file.items.len();
                self.store(&file)?;
                format!("cleared {} done task(s)", removed)
            }
            other => {
                return Ok(ToolResult::error(
                    call_id,
                    format!(
                        "unknown action '{}': use list, add, update, remove or clear_done",
                        other
                    ),
                ));
            }
        };

        Ok(ToolResult::success(call_id, reply))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(dir: &tempfile::TempDir) -> TodoListTool {
        TodoListTool::new(dir.path().to_path_buf())
    }

    async fn run(tool: &TodoListTool, args: serde_json::Value) -> ToolResult {
        tool.execute("c1", &args, &CancellationToken::new())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn add_lists_and_updates() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(&dir);

        let r = run(&t, json!({"action": "add", "text": "write the parser"})).await;
        assert!(r.content.contains("added [0]"));
        let r = run(&t, json!({"action": "add", "text": "test it"})).await;
        assert!(r.content.contains("added [1]"));

        let r = run(&t, json!({"action": "update", "id": 1, "status": "done"})).await;
        assert!(r.content.contains("[1] done"));
        assert!(!r.is_error);

        let r = run(&t, json!({"action": "list"})).await;
        assert!(r.content.contains("[0] pending write the parser"));
        assert!(r.content.contains("[1] done test it"));
    }

    #[tokio::test]
    async fn ids_are_never_reused_after_removal() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(&dir);

        run(&t, json!({"action": "add", "text": "a"})).await;
        run(&t, json!({"action": "remove", "id": 0})).await;
        let r = run(&t, json!({"action": "add", "text": "b"})).await;
        assert!(r.content.contains("added [1]"));
    }

    #[tokio::test]
    async fn clear_done_keeps_open_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(&dir);

        run(&t, json!({"action": "add", "text": "done thing"})).await;
        run(&t, json!({"action": "add", "text": "open thing"})).await;
        run(&t, json!({"action": "update", "id": 0, "status": "done"})).await;

        let r = run(&t, json!({"action": "clear_done"})).await;
        assert!(r.content.contains("cleared 1"));
        let r = run(&t, json!({"action": "list"})).await;
        assert!(r.content.contains("open thing"));
        assert!(!r.content.contains("done thing"));
    }

    #[tokio::test]
    async fn bad_input_is_a_tool_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(&dir);

        let r = run(&t, json!({"action": "add"})).await;
        assert!(r.is_error);
        let r = run(&t, json!({"action": "update", "id": 9, "status": "done"})).await;
        assert!(r.is_error);
        let r = run(&t, json!({"action": "update", "id": 0, "status": "finished"})).await;
        assert!(r.is_error);
        let r = run(&t, json!({"action": "explode"})).await;
        assert!(r.is_error);
        let r = run(&t, json!({"action": "remove"})).await;
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn empty_list_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(&dir);
        let r = run(&t, json!({})).await;
        assert_eq!(r.content, "[todo list is empty]");
    }

    #[tokio::test]
    async fn state_persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        run(&tool(&dir), json!({"action": "add", "text": "survives"})).await;
        let r = run(&tool(&dir), json!({"action": "list"})).await;
        assert!(r.content.contains("survives"));
    }

    #[tokio::test]
    async fn bulk_add_tasks_array_and_multiline() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(&dir);

        // 1. Array of tasks
        let r = run(&t, json!({"action": "add", "tasks": ["Task one", "Task two", "Task three"]})).await;
        assert!(!r.is_error);
        assert!(r.content.contains("added 3 tasks"));

        let list = run(&t, json!({"action": "list"})).await;
        assert!(list.content.contains("[0] pending Task one"));
        assert!(list.content.contains("[1] pending Task two"));
        assert!(list.content.contains("[2] pending Task three"));

        // 2. Multiline list with markdown bullets
        let r2 = run(&t, json!({"action": "add", "text": "- First step\n- Second step\n- Third step"})).await;
        assert!(!r2.is_error);
        assert!(r2.content.contains("added 3 tasks"));

        let list2 = run(&t, json!({"action": "list"})).await;
        assert!(list2.content.contains("[3] pending First step"));
        assert!(list2.content.contains("[4] pending Second step"));
        assert!(list2.content.contains("[5] pending Third step"));
    }
}
