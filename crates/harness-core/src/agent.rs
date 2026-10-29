//! The agent loop: types and wiring live here, the turn/fold/gate/prompt/session mechanics in the
//! topical submodules below.

mod fold;
mod gate;
mod prompt;
mod session_ops;
mod turn;

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::approval::{ApprovalSettings, Approver, DenyAll};
use crate::checkpoint::CheckpointController;
use crate::error::HarnessError;
use crate::hooks::HookRunner;
use crate::permission::PermissionEngine;
use crate::saver::{BackgroundSaver, SaverHook};
use crate::session::{CompactionOutcome, SessionStore};
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
    /// Whether automatic compaction is enabled when transcript crosses threshold.
    #[serde(default = "default_auto_compact_enabled")]
    pub auto_compact_enabled: bool,
    /// Token threshold at which automatic compaction triggers (100k - 700k).
    #[serde(default = "default_auto_compact_threshold")]
    pub auto_compact_threshold: usize,
    /// Turns kept verbatim when automatic compaction triggers.
    ///
    /// This is a *retained* count, like [`Self::keep_recent_turns`], not a count of turns to
    /// Percentage of transcript context to compact into a summary (1% - 90%).
    /// When set (or default), auto-compaction targets this percentage of context tokens.
    #[serde(default = "default_auto_compact_percent")]
    pub auto_compact_percent: Option<usize>,
    /// Number of newest turns each auto-compaction fold keeps verbatim.
    #[serde(default = "default_auto_compact_turns")]
    pub auto_compact_turns: usize,
}

fn default_auto_compact_percent() -> Option<usize> {
    Some(50)
}

fn default_auto_compact_enabled() -> bool {
    true
}

/// How long to wait before retrying a failed model request.
///
/// A provider that says how long to wait says it for a reason: retrying earlier is the same
/// request that just failed, and hammering a rate limit is how one 429 becomes a ban. So the
/// provider's `Retry-After` wins, and the exponential backoff is only the floor for errors that
/// carry no timing of their own.
pub fn retry_delay_secs(error: &HarnessError, attempt: u32) -> u64 {
    let exponential_floor = 2u64.pow(attempt);
    match error {
        HarnessError::RateLimited {
            retry_after_secs: Some(seconds),
            ..
        } => (*seconds).max(exponential_floor),
        _ => exponential_floor,
    }
}

fn default_auto_compact_threshold() -> usize {
    150_000
}

fn default_auto_compact_turns() -> usize {
    20
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
            auto_compact_enabled: true,
            auto_compact_threshold: 150_000,
            auto_compact_percent: None,
            auto_compact_turns: 20,
        }
    }
}

/// What the fold targets (percentage of tokens or count of turns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldTarget {
    Percent(usize),
    Turns(usize),
}

/// When the transcript gets folded, and how much of it stays verbatim afterwards.
///
/// There is one fold in this harness, so there is one policy for it: the auto-compact settings
/// when that feature is on, the long-standing `compact_at_tokens` / `keep_recent_turns` pair when
/// it is not. Auto-compact moves the bar and the kept tail — nothing else. Everything a fold does
/// (summarising, writing the summary back, bounding the sent vector, announcing the new size) comes
/// from [`Agent::compact_now`] either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FoldPolicy {
    /// Fold once the transcript estimate crosses this many tokens.
    threshold: usize,
    /// Target percentage or count of turns.
    target: FoldTarget,
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
    CompactionStarted {
        forced: bool,
    },
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
    // No `compaction` field: a turn no longer waits for a fold, so it has no fold result to report.
    // A fold is announced by `AgentEvent::CompactionStarted` / `CompactionFinished`, which arrive
    // while the turn runs and long after it returns.
    /// Provider-reported tokens summed over every model call in this turn, when the provider
    /// reported any. `None` is "unknown", never "zero".
    pub usage: Option<TokenUsage>,
    /// Wall-clock time the turn took, including tool execution and retries.
    pub duration_ms: u64,
}

pub struct Agent<P, M, N, R>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    R: ToolRegistry + 'static,
{
    provider: Arc<P>,
    memory: Arc<M>,
    notes: Arc<N>,
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
    /// `true` while an automatic fold is in flight.
    ///
    /// The fold runs off the turn's path, so a later turn can arrive while the summary call is
    /// still going. This is what stops that turn from starting a second fold over the same
    /// transcript: one fold at a time, and the next turn reaps the result.
    fold_in_flight: Arc<AtomicBool>,
    /// Set when compaction in the active turn produces no fold or fails, preventing repeated retries.
    fold_disabled_this_turn: Arc<AtomicBool>,
    /// Completed fold outcome delivered asynchronously from background compaction task.
    completed_fold: Arc<Mutex<Option<CompactionOutcome>>>,
}

/// Closes the turn's checkpoint on every exit path, including `?` and panics.
pub(super) struct CheckpointGuard<'a> {
    controller: Option<&'a CheckpointController>,
}

/// Clears [`Agent::fold_in_flight`] however the fold ends — including a panic inside the spawned
/// task. A flag left set by a fold that never finished would silently disable folding for the rest
/// of the session, which is the one failure mode worse than a fold that ran twice.
pub(super) struct FoldInFlight(pub(super) Arc<AtomicBool>);

impl Drop for FoldInFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Clears in-flight fold outcome when a turn finishes so subsequent turns replay fresh from store.
pub(super) struct FoldReaperGuard<'a>(&'a Mutex<Option<CompactionOutcome>>);

impl Drop for FoldReaperGuard<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

impl Drop for CheckpointGuard<'_> {
    fn drop(&mut self) {
        if let Some(controller) = self.controller {
            controller.finish();
        }
    }
}

impl<P, M, N, R> Agent<P, M, N, R>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    R: ToolRegistry + 'static,
{
    pub fn new(
        provider: Arc<P>,
        memory: Arc<M>,
        notes: Arc<N>,
        registry: Arc<R>,
        config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            memory,
            notes,
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
            fold_in_flight: Arc::new(AtomicBool::new(false)),
            fold_disabled_this_turn: Arc::new(AtomicBool::new(false)),
            completed_fold: Arc::new(Mutex::new(None)),
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

    /// Update whether auto-compaction is enabled at runtime.
    pub fn set_auto_compact_enabled(&mut self, enabled: bool) {
        self.config.auto_compact_enabled = enabled;
    }

    /// Update the auto-compaction token threshold at runtime.
    pub fn set_auto_compact_threshold(&mut self, threshold: usize) {
        self.config.auto_compact_threshold = threshold;
    }

    /// Update the number of turns to compact at runtime.
    pub fn set_auto_compact_turns(&mut self, turns: usize) {
        self.config.auto_compact_turns = turns;
    }

    /// Update the percentage of transcript to compact at runtime.
    pub fn set_auto_compact_percent(&mut self, percent: usize) {
        self.config.auto_compact_percent = Some(percent.clamp(
            crate::config::AUTO_COMPACT_MIN_PERCENT,
            crate::config::AUTO_COMPACT_MAX_PERCENT,
        ));
    }

    /// Current auto-compaction settings (enabled, threshold, percent).
    pub fn auto_compact_settings(&self) -> (bool, usize, usize) {
        (
            self.config.auto_compact_enabled,
            self.config.auto_compact_threshold,
            self.config
                .auto_compact_percent
                .unwrap_or(crate::config::AUTO_COMPACT_DEFAULT_PERCENT),
        )
    }

    /// Which threshold and kept-tail the fold uses this turn.
    ///
    /// Auto-compact is not a second mechanism, only a lower bar: with it on, the fold fires at the
    /// *smaller* of the two thresholds, so switching it on can fold earlier than the default but
    /// can never let a conversation grow past what the harness already allowed.
    pub(super) fn fold_policy(&self) -> FoldPolicy {
        if self.config.auto_compact_enabled {
            let target = match self.config.auto_compact_percent {
                Some(pct) => FoldTarget::Percent(pct),
                None => FoldTarget::Turns(self.config.auto_compact_turns),
            };
            FoldPolicy {
                threshold: self
                    .config
                    .auto_compact_threshold
                    .min(self.config.compact_at_tokens),
                target,
            }
        } else {
            FoldPolicy {
                threshold: self.config.compact_at_tokens,
                target: FoldTarget::Turns(self.config.keep_recent_turns),
            }
        }
    }

    /// The policy a fold the user asked for by hand runs under: the threshold is irrelevant
    /// (`force` skips the check), and the target is the configured percent or `keep_recent_turns`.
    pub(super) fn explicit_fold_policy(&self) -> FoldPolicy {
        let target = match self.config.auto_compact_percent {
            Some(pct) => FoldTarget::Percent(pct),
            None => FoldTarget::Turns(self.config.keep_recent_turns),
        };
        FoldPolicy {
            threshold: self.config.compact_at_tokens,
            target,
        }
    }

    /// The attached saver, if any (used by the CLI to report the mode).
    pub fn background_saver(&self) -> Option<&SaverHook> {
        self.saver.as_ref()
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

    pub fn memory(&self) -> &Arc<M> {
        &self.memory
    }

    pub fn notes(&self) -> &Arc<N> {
        &self.notes
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
}
