use std::sync::Arc;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::approval::{ApprovalDecision, ApprovalRequest, ApprovalSettings, Approver, DenyAll};
use crate::error::HarnessError;
use crate::session::{compact, estimate_tokens, CompactionOutcome, SessionStore, StoredMessage};
use crate::traits::*;
use crate::types::*;

/// Agent-loop tunables. Deserialized from the `agent:` section of `~/.luna/config.yaml`
/// (see `crate::config`), which is why the defaults below are the single source of truth for the
/// config file's `agent.*` defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub max_consecutive_failures: usize,
    pub require_hitl_for_mutation: bool,
    pub system_prompt_prefix: Option<String>,
    /// Estimated transcript size at which the oldest turns are summarised.
    pub compact_at_tokens: usize,
    /// Trailing user turns kept verbatim when compacting.
    pub keep_recent_turns: usize,
    /// Model that writes the summary; `None` means the session's model.
    pub compaction_model: Option<String>,
    /// Provider that serves the compaction call; `None` means the session's provider.
    pub compaction_provider: Option<String>,
    /// Model used by the background engineer agent; `None` means the session's model.
    pub engineer_model: Option<String>,
    /// Provider used by the background engineer agent; `None` means the session's provider.
    pub engineer_provider: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_consecutive_failures: 5,
            require_hitl_for_mutation: true,
            system_prompt_prefix: None,
            compact_at_tokens: 500_000,
            keep_recent_turns: 8,
            compaction_model: None,
            compaction_provider: None,
            engineer_model: None,
            engineer_provider: None,
        }
    }
}

/// The provider and model that write summaries. A separate provider is what makes a failing
/// compaction reachable (point `agent.compaction_provider` at a provider with no credentials).
pub struct CompactionSetup {
    pub provider: Arc<dyn ModelProvider>,
    pub model: String,
}

/// The session a run appends to and replays from.
pub struct SessionBinding {
    pub store: Arc<dyn SessionStore>,
    pub session_id: String,
    /// Messages replayed at most, from `session.resume_max_messages`.
    pub resume_max_messages: usize,
}

/// What a replay did, for the caller to report.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayNotice {
    pub session_id: String,
    pub replayed: usize,
    pub total: usize,
}

/// What this turn's compaction did, for the caller to report.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionNotice {
    pub session_id: String,
    /// User turns folded into the summary.
    pub turns: usize,
}

/// Tool execution info recorded during a turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
    pub output: String,
    pub is_error: bool,
}

/// Events emitted during agent execution loop for live observing.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// One increment of the model's reasoning, emitted as it arrives so the UI can show thinking
    /// while it is still being produced. Concatenating every delta yields the turn's reasoning.
    ReasoningDelta(String),
    ToolExecuting {
        name: String,
        arguments: serde_json::Value,
    },
    ToolExecuted {
        name: String,
        output: String,
        is_error: bool,
    },
}

pub type AgentObserver = Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// Everything a turn produced: the reply, plus what the console should say about persistence.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnOutcome {
    pub reply: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<TurnToolCall>,
    /// `Some` whenever the run is bound to a session.
    pub replayed: Option<ReplayNotice>,
    /// `Some` only on a turn that actually compacted.
    pub compaction: Option<CompactionNotice>,
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
    approver: Arc<dyn Approver>,
    approval: ApprovalSettings,
    compaction: Option<CompactionSetup>,
    session: Option<SessionBinding>,
    observer: Option<AgentObserver>,
    reasoning_effort: Option<ReasoningEffort>,
}

/// What the gate decided for one call.
enum GateOutcome {
    /// Run it — possibly with the arguments the human edited.
    Run(Option<serde_json::Value>),
    /// Do not run it. The string is the reason, which goes into the tool result and the log.
    Denied(String),
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
            // Fail closed: an agent that was never given an approver denies mutating calls rather
            // than running them ungated. The CLI injects the real one from `tools.approval`.
            approver: Arc::new(DenyAll),
            approval: ApprovalSettings::default(),
            compaction: None,
            session: None,
            observer: None,
            reasoning_effort: None,
        }
    }

    /// Update the active model provider dynamically during a session.
    pub fn set_provider(&mut self, provider: Arc<P>) {
        self.provider = provider;
    }

    /// Update the reasoning effort setting dynamically during a session.
    pub fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffort>) {
        self.reasoning_effort = effort;
    }

    /// Current reasoning effort setting.
    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.reasoning_effort
    }

    /// Attach an event observer for live thinking and tool-call rendering.
    pub fn with_observer(mut self, observer: Arc<dyn Fn(AgentEvent) + Send + Sync>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Who answers the approval prompt, and the gate's timeout / override list.
    pub fn with_approval(mut self, approver: Arc<dyn Approver>, approval: ApprovalSettings) -> Self {
        self.approver = approver;
        self.approval = approval;
        self
    }

    /// Persist and replay through `binding`. Without it the agent runs a single stateless turn,
    /// which is the previous behaviour.
    pub fn with_session(mut self, binding: SessionBinding) -> Self {
        self.session = Some(binding);
        self
    }

    /// Rebind this agent to a new or different session.
    pub fn set_session(&mut self, binding: SessionBinding) {
        self.session = Some(binding);
    }

    /// Swap the approver at runtime — the interactive chat's "approve all for this session"
    /// answer replaces the ask-every-time approver with an allow-all one.
    pub fn set_approval(&mut self, approver: Arc<dyn Approver>) {
        self.approver = approver;
    }

    /// Serve the summarising call through a provider other than the session's.
    pub fn with_compaction(mut self, setup: CompactionSetup) -> Self {
        self.compaction = Some(setup);
        self
    }

    /// Set or update the compaction setup at runtime.
    pub fn set_compaction(&mut self, setup: Option<CompactionSetup>) {
        self.compaction = setup;
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

    pub fn provider(&self) -> &Arc<P> {
        &self.provider
    }

    pub fn compaction(&self) -> Option<&CompactionSetup> {
        self.compaction.as_ref()
    }

    /// Summarise the current session's conversation history using the configured
    /// compaction model (or the session's active provider and `fallback_model`).
    pub async fn summarize_current_session(&self, fallback_model: &str) -> Result<String, HarnessError> {
        let Some(binding) = &self.session else {
            return Err(HarnessError::SessionError(
                "no session bound to agent".to_string(),
            ));
        };
        let replay = binding
            .store
            .replay(&binding.session_id, usize::MAX)
            .await?;
        if replay.history.is_empty() {
            return Err(HarnessError::SessionError(
                "no messages in current session to summarize".to_string(),
            ));
        }
        let (provider, model): (&dyn ModelProvider, &str) = if let Some(setup) = &self.compaction {
            (setup.provider.as_ref(), &setup.model)
        } else {
            (self.provider.as_ref(), fallback_model)
        };
        crate::session::summarize_session(provider, model, &replay.history).await
    }

    /// Get the estimated token count for the current session's conversation history.
    /// This loads the history from the session store and runs the compaction estimator.
    pub async fn estimate_session_tokens(&self) -> Result<u32, HarnessError> {
        let (history, _replay_notice) = self.load_history().await?;
        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.extend(history.iter().map(|stored| stored.message.clone()));
        let estimate = estimate_tokens(&messages);
        Ok(estimate.tokens)
    }

    /// Fold the oldest turns of the current session into a summary, ignoring
    /// `agent.compact_at_tokens` — this is the `/compact` command. Nothing to fold (the session
    /// holds no more turns than `agent.keep_recent_turns`) or no compaction setup is an error,
    /// because the user asked for the fold explicitly.
    pub async fn compact_current_session(&self) -> Result<CompactionNotice, HarnessError> {
        let (history, _) = self.load_history().await?;
        let system_text = self.system_prompt("").await?;
        let mut messages = Vec::new();
        let notice = self
            .compact_once(&history, &system_text, &mut messages, true)
            .await?
            .ok_or_else(|| {
                HarnessError::SessionError(
                    "nothing to compact: the session holds no more turns than agent.keep_recent_turns"
                        .to_string(),
                )
            })?;
        Ok(notice)
    }

    /// Whether this call must be put to the human: the flag is on *and* the tool mutates (or
    /// `tools.mutating_overrides` names it).
    async fn needs_approval(&self, name: &str) -> bool {
        if !self.config.require_hitl_for_mutation {
            return false;
        }
        if self.approval.mutating_overrides.iter().any(|tool| tool == name) {
            return true;
        }
        match self.registry.is_mutating(name).await {
            Ok(mutating) => mutating,
            Err(error) => {
                // Fail closed: a registry that cannot answer is not permission to write.
                warn!(
                    "Cannot tell whether tool '{}' mutates ({}): treating it as mutating",
                    name, error
                );
                true
            }
        }
    }

    /// Put a mutating call to the approver, inside `tools.approval_timeout_secs`. Every outcome
    /// other than Run is a denial, including "nobody answered".
    async fn gate_decision(&self, call: &ToolCall) -> GateOutcome {
        if !self.needs_approval(&call.name).await {
            return GateOutcome::Run(None);
        }

        let preview = self.registry.preview(call).await.unwrap_or_default();
        let request = ApprovalRequest {
            tool: call.name.clone(),
            call_id: call.id.clone(),
            target: preview.target,
            diff: preview.diff,
            arguments: call.arguments.clone(),
        };

        // Tool name and target only: never file contents, never the argument blob.
        debug!(
            tool = %request.tool,
            target = %request.target.as_deref().unwrap_or("-"),
            "mutating tool call needs approval"
        );

        let timeout = self.approval.timeout;
        match tokio::time::timeout(timeout, self.approver.approve(&request)).await {
            Ok(Ok(ApprovalDecision::Run)) => GateOutcome::Run(None),
            Ok(Ok(ApprovalDecision::RunWithArguments(arguments))) => {
                GateOutcome::Run(Some(arguments))
            }
            Ok(Ok(ApprovalDecision::Deny)) => GateOutcome::Denied("denied at the prompt".to_string()),
            Ok(Err(HarnessError::ApprovalRejected(reason))) => GateOutcome::Denied(reason),
            Ok(Err(error)) => GateOutcome::Denied(error.to_string()),
            Err(_elapsed) => GateOutcome::Denied(format!(
                "no answer within {}s",
                timeout.as_secs()
            )),
        }
    }

    /// The turn's reply only. Kept for callers with no console to report to (the voice pipeline);
    /// [`Agent::run_turn`] is the same run with the compaction and replay facts attached.
    pub async fn run_task(
        &self,
        user_input: &str,
        cancel_token: CancellationToken,
    ) -> Result<String, HarnessError> {
        self.run_turn(user_input, cancel_token)
            .await
            .map(|outcome| outcome.reply)
    }

    /// Run one turn: replay the session, bound the context, call the model until it stops asking
    /// for tools, and append everything to the session store as it happens.
    pub async fn run_turn(
        &self,
        user_input: &str,
        cancel_token: CancellationToken,
    ) -> Result<TurnOutcome, HarnessError> {
        let system_text = self.system_prompt(user_input).await?;

        // 1. Replay the session's recent transcript, bounded by `session.resume_max_messages`.
        let (history, replay_notice) = self.load_history().await?;
        let mut messages = Vec::with_capacity(history.len() + 3);
        messages.push(Message::system(system_text.clone()));
        messages.extend(history.iter().map(|stored| stored.message.clone()));
        messages.push(Message::user(user_input));

        // 2. Bound the context, once per turn: compaction either succeeds, finds nothing to fold,
        // or fails and leaves the transcript alone. No retry inside the turn.
        let compaction = self.compact_once(&history, &system_text, &mut messages, false).await?;

        let user_message = Message::user(user_input);
        let mut user_appended = false;

        let mut consecutive_failures = 0;
        let mut executed_tools = Vec::new();
        let mut last_reasoning = None;
        let tools = self.registry.list_tools().await?;

        debug!("Starting agent task loop for query: '{}'", user_input);

        // Unbounded task loop: runs as long as productive progress is made
        loop {
            if cancel_token.is_cancelled() {
                warn!("Task loop cancelled cooperatively");
                return Err(HarnessError::Cancelled);
            }

            debug!("Invoking model completion with {} messages and {} tools", messages.len(), tools.len());
            let mut options = CompletionOptions::default();
            options.reasoning_effort = self.reasoning_effort;

            // Retrying on transient / overloaded errors with backoff
            let max_retries = 3;
            let mut response = None;
            for attempt in 0..=max_retries {
                match self.stream_completion(&messages, &tools, &options, &cancel_token).await {
                    Ok(res) => {
                        response = Some(res);
                        break;
                    }
                    Err(e) if e.is_retryable() && attempt < max_retries => {
                        let delay_secs = 2u64.pow(attempt as u32);
                        warn!(
                            "Model request failed ({}), retrying attempt {}/{} in {}s...",
                            e, attempt + 1, max_retries, delay_secs
                        );
                        if let Some(ref observer) = self.observer {
                            observer(AgentEvent::ReasoningDelta(format!(
                                "\n[Model busy/overloaded: {}. Retrying in {}s (attempt {}/{})]…\n",
                                e, delay_secs, attempt + 1, max_retries
                            )));
                        }
                        tokio::select! {
                            _ = tokio::time::sleep(tokio::time::Duration::from_secs(delay_secs)) => {},
                            _ = cancel_token.cancelled() => return Err(HarnessError::Cancelled),
                        }
                    }
                    Err(e) => return Err(e),
                }
            }

            let response = response.ok_or_else(|| HarnessError::ProviderError {
                provider: self.provider.provider_id().to_string(),
                message: "Model request failed after retries".to_string(),
            })?;

            if let Some(ref r) = response.reasoning {
                last_reasoning = Some(r.clone());
            }

            // If no tools requested, assistant concluded task
            if response.tool_calls.is_empty() {
                debug!("Task successfully completed without further tool calls");
                let reply_text = response.content.as_deref().unwrap_or("").trim().to_string();
                if reply_text.is_empty() && last_reasoning.is_none() {
                    return Err(HarnessError::ProviderError {
                        provider: self.provider.provider_id().to_string(),
                        message: "Model returned an empty response. The model may be temporarily overloaded or unavailable.".to_string(),
                    });
                }
                let final_reply = if reply_text.is_empty() {
                    "(The model completed reasoning but produced no text output. Press Ctrl+O to inspect reasoning.)".to_string()
                } else {
                    reply_text
                };

                if !user_appended {
                    self.append(&user_message, false).await?;
                }

                let mut final_response = response.clone();
                final_response.content = Some(final_reply.clone());
                self.append(&final_response, false).await?;
                return Ok(TurnOutcome {
                    reply: final_reply,
                    reasoning: last_reasoning,
                    tool_calls: executed_tools,
                    replayed: replay_notice,
                    compaction,
                });
            }

            if !user_appended {
                self.append(&user_message, false).await?;
                user_appended = true;
            }

            // Append assistant response with tool calls to conversation history
            self.append(&response, false).await?;
            messages.push(response.clone());

            // Execute each requested tool call
            for tool_call in response.tool_calls {
                if cancel_token.is_cancelled() {
                    return Err(HarnessError::Cancelled);
                }

                debug!("Executing tool: '{}' (call id: {})", tool_call.name, tool_call.id);
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::ToolExecuting {
                        name: tool_call.name.clone(),
                        arguments: tool_call.arguments.clone(),
                    });
                }
                // The gate first: a mutating call is put to the human before it runs, and a
                // denial comes back as an ordinary tool result the model can react to.
                let outcome: Result<ToolResult, HarnessError> = match self.gate_decision(&tool_call).await {
                    GateOutcome::Denied(reason) => {
                        warn!(tool = %tool_call.name, "approval denied: {}", reason);
                        Ok(ToolResult::error(
                            &tool_call.id,
                            format!("approval denied: {reason}"),
                        ))
                    }
                    GateOutcome::Run(edited) => {
                        let call = ToolCall {
                            arguments: edited.unwrap_or_else(|| tool_call.arguments.clone()),
                            ..tool_call.clone()
                        };
                        self.registry.execute(&call, &cancel_token).await
                    }
                };
                let execution_result = match outcome {
                    Ok(res) => res,
                    Err(e) => {
                        error!("Tool execution error for '{}': {}", tool_call.name, e);
                        ToolResult::error(&tool_call.id, e.to_string())
                    }
                };

                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::ToolExecuted {
                        name: tool_call.name.clone(),
                        output: execution_result.content.clone(),
                        is_error: execution_result.is_error,
                    });
                }

                executed_tools.push(TurnToolCall {
                    name: tool_call.name.clone(),
                    arguments: tool_call.arguments.clone(),
                    output: execution_result.content.clone(),
                    is_error: execution_result.is_error,
                });

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

                // ...and if a note was deleted, drop its pointer so memory holds no dangling
                // reference to a document that no longer exists.
                if tool_call.name == "delete_note" && !execution_result.is_error {
                    if let Ok(removed) = serde_json::from_str::<serde_json::Value>(&execution_result.content) {
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
                messages.push(tool_message);
            }
        }
    }

    /// One model call, taken through the provider's streaming API so reasoning reaches the
    /// observer while the model is still producing it. The stream is assembled into the same
    /// `Message` the buffered call returns, so tool execution and persistence are unchanged.
    ///
    /// A provider that cannot stream (or fails before producing anything) falls back to the
    /// buffered call — the turn is worth more than the live view of it.
    async fn stream_completion(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        cancel_token: &CancellationToken,
    ) -> Result<Message, HarnessError> {
        let mut stream = tokio::select! {
            _ = cancel_token.cancelled() => return Err(HarnessError::Cancelled),
            res = self.provider.stream(messages, tools, options) => match res {
                Ok(stream) => stream,
                Err(error) => {
                    debug!("Provider stream unavailable ({}), falling back to a buffered call", error);
                    tokio::select! {
                        _ = cancel_token.cancelled() => return Err(HarnessError::Cancelled),
                        buffered = self.provider.complete(messages, tools, options) => return buffered,
                    }
                }
            }
        };

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut usage = None;
        // Tool-call fragments arrive keyed by the index the provider assigned them.
        let mut calls: Vec<(usize, ToolCall)> = Vec::new();

        loop {
            let event = tokio::select! {
                _ = cancel_token.cancelled() => return Err(HarnessError::Cancelled),
                ev = stream.next() => match ev {
                    Some(event) => event,
                    None => break,
                }
            };

            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    // Nothing arrived, so the transport never worked: the buffered path may still
                    // succeed, and a silent no-op turn would be the worse failure.
                    if content.is_empty() && reasoning.is_empty() && calls.is_empty() {
                        debug!("Stream failed before any output ({}), retrying buffered", error);
                        return self.provider.complete(messages, tools, options).await;
                    }
                    return Err(error);
                }
            };

            match event {
                StreamEvent::ReasoningDelta(text) => {
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::ReasoningDelta(text.clone()));
                    }
                    reasoning.push_str(&text);
                }
                StreamEvent::ContentDelta(text) => content.push_str(&text),
                StreamEvent::ToolCallDelta { index, id, name, arguments_delta } => {
                    let slot = match calls.iter_mut().find(|(i, _)| *i == index) {
                        Some((_, call)) => call,
                        None => {
                            calls.push((
                                index,
                                ToolCall {
                                    id: String::new(),
                                    name: String::new(),
                                    arguments: serde_json::Value::Null,
                                },
                            ));
                            &mut calls.last_mut().expect("just pushed").1
                        }
                    };
                    if let Some(id) = id {
                        slot.id = id;
                    }
                    if let Some(name) = name {
                        slot.name = name;
                    }
                    if let Some(fragment) = arguments_delta {
                        // Arguments stream as JSON text, so each fragment is stitched onto the raw
                        // string and parsed once at the end.
                        let mut raw = match slot.arguments.take() {
                            serde_json::Value::String(raw) => raw,
                            _ => String::new(),
                        };
                        raw.push_str(&fragment);
                        slot.arguments = serde_json::Value::String(raw);
                    }
                }
                StreamEvent::Completed { usage: event_usage, .. } => {
                    if event_usage.is_some() {
                        usage = event_usage;
                    }
                }
            }
        }

        calls.sort_by_key(|(index, _)| *index);
        let tool_calls: Vec<ToolCall> = calls
            .into_iter()
            .enumerate()
            .map(|(position, (_, mut call))| {
                call.arguments = match call.arguments {
                    serde_json::Value::String(raw) => serde_json::from_str(&raw).unwrap_or(json!({})),
                    value => value,
                };
                if call.id.is_empty() {
                    call.id = format!("call_{}", position);
                }
                call
            })
            .collect();

        if content.trim().is_empty() && tool_calls.is_empty() {
            debug!("Stream produced empty content and no tool calls, falling back to buffered completion");
            let buffered = self.provider.complete(messages, tools, options).await?;
            if buffered.content.as_deref().unwrap_or("").trim().is_empty() && buffered.tool_calls.is_empty() {
                if let Some(r) = buffered.reasoning.or(if reasoning.is_empty() { None } else { Some(reasoning) }) {
                    return Ok(Message {
                        role: Role::Assistant,
                        content: Some("(Completed reasoning with no text response)".to_string()),
                        reasoning: Some(r),
                        tool_calls: Vec::new(),
                        tool_result: None,
                        usage,
                    });
                }
                return Err(HarnessError::ProviderError {
                    provider: self.provider.provider_id().to_string(),
                    message: "Model returned an empty response. The model may be overloaded or unavailable.".to_string(),
                });
            }
            return Ok(buffered);
        }

        if reasoning.is_empty() {
            for (open_tag, close_tag) in &[("<think>", "</think>"), ("<thought>", "</thought>")] {
                if let Some(start) = content.find(open_tag) {
                    if let Some(end) = content.find(close_tag) {
                        let think_text = content[start + open_tag.len()..end].trim().to_string();
                        let rest = format!("{}{}", &content[..start], &content[end + close_tag.len()..]).trim().to_string();
                        if !think_text.is_empty() {
                            reasoning = think_text;
                        }
                        content = rest;
                        break;
                    }
                }
            }
        }

        let mut response = Message::assistant(content);
        response.reasoning = if reasoning.is_empty() { None } else { Some(reasoning) };
        response.tool_calls = tool_calls;
        response.usage = usage;
        Ok(response)
    }

    /// The system prompt: the `prompts.md` template with the configured prefix and the retrieved
    /// memories substituted in.
    ///
    /// Substitution is a literal `replace`, not `format!`, so the prompt text stays free of format
    /// syntax — a stray brace in the file can never abort a turn.
    async fn system_prompt(&self, user_input: &str) -> Result<String, HarnessError> {
        let relevant_memories = self.memory.retrieve(user_input, 10).await?;
        let memory_prompt = self.memory.format_as_system_prompt(&relevant_memories);

        Ok(crate::prompts::section(crate::prompts::SYSTEM_PROMPT)?
            .replace(
                "{{SYSTEM_PROMPT_PREFIX}}",
                self.config.system_prompt_prefix.as_deref().unwrap_or(""),
            )
            .replace("{{ACTIVE_MEMORIES}}", &memory_prompt))
    }

    /// The session's recent transcript, plus the notice the console prints about the bound.
    async fn load_history(&self) -> Result<(Vec<StoredMessage>, Option<ReplayNotice>), HarnessError> {
        let Some(binding) = &self.session else {
            return Ok((Vec::new(), None));
        };

        let replay = binding
            .store
            .replay(&binding.session_id, binding.resume_max_messages)
            .await?;
        let notice = ReplayNotice {
            session_id: binding.session_id.clone(),
            replayed: replay.replayed,
            total: replay.total,
        };
        debug!(
            "Replayed {} of {} messages of session '{}'",
            replay.replayed, replay.total, binding.session_id
        );
        Ok((replay.history, Some(notice)))
    }

    /// Summarise the oldest turns when the estimate crosses `agent.compact_at_tokens`, and swap the
    /// sent vector for the bounded one. A provider failure is non-fatal: the vector is untouched,
    /// a warning is logged, and the turn continues on the full history.
    async fn compact_once(
        &self,
        history: &[StoredMessage],
        system_text: &str,
        messages: &mut Vec<Message>,
        force: bool,
    ) -> Result<Option<CompactionNotice>, HarnessError> {
        let (Some(binding), Some(setup)) = (&self.session, &self.compaction) else {
            return Ok(None);
        };

        if !force {
            let estimate = estimate_tokens(messages);
            if estimate.tokens as usize <= self.config.compact_at_tokens {
                debug!(
                    "Transcript estimate {} tokens ({:?}) is within agent.compact_at_tokens = {}",
                    estimate.tokens, estimate.source, self.config.compact_at_tokens
                );
                return Ok(None);
            }

            debug!(
                "Transcript estimate {} tokens exceeds agent.compact_at_tokens = {}; compacting",
                estimate.tokens, self.config.compact_at_tokens
            );
        }

        let outcome: CompactionOutcome =
            match compact(setup.provider.as_ref(), &setup.model, history, self.config.keep_recent_turns)
                .await
            {
                Ok(Some(outcome)) => outcome,
                Ok(None) => {
                    debug!("Nothing to fold: the session holds no more turns than agent.keep_recent_turns");
                    return Ok(None);
                }
                Err(failure) => {
                    // Non-fatal by design: the transcript is unchanged, the turn runs on the full
                    // history, and this turn makes no second attempt.
                    debug!("Compaction failed, continuing on the full transcript: {}", failure);
                    return Ok(None);
                }
            };

        // The summary row is what a later resume replays, and the folded rows stay readable
        // (`compacted_at` set) for a future rewind. A store failure here is an error, not a
        // warning: silently running without the summary would make the next resume replay a
        // transcript this turn never sent.
        binding
            .store
            .record_compaction(&binding.session_id, &outcome.folded_ids, &outcome.summary)
            .await?;

        let user_line = messages.pop();
        *messages = outcome.bounded_messages(system_text.to_string());
        messages.extend(user_line);

        Ok(Some(CompactionNotice {
            session_id: binding.session_id.clone(),
            turns: outcome.folded_turns,
        }))
    }

    /// Append to the session store, if this run is bound to one. A failed write is an error: the
    /// session is the record of what was sent, and a turn that cannot be recorded must not be
    /// reported as a normal turn.
    async fn append(&self, message: &Message, is_summary: bool) -> Result<(), HarnessError> {
        let Some(binding) = &self.session else {
            return Ok(());
        };
        binding
            .store
            .append(&binding.session_id, message, is_summary)
            .await
            .map(|_| ())
    }
}
