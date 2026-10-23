use std::path::PathBuf;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::approval::{ApprovalDecision, ApprovalRequest, ApprovalSettings, Approver, DenyAll};
use crate::checkpoint::CheckpointController;
use crate::error::HarnessError;
use crate::hooks::{HookContext, HookEvent, HookRunner, HookVerdict};
use crate::permission::{PermissionEngine, RuleVerdict};
use crate::saver::{transcript_tail, BackgroundSaver, SaverHook};
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
    /// When specified, completely replaces the default system prompt template (used for specialized subagents).
    pub system_prompt_override: Option<String>,
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
    /// Compact one-line summary index of available skills (e.g. "- name: description")
    #[serde(default)]
    pub skills_summary: Option<String>,
    /// Root directory of the workspace. Injected into system prompt so the model knows its working directory.
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
    /// Mode for memory and note persistence: Session writer vs Background saver.
    #[serde(default)]
    pub memory_mode: crate::config::MemoryMode,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_consecutive_failures: 5,
            require_hitl_for_mutation: true,
            system_prompt_prefix: None,
            system_prompt_override: None,
            compact_at_tokens: 500_000,
            keep_recent_turns: 8,
            compaction_model: None,
            compaction_provider: None,
            engineer_model: None,
            engineer_provider: None,
            skills_summary: None,
            workspace_root: None,
            memory_mode: crate::config::MemoryMode::Session,
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
    /// Messages folded into the summary. A fold that took no whole turn is still a fold.
    pub messages: usize,
}

/// Tool execution info recorded during a turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
    pub output: String,
    pub is_error: bool,
    /// Refused by the gate or a permission rule rather than attempted and failing. A caller that
    /// reports status (a headless run, CI) needs the difference: a denial means the work did not
    /// happen on purpose, not that the tool misbehaved.
    pub denied: bool,
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
    /// A steering message arrived and was injected mid-turn into conversation context.
    Steered {
        text: String,
    },
    /// A fold of the oldest turns is underway: the summary call is in flight and the transcript
    /// will be rewritten when it lands. Emitted for both the automatic pass at the top of a turn
    /// and an explicit `/compact`, which is why it carries `forced`.
    ///
    /// A UI needs this to animate: the fold can take seconds, and without a start event the only
    /// evidence of it is the summary that appears afterwards.
    CompactionStarted { forced: bool },
    /// The fold finished, whatever came of it: `folded` is the summary that replaced the oldest
    /// turns, `error` is why a fold the user asked for did not happen. Both `None` means there was
    /// nothing to fold (or an automatic fold failed and the turn continued on the full transcript).
    CompactionFinished {
        folded: Option<CompactionNotice>,
        error: Option<String>,
    },
    ContextTokensUpdated {
        tokens_used: u32,
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
    /// Provider-reported tokens summed over every model call in this turn, when the provider
    /// reported any. `None` is "unknown", never "zero".
    pub usage: Option<TokenUsage>,
    /// Wall-clock time the turn took, including tool execution and retries.
    pub duration_ms: u64,
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
    permissions: PermissionEngine,
    approval: ApprovalSettings,
    compaction: Option<CompactionSetup>,
    session: Option<SessionBinding>,
    hooks: Option<Arc<HookRunner>>,
    checkpoints: Option<Arc<CheckpointController>>,
    observer: Option<AgentObserver>,
    reasoning_effort: Option<ReasoningEffort>,
    steer_queue: Arc<Mutex<VecDeque<String>>>,
    /// The independent memory writer, when `memory.mode` is `background`.
    saver: Option<SaverHook>,
    /// Completed turns in this agent's lifetime; the saver's interval counts turns, not wall time.
    turns_completed: AtomicUsize,
}

/// Closes the turn's checkpoint on every exit path, including `?` and panics.
struct CheckpointGuard<'a> {
    controller: Option<&'a CheckpointController>,
}

impl Drop for CheckpointGuard<'_> {
    fn drop(&mut self) {
        if let Some(controller) = self.controller {
            controller.finish();
        }
    }
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
            permissions: PermissionEngine::default(),
            approval: ApprovalSettings::default(),
            compaction: None,
            session: None,
            hooks: None,
            checkpoints: None,
            observer: None,
            reasoning_effort: None,
            steer_queue: Arc::new(Mutex::new(VecDeque::new())),
            saver: None,
            turns_completed: AtomicUsize::new(0),
        }
    }

    /// Attach an independent memory saver (see [`crate::saver`]).
    ///
    /// With one attached, a session configured as `memory.mode: background` still writes memory
    /// through the same tools — just from a separate pass over the finished turn instead of from
    /// the turn's own tool calls.
    pub fn with_background_saver(mut self, saver: Arc<dyn BackgroundSaver>) -> Self {
        self.saver = Some(SaverHook::from_saver(saver));
        self
    }

    /// Replace (or clear) the memory saver at runtime.
    pub fn set_background_saver(&mut self, saver: Option<Arc<dyn BackgroundSaver>>) {
        if saver.is_some() {
            self.config.memory_mode = crate::config::MemoryMode::Background;
        } else {
            self.config.memory_mode = crate::config::MemoryMode::Session;
        }
        self.saver = saver.map(SaverHook::from_saver);
    }

    /// Update the memory mode at runtime.
    pub fn set_memory_mode(&mut self, mode: crate::config::MemoryMode) {
        self.config.memory_mode = mode;
    }

    /// The attached saver, if any (used by the CLI to report the mode).
    pub fn background_saver(&self) -> Option<&SaverHook> {
        self.saver.as_ref()
    }

    /// Build the full system prompt for a turn, with memories, skills, and memory mode instructions substituted.
    pub async fn build_system_prompt(&self, user_input: &str) -> Result<String, HarnessError> {
        self.system_prompt(user_input).await
    }

    /// Hand the finished turn to the saver when its interval is due.
    ///
    /// Detached on purpose: the reply is already written by now, so a second model call must not
    /// hold the terminal. A failure is logged, never returned — a saver that cannot reach its
    /// provider must not turn a completed turn into a failed one.
    fn spawn_background_saver(&self, messages: &[Message]) {
        let Some(hook) = self.saver.as_ref() else {
            return;
        };
        let completed = self.turns_completed.fetch_add(1, Ordering::Relaxed) + 1;
        if !hook.due(completed) {
            return;
        }
        let tail = transcript_tail(messages, hook.saver.tail_limit());
        if tail.is_empty() {
            return;
        }
        let saver = hook.saver.clone();
        debug!(
            "dispatching background memory saver after turn {} ({} messages, model {})",
            completed,
            tail.len(),
            saver.model()
        );
        tokio::spawn(async move {
            match saver.distil(&tail).await {
                Ok(report) => info!("background memory saver: {}", report.summary()),
                Err(error) => warn!("background memory saver failed: {}", error),
            }
        });
    }

    /// Queue guidance to steer the active turn mid-run without cancelling or restarting.
    pub fn steer(&self, text: &str) -> bool {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return false;
        }
        self.steer_queue
            .lock()
            .unwrap()
            .push_back(trimmed.to_string());
        true
    }

    /// Obtain a clone of the steering queue handle for mid-run submission without locking the outer Agent.
    pub fn steer_handle(&self) -> Arc<Mutex<VecDeque<String>>> {
        self.steer_queue.clone()
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

    /// Replace the observer after construction (the headless command picks its renderer once it
    /// knows whether it is writing JSON or plain text).
    pub fn set_observer(&mut self, observer: AgentObserver) {
        self.observer = Some(observer);
    }

    /// Who answers the approval prompt, and the gate's timeout / override list.
    pub fn with_approval(
        mut self,
        approver: Arc<dyn Approver>,
        approval: ApprovalSettings,
    ) -> Self {
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

    /// Install the standing permission rules evaluated before the human gate.
    pub fn with_permissions(mut self, permissions: PermissionEngine) -> Self {
        self.permissions = permissions;
        self
    }

    /// Replace the rules at runtime (config reload, `/permissions`).
    pub fn set_permissions(&mut self, permissions: PermissionEngine) {
        self.permissions = permissions;
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

    /// Attach lifecycle hooks (see [`crate::hooks`]). Without this the agent emits no events.
    pub fn with_hooks(mut self, hooks: Arc<HookRunner>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Replace (or clear) the hook runner at runtime.
    pub fn set_hooks(&mut self, hooks: Option<Arc<HookRunner>>) {
        self.hooks = hooks;
    }

    /// Attach per-turn file checkpoints (see [`crate::checkpoint`]). The registry middleware that
    /// fills them is registered separately, because it has to run inside tool execution.
    pub fn with_checkpoints(mut self, controller: Arc<CheckpointController>) -> Self {
        self.checkpoints = Some(controller);
        self
    }

    pub fn set_checkpoints(&mut self, controller: Option<Arc<CheckpointController>>) {
        self.checkpoints = controller;
    }

    /// The checkpoint controller this agent writes into, if any (used by `/rewind`).
    pub fn checkpoints(&self) -> Option<Arc<CheckpointController>> {
        self.checkpoints.clone()
    }

    /// Fire an event that has no tool attached (`session_start`, `stop`, …).
    ///
    /// A denial is surfaced as [`HarnessError::ApprovalRejected`] so no caller can mistake a
    /// refused event for a completed one.
    pub async fn emit_hook_event(
        &self,
        event: HookEvent,
        message: Option<&str>,
    ) -> Result<(), HarnessError> {
        let Some(hooks) = self.hooks.as_ref() else {
            return Ok(());
        };
        let session_id = self
            .session
            .as_ref()
            .map(|binding| binding.session_id.as_str());
        let context = HookContext {
            session_id,
            message,
            ..HookContext::default()
        };
        let outcome = hooks.run(event, &context).await;
        for note in &outcome.notes {
            debug!("{} hook: {}", event.as_str(), note);
        }
        match outcome.denial() {
            // A refusal from an event that was only watching is reported, not obeyed: a
            // `session_end` hook exiting 2 must not turn a finished turn into a failed one.
            Some(reason) if event.refusal_blocks_turn() => Err(HarnessError::ApprovalRejected(
                format!("{} hook refused: {reason}", event.as_str()),
            )),
            Some(reason) => {
                warn!(
                    "{} hook refused, which this event cannot do: {}",
                    event.as_str(),
                    reason
                );
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Apply an editable SurfaceSpec (Harness² c1–c8 components) to the agent loop.
    pub fn with_surface(
        mut self,
        surface: crate::surface::SurfaceSpec,
    ) -> Result<Self, HarnessError> {
        surface.validate()?;
        self.config.max_consecutive_failures = surface.c2_guardrails.max_consecutive_failures;
        self.config.require_hitl_for_mutation = surface.c2_guardrails.require_hitl_for_mutation;

        let rendered = surface.render_system_prompt();
        if !rendered.is_empty() {
            self.config.system_prompt_prefix = Some(rendered);
        }

        if !surface.c4_skills.is_empty() {
            let summaries: Vec<String> = surface
                .c4_skills
                .iter()
                .map(|s| format!("- {}: {}", s.name, s.description))
                .collect();
            self.config.skills_summary = Some(summaries.join("\n"));
        }

        Ok(self)
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
    pub async fn summarize_current_session(
        &self,
        fallback_model: &str,
    ) -> Result<String, HarnessError> {
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

        if history.is_empty() {
            return Ok(0);
        }

        // 1. If any prior turn recorded actual usage from the provider, use the most recent report
        if let Some(latest_usage) = history
            .iter()
            .rev()
            .find_map(|stored| stored.message.usage.as_ref())
        {
            return Ok(latest_usage
                .prompt_tokens
                .saturating_add(latest_usage.completion_tokens));
        }

        // 2. Otherwise estimate accurately from session messages
        let messages: Vec<Message> = history
            .iter()
            .map(|stored| stored.message.clone())
            .collect();
        let estimate = estimate_tokens(&messages);
        Ok(estimate.tokens)
    }

    /// Fold the oldest turns of the current session into a summary, ignoring
    /// `agent.compact_at_tokens` — this is the `/compact` command. Nothing to fold (the session
    /// holds no more turns than `agent.keep_recent_turns`), no compaction setup, or a failed
    /// summary is an error, because the user asked for the fold explicitly.
    ///
    /// Every outcome is also announced as a [`AgentEvent::CompactionFinished`] with `forced: true`,
    /// including the failures raised here before `compact_once` would have run — a UI that reports
    /// folds from the events alone must never miss the reason a fold it asked for did not happen.
    pub async fn compact_current_session(&self) -> Result<CompactionNotice, HarnessError> {
        if self.compaction.is_none() {
            let error = HarnessError::SessionError(
                "compaction is unavailable: no compaction provider is configured — set agent.compaction_provider and agent.compaction_model, or pick one with /compact-model"
                    .to_string(),
            );
            self.announce_compaction_failure(&error);
            return Err(error);
        }
        let (history, _) = self.load_history().await?;
        let system_text = self.system_prompt("").await?;
        let mut messages = Vec::new();
        let notice = match self
            .compact_once(&history, &system_text, &mut messages, true)
            .await?
        {
            Some(notice) => notice,
            None => {
                let error = HarnessError::SessionError(
                    "nothing to compact: the transcript holds no more turns than agent.keep_recent_turns and fits the compaction budget"
                        .to_string(),
                );
                self.announce_compaction_failure(&error);
                return Err(error);
            }
        };
        Ok(notice)
    }

    /// Tell the observer a fold the user asked for will not happen, and why.
    fn announce_compaction_failure(&self, error: &HarnessError) {
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::CompactionFinished {
                folded: None,
                error: Some(error.to_string()),
            });
        }
    }

    /// Whether this call must be put to the human: the flag is on *and* the tool mutates (or
    /// `tools.mutating_overrides` names it).
    async fn needs_approval(&self, call: &ToolCall) -> bool {
        if !self.config.require_hitl_for_mutation {
            return false;
        }
        if self
            .approval
            .mutating_overrides
            .iter()
            .any(|tool| tool == &call.name)
        {
            return true;
        }
        match self.registry.is_mutating_call(call).await {
            Ok(mutating) => mutating,
            Err(error) => {
                // Fail closed: a registry that cannot answer is not permission to write.
                warn!(
                    "Cannot tell whether tool '{}' mutates ({}): treating it as mutating",
                    call.name, error
                );
                true
            }
        }
    }

    /// Decide what happens to a call, in one place: standing rules first, then the human gate.
    ///
    /// A rule can refuse (`deny`), force a prompt (`ask`), or skip the prompt (`allow`). Only a
    /// call no rule matched is put through the tool-mutability default — which is why the rules
    /// can quiet the forty `cargo test` prompts without being able to approve a `write_file` the
    /// gate would have asked about.
    async fn gate_decision(&self, call: &ToolCall) -> GateOutcome {
        match self.permissions.evaluate(call) {
            RuleVerdict::Deny(reason) => {
                debug!(tool = %call.name, "denied by permission rule: {}", reason);
                return GateOutcome::Denied(reason);
            }
            RuleVerdict::Allow => {
                debug!(tool = %call.name, "allowed by permission rule");
                return GateOutcome::Run(None);
            }
            RuleVerdict::Ask => {}
            RuleVerdict::NoOpinion => {
                if !self.needs_approval(call).await {
                    return GateOutcome::Run(None);
                }
            }
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
            Ok(Ok(ApprovalDecision::Deny)) => {
                GateOutcome::Denied("denied at the prompt".to_string())
            }
            Ok(Err(HarnessError::ApprovalRejected(reason))) => GateOutcome::Denied(reason),
            Ok(Err(error)) => GateOutcome::Denied(error.to_string()),
            Err(_elapsed) => {
                GateOutcome::Denied(format!("no answer within {}s", timeout.as_secs()))
            }
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

        // 1.5 Hooks see the prompt before the model does; a refusal ends the turn unspent.
        self.emit_hook_event(HookEvent::UserPromptSubmit, Some(user_input))
            .await?;

        // 2. Bound the context, once per turn: compaction either succeeds, finds nothing to fold,
        // or fails and leaves the transcript alone. No retry inside the turn.
        let compaction = self
            .compact_once(&history, &system_text, &mut messages, false)
            .await?;

        // 2.5 Open this turn's file snapshot. Every return path closes it, including the error
        // ones, so a later turn cannot silently capture into the wrong checkpoint.
        if let Some(controller) = self.checkpoints.as_ref() {
            controller.begin(
                self.session
                    .as_ref()
                    .map(|binding| binding.session_id.as_str())
                    .unwrap_or("unbound"),
                Some(user_input),
            );
        }
        let _checkpoint_guard = CheckpointGuard {
            controller: self.checkpoints.as_deref(),
        };

        let user_message = Message::user(user_input);
        let mut user_appended = false;

        let mut consecutive_failures = 0;
        let mut executed_tools = Vec::new();
        let mut last_reasoning = None;
        // A turn can be several model calls (tool loop, retries); the ledger wants the turn's
        // total, so every reported usage is folded in here rather than kept per call.
        let turn_started = std::time::Instant::now();
        let mut turn_usage: Option<TokenUsage> = None;
        let tools = self.registry.list_tools().await?;

        debug!("Starting agent task loop for query: '{}'", user_input);

        // Unbounded task loop: runs as long as productive progress is made
        loop {
            if cancel_token.is_cancelled() {
                warn!("Task loop cancelled cooperatively");
                return Err(HarnessError::Cancelled);
            }

            let current_tokens = estimate_tokens(&messages).tokens;
            if let Some(ref observer) = self.observer {
                observer(AgentEvent::ContextTokensUpdated {
                    tokens_used: current_tokens,
                });
            }

            debug!(
                "Invoking model completion with {} messages and {} tools",
                messages.len(),
                tools.len()
            );
            let mut options = CompletionOptions::default();
            options.reasoning_effort = self.reasoning_effort;

            // Retrying on transient / overloaded errors with backoff
            let max_retries = 3;
            let mut response = None;
            for attempt in 0..=max_retries {
                match self
                    .stream_completion(&messages, &tools, &options, &cancel_token)
                    .await
                {
                    Ok(res) => {
                        response = Some(res);
                        break;
                    }
                    Err(e) if e.is_retryable() && attempt < max_retries => {
                        let delay_secs = 2u64.pow(attempt as u32);
                        warn!(
                            "Model request failed ({}), retrying attempt {}/{} in {}s...",
                            e,
                            attempt + 1,
                            max_retries,
                            delay_secs
                        );
                        if let Some(ref observer) = self.observer {
                            observer(AgentEvent::ReasoningDelta(format!(
                                "\n[Model busy/overloaded: {}. Retrying in {}s (attempt {}/{})]…\n",
                                e,
                                delay_secs,
                                attempt + 1,
                                max_retries
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

            if let Some(ref usage) = response.usage {
                let accumulated = turn_usage.get_or_insert_with(TokenUsage::default);
                accumulated.prompt_tokens = accumulated
                    .prompt_tokens
                    .saturating_add(usage.prompt_tokens);
                accumulated.completion_tokens = accumulated
                    .completion_tokens
                    .saturating_add(usage.completion_tokens);
                accumulated.total_tokens = accumulated
                    .prompt_tokens
                    .saturating_add(accumulated.completion_tokens);

                let actual_tokens = usage.prompt_tokens.saturating_add(usage.completion_tokens);
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::ContextTokensUpdated {
                        tokens_used: actual_tokens,
                    });
                }
            }

            if let Some(ref r) = response.reasoning {
                last_reasoning = Some(r.clone());
            }

            // If no tools requested, assistant concluded task
            if response.tool_calls.is_empty() {
                let pending_steers: Vec<String> = {
                    let mut sq = self.steer_queue.lock().unwrap();
                    sq.drain(..).collect()
                };
                if !pending_steers.is_empty() {
                    let joined_steer = pending_steers.join("\n\n");
                    let steer_prompt = format!(
                        "[OUT-OF-BAND USER MESSAGE — a direct message from the user, delivered once at this position; not tool output and not a new delivery when replayed from conversation history]\n{}\n[/OUT-OF-BAND USER MESSAGE]",
                        joined_steer
                    );
                    let reply_text = response.content.as_deref().unwrap_or("").trim().to_string();
                    if !reply_text.is_empty() {
                        let mut interim_resp = response.clone();
                        interim_resp.content = Some(reply_text);
                        self.append(&interim_resp, false).await?;
                        messages.push(interim_resp);
                    }
                    if !user_appended {
                        self.append(&user_message, false).await?;
                        user_appended = true;
                    }
                    let steer_msg = Message::user(steer_prompt);
                    self.append(&steer_msg, false).await?;
                    messages.push(steer_msg);
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::Steered { text: joined_steer });
                    }
                    continue;
                }

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
                self.emit_hook_event(HookEvent::Stop, Some(&final_reply))
                    .await?;
                // The saver reads the turn as the user saw it, reply included. The transcript is
                // dropped when this function returns, so appending the final message here changes
                // nothing for the next turn (which replays from the session store).
                messages.push(final_response);
                // The reply is final: anything the saver does now is off the critical path.
                self.spawn_background_saver(&messages);
                return Ok(TurnOutcome {
                    reply: final_reply,
                    reasoning: last_reasoning,
                    tool_calls: executed_tools,
                    replayed: replay_notice,
                    compaction,
                    usage: turn_usage,
                    duration_ms: turn_started.elapsed().as_millis() as u64,
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

                debug!(
                    "Dispatching tool call: '{}' (call id: {})",
                    tool_call.name, tool_call.id
                );

                // Hooks run before the human gate and can only narrow the call: a denial replaces
                // its result, an argument rewrite is what the gate and the tool then see. A hook
                // can never approve what the gate would refuse, so the two cannot disagree.
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

                // The gate next: a mutating call is put to the human before it runs, and a
                // denial comes back as an ordinary tool result the model can react to.
                let gate_outcome = match hook_denial {
                    Some(ref reason) => GateOutcome::Denied(format!("hook: {reason}")),
                    None => {
                        let gated = ToolCall {
                            arguments: effective_arguments.clone(),
                            ..tool_call.clone()
                        };
                        self.gate_decision(&gated).await
                    }
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

                // `post_tool_use` can replace what the model reads without changing what the tool
                // did: a redaction or truncation hook whose output was discarded would be a
                // guardrail that silently guards nothing, which is worse than no hook at all.
                let mut execution_result = execution_result;
                if let Some(hooks) = self.hooks.as_ref() {
                    let event = if execution_result.is_error {
                        HookEvent::PostToolUseFailure
                    } else {
                        HookEvent::PostToolUse
                    };
                    let context = HookContext {
                        session_id: self.session.as_ref().map(|b| b.session_id.as_str()),
                        tool: Some(tool_call.name.as_str()),
                        arguments: Some(&effective_arguments),
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
                        match hook_replacement_text(&replacement) {
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

                executed_tools.push(TurnToolCall {
                    name: tool_call.name.clone(),
                    arguments: tool_call.arguments.clone(),
                    output: execution_result.content.clone(),
                    is_error: execution_result.is_error,
                    denied: is_denial,
                });

                // Track consecutive non-working calls (failure thrashing guardrail).
                // Human/policy denials are intentional choices, not malfunctioning tool thrashing.
                if execution_result.is_error && !is_denial {
                    consecutive_failures += 1;
                    warn!(
                        "Tool '{}' failed (consecutive failures: {}/{})",
                        tool_call.name, consecutive_failures, self.config.max_consecutive_failures
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
                        debug!(
                            "Resetting consecutive failures counter from {} to 0",
                            consecutive_failures
                        );
                    }
                    consecutive_failures = 0;
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
                        let deleted =
                            removed.get("status").and_then(|s| s.as_str()) == Some("deleted");
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

                let current_tokens = estimate_tokens(&messages).tokens;
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::ContextTokensUpdated {
                        tokens_used: current_tokens,
                    });
                }
            }

            // Check for mid-turn steering queued while tools were executing
            let pending_steers: Vec<String> = {
                let mut sq = self.steer_queue.lock().unwrap();
                sq.drain(..).collect()
            };
            if !pending_steers.is_empty() {
                let joined_steer = pending_steers.join("\n\n");
                let steer_prompt = format!(
                    "[OUT-OF-BAND USER MESSAGE — a direct message from the user, delivered once at this position; not tool output and not a new delivery when replayed from conversation history]\n{}\n[/OUT-OF-BAND USER MESSAGE]",
                    joined_steer
                );
                let steer_msg = Message::user(steer_prompt);
                self.append(&steer_msg, false).await?;
                messages.push(steer_msg);
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::Steered { text: joined_steer });
                }
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
                    // Nothing arrived, or only reasoning arrived without content/tools:
                    // the buffered path or a clean retry can recover without failing the turn.
                    if content.is_empty() && calls.is_empty() {
                        debug!(
                            "Stream failed before any content/tool output ({}), retrying buffered",
                            error
                        );
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
                StreamEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                    signature,
                } => {
                    let slot = match calls.iter_mut().find(|(i, _)| *i == index) {
                        Some((_, call)) => call,
                        None => {
                            calls.push((
                                index,
                                ToolCall {
                                    id: String::new(),
                                    name: String::new(),
                                    arguments: serde_json::Value::Null,
                                    signature: None,
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
                    if let Some(signature) = signature {
                        slot.signature = Some(signature);
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
                StreamEvent::Completed {
                    usage: event_usage, ..
                } => {
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
                    serde_json::Value::String(raw) => {
                        serde_json::from_str(&raw).unwrap_or(json!({}))
                    }
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
            if buffered.content.as_deref().unwrap_or("").trim().is_empty()
                && buffered.tool_calls.is_empty()
            {
                if let Some(r) = buffered.reasoning.or(if reasoning.is_empty() {
                    None
                } else {
                    Some(reasoning)
                }) {
                    return Ok(Message {
                        role: Role::Assistant,
                        content: Some(crate::types::NO_TEXT_RESPONSE.to_string()),
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
                        let rest =
                            format!("{}{}", &content[..start], &content[end + close_tag.len()..])
                                .trim()
                                .to_string();
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
        response.reasoning = if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        };
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
        if let Some(ref custom_prompt) = self.config.system_prompt_override {
            return Ok(custom_prompt.clone());
        }

        let relevant_memories = self.memory.retrieve(user_input, 10).await?;
        let memory_prompt = self.memory.format_as_system_prompt(&relevant_memories);
        let skills_prompt = self
            .config
            .skills_summary
            .as_deref()
            .unwrap_or("(no specialized skills installed)");

        let memory_instruction = match self.config.memory_mode {
            crate::config::MemoryMode::Session => {
                format!(
                    "{}\n\n",
                    crate::prompts::section(crate::prompts::MEMORY_SESSION_PROMPT)?
                )
            }
            crate::config::MemoryMode::Background => String::new(),
        };

        let base = crate::prompts::section(crate::prompts::SYSTEM_PROMPT)?
            .replace(
                "{{SYSTEM_PROMPT_PREFIX}}",
                self.config.system_prompt_prefix.as_deref().unwrap_or(""),
            )
            .replace("{{ACTIVE_MEMORIES}}", &memory_prompt)
            .replace("{{AVAILABLE_SKILLS}}", skills_prompt)
            .replace("{{MEMORY_AND_NOTE_PROTOCOL}}\n\n", &memory_instruction)
            .replace("{{MEMORY_AND_NOTE_PROTOCOL}}", &memory_instruction);

        let env_info = if let Some(ref root) = self.config.workspace_root {
            let cwd = std::env::current_dir().unwrap_or_else(|_| root.clone());
            format!(
                "\n\nRUNTIME ENVIRONMENT:\n- Workspace root: {}\n- Current working directory: {}\n- Operating system: {}\nAll relative tool paths (read_file, search_files, patch, write_file) resolve against the workspace root. Changing directory with cd in terminal does not persist to native tools or across turns.",
                root.display(),
                cwd.display(),
                std::env::consts::OS,
            )
        } else if let Ok(cwd) = std::env::current_dir() {
            format!(
                "\n\nRUNTIME ENVIRONMENT:\n- Workspace root: {}\n- Current working directory: {}\n- Operating system: {}\nAll relative tool paths (read_file, search_files, patch, write_file) resolve against the workspace root.",
                cwd.display(),
                cwd.display(),
                std::env::consts::OS,
            )
        } else {
            String::new()
        };

        Ok(format!("{}{}", base, env_info))
    }

    /// The session's recent transcript, plus the notice the console prints about the bound.
    async fn load_history(
        &self,
    ) -> Result<(Vec<StoredMessage>, Option<ReplayNotice>), HarnessError> {
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

        // From here on the fold really happens, so the observer is told before the summary call is
        // made: the wait is seconds long and a UI that only learns about it afterwards has nothing
        // to show while the user is waiting.
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::CompactionStarted { forced: force });
        }

        let outcome: CompactionOutcome = match compact(
            setup.provider.as_ref(),
            &setup.model,
            history,
            self.config.keep_recent_turns,
        )
        .await
        {
            Ok(Some(outcome)) => outcome,
            Ok(None) => {
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::CompactionFinished {
                        folded: None,
                        error: None,
                    });
                }
                debug!(
                        "Nothing to fold: the transcript holds no more turns than agent.keep_recent_turns and fits the compaction budget"
                    );
                return Ok(None);
            }
            Err(failure) => {
                // The user asked for the fold: a failure is the answer, not a silent
                // "there was nothing to compact". Say what actually went wrong.
                if force {
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::CompactionFinished {
                            folded: None,
                            error: Some(failure.to_string()),
                        });
                    }
                    return Err(failure);
                }
                // Non-fatal by design: the transcript is unchanged, the turn runs on the full
                // history, and this turn makes no second attempt.
                warn!(
                    "Compaction failed, continuing on the full transcript: {}",
                    failure
                );
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::CompactionFinished {
                        folded: None,
                        error: None,
                    });
                }
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

        let notice = CompactionNotice {
            session_id: binding.session_id.clone(),
            turns: outcome.folded_turns,
            messages: outcome.folded_messages,
        };
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::CompactionFinished {
                folded: Some(notice.clone()),
                error: None,
            });
        }
        Ok(Some(notice))
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

/// The text a `post_tool_use` hook wants the model to see instead of the tool's own output.
///
/// Two shapes are accepted, because both are what a shell one-liner naturally produces:
/// `{"content":"…"}` (the documented one) and a bare JSON string. Anything else is not a
/// replacement, so the caller keeps the original output rather than guessing at intent.
fn hook_replacement_text(payload: &serde_json::Value) -> Option<String> {
    match payload {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Object(map) => map
            .get("content")
            .or_else(|| map.get("output"))
            .and_then(|value| value.as_str())
            .map(|text| text.to_string()),
        _ => None,
    }
}
