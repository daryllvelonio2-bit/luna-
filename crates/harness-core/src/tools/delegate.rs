use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::agent::{Agent, AgentEvent, SessionBinding};
use crate::approval::{ApprovalSettings, NonInteractive};
use crate::config::Config;
use crate::error::HarnessError;
use crate::registry::DefaultToolRegistry;
use crate::session::{SessionStore, SqliteSessionStore};
use crate::traits::{MemoryEngine, ModelProvider, NotesEngine, Tool, ToolRegistry};
use crate::types::{
    CompletionOptions, MemoryRecord, Message, Note, StreamEvent, ToolDefinition, ToolResult,
};

/// A subagent fails only after this long without any model or tool activity.
///
/// This is deliberately an inactivity limit, not a task-duration limit: a long task that keeps
/// reporting progress must be allowed to finish.
const SUBAGENT_IDLE_TIMEOUT_SECS: u64 = 180;

/// How long an explicit `delegate_task(action: "wait")` waits before returning control to the
/// caller. The background task keeps running after this window.
const SUBAGENT_WAIT_TIMEOUT_SECS: u64 = 300;

/// How often the inactivity watchdog checks the last activity timestamp.
const SUBAGENT_IDLE_CHECK_INTERVAL_SECS: u64 = 1;

/// What a delegated child is doing right now.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubagentActivity {
    /// The model is producing a response.
    ModelStreaming,
    /// The child started a tool call.
    ToolStarted { name: String },
    /// The child's tool call finished.
    ToolFinished { name: String, is_error: bool },
}

/// A lifecycle or progress event from a delegated child.
#[derive(Debug, Clone)]
pub enum SubagentTaskEvent {
    Started {
        id: String,
        goal: String,
    },
    Progress {
        id: String,
        goal: String,
        activity: SubagentActivity,
    },
    Finished {
        record: SubagentTaskRecord,
    },
}

pub type SubagentTaskEventNotifier = Arc<dyn Fn(SubagentTaskEvent) + Send + Sync>;
type SubagentActivityNotifier = Arc<dyn Fn(SubagentActivity) + Send + Sync>;

#[derive(Clone)]
struct SubagentLogger {
    path: std::path::PathBuf,
}

impl SubagentLogger {
    fn new(path: std::path::PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self { path }
    }

    fn append_line(&self, line: &str) {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
        }
    }

    fn append(&self, text: &str) {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            use std::io::Write;
            let _ = write!(file, "{text}");
        }
    }
}

#[derive(Clone)]
struct DynProvider {
    inner: Arc<dyn ModelProvider>,
    activity: Option<SubagentActivityNotifier>,
    tracker: Option<SubagentActivityTracker>,
    logger: Option<SubagentLogger>,
}

#[async_trait]
impl ModelProvider for DynProvider {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }
    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        self.inner.fetch_models().await
    }
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        if let Some(ref activity) = self.activity {
            activity(SubagentActivity::ModelStreaming);
        }
        let res = self.inner.complete(messages, tools, options).await;
        if let Ok(ref msg) = res {
            if let Some(ref text) = msg.content {
                if let Some(ref l) = self.logger {
                    l.append_line(text);
                }
            }
        }
        res
    }
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        if let Some(ref activity) = self.activity {
            activity(SubagentActivity::ModelStreaming);
        }
        let stream = self.inner.stream(messages, tools, options).await?;
        let tracker = self.tracker.clone();
        let logger = self.logger.clone();
        Ok(stream
            .map(move |event| {
                if let Ok(event) = &event {
                    if let Some(ref t) = tracker {
                        t.touch();
                    }
                    match event {
                        StreamEvent::ContentDelta(delta) => {
                            if let Some(ref l) = logger {
                                l.append(delta);
                            }
                        }
                        _ => {}
                    }
                }
                event
            })
            .boxed())
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        self.inner.embed(texts).await
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
    async fn get_record(&self, id: &str) -> Result<Option<MemoryRecord>, HarnessError> {
        self.0.get_record(id).await
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubagentStatus {
    Running,
    Completed {
        reply: String,
        finished_at: DateTime<Utc>,
    },
    Failed {
        error: String,
        finished_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentTaskRecord {
    pub id: String,
    pub goal: String,
    pub context: Option<String>,
    pub status: SubagentStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub log_path: Option<String>,
}

pub type SubagentCompletionNotifier = Arc<dyn Fn(&SubagentTaskRecord) + Send + Sync>;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubagentTaskSpec {
    pub goal: String,
    pub context: Option<String>,
}

/// Monotonic milliseconds used by the inactivity watchdog.
fn monotonic_millis() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let elapsed = START.get_or_init(std::time::Instant::now).elapsed();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// Tracks whether a delegated child is still making progress.
#[derive(Clone)]
struct SubagentActivityTracker {
    id: String,
    goal: String,
    last_activity_ms: Arc<AtomicU64>,
    active_tools: Arc<std::sync::atomic::AtomicUsize>,
    notifier: Option<SubagentTaskEventNotifier>,
}

impl SubagentActivityTracker {
    fn new(id: String, goal: String, notifier: Option<SubagentTaskEventNotifier>) -> Self {
        Self {
            id,
            goal,
            last_activity_ms: Arc::new(AtomicU64::new(monotonic_millis())),
            active_tools: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            notifier,
        }
    }

    fn touch(&self) {
        self.last_activity_ms
            .store(monotonic_millis(), Ordering::Relaxed);
    }

    fn record(&self, activity: SubagentActivity) {
        match &activity {
            SubagentActivity::ToolStarted { .. } => {
                self.active_tools.fetch_add(1, Ordering::SeqCst);
            }
            SubagentActivity::ToolFinished { .. } => {
                let _ = self
                    .active_tools
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                        Some(n.saturating_sub(1))
                    });
            }
            _ => {}
        }
        self.touch();
        if let Some(notifier) = &self.notifier {
            notifier(SubagentTaskEvent::Progress {
                id: self.id.clone(),
                goal: self.goal.clone(),
                activity,
            });
        }
    }
}

fn notify_subagent_event(notifier: Option<&SubagentTaskEventNotifier>, event: SubagentTaskEvent) {
    if let Some(notifier) = notifier {
        notifier(event);
    }
}

fn current_event_notifier(
    slot: &Arc<RwLock<Option<SubagentTaskEventNotifier>>>,
) -> Option<SubagentTaskEventNotifier> {
    slot.try_read().ok().and_then(|guard| guard.clone())
}

/// Await a child turn until it finishes or stops making progress.
///
/// The old implementation put a wall-clock timeout around the whole turn, so a healthy child
/// that kept working was killed merely because it ran for too long. This watchdog measures the
/// gap between progress events instead: every streamed model response or tool event resets it.
async fn await_subagent_with_inactivity_timeout<F>(
    future: F,
    tracker: &SubagentActivityTracker,
    idle_timeout: Duration,
    cancel_token: &CancellationToken,
) -> Result<F::Output, String>
where
    F: Future,
{
    tokio::pin!(future);
    let check_interval = idle_timeout
        .min(Duration::from_secs(SUBAGENT_IDLE_CHECK_INTERVAL_SECS))
        .max(Duration::from_millis(10));
    let mut ticker = tokio::time::interval(check_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;

    loop {
        tokio::select! {
            output = &mut future => return Ok(output),
            _ = cancel_token.cancelled() => return Err("subagent cancelled".to_string()),
            _ = ticker.tick() => {
                if tracker.active_tools.load(Ordering::SeqCst) > 0 {
                    tracker.touch();
                    continue;
                }
                let idle_limit_ms = u64::try_from(idle_timeout.as_millis()).unwrap_or(u64::MAX);
                let idle_ms = monotonic_millis().saturating_sub(
                    tracker.last_activity_ms.load(Ordering::Relaxed),
                );
                if idle_ms >= idle_limit_ms {
                    cancel_token.cancel();
                    return Err(format!(
                        "subagent made no progress for {} seconds",
                        idle_timeout.as_secs()
                    ));
                }
            }
        }
    }
}

pub type SteerQueueHandle = Arc<RwLock<Option<Arc<Mutex<VecDeque<String>>>>>>;

pub struct DelegateTaskTool {
    provider: Arc<dyn ModelProvider>,
    memory: Arc<dyn MemoryEngine>,
    notes: Arc<dyn NotesEngine>,
    store: Option<Arc<SqliteSessionStore>>,
    config: Arc<RwLock<Config>>,
    max_concurrent: usize,
    tasks_state: Arc<RwLock<HashMap<String, SubagentTaskRecord>>>,
    notifier: Arc<RwLock<Option<SubagentCompletionNotifier>>>,
    event_notifier: Arc<RwLock<Option<SubagentTaskEventNotifier>>>,
    steer_handle: SteerQueueHandle,
    cancels: Arc<RwLock<HashMap<String, CancellationToken>>>,
}

impl DelegateTaskTool {
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        memory: Arc<dyn MemoryEngine>,
        notes: Arc<dyn NotesEngine>,
        store: Option<Arc<SqliteSessionStore>>,
        config: Config,
    ) -> Self {
        Self {
            provider,
            memory,
            notes,
            store,
            config: Arc::new(RwLock::new(config)),
            max_concurrent: 4,
            tasks_state: Arc::new(RwLock::new(HashMap::new())),
            notifier: Arc::new(RwLock::new(None)),
            event_notifier: Arc::new(RwLock::new(None)),
            steer_handle: Arc::new(RwLock::new(None)),
            cancels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn set_workspace_root(&self, root: std::path::PathBuf) {
        self.config.write().await.paths.workspace_root = root;
    }

    pub fn set_notifier(&self, notifier: SubagentCompletionNotifier) {
        if let Ok(mut lock) = self.notifier.try_write() {
            *lock = Some(notifier);
        }
    }

    /// Register a listener for child lifecycle and progress events.
    pub fn set_event_notifier(&self, notifier: SubagentTaskEventNotifier) {
        if let Ok(mut lock) = self.event_notifier.try_write() {
            *lock = Some(notifier);
        }
    }

    pub fn set_steer_handle(&self, steer_handle: Arc<Mutex<VecDeque<String>>>) {
        if let Ok(mut lock) = self.steer_handle.try_write() {
            *lock = Some(steer_handle);
        }
    }

    pub fn with_notifier(self, notifier: SubagentCompletionNotifier) -> Self {
        if let Ok(mut lock) = self.notifier.try_write() {
            *lock = Some(notifier);
        }
        self
    }

    pub fn with_steer_handle(self, steer_handle: Arc<Mutex<VecDeque<String>>>) -> Self {
        if let Ok(mut lock) = self.steer_handle.try_write() {
            *lock = Some(steer_handle);
        }
        self
    }

    pub async fn list_tasks(&self) -> Vec<SubagentTaskRecord> {
        self.tasks_state.read().await.values().cloned().collect()
    }

    pub async fn kill(&self, id: &str) -> bool {
        let mut tasks = self.tasks_state.write().await;
        if let Some(record) = tasks.get_mut(id) {
            if matches!(record.status, SubagentStatus::Running) {
                record.status = SubagentStatus::Failed {
                    error: "killed by user".to_string(),
                    finished_at: Utc::now(),
                };
                if let Some(cancel) = self.cancels.read().await.get(id) {
                    cancel.cancel();
                }
                return true;
            }
        }
        false
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_single_subagent(
    provider: Arc<dyn ModelProvider>,
    memory: Arc<dyn MemoryEngine>,
    notes: Arc<dyn NotesEngine>,
    store: Option<Arc<SqliteSessionStore>>,
    config: Config,
    child_session_id: String,
    task: SubagentTaskSpec,
    sub_cancel: CancellationToken,
    event_notifier: Option<SubagentTaskEventNotifier>,
) -> (String, String, Result<String, String>) {
    let child_registry = Arc::new(DefaultToolRegistry::new());
    let read_cache = crate::tools::ReadCache::new();
    let sudo_cache = Arc::new(std::sync::Mutex::new(None));
    let reg_dyn: Arc<dyn ToolRegistry> = child_registry.clone();
    let todo_scope =
        crate::tools::TodoScope::new(config.paths.todos_dir.clone(), &child_session_id);
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
        warn!(
            "Failed to register tools for subagent {}: {}",
            child_session_id, e
        );
        return (
            child_session_id,
            task.goal,
            Err(format!("failed to register child tools: {e}")),
        );
    }

    let mut child_cfg = config.agent.clone();
    child_cfg.max_consecutive_failures = 5;
    child_cfg.require_hitl_for_mutation = config.agent.require_hitl_for_mutation;

    let log_path = config.paths.jobs_dir.join(format!("{}.log", child_session_id));
    let logger = SubagentLogger::new(log_path);

    let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    logger.append_line(&format!("=== Subagent Started: {child_session_id} [{now_str}] ==="));
    logger.append_line(&format!("Goal: {}", task.goal));
    if let Some(ctx) = &task.context {
        logger.append_line(&format!("Context: {ctx}"));
    }
    logger.append_line("──────────────────────────────────────────────────");

    let tracker =
        SubagentActivityTracker::new(child_session_id.clone(), task.goal.clone(), event_notifier);
    let activity_tracker = tracker.clone();
    let provider = Arc::new(DynProvider {
        inner: provider,
        activity: Some(Arc::new(move |activity| activity_tracker.record(activity))),
        tracker: Some(tracker.clone()),
        logger: Some(logger.clone()),
    });
    let memory = Arc::new(DynMemory(memory));
    let notes = Arc::new(DynNotes(notes));

    let observer_tracker = tracker.clone();
    let log_obs = logger.clone();
    let mut agent = Agent::new(provider, memory, notes, child_registry, child_cfg)
        .with_permissions(
            crate::permission::PermissionEngine::validate(config.tools.permissions.clone())
                .unwrap_or_default(),
        )
        .with_approval(
            Arc::new(NonInteractive::new(
                "delegated subagent: interactive approval is unavailable in background execution",
            )),
            ApprovalSettings::default(),
        )
        .with_observer(Arc::new(move |event| match event {
            AgentEvent::ReasoningDelta(delta) => {
                log_obs.append(&delta);
                observer_tracker.touch();
            }
            AgentEvent::ToolExecuting { name, arguments } => {
                let args_str = serde_json::to_string(&arguments).unwrap_or_default();
                log_obs.append_line(&format!("\n▶ Tool Call: {name} {args_str}"));
                observer_tracker.record(SubagentActivity::ToolStarted { name });
            }
            AgentEvent::ToolExecuted { name, output, is_error } => {
                let status = if is_error { "FAILED" } else { "SUCCESS" };
                log_obs.append_line(&format!("✔ Tool Result [{name}] ({status}):\n{output}\n"));
                observer_tracker.record(SubagentActivity::ToolFinished { name, is_error });
            }
            _ => {
                observer_tracker.touch();
            }
        }));

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

    let run_future = agent.run_turn(&prompt, sub_cancel.clone());
    let outcome = match await_subagent_with_inactivity_timeout(
        run_future,
        &tracker,
        Duration::from_secs(SUBAGENT_IDLE_TIMEOUT_SECS),
        &sub_cancel,
    )
    .await
    {
        Ok(Ok(outcome)) => Ok(outcome.reply),
        Ok(Err(e)) => Err(format!("agent turn error: {e}")),
        Err(e) => Err(e),
    };

    let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    match &outcome {
        Ok(reply) => {
            logger.append_line(&format!("\n=== Subagent Completed [{now_str}] ==="));
            logger.append_line(&format!("Reply:\n{reply}"));
        }
        Err(e) => {
            logger.append_line(&format!("\n=== Subagent Failed [{now_str}] ==="));
            logger.append_line(&format!("Error: {e}"));
        }
    }

    (child_session_id, task.goal, outcome)
}

#[async_trait]
impl Tool for DelegateTaskTool {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn description(&self) -> &str {
        "Spawn isolated child subagents to execute tasks in parallel without polluting the main conversation. Subagents run asynchronously in the background so you can continue replying and working immediately. A child is stopped only after it makes no model or tool progress for the inactivity timeout; every response or tool event resets that timer."
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
                    "description": format!(
                        "Optional timeout in seconds when using action: 'wait' (default: {}). The delegated task keeps running after this wait expires.",
                        SUBAGENT_WAIT_TIMEOUT_SECS
                    )
                }
            }
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    fn is_mutating_call(&self, arguments: &serde_json::Value) -> bool {
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
        matches!(action, "spawn")
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
                    .unwrap_or(SUBAGENT_WAIT_TIMEOUT_SECS);
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
                            out.push_str(&format!("**Status:** Failed\n**Error:** {}\n\n", error));
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
                let event_notifier = current_event_notifier(&self.event_notifier);

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
                    let current_cfg = self.config.read().await.clone();
                    for (idx, task) in tasks_to_run.into_iter().enumerate() {
                        let provider = self.provider.clone();
                        let memory = self.memory.clone();
                        let notes = self.notes.clone();
                        let store = self.store.clone();
                        let config = current_cfg.clone();
                        let sub_cancel = cancel_token.child_token();
                        let sub_idx = idx + 1;
                        let started_at = Utc::now();
                        let child_session_id =
                            format!("sub_{}_{}", started_at.format("%Y%m%d_%H%M%S"), sub_idx);
                        let goal = task.goal.clone();
                        let context = task.context.clone();
                        let child_event_notifier = event_notifier.clone();

                        notify_subagent_event(
                            child_event_notifier.as_ref(),
                            SubagentTaskEvent::Started {
                                id: child_session_id.clone(),
                                goal: goal.clone(),
                            },
                        );

                        join_set.spawn(async move {
                            let (id, goal, res) = run_single_subagent(
                                provider,
                                memory,
                                notes,
                                store,
                                config,
                                child_session_id,
                                task,
                                sub_cancel,
                                child_event_notifier,
                            )
                            .await;
                            (sub_idx, id, goal, res, started_at, context)
                        });
                    }

                    let mut results = Vec::new();
                    while let Some(res) = join_set.join_next().await {
                        match res {
                            Ok((idx, id, goal, outcome, started_at, context)) => {
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
                                let log_path_str = current_cfg
                                    .paths
                                    .jobs_dir
                                    .join(format!("{}.log", id))
                                    .to_string_lossy()
                                    .to_string();
                                let record = SubagentTaskRecord {
                                    id: id.clone(),
                                    goal: goal.clone(),
                                    context,
                                    status,
                                    started_at,
                                    log_path: Some(log_path_str),
                                };
                                self.tasks_state
                                    .write()
                                    .await
                                    .insert(id.clone(), record.clone());
                                notify_subagent_event(
                                    event_notifier.as_ref(),
                                    SubagentTaskEvent::Finished { record },
                                );
                                results.push((idx, goal, outcome));
                            }
                            Err(e) => {
                                results.push((
                                    0,
                                    "unknown".to_string(),
                                    Err(format!("task panicked or failed: {e}")),
                                ));
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
                let current_cfg = self.config.read().await.clone();

                for (idx, task) in tasks_to_run.into_iter().enumerate() {
                    let sub_idx = idx + 1;
                    let child_session_id =
                        format!("sub_{}_{}", now.format("%Y%m%d_%H%M%S"), sub_idx);
                    let log_path_str = current_cfg
                        .paths
                        .jobs_dir
                        .join(format!("{}.log", child_session_id))
                        .to_string_lossy()
                        .to_string();

                    let record = SubagentTaskRecord {
                        id: child_session_id.clone(),
                        goal: task.goal.clone(),
                        context: task.context.clone(),
                        status: SubagentStatus::Running,
                        started_at: now,
                        log_path: Some(log_path_str.clone()),
                    };

                    self.tasks_state
                        .write()
                        .await
                        .insert(child_session_id.clone(), record);

                    notify_subagent_event(
                        event_notifier.as_ref(),
                        SubagentTaskEvent::Started {
                            id: child_session_id.clone(),
                            goal: task.goal.clone(),
                        },
                    );
                    dispatched.push((child_session_id.clone(), task.goal.clone()));

                    let provider = self.provider.clone();
                    let memory = self.memory.clone();
                    let notes = self.notes.clone();
                    let store = self.store.clone();
                    let config = current_cfg.clone();
                    let sub_cancel = CancellationToken::new();
                    self.cancels
                        .write()
                        .await
                        .insert(child_session_id.clone(), sub_cancel.clone());
                    let tasks_state = self.tasks_state.clone();
                    let cancels = self.cancels.clone();
                    let notifier = self.notifier.clone();
                    let steer_handle = self.steer_handle.clone();
                    let cid = child_session_id.clone();
                    let log_path_task = log_path_str;
                    let goal_str = task.goal.clone();
                    let ctx_opt = task.context.clone();
                    let child_event_notifier = event_notifier.clone();
                    let finish_event_notifier = child_event_notifier.clone();

                    tokio::spawn(async move {
                        let (id, goal, res) = run_single_subagent(
                            provider,
                            memory,
                            notes,
                            store,
                            config,
                            cid.clone(),
                            SubagentTaskSpec {
                                goal: goal_str,
                                context: ctx_opt.clone(),
                            },
                            sub_cancel,
                            child_event_notifier,
                        )
                        .await;

                        cancels.write().await.remove(&cid);

                        let finished_at = Utc::now();
                        let status = match res {
                            Ok(reply) => SubagentStatus::Completed { reply, finished_at },
                            Err(error) => SubagentStatus::Failed { error, finished_at },
                        };

                        let finished_record = SubagentTaskRecord {
                            id: id.clone(),
                            goal: goal.clone(),
                            context: ctx_opt,
                            status: status.clone(),
                            started_at: now,
                            log_path: Some(log_path_task),
                        };

                        tasks_state
                            .write()
                            .await
                            .insert(id.clone(), finished_record.clone());

                        notify_subagent_event(
                            finish_event_notifier.as_ref(),
                            SubagentTaskEvent::Finished {
                                record: finished_record.clone(),
                            },
                        );

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
                                SubagentStatus::Failed { error, .. } => ("failed", error.as_str()),
                                _ => ("finished", ""),
                            };
                            let steer_notice = format!(
                                "[SUBAGENT NOTIFICATION]\nSubagent [{}] (Goal: {}) {}.\nOutput:\n{}\n[/SUBAGENT NOTIFICATION]",
                                id, goal, status_label, output_text
                            );
                            if let Ok(mut steer_q) = steer.lock() {
                                steer_q.push_back(steer_notice);
                            }
                        }
                    });
                }

                let mut out = format!(
                    "### Subagent Task(s) Dispatched in Background ({} tasks)\n\n",
                    dispatched.len()
                );
                for (id, goal) in &dispatched {
                    out.push_str(&format!(
                        "- **[{}]** Goal: {}\n  Status: running\n",
                        id, goal
                    ));
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

#[cfg(test)]
mod tests;
