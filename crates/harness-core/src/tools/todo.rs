use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::approval::ToolPreview;
use crate::error::HarnessError;
use crate::traits::Tool;
use crate::types::ToolResult;

/// A session's todo list, and where it lives.
///
/// The list belongs to the session, not to the workspace: two conversations in the same
/// workspace must not see each other's tasks, and a new conversation starts with an
/// empty list. The scope is a shared handle, so a host can point it at another session
/// — a fork continues the work of the conversation it was forked from, a new session
/// starts over — without rebuilding the tool registry.
#[derive(Clone, Debug, Default)]
pub struct TodoScope {
    inner: Arc<RwLock<ScopeInner>>,
}

#[derive(Clone, Debug, Default)]
struct ScopeInner {
    dir: PathBuf,
    session: String,
}

impl TodoScope {
    /// A scope for one session, with the lists kept in `dir`.
    pub fn new(dir: impl Into<PathBuf>, session: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ScopeInner {
                dir: dir.into(),
                session: session.into(),
            })),
        }
    }

    /// Point the scope at another session.
    pub fn set_session(&self, session: impl Into<String>) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.session = session.into();
    }

    /// The session the scope points at.
    pub fn session(&self) -> String {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .session
            .clone()
    }

    /// The file the current session's list lives in.
    pub fn path(&self) -> PathBuf {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let name = if inner.session.trim().is_empty() {
            "unscoped".to_string()
        } else {
            sanitize_file_stem(&inner.session)
        };
        inner.dir.join(format!("{}.json", name))
    }
}

/// Keeps a session id from escaping the directory it names.
fn sanitize_file_stem(session: &str) -> String {
    let cleaned: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "unscoped".to_string()
    } else {
        cleaned
    }
}

/// On-disk shape. `next_id` is the max id ever issued plus one, so ids are stable across
/// removals and never reused (a stale model reference cannot point at a different task).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct TodoFile {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    items: Vec<TodoItem>,
}

/// One task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: u64,
    pub text: String,
    pub status: TodoStatus,
}

/// Where a task stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Done,
}

impl TodoStatus {
    /// The glyph that carries the state: a tick for what is finished, a triangle for
    /// what is running, an empty circle for what is still open.
    ///
    /// They read apart at a glance, which a set of near-identical checkbox outlines
    /// does not.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Pending => "○",
            Self::InProgress => "▸",
            Self::Done => "✔",
        }
    }

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

/// A list as it stands on disk, for a reader that only wants to show it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TodoSnapshot {
    #[serde(default)]
    pub items: Vec<TodoItem>,
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
    pub fn find(&self, id: u64) -> Option<&TodoItem> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// Read a list from disk. A missing or unreadable file is an empty list, not an error:
/// the panel is decoration, and decoration must never break the transcript.
pub fn read_snapshot(path: &Path) -> TodoSnapshot {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return TodoSnapshot::default();
    };
    let Ok(file) = serde_json::from_str::<TodoFile>(&raw) else {
        return TodoSnapshot::default();
    };
    TodoSnapshot { items: file.items }
}

pub struct TodoListTool {
    scope: TodoScope,
    /// Serializes read-modify-write cycles so two concurrent calls cannot lose an update.
    lock: Arc<Mutex<()>>,
}

impl TodoListTool {
    pub fn new(scope: TodoScope) -> Self {
        Self {
            scope,
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn load(&self) -> Result<TodoFile, HarnessError> {
        let path = self.scope.path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            // A missing file is an empty list, not an error — the first `add` creates it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TodoFile::default()),
            Err(e) => {
                return Err(HarnessError::ToolExecutionFailed {
                    tool: self.name().to_string(),
                    reason: format!("cannot read {}: {}", path.display(), e),
                });
            }
        };
        serde_json::from_str(&raw).map_err(|e| HarnessError::ToolExecutionFailed {
            tool: self.name().to_string(),
            reason: format!("{} is corrupt: {}", path.display(), e),
        })
    }

    /// Persists via temp-file + rename, so a crash mid-write cannot truncate the list.
    fn store(&self, file: &TodoFile) -> Result<(), HarnessError> {
        let path = self.scope.path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("json.tmp");
        let raw = serde_json::to_string_pretty(file).map_err(|e| HarnessError::ToolExecutionFailed {
            tool: self.name().to_string(),
            reason: format!("cannot serialize todo list: {}", e),
        })?;
        std::fs::write(&tmp, raw).map_err(|e| HarnessError::ToolExecutionFailed {
            tool: self.name().to_string(),
            reason: format!("cannot write {}: {}", tmp.display(), e),
        })?;
        std::fs::rename(&tmp, &path).map_err(|e| HarnessError::ToolExecutionFailed {
            tool: self.name().to_string(),
            reason: format!("cannot replace {}: {}", path.display(), e),
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
        "Maintain this conversation's todo task list for major, complex multi-phase tasks that span extensive turns. Do NOT use for simple, standard, or direct tasks. Actions: 'list' (no args), 'add' (text), 'update' (id + optional text/status), 'remove' (id), 'clear_done'."
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
        false
    }

    fn is_mutating_call(&self, _arguments: &serde_json::Value) -> bool {
        false
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
                                .trim_start_matches(['-', '*', '+', '•'])
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

    fn scope(dir: &tempfile::TempDir, session: &str) -> TodoScope {
        TodoScope::new(dir.path().join("todos"), session)
    }

    fn tool(dir: &tempfile::TempDir, session: &str) -> TodoListTool {
        TodoListTool::new(scope(dir, session))
    }

    async fn run(tool: &TodoListTool, args: serde_json::Value) -> ToolResult {
        tool.execute("c1", &args, &CancellationToken::new())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn add_lists_and_updates() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(&dir, "s1");

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

    /// The list belongs to the session: another session must not see it, and must not
    /// share its ids.
    #[tokio::test]
    async fn a_list_is_invisible_to_another_session() {
        let dir = tempfile::tempdir().unwrap();
        let first = tool(&dir, "s1");
        let second = tool(&dir, "s2");

        run(&first, json!({"action": "add", "text": "mine"})).await;

        let r = run(&second, json!({"action": "list"})).await;
        assert_eq!(r.content, "[todo list is empty]");

        // Ids start again in the new session: they are per list, not global.
        let r = run(&second, json!({"action": "add", "text": "theirs"})).await;
        assert!(r.content.contains("added [0]"));

        let r = run(&first, json!({"action": "list"})).await;
        assert!(r.content.contains("mine"));
        assert!(!r.content.contains("theirs"));
    }

    /// A scope that is pointed at another session follows it — that is how a fork
    /// continues the work of the conversation it came from.
    #[tokio::test]
    async fn a_scope_follows_the_session_it_is_pointed_at() {
        let dir = tempfile::tempdir().unwrap();
        let shared = scope(&dir, "s1");
        let t = TodoListTool::new(shared.clone());

        run(&t, json!({"action": "add", "text": "carried over"})).await;

        shared.set_session("s2");
        let r = run(&t, json!({"action": "list"})).await;
        assert_eq!(r.content, "[todo list is empty]");

        shared.set_session("s1");
        let r = run(&t, json!({"action": "list"})).await;
        assert!(r.content.contains("carried over"));
    }

    #[tokio::test]
    async fn the_list_lives_beside_the_other_session_data() {
        let dir = tempfile::tempdir().unwrap();
        let shared = scope(&dir, "s1");
        let t = TodoListTool::new(shared.clone());
        run(&t, json!({"action": "add", "text": "on disk"})).await;

        let path = shared.path();
        assert!(path.exists(), "the list should be a file at {}", path.display());
        assert!(path.starts_with(dir.path()), "got: {}", path.display());

        let snapshot = read_snapshot(&path);
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].text, "on disk");
        assert_eq!(snapshot.counts(), (0, 0, 1));
    }

    #[tokio::test]
    async fn ids_are_never_reused_after_removal() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(&dir, "s1");

        run(&t, json!({"action": "add", "text": "a"})).await;
        run(&t, json!({"action": "remove", "id": 0})).await;
        let r = run(&t, json!({"action": "add", "text": "b"})).await;
        assert!(r.content.contains("added [1]"));
    }

    #[tokio::test]
    async fn clear_done_keeps_open_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(&dir, "s1");

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
        let t = tool(&dir, "s1");

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
        let t = tool(&dir, "s1");
        let r = run(&t, json!({})).await;
        assert_eq!(r.content, "[todo list is empty]");
    }

    #[tokio::test]
    async fn state_persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        run(&tool(&dir, "s1"), json!({"action": "add", "text": "survives"})).await;
        let r = run(&tool(&dir, "s1"), json!({"action": "list"})).await;
        assert!(r.content.contains("survives"));
    }

    #[tokio::test]
    async fn bulk_add_tasks_array_and_multiline() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(&dir, "s1");

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

    #[test]
    fn a_session_id_cannot_escape_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let scope = TodoScope::new(dir.path(), "../../etc/passwd");
        let path = scope.path();
        assert_eq!(path.parent().unwrap(), dir.path());
        assert!(!path.to_string_lossy().contains(".."));
    }
}
