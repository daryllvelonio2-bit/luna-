use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::error::HarnessError;
use crate::traits::*;
use crate::types::*;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub max_consecutive_failures: usize,
    pub require_hitl_for_mutation: bool,
    pub system_prompt_prefix: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_consecutive_failures: 5,
            require_hitl_for_mutation: true,
            system_prompt_prefix: None,
        }
    }
}

pub struct Agent<P, M, N, E, R>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    E: EvolverEngine + 'static,
    R: ToolRegistry + 'static,
{
    provider: Arc<P>,
    memory: Arc<M>,
    notes: Arc<N>,
    evolver: Arc<E>,
    registry: Arc<R>,
    config: AgentConfig,
}

impl<P, M, N, E, R> Agent<P, M, N, E, R>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    E: EvolverEngine + 'static,
    R: ToolRegistry + 'static,
{
    pub fn new(
        provider: Arc<P>,
        memory: Arc<M>,
        notes: Arc<N>,
        evolver: Arc<E>,
        registry: Arc<R>,
        config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            memory,
            notes,
            evolver,
            registry,
            config,
        }
    }

    pub fn memory(&self) -> &Arc<M> {
        &self.memory
    }

    pub fn notes(&self) -> &Arc<N> {
        &self.notes
    }

    pub fn evolver(&self) -> &Arc<E> {
        &self.evolver
    }

    pub fn registry(&self) -> &Arc<R> {
        &self.registry
    }

    pub async fn run_task(
        &self,
        user_input: &str,
        cancel_token: CancellationToken,
    ) -> Result<String, HarnessError> {
        let mut messages = Vec::new();

        // 1. Retrieve relevant memories (includes active facts & 1-5 word note pointers)
        let relevant_memories = self.memory.retrieve(user_input, 10).await?;
        let memory_prompt = self.memory.format_as_system_prompt(&relevant_memories);

        let system_text = format!(
            "You are Luna, an autonomous AI agent capable of self-improvement and stateful persistence.\n\
             {}\n\
             MEMORY PROTOCOL:\n\
             You possess native memory tools: `save_memory`, `update_memory`, `forget_memory`.\n\
             The decision of what is worth remembering or correcting is placed entirely on YOU.\n\
             When the user states durable preferences, facts, or habits, save them.\n\
             \n\
             NOTE PROTOCOL:\n\
             You possess note tools: `save_note`, `search_notes`, `read_note`, `update_note`.\n\
             For large texts, specs, or logs, store them in notes. When saved, a strict 1-5 word pointer will be automatically indexed in your memory.\n\
             \n\
             SELF-IMPROVEMENT PROTOCOL:\n\
             You possess harness tools: `inspect_harness_source`, `propose_harness_patch`, `apply_harness_update`.\n\
             If you encounter bugs, missing features, or friction in your own harness, you can inspect, patch, test, and evolve yourself.\n\
             \n\
             CURRENT ACTIVE MEMORIES:\n\
             {}\n",
            self.config.system_prompt_prefix.as_deref().unwrap_or(""),
            memory_prompt
        );

        messages.push(Message::system(system_text));
        messages.push(Message::user(user_input));

        let mut consecutive_failures = 0;
        let tools = self.registry.list_tools().await?;

        info!("Starting agent task loop for query: '{}'", user_input);

        // Unbounded task loop: runs as long as productive progress is made
        loop {
            if cancel_token.is_cancelled() {
                warn!("Task loop cancelled cooperatively");
                return Err(HarnessError::Cancelled);
            }

            debug!("Invoking model completion with {} messages and {} tools", messages.len(), tools.len());
            let response = self
                .provider
                .complete(&messages, &tools, &CompletionOptions::default())
                .await?;

            // If no tools requested, assistant concluded task
            if response.tool_calls.is_empty() {
                info!("Task successfully completed without further tool calls");
                return Ok(response.content.unwrap_or_default());
            }

            // Append assistant response with tool calls to conversation history
            messages.push(response.clone());

            // Execute each requested tool call
            for tool_call in response.tool_calls {
                if cancel_token.is_cancelled() {
                    return Err(HarnessError::Cancelled);
                }

                info!("Executing tool: '{}' (call id: {})", tool_call.name, tool_call.id);
                let execution_result = match self.registry.execute(&tool_call, &cancel_token).await {
                    Ok(res) => res,
                    Err(e) => {
                        error!("Tool execution error for '{}': {}", tool_call.name, e);
                        ToolResult::error(&tool_call.id, e.to_string())
                    }
                };

                // Track consecutive non-working calls (failure thrashing guardrail)
                if execution_result.is_error {
                    consecutive_failures += 1;
                    warn!(
                        "Tool '{}' failed (consecutive failures: {}/{})",
                        tool_call.name,
                        consecutive_failures,
                        self.config.max_consecutive_failures
                    );

                    if consecutive_failures >= self.config.max_consecutive_failures {
                        error!(
                            "Halt triggered: {} consecutive tool failures exceeded threshold",
                            consecutive_failures
                        );
                        return Err(HarnessError::ConsecutiveFailuresExceeded {
                            failures: consecutive_failures,
                            last_tool: tool_call.name.clone(),
                        });
                    }
                } else {
                    // Reset consecutive failures on successful execution
                    if consecutive_failures > 0 {
                        debug!("Resetting consecutive failures counter from {} to 0", consecutive_failures);
                    }
                    consecutive_failures = 0;
                }

                // If a note was saved successfully, ensure memory pointer bridge is updated
                if tool_call.name == "save_note" && !execution_result.is_error {
                    if let Ok(note) = serde_json::from_str::<Note>(&execution_result.content) {
                        debug!("Registering 1-5 word note pointer: '{}' -> '{}'", note.id, note.summary_pointer);
                        if let Err(e) = self.memory.add_note_pointer(&note.id, &note.summary_pointer).await {
                            warn!("Failed to add note pointer to memory: {}", e);
                        }
                    }
                }

                // Add tool result to conversation history
                messages.push(Message::tool(execution_result));
            }
        }
    }
}
