use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::agent::{Agent, SessionBinding};
use crate::approval::{AllowAll, ApprovalSettings, ToolPreview};
use crate::config::Config;
use crate::error::HarnessError;
use crate::registry::DefaultToolRegistry;
use crate::session::{SessionStore, SqliteSessionStore};
use crate::traits::{
    EvolverEngine, MemoryEngine, ModelProvider, NotesEngine, Tool, ToolRegistry,
};
use crate::types::{
    CompletionOptions, MemoryRecord, Message, Note, StreamEvent, ToolDefinition,
    ToolResult,
};

#[derive(Clone)]
struct DynProvider(Arc<dyn ModelProvider>);

#[async_trait]
impl ModelProvider for DynProvider {
    fn provider_id(&self) -> &str {
        self.0.provider_id()
    }
    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        self.0.fetch_models().await
    }
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        self.0.complete(messages, tools, options).await
    }
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        self.0.stream(messages, tools, options).await
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        self.0.embed(texts).await
    }
}

#[derive(Clone)]
struct DynMemory(Arc<dyn MemoryEngine>);

#[async_trait]
impl MemoryEngine for DynMemory {
    async fn retrieve(&self, query: &str, limit: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
        self.0.retrieve(query, limit).await
    }
    async fn save(&self, record: &MemoryRecord) -> Result<(), HarnessError> {
        self.0.save(record).await
    }
    async fn update(&self, id: &str, updated: &MemoryRecord) -> Result<(), HarnessError> {
        self.0.update(id, updated).await
    }
    async fn forget(&self, id: &str) -> Result<(), HarnessError> {
        self.0.forget(id).await
    }
    async fn add_note_pointer(&self, note_id: &str, pointer: &str) -> Result<(), HarnessError> {
        self.0.add_note_pointer(note_id, pointer).await
    }
    async fn remove_note_pointer(&self, note_id: &str) -> Result<(), HarnessError> {
        self.0.remove_note_pointer(note_id).await
    }
    fn format_as_system_prompt(&self, memories: &[MemoryRecord]) -> String {
        self.0.format_as_system_prompt(memories)
    }
}

#[derive(Clone)]
struct DynNotes(Arc<dyn NotesEngine>);

#[async_trait]
impl NotesEngine for DynNotes {
    async fn save_note(
        &self,
        title: &str,
        content: &str,
        tags: &[String],
    ) -> Result<Note, HarnessError> {
        self.0.save_note(title, content, tags).await
    }
    async fn search_notes(&self, query: &str, limit: usize) -> Result<Vec<Note>, HarnessError> {
        self.0.search_notes(query, limit).await
    }
    async fn list_notes(&self, limit: usize, offset: usize) -> Result<Vec<Note>, HarnessError> {
        self.0.list_notes(limit, offset).await
    }
    async fn read_note(&self, id: &str) -> Result<Note, HarnessError> {
        self.0.read_note(id).await
    }
    async fn update_note(
        &self,
        id: &str,
        title: Option<&str>,
        content: Option<&str>,
        tags: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        self.0.update_note(id, title, content, tags).await
    }
    async fn delete_note(&self, id: &str) -> Result<bool, HarnessError> {
        self.0.delete_note(id).await
    }
}

#[derive(Clone)]
struct DynEvolver(Arc<dyn EvolverEngine>);

#[async_trait]
impl EvolverEngine for DynEvolver {
    async fn inspect_source(&self, path: &str) -> Result<String, HarnessError> {
        self.0.inspect_source(path).await
    }
    async fn propose_patch(&self, description: &str, diff: &str) -> Result<String, HarnessError> {
        self.0.propose_patch(description, diff).await
    }
    async fn test_patch_sandbox(&self, patch_id: &str) -> Result<bool, HarnessError> {
        self.0.test_patch_sandbox(patch_id).await
    }
    async fn apply_patch(&self, patch_id: &str) -> Result<(), HarnessError> {
        self.0.apply_patch(patch_id).await
    }
    async fn preview_patch(&self, patch_id: &str) -> Option<ToolPreview> {
        self.0.preview_patch(patch_id).await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubagentStatus {
    Running,
    Completed { reply: String, finished_at: DateTime<Utc> },
    Failed { error: String, finished_at: DateTime<Utc> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentTaskRecord {
    pub id: String,
    pub goal: String,
    pub context: Option<String>,
    pub status: SubagentStatus,
    pub started_at: DateTime<Utc>,
}

pub type SubagentCompletionNotifier = Arc<dyn Fn(&SubagentTaskRecord) + Send + Sync>;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubagentTaskSpec {
    pub goal: String,
    pub context: Option<String>,
}

pub struct DelegateTaskTool {
    provider: Arc<dyn ModelProvider>,
    memory: Arc<dyn MemoryEngine>,
    notes: Arc<dyn NotesEngine>,
    evolver: Arc<dyn EvolverEngine>,
    store: Option<Arc<SqliteSessionStore>>,
    config: Config,
    max_concurrent: usize,
    tasks_state: Arc<RwLock<HashMap<String, SubagentTaskRecord>>>,
    notifier: Arc<RwLock<Option<SubagentCompletionNotifier>>>,
    steer_handle: Arc<RwLock<Option<Arc<Mutex<VecDeque<String>>>>>>,
}

impl DelegateTaskTool {
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        memory: Arc<dyn MemoryEngine>,
        notes: Arc<dyn NotesEngine>,
        evolver: Arc<dyn EvolverEngine>,
        store: Option<Arc<SqliteSessionStore>>,
        config: Config,
    ) -> Self {
        Self {
            provider,
            memory,
            notes,
            evolver,
            store,
            config,
            max_concurrent: 4,
            tasks_state: Arc::new(RwLock::new(HashMap::new())),
            notifier: Arc::new(RwLock::new(None)),
            steer_handle: Arc::new(RwLock::new(None)),
        }
    }

    pub fn set_notifier(&self, notifier: SubagentCompletionNotifier) {
        if let Ok(mut lock) = self.notifier.try_write() {
            *lock = Some(notifier);
        }
    }

    pub fn set_steer_handle(&self, steer_handle: Arc<Mutex<VecDeque<String>>>) {
        if let Ok(mut lock) = self.steer_handle.try_write() {
            *lock = Some(steer_handle);
        }
    }

    pub fn with_notifier(self, notifier: SubagentCompletionNotifier) -> Self {
        *self.notifier.try_write().unwrap() = Some(notifier);
        self
    }

    pub fn with_steer_handle(self, steer_handle: Arc<Mutex<VecDeque<String>>>) -> Self {
        *self.steer_handle.try_write().unwrap() = Some(steer_handle);
        self
    }

    pub async fn list_tasks(&self) -> Vec<SubagentTaskRecord> {
        self.tasks_state.read().await.values().cloned().collect()
    }
}

async fn run_single_subagent(
    provider: Arc<dyn ModelProvider>,
    memory: Arc<dyn MemoryEngine>,
    notes: Arc<dyn NotesEngine>,
    evolver: Arc<dyn EvolverEngine>,
    store: Option<Arc<SqliteSessionStore>>,
    config: Config,
    child_session_id: String,
    task: SubagentTaskSpec,
    sub_cancel: CancellationToken,
) -> (String, String, Result<String, String>) {
    let child_registry = Arc::new(DefaultToolRegistry::new());
    let read_cache = crate::tools::ReadCache::new();
    let sudo_cache = Arc::new(std::sync::Mutex::new(None));
    let reg_dyn: Arc<dyn ToolRegistry> = child_registry.clone();
    let todo_scope = crate::tools::TodoScope::new(
        config.paths.todos_dir.clone(),
        &child_session_id,
    );
    if let Err(e) = crate::tools::register_core_tools_with_sudo(
        &reg_dyn,
        &config,
        read_cache,
        Some(sudo_cache),
        None,
        todo_scope,
    )
    .await
    {
        warn!("Failed to register tools for subagent {}: {}", child_session_id, e);
        return (
            child_session_id,
            task.goal,
            Err(format!("failed to register child tools: {e}")),
        );
    }

    let mut child_cfg = config.agent.clone();
    child_cfg.max_consecutive_failures = 5;
    child_cfg.require_hitl_for_mutation = false;

    let provider = Arc::new(DynProvider(provider));
    let memory = Arc::new(DynMemory(memory));
    let notes = Arc::new(DynNotes(notes));
    let evolver = Arc::new(DynEvolver(evolver));

    let mut agent = Agent::new(
        provider,
        memory,
        notes,
        evolver,
        child_registry,
        child_cfg,
    )
    .with_permissions(
        crate::permission::PermissionEngine::validate(config.tools.permissions.clone())
            .unwrap_or_default(),
    )
    .with_approval(Arc::new(AllowAll), ApprovalSettings::default());

    if let Some(s) = store {
        let _ = s.ensure_session(&child_session_id).await;
        agent = agent.with_session(SessionBinding {
            store: s,
            session_id: child_session_id.clone(),
            resume_max_messages: 50,
        });
    }

    let prompt = match &task.context {
        Some(ctx) => format!("GOAL: {}\n\nCONTEXT:\n{}", task.goal, ctx),
        None => format!("GOAL: {}", task.goal),
    };

    let run_future = agent.run_turn(&prompt, sub_cancel);
    let outcome = match tokio::time::timeout(Duration::from_secs(180), run_future).await {
        Ok(Ok(outcome)) => Ok(outcome.reply),
        Ok(Err(e)) => Err(format!("agent turn error: {e}")),
        Err(_) => Err("subagent timed out after 180 seconds".to_string()),
    };

    (child_session_id, task.goal, outcome)
}

#[async_trait]
impl Tool for DelegateTaskTool {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn description(&self) -> &str {
        "Spawn isolated child subagents to execute tasks in parallel without polluting the main conversation. Subagents run asynchronously in the background so you can continue replying and working immediately."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Action to perform: 'spawn' (default), 'list', 'poll', or 'wait'.",
                    "enum": ["spawn", "list", "poll", "wait"]
                },
                "tasks": {
                    "type": "array",
                    "description": "List of subagent tasks to execute concurrently (max 4). Required for 'spawn'.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "goal": {
                                "type": "string",
                                "description": "Specific task objective for the subagent to accomplish."
                            },
                            "context": {
                                "type": "string",
                                "description": "Optional background info, file paths, constraints, or instructions."
                            }
                        },
                        "required": ["goal"]
                    }
                },
                "background": {
                    "type": "boolean",
                    "description": "Whether to execute subagents in the background so Luna can continue working and reply immediately (default: true). Set to false to block and wait synchronously."
                },
                "task_id": {
                    "type": "string",
                    "description": "Optional subagent task ID to query, poll, or wait for."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Optional timeout in seconds when using action: 'wait' (default: 180)."
                }
            }
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let action = arguments
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or_else(|| {
                if arguments.get("tasks").is_some() {
                    "spawn"
                } else {
                    "list"
                }
            });

        match action {
            "list" => {
                let state = self.tasks_state.read().await;
                if state.is_empty() {
                    return Ok(ToolResult::success(
                        call_id,
                        "No delegated subagent tasks in this session.",
                    ));
                }
                let mut list: Vec<&SubagentTaskRecord> = state.values().collect();
                list.sort_by_key(|r| r.started_at);
                let mut out = format!("### Delegated Subagent Tasks ({} total)\n\n", list.len());
                for item in list {
                    let (status_str, extra) = match &item.status {
                        SubagentStatus::Running => {
                            let elapsed = Utc::now()
                                .signed_duration_since(item.started_at)
                                .num_seconds()
                                .max(0);
                            ("Running", format!("(running for {}s)", elapsed))
                        }
                        SubagentStatus::Completed { finished_at, .. } => {
                            let dur = finished_at
                                .signed_duration_since(item.started_at)
                                .num_seconds()
                                .max(0);
                            ("Completed", format!("(completed in {}s)", dur))
                        }
                        SubagentStatus::Failed { error, finished_at } => {
                            let dur = finished_at
                                .signed_duration_since(item.started_at)
                                .num_seconds()
                                .max(0);
                            ("Failed", format!("(failed in {}s: {})", dur, error))
                        }
                    };
                    out.push_str(&format!(
                        "- **[{}]** Goal: {}\n  Status: {} {}\n",
                        item.id, item.goal, status_str, extra
                    ));
                }
                Ok(ToolResult::success(call_id, out.trim()))
            }
            "poll" => {
                let state = self.tasks_state.read().await;
                if let Some(target_id) = arguments.get("task_id").and_then(|t| t.as_str()) {
                    if let Some(task) = state.get(target_id) {
                        let msg = match &task.status {
                            SubagentStatus::Running => {
                                let elapsed = Utc::now()
                                    .signed_duration_since(task.started_at)
                                    .num_seconds()
                                    .max(0);
                                format!(
                                    "Subagent [{}] is still running (Goal: {}). Elapsed: {}s.",
                                    task.id, task.goal, elapsed
                                )
                            }
                            SubagentStatus::Completed { reply, finished_at } => {
                                let dur = finished_at
                                    .signed_duration_since(task.started_at)
                                    .num_seconds()
                                    .max(0);
                                format!(
                                    "Subagent [{}] completed in {}s (Goal: {}).\n\n### Output:\n{}",
                                    task.id, dur, task.goal, reply
                                )
                            }
                            SubagentStatus::Failed { error, finished_at } => {
                                let dur = finished_at
                                    .signed_duration_since(task.started_at)
                                    .num_seconds()
                                    .max(0);
                                format!(
                                    "Subagent [{}] failed in {}s (Goal: {}).\n\n### Error:\n{}",
                                    task.id, dur, task.goal, error
                                )
                            }
                        };
                        Ok(ToolResult::success(call_id, msg))
                    } else {
                        Ok(ToolResult::error(
                            call_id,
                            format!("No subagent task found with ID '{}'", target_id),
                        ))
                    }
                } else {
                    let finished: Vec<&SubagentTaskRecord> = state
                        .values()
                        .filter(|r| !matches!(r.status, SubagentStatus::Running))
                        .collect();
                    if finished.is_empty() {
                        return Ok(ToolResult::success(
                            call_id,
                            "No subagent tasks have completed yet.",
                        ));
                    }
                    let mut out = String::from("### Completed Subagent Results\n\n");
                    for task in finished {
                        match &task.status {
                            SubagentStatus::Completed { reply, .. } => {
                                out.push_str(&format!(
                                    "#### [{}] Goal: {}\n**Status:** Completed\n**Output:**\n{}\n\n",
                                    task.id,
                                    task.goal,
                                    reply.trim()
                                ));
                            }
                            SubagentStatus::Failed { error, .. } => {
                                out.push_str(&format!(
                                    "#### [{}] Goal: {}\n**Status:** Failed\n**Error:** {}\n\n",
                                    task.id, task.goal, error
                                ));
                            }
                            _ => {}
                        }
                    }
                    Ok(ToolResult::success(call_id, out.trim()))
                }
            }
            "wait" => {
                let target_id = arguments
                    .get("task_id")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string());
                let timeout_secs = arguments
                    .get("timeout_secs")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(180);
                let start_wait = std::time::Instant::now();
                let mut interval = tokio::time::interval(Duration::from_millis(150));

                loop {
                    if cancel_token.is_cancelled() {
                        return Err(HarnessError::Cancelled);
                    }
                    if start_wait.elapsed().as_secs() > timeout_secs {
                        return Ok(ToolResult::error(
                            call_id,
                            format!("Timed out waiting for subagents after {}s", timeout_secs),
                        ));
                    }

                    let all_done = {
                        let state = self.tasks_state.read().await;
                        if let Some(ref tid) = target_id {
                            match state.get(tid) {
                                Some(task) => !matches!(task.status, SubagentStatus::Running),
                                None => {
                                    return Ok(ToolResult::error(
                                        call_id,
                                        format!("No subagent task with ID '{}'", tid),
                                    ))
                                }
                            }
                        } else {
                            !state.is_empty()
                                && state
                                    .values()
                                    .all(|task| !matches!(task.status, SubagentStatus::Running))
                        }
                    };

                    if all_done {
                        break;
                    }
                    interval.tick().await;
                }

                let state = self.tasks_state.read().await;
                let mut out = String::from("### Subagent Task Results\n\n");
                let mut items: Vec<&SubagentTaskRecord> = if let Some(ref tid) = target_id {
                    state.get(tid).into_iter().collect()
                } else {
                    state.values().collect()
                };
                items.sort_by_key(|r| r.started_at);
                for task in items {
                    out.push_str(&format!("#### [{}] Goal: {}\n", task.id, task.goal));
                    match &task.status {
                        SubagentStatus::Completed { reply, .. } => {
                            out.push_str(&format!(
                                "**Status:** Completed\n**Output:**\n{}\n\n",
                                reply.trim()
                            ));
                        }
                        SubagentStatus::Failed { error, .. } => {
                            out.push_str(&format!(
                                "**Status:** Failed\n**Error:** {}\n\n",
                                error
                            ));
                        }
                        SubagentStatus::Running => {
                            out.push_str("**Status:** Running\n\n");
                        }
                    }
                }
                Ok(ToolResult::success(call_id, out.trim()))
            }
            "spawn" => {
                let tasks: Vec<SubagentTaskSpec> = match arguments.get("tasks") {
                    Some(t) => match serde_json::from_value(t.clone()) {
                        Ok(v) => v,
                        Err(e) => {
                            return Ok(ToolResult::error(
                                call_id,
                                format!("invalid tasks array: {e}"),
                            ));
                        }
                    },
                    None => {
                        return Ok(ToolResult::error(
                            call_id,
                            "missing required 'tasks' argument for spawn",
                        ));
                    }
                };

                if tasks.is_empty() {
                    return Ok(ToolResult::error(
                        call_id,
                        "'tasks' list must contain at least 1 task",
                    ));
                }

                let background = arguments
                    .get("background")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(true);

                let tasks_to_run: Vec<SubagentTaskSpec> =
                    tasks.into_iter().take(self.max_concurrent).collect();
                let total = tasks_to_run.len();
                info!(
                    "Spawning {} subagent task(s) via delegate_task (background={})",
                    total, background
                );

                if !background {
                    // Synchronous blocking mode
                    let mut join_set = JoinSet::new();
                    for (idx, task) in tasks_to_run.into_iter().enumerate() {
                        let provider = Arc::new(DynProvider(self.provider.clone()));
                        let memory = Arc::new(DynMemory(self.memory.clone()));
                        let notes = Arc::new(DynNotes(self.notes.clone()));
                        let evolver = Arc::new(DynEvolver(self.evolver.clone()));
                        let store = self.store.clone();
                        let config = self.config.clone();
                        let sub_cancel = cancel_token.child_token();
                        let sub_idx = idx + 1;
                        let child_session_id = format!(
                            "sub_{}_{}",
                            Utc::now().format("%Y%m%d_%H%M%S"),
                            sub_idx
                        );

                        join_set.spawn(async move {
                            let (id, goal, res) = run_single_subagent(
                                provider,
                                memory,
                                notes,
                                evolver,
                                store,
                                config,
                                child_session_id,
                                task,
                                sub_cancel,
                            )
                            .await;
                            (sub_idx, id, goal, res)
                        });
                    }

                    let mut results = Vec::new();
                    while let Some(res) = join_set.join_next().await {
                        match res {
                            Ok((idx, id, goal, outcome)) => {
                                let finished_at = Utc::now();
                                let status = match &outcome {
                                    Ok(reply) => SubagentStatus::Completed {
                                        reply: reply.clone(),
                                        finished_at,
                                    },
                                    Err(error) => SubagentStatus::Failed {
                                        error: error.clone(),
                                        finished_at,
                                    },
                                };
                                self.tasks_state.write().await.insert(
                                    id.clone(),
                                    SubagentTaskRecord {
                                        id,
                                        goal: goal.clone(),
                                        context: None,
                                        status,
                                        started_at: Utc::now(),
                                    },
                                );
                                results.push((idx, goal, outcome));
                            }
                            Err(e) => {
                                results.push((0, "unknown".to_string(), Err(format!("task panicked or failed: {e}"))));
                            }
                        }
                    }

                    results.sort_by_key(|(idx, _, _)| *idx);
                    let mut output = String::from("### Subagent Task Results\n\n");
                    for (idx, goal, res) in results {
                        output.push_str(&format!("#### [Subagent {}] Goal: {}\n", idx, goal));
                        match res {
                            Ok(reply) => {
                                output.push_str(&format!(
                                    "**Status:** Completed\n**Output:**\n{}\n\n",
                                    reply.trim()
                                ));
                            }
                            Err(err) => {
                                output.push_str(&format!(
                                    "**Status:** Failed\n**Error:** {}\n\n",
                                    err
                                ));
                            }
                        }
                    }
                    return Ok(ToolResult::success(call_id, output.trim()));
                }

                // Asynchronous background mode: spawn detached Tokio tasks and return immediately
                let mut dispatched = Vec::new();
                let now = Utc::now();

                for (idx, task) in tasks_to_run.into_iter().enumerate() {
                    let sub_idx = idx + 1;
                    let child_session_id = format!(
                        "sub_{}_{}",
                        now.format("%Y%m%d_%H%M%S"),
                        sub_idx
                    );

                    let record = SubagentTaskRecord {
                        id: child_session_id.clone(),
                        goal: task.goal.clone(),
                        context: task.context.clone(),
                        status: SubagentStatus::Running,
                        started_at: now,
                    };

                    self.tasks_state
                        .write()
                        .await
                        .insert(child_session_id.clone(), record);

                    dispatched.push((child_session_id.clone(), task.goal.clone()));

                    let provider = Arc::new(DynProvider(self.provider.clone()));
                    let memory = Arc::new(DynMemory(self.memory.clone()));
                    let notes = Arc::new(DynNotes(self.notes.clone()));
                    let evolver = Arc::new(DynEvolver(self.evolver.clone()));
                    let store = self.store.clone();
                    let config = self.config.clone();
                    let sub_cancel = cancel_token.child_token();
                    let tasks_state = self.tasks_state.clone();
                    let notifier = self.notifier.clone();
                    let steer_handle = self.steer_handle.clone();
                    let cid = child_session_id.clone();
                    let goal_str = task.goal.clone();
                    let ctx_opt = task.context.clone();

                    tokio::spawn(async move {
                        let (id, goal, res) = run_single_subagent(
                            provider,
                            memory,
                            notes,
                            evolver,
                            store,
                            config,
                            cid,
                            SubagentTaskSpec {
                                goal: goal_str,
                                context: ctx_opt.clone(),
                            },
                            sub_cancel,
                        )
                        .await;

                        let finished_at = Utc::now();
                        let status = match res {
                            Ok(reply) => SubagentStatus::Completed {
                                reply,
                                finished_at,
                            },
                            Err(error) => SubagentStatus::Failed {
                                error,
                                finished_at,
                            },
                        };

                        let finished_record = SubagentTaskRecord {
                            id: id.clone(),
                            goal: goal.clone(),
                            context: ctx_opt,
                            status: status.clone(),
                            started_at: now,
                        };

                        tasks_state
                            .write()
                            .await
                            .insert(id.clone(), finished_record.clone());

                        // Invoke notifier callback if registered
                        if let Some(ref cb) = *notifier.read().await {
                            cb(&finished_record);
                        }

                        // Send steer notice into agent loop
                        if let Some(ref steer) = *steer_handle.read().await {
                            let (status_label, output_text) = match &status {
                                SubagentStatus::Completed { reply, .. } => {
                                    ("completed", reply.as_str())
                                }
                                SubagentStatus::Failed { error, .. } => {
                                    ("failed", error.as_str())
                                }
                                _ => ("finished", ""),
                            };
                            let steer_notice = format!(
                                "[SUBAGENT NOTIFICATION]\nSubagent [{}] (Goal: {}) {}.\nOutput:\n{}\n[/SUBAGENT NOTIFICATION]",
                                id, goal, status_label, output_text
                            );
                            steer.lock().unwrap().push_back(steer_notice);
                        }
                    });
                }

                let mut out = format!(
                    "### Subagent Task(s) Dispatched in Background ({} tasks)\n\n",
                    dispatched.len()
                );
                for (id, goal) in &dispatched {
                    out.push_str(&format!("- **[{}]** Goal: {}\n  Status: running\n", id, goal));
                }
                out.push_str("\nSubagents are running concurrently in the background. Luna can now reply to the user or proceed with other tasks without waiting.\nWhen subagents complete, their results will be notified automatically into the conversation, or you can query them at any time with `delegate_task(action: \"list\")` or `delegate_task(action: \"poll\", task_id: \"...\")`.");

                Ok(ToolResult::success(call_id, out.trim()))
            }
            other => Ok(ToolResult::error(
                call_id,
                format!("unknown action '{}': use spawn, list, poll, or wait", other),
            )),
        }
    }
}
