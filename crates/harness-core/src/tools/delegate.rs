use std::sync::Arc;
use std::time::Duration;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::json;
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

#[derive(Debug, Deserialize, Serialize)]
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
        }
    }
}

#[async_trait]
impl Tool for DelegateTaskTool {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn description(&self) -> &str {
        "Spawn isolated child subagents to execute tasks in parallel without polluting the main conversation. Use for multi-step research, parallel explorations, or isolated subtasks."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "List of subagent tasks to execute concurrently (max 4).",
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
                }
            },
            "required": ["tasks"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
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
                    "missing required 'tasks' argument",
                ));
            }
        };

        if tasks.is_empty() {
            return Ok(ToolResult::error(
                call_id,
                "'tasks' list must contain at least 1 task",
            ));
        }

        let tasks_to_run: Vec<SubagentTaskSpec> =
            tasks.into_iter().take(self.max_concurrent).collect();
        let total = tasks_to_run.len();
        info!(
            "Spawning {} subagent task(s) via delegate_task",
            total
        );

        let mut join_set = JoinSet::new();

        for (idx, task) in tasks_to_run.into_iter().enumerate() {
            let provider = Arc::new(DynProvider(self.provider.clone()));
            let memory = Arc::new(DynMemory(self.memory.clone()));
            let notes = Arc::new(DynNotes(self.notes.clone()));
            let evolver = Arc::new(DynEvolver(self.evolver.clone()));
            let store = self.store.clone();
            let config = self.config.clone();
            let sub_cancel = cancel_token.child_token();

            join_set.spawn(async move {
                let sub_idx = idx + 1;
                let child_session_id = format!(
                    "sub_{}_{}",
                    chrono::Utc::now().format("%Y%m%d_%H%M%S"),
                    sub_idx
                );

                // Build child tool registry with core tools (NO delegate_task to prevent recursion)
                let child_registry = Arc::new(DefaultToolRegistry::new());
                let read_cache = crate::tools::ReadCache::new();
                let sudo_cache = Arc::new(std::sync::Mutex::new(None));
                let reg_dyn: Arc<dyn ToolRegistry> = child_registry.clone();
                if let Err(e) = crate::tools::register_core_tools_with_sudo(
                    &reg_dyn,
                    &config,
                    read_cache,
                    Some(sudo_cache),
                    None,
                )
                .await
                {
                    warn!("Failed to register tools for subagent {}: {}", sub_idx, e);
                    return (
                        sub_idx,
                        task.goal,
                        Err(format!("failed to register child tools: {e}")),
                    );
                }

                let mut child_cfg = config.agent.clone();
                child_cfg.max_consecutive_failures = 5;
                child_cfg.require_hitl_for_mutation = false;

                let mut agent = Agent::new(
                    provider,
                    memory,
                    notes,
                    evolver,
                    child_registry,
                    child_cfg,
                )
                // A subagent inherits the parent's standing rules: a `deny` the user wrote is
                // not something delegation gets to walk around.
                .with_permissions(
                    crate::permission::PermissionEngine::validate(config.tools.permissions.clone())
                        .unwrap_or_default(),
                )
                .with_approval(Arc::new(AllowAll), ApprovalSettings::default());

                if let Some(s) = store {
                    let _ = s.ensure_session(&child_session_id).await;
                    agent = agent.with_session(SessionBinding {
                        store: s,
                        session_id: child_session_id,
                        resume_max_messages: 50,
                    });
                }

                let prompt = match &task.context {
                    Some(ctx) => format!("GOAL: {}\n\nCONTEXT:\n{}", task.goal, ctx),
                    None => format!("GOAL: {}", task.goal),
                };

                // Bounded execution: max 180 seconds per subagent
                let run_future = agent.run_turn(&prompt, sub_cancel);
                match tokio::time::timeout(Duration::from_secs(180), run_future).await {
                    Ok(Ok(outcome)) => (sub_idx, task.goal, Ok(outcome.reply)),
                    Ok(Err(e)) => (sub_idx, task.goal, Err(format!("agent turn error: {e}"))),
                    Err(_) => (sub_idx, task.goal, Err("subagent timed out after 180 seconds".to_string())),
                }
            });
        }

        let mut results = Vec::new();
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok((idx, goal, outcome)) => {
                    results.push((idx, goal, outcome));
                }
                Err(e) => {
                    results.push((0, "unknown".to_string(), Err(format!("task panicked or failed: {e}"))));
                }
            }
        }

        // Sort by subagent index
        results.sort_by_key(|(idx, _, _)| *idx);

        let mut output = String::from("### Subagent Task Results\n\n");
        for (idx, goal, res) in results {
            output.push_str(&format!("#### [Subagent {}] Goal: {}\n", idx, goal));
            match res {
                Ok(reply) => {
                    output.push_str(&format!("**Status:** Completed\n**Output:**\n{}\n\n", reply.trim()));
                }
                Err(err) => {
                    output.push_str(&format!("**Status:** Failed\n**Error:** {}\n\n", err));
                }
            }
        }

        Ok(ToolResult::success(call_id, output.trim()))
    }
}
