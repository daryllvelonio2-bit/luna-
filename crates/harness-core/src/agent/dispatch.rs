//! Tool dispatch: the pre/post hooks, the human gate, and how many calls may run at once.
//!
//! One model reply can ask for eight files at once, and the loop used to read them one after
//! another — eight sequential round trips for a list of independent reads. This module keeps the
//! order the model asked in (results, observers, the failure counter and the transcript all see
//! the call order) while running *consecutive read-only* calls together.
//!
//! What may run together is deliberately narrow. A call is batched only when all of this holds:
//!
//! * the tool is read-only for **this call** — `is_mutating_call` on the arguments the gate will
//!   actually see, because `todo_list` reads on `list` and writes on `add`;
//! * no `pre_tool_use` hook denied it (a denial is an ordinary result, recorded in place);
//! * the permission rules and the mutability default answer "run" without asking anyone
//!   ([`GateVerdict::Run`]), so batching can never skip a prompt or reorder one.
//!
//! Everything else — mutating calls, hook denials, anything a rule wants a human to confirm — goes
//! through the gate one call at a time, with any pending batch flushed first so a human is never
//! shown a call out of order.

use tracing::{debug, error, warn};

use super::gate::{GateOutcome, GateVerdict};
use super::{Agent, AgentEvent, TurnToolCall};
use crate::error::HarnessError;
use crate::hooks::{HookContext, HookEvent, HookVerdict};
use crate::session::estimate_tokens;
use crate::traits::*;
use crate::types::*;
use tokio_util::sync::CancellationToken;

/// How many read-only calls may be in flight at once.
///
/// Not a free-for-all: a read-only call is still work — `search_files` walks a tree, `read_file`
/// reads and decodes a file — and this box is a 7.6 GB / 8-core machine that other workers share
/// (which is why builds are pinned to `CARGO_BUILD_JOBS=2`). Four hides the latency of a model
/// asking for a handful of files without turning one turn into a fork bomb.
pub(super) const MAX_PARALLEL_TOOL_CALLS: usize = 4;

/// The turn-loop state a dispatched call has to touch, bundled so the serial and the batched path
/// cannot bookkeep differently.
pub(super) struct DispatchState<'a> {
    pub messages: &'a mut Vec<Message>,
    pub turn_user_idx: &'a mut usize,
    pub fold_watermark: &'a mut Option<i64>,
    pub system_text: &'a str,
    pub executed_tools: &'a mut Vec<TurnToolCall>,
    pub consecutive_failures: &'a mut usize,
    pub cancel_token: &'a CancellationToken,
}

impl<P, M, N, R> Agent<P, M, N, R>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    R: ToolRegistry + 'static,
{
    /// Run every tool call the model asked for, in the order it asked for them, with consecutive
    /// read-only calls in flight together.
    pub(super) async fn dispatch_tool_calls(
        &self,
        calls: Vec<ToolCall>,
        state: &mut DispatchState<'_>,
    ) -> Result<(), HarnessError> {
        let mut batch: Vec<(ToolCall, serde_json::Value)> = Vec::new();

        for tool_call in calls {
            if state.cancel_token.is_cancelled() {
                return Err(HarnessError::Cancelled);
            }

            debug!(
                "Dispatching tool call: '{}' (call id: {})",
                tool_call.name, tool_call.id
            );

            // Hooks run before the human gate and can only narrow the call: a denial replaces its
            // result, an argument rewrite is what the gate and the tool then see. A hook can never
            // approve what the gate would refuse, so the two cannot disagree.
            let (effective_arguments, hook_denial) = self.pre_tool_hook(&tool_call).await;
            let gated = ToolCall {
                arguments: effective_arguments.clone(),
                ..tool_call.clone()
            };

            let runnable_alone = hook_denial.is_some()
                || !self.call_is_read_only(&gated).await
                || !matches!(self.gate_verdict(&gated).await, GateVerdict::Run);

            if !runnable_alone {
                // The batch ran to its width: hand it over before taking another call, so the
                // concurrency bound is a bound on what is in flight, not on the run length.
                if batch.len() >= MAX_PARALLEL_TOOL_CALLS {
                    let full = std::mem::take(&mut batch);
                    self.run_read_only_batch(full, state).await?;
                }
                batch.push((gated, effective_arguments));
                continue;
            }

            // A call the gate may put to a human waits for the read-only calls before it: results
            // are appended in call order, and a prompt that arrived out of order would be a
            // different question about a different transcript.
            let pending = std::mem::take(&mut batch);
            self.run_read_only_batch(pending, state).await?;
            self.run_gated_call(&tool_call, gated, effective_arguments, hook_denial, state)
                .await?;
        }

        let pending = std::mem::take(&mut batch);
        self.run_read_only_batch(pending, state).await
    }

    /// Whether this exact call may run beside another one.
    ///
    /// Fail closed: a registry that cannot say is not permission to run concurrently. The
    /// check is on the *call*, not the tool, so a tool with a read action and a write action is
    /// only batched for the read one.
    async fn call_is_read_only(&self, call: &ToolCall) -> bool {
        match self.registry.is_mutating_call(call).await {
            Ok(mutating) => !mutating,
            Err(error) => {
                warn!(
                    "Cannot tell whether tool '{}' mutates ({}): running it alone",
                    call.name, error
                );
                false
            }
        }
    }

    /// Run `pre_tool_use` for one call: the arguments the gate and the tool will see, and the
    /// denial when a hook refused the call.
    async fn pre_tool_hook(&self, tool_call: &ToolCall) -> (serde_json::Value, Option<String>) {
        let mut effective_arguments = tool_call.arguments.clone();
        let mut hook_denial: Option<String> = None;
        if let Some(hooks) = self.hooks.as_ref() {
            let context = HookContext {
                session_id: self.session.as_ref().map(|b| b.session_id.as_str()),
                tool: Some(tool_call.name.as_str()),
                arguments: Some(&tool_call.arguments),
                ..HookContext::default()
            };
            let hook_outcome = hooks.run(HookEvent::PreToolUse, &context).await;
            for note in &hook_outcome.notes {
                debug!(tool = %tool_call.name, "pre_tool_use hook: {}", note);
            }
            match hook_outcome.verdict {
                HookVerdict::Deny(reason) => {
                    warn!(tool = %tool_call.name, "pre_tool_use hook denied the call: {}", reason);
                    hook_denial = Some(reason);
                }
                HookVerdict::ProceedWith(arguments) => {
                    debug!(tool = %tool_call.name, "pre_tool_use hook rewrote the arguments");
                    effective_arguments = arguments;
                }
                HookVerdict::Proceed => {}
            }
        }
        (effective_arguments, hook_denial)
    }

    /// Run calls whose gate verdict is already "run": no prompt is possible, so they may share the
    /// turn. The tool bodies run together; everything downstream of them stays in call order.
    async fn run_read_only_batch(
        &self,
        batch: Vec<(ToolCall, serde_json::Value)>,
        state: &mut DispatchState<'_>,
    ) -> Result<(), HarnessError> {
        if batch.is_empty() {
            return Ok(());
        }

        let cancel = state.cancel_token.clone();
        for (call, _) in &batch {
            if let Some(ref observer) = self.observer {
                observer(AgentEvent::ToolExecuting {
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
            }
        }

        let results: Vec<Result<ToolResult, HarnessError>> = if batch.len() == 1 {
            let (call, _) = &batch[0];
            vec![self.registry.execute(call, &cancel).await]
        } else {
            debug!(
                calls = batch.len(),
                "Running {} read-only tool calls together",
                batch.len()
            );
            let futures = batch.iter().map(|(call, _)| {
                let call = call.clone();
                let token = cancel.clone();
                async move { self.registry.execute(&call, &token).await }
            });
            tokio::select! {
                _ = cancel.cancelled() => return Err(HarnessError::Cancelled),
                results = futures_util::future::join_all(futures) => results,
            }
        };

        for ((tool_call, effective_arguments), outcome) in batch.into_iter().zip(results) {
            let execution_result = match outcome {
                Ok(res) => res,
                Err(error) => {
                    error!("Tool execution error for '{}': {}", tool_call.name, error);
                    ToolResult::error(&tool_call.id, error.to_string())
                }
            };
            self.finish_call(
                &tool_call,
                &effective_arguments,
                execution_result,
                false,
                state,
            )
            .await?;
        }
        Ok(())
    }

    /// One call through the human gate. This is the path a mutating call, a hook denial, or
    /// anything a permission rule wants confirmed takes — alone, in order.
    async fn run_gated_call(
        &self,
        tool_call: &ToolCall,
        gated: ToolCall,
        effective_arguments: serde_json::Value,
        hook_denial: Option<String>,
        state: &mut DispatchState<'_>,
    ) -> Result<(), HarnessError> {
        let gate_outcome = match hook_denial {
            Some(reason) => GateOutcome::Denied(format!("hook: {reason}")),
            None => self.gate_decision(&gated).await,
        };
        let is_denial = matches!(gate_outcome, GateOutcome::Denied(_));
        let outcome: Result<ToolResult, HarnessError> = match gate_outcome {
            GateOutcome::Denied(reason) => {
                warn!(tool = %tool_call.name, "approval denied: {}", reason);
                Ok(ToolResult::error(
                    &tool_call.id,
                    format!("approval denied: {reason}"),
                ))
            }
            GateOutcome::Run(edited) => {
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::ToolExecuting {
                        name: tool_call.name.clone(),
                        arguments: tool_call.arguments.clone(),
                    });
                }
                let call = ToolCall {
                    arguments: edited.unwrap_or_else(|| effective_arguments.clone()),
                    ..tool_call.clone()
                };
                self.registry.execute(&call, state.cancel_token).await
            }
        };

        let execution_result = match outcome {
            Ok(res) => res,
            Err(error) => {
                error!("Tool execution error for '{}': {}", tool_call.name, error);
                ToolResult::error(&tool_call.id, error.to_string())
            }
        };

        self.finish_call(
            tool_call,
            &effective_arguments,
            execution_result,
            is_denial,
            state,
        )
        .await
    }

    /// Everything that happens to a call once its result exists: the `post_tool_use` hook, the
    /// observer, the turn's record of it, the failure guardrail, the note-pointer bridge, and the
    /// transcript append. One body for the batched and the gated path, so both report the same
    /// turn and neither can quietly skip the guards.
    async fn finish_call(
        &self,
        tool_call: &ToolCall,
        effective_arguments: &serde_json::Value,
        execution_result: ToolResult,
        is_denial: bool,
        state: &mut DispatchState<'_>,
    ) -> Result<(), HarnessError> {
        // `post_tool_use` can replace what the model reads without changing what the tool did: a
        // redaction or truncation hook whose output was discarded would be a guardrail that
        // silently guards nothing, which is worse than no hook at all.
        let mut execution_result = execution_result;

        // Safety cap: Never allow an unbounded tool result (from any tool, MCP, or plugin)
        // to blow past model context limits.
        const MAX_TOOL_RESULT_CHARS: usize = 200_000;
        if execution_result.content.len() > MAX_TOOL_RESULT_CHARS {
            let cut = crate::tools::char_safe_cut(&execution_result.content, MAX_TOOL_RESULT_CHARS);
            execution_result.content = format!(
                "{}\n\n[Tool output truncated: exceeded {} characters to protect context window]",
                cut, MAX_TOOL_RESULT_CHARS
            );
        }
        if let Some(hooks) = self.hooks.as_ref() {
            let event = if execution_result.is_error {
                HookEvent::PostToolUseFailure
            } else {
                HookEvent::PostToolUse
            };
            let context = HookContext {
                session_id: self.session.as_ref().map(|b| b.session_id.as_str()),
                tool: Some(tool_call.name.as_str()),
                arguments: Some(effective_arguments),
                message: Some(execution_result.content.as_str()),
                reason: execution_result
                    .is_error
                    .then_some(execution_result.content.as_str()),
            };
            let hook_outcome = hooks.run(event, &context).await;
            for note in &hook_outcome.notes {
                debug!(tool = %tool_call.name, "{} hook: {}", event.as_str(), note);
            }
            if let HookVerdict::ProceedWith(replacement) = hook_outcome.verdict {
                match crate::hooks::replacement_text(&replacement) {
                    Some(text) => {
                        debug!(
                            tool = %tool_call.name,
                            "{} hook replaced the result the model sees ({} -> {} chars)",
                            event.as_str(),
                            execution_result.content.chars().count(),
                            text.chars().count()
                        );
                        execution_result.content = text;
                    }
                    None => warn!(
                        tool = %tool_call.name,
                        "{} hook returned a payload with no \"content\"; ignoring it",
                        event.as_str()
                    ),
                }
            }
        }

        if let Some(ref observer) = self.observer {
            observer(AgentEvent::ToolExecuted {
                name: tool_call.name.clone(),
                output: execution_result.content.clone(),
                is_error: execution_result.is_error,
            });
        }

        state.executed_tools.push(TurnToolCall {
            name: tool_call.name.clone(),
            arguments: tool_call.arguments.clone(),
            output: execution_result.content.clone(),
            is_error: execution_result.is_error,
            denied: is_denial,
        });

        // Track consecutive non-working calls (failure thrashing guardrail).
        // Human/policy denials are intentional choices, not malfunctioning tool thrashing.
        if execution_result.is_error && !is_denial {
            *state.consecutive_failures += 1;
            warn!(
                "Tool '{}' failed (consecutive failures: {}/{})",
                tool_call.name, state.consecutive_failures, self.config.max_consecutive_failures
            );

            if *state.consecutive_failures >= self.config.max_consecutive_failures {
                error!(
                    "Halt triggered: {} consecutive tool failures exceeded threshold",
                    state.consecutive_failures
                );
                return Err(HarnessError::ConsecutiveFailuresExceeded {
                    failures: *state.consecutive_failures,
                    last_tool: tool_call.name.clone(),
                });
            }
        } else {
            // Reset consecutive failures on successful execution
            if *state.consecutive_failures > 0 {
                debug!(
                    "Resetting consecutive failures counter from {} to 0",
                    state.consecutive_failures
                );
            }
            *state.consecutive_failures = 0;
        }

        // If a note was saved successfully, ensure memory pointer bridge is updated
        if tool_call.name == "save_note" && !execution_result.is_error {
            if let Ok(note) = serde_json::from_str::<Note>(&execution_result.content) {
                debug!(
                    "Registering 1-5 word note pointer: '{}' -> '{}'",
                    note.id, note.summary_pointer
                );
                if let Err(e) = self
                    .memory
                    .add_note_pointer(&note.id, &note.summary_pointer)
                    .await
                {
                    warn!("Failed to add note pointer to memory: {}", e);
                }
            }
        }

        // ...and if a note was deleted, drop its pointer so memory holds no dangling
        // reference to a document that no longer exists.
        if tool_call.name == "delete_note" && !execution_result.is_error {
            if let Ok(removed) =
                serde_json::from_str::<serde_json::Value>(&execution_result.content)
            {
                let deleted = removed.get("status").and_then(|s| s.as_str()) == Some("deleted");
                if deleted {
                    if let Some(note_id) = removed.get("note_id").and_then(|n| n.as_str()) {
                        debug!("Removing note pointer for deleted note '{}'", note_id);
                        if let Err(e) = self.memory.remove_note_pointer(note_id).await {
                            warn!("Failed to remove note pointer from memory: {}", e);
                        }
                    }
                }
            }
        }

        // Add tool result to conversation history
        let tool_message = Message::tool(execution_result);
        self.append(&tool_message, false).await?;
        state.messages.push(tool_message);

        self.try_rebind_completed_fold(
            state.messages,
            state.turn_user_idx,
            state.fold_watermark,
            state.system_text,
        );

        let current_tokens = estimate_tokens(state.messages).tokens;
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::ContextTokensUpdated {
                tokens_used: current_tokens,
            });
        }

        Ok(())
    }
}
