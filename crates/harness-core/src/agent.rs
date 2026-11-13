//! The agent loop: types and wiring live here, the turn/fold/gate/prompt/session mechanics in the
//! topical submodules below.

mod dispatch;
mod fold;
mod gate;
mod interrupt;
mod prompt;
mod session_ops;
mod turn;

pub use interrupt::{InterruptHandle, InterruptOutcome};

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
use crate::saver::{BackgroundSaver, MemorySaveStatus, SaveStatus, SaverHook};
use crate::session::{CompactionOutcome, SessionStore};
use crate::traits::*;
use crate::types::*;

/// Fraction of a model's context window a fold may fill before it fires.
///
/// The remaining 30% is headroom, not waste: the summary call itself replays part of the folded
/// transcript plus the provider's system prompt, and the turn that crosses the bar still has to
/// finish with a reply and another tool round. A threshold at 100% is not a bound — it is a
/// request that fails.
pub const FOLD_CONTEXT_FRACTION_NUMERATOR: usize = 7;
pub const FOLD_CONTEXT_FRACTION_DENOMINATOR: usize = 10;

/// The threshold a fold actually fires at: `configured`, tightened when the model's window is
/// known.
///
/// A bar above the window is not a bound — it fires only after the provider has refused the
/// request — so the window wins whenever it is lower.
pub fn fold_threshold(configured: usize, context_window: Option<usize>) -> usize {
    match context_window {
        Some(window) if window > 0 => configured.min(
            window.saturating_mul(FOLD_CONTEXT_FRACTION_NUMERATOR)
                / FOLD_CONTEXT_FRACTION_DENOMINATOR,
        ),
        _ => configured,
    }
}

/// Fraction of the model window above which a fold may no longer wait until the next turn.
///
/// The ordinary bar is deliberately soft: a fold that lands just after the request it was meant
/// to bound is still useful, and making every crossing wait for the summariser put a whole model
/// call between the user's prompt and its answer. This bar is the hard one. A transcript past it
/// may be refused outright, so the fold runs before the first model call even though it delays
/// that turn.
pub const FOLD_URGENT_FRACTION_NUMERATOR: usize = 9;
pub const FOLD_URGENT_FRACTION_DENOMINATOR: usize = 10;

/// The hard ceiling for the model window, or `None` when the host did not report one.
pub fn urgent_fold_threshold(context_window: Option<usize>) -> Option<usize> {
    match context_window {
        Some(window) if window > 0 => Some(
            window.saturating_mul(FOLD_URGENT_FRACTION_NUMERATOR)
                / FOLD_URGENT_FRACTION_DENOMINATOR,
        ),
        _ => None,
    }
}

/// Fraction of the model window replayed from the session store before the fold even runs.
///
/// `session.resume_max_messages` bounds rows, not tokens: a 200-row agentic window measured about
/// 365k heuristic tokens, which can exceed a 256k model's whole window. Replaying at three fifths
/// of the window leaves room for the system prompt, the new user line, a reply and the next tool
/// round — and stays below the 70% fold bar, so a normal resume does not immediately fold.
pub const REPLAY_CONTEXT_FRACTION_NUMERATOR: usize = 3;
pub const REPLAY_CONTEXT_FRACTION_DENOMINATOR: usize = 5;

/// The token budget for replayed history, or `None` when the window is unknown.
pub fn replay_token_budget(context_window: Option<usize>) -> Option<usize> {
    match context_window {
        Some(window) if window > 0 => Some(
            window.saturating_mul(REPLAY_CONTEXT_FRACTION_NUMERATOR)
                / REPLAY_CONTEXT_FRACTION_DENOMINATOR,
        ),
        _ => None,
    }
}

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
    /// Runtime-only persona text from the active profile. Never read from the config file: the
    /// profile loader sets it after resolution, so a profile cannot be edited through `config set`.
    #[serde(skip)]
    pub profile_persona: Option<String>,
    /// Runtime-only behavior text from the active profile. See [`Self::profile_persona`].
    #[serde(skip)]
    pub profile_behavior: Option<String>,
    /// Runtime-only: this session is rendered by the full-screen TUI, which understands Markdown
    /// tables and a small ANSI SGR palette. Never read from the config file.
    #[serde(skip)]
    pub tui_output: bool,
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
    /// Whether saved-memory tools, retrieval, and the background saver are active this session.
    #[serde(default = "default_true")]
    pub memory_enabled: bool,
    /// Whether note tools and the note protocol are active this session.
    #[serde(default = "default_true")]
    pub notes_enabled: bool,
    /// Whether skill tools and skill commands are active this session.
    #[serde(default = "default_true")]
    pub skills_enabled: bool,
    /// Whether `mcp__*` tools and the MCP prompt block are active this session.
    #[serde(default = "default_true")]
    pub mcp_enabled: bool,
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
    /// The active model's context window in tokens, when the caller knows it.
    ///
    /// Compaction is a bound on the model's context, so one fixed token bar cannot be right for
    /// every model: 300k folds late on a 256k window and early on a 1M one. When set, the fold
    /// threshold is clamped to [`FOLD_CONTEXT_FRACTION_NUMERATOR`] /
    /// [`FOLD_CONTEXT_FRACTION_DENOMINATOR`] of this, leaving room for the summary request, the
    /// reply, and the next tool round. `None` keeps the configured thresholds as they are — the
    /// host may not know the window (tests, embedded callers, a new provider).
    #[serde(default)]
    pub context_window: Option<usize>,
}

fn default_auto_compact_percent() -> Option<usize> {
    Some(50)
}

fn default_auto_compact_enabled() -> bool {
    true
}

fn default_true() -> bool {
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

/// Jitter is a quarter of the base delay, bounded so a short backoff still spreads out and a long
/// one does not turn into minutes of extra wait.
const RETRY_JITTER_PER_SEC_MS: u64 = 250;
const RETRY_JITTER_MIN_MS: u64 = 100;
const RETRY_JITTER_MAX_MS: u64 = 1_500;

/// How long to wait before retrying a failed model request, with jittered exponential backoff
/// to avoid thundering herd contention on 429/503 responses.
pub fn retry_delay_with_jitter(error: &HarnessError, attempt: u32) -> std::time::Duration {
    let base_secs = retry_delay_secs(error, attempt);
    let max_jitter_ms =
        (base_secs * RETRY_JITTER_PER_SEC_MS).clamp(RETRY_JITTER_MIN_MS, RETRY_JITTER_MAX_MS);
    let jitter_ms = rand::random::<u64>() % max_jitter_ms;
    std::time::Duration::from_millis(base_secs * 1000 + jitter_ms)
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
            profile_persona: None,
            profile_behavior: None,
            tui_output: false,
            compact_at_tokens: 500_000,
            keep_recent_turns: 8,
            compaction_model: None,
            compaction_provider: None,
            engineer_model: None,
            engineer_provider: None,
            memory_enabled: true,
            notes_enabled: true,
            skills_enabled: true,
            mcp_enabled: true,
            workspace_root: None,
            memory_mode: crate::config::MemoryMode::Session,
            auto_compact_enabled: true,
            auto_compact_threshold: 150_000,
            auto_compact_percent: None,
            auto_compact_turns: 20,
            context_window: None,
        }
    }
}

impl AgentConfig {
    /// Whether a tool may run under the session's runtime feature toggles.
    ///
    /// This is the single place that maps a tool name to the feature that owns it, so the turn
    /// loop's tool list and the dispatcher's rejection check can never disagree about what
    /// `/memory off`, `/notes off`, `/skills off`, or `/mcp off` actually disabled.
    pub fn tool_enabled(&self, name: &str) -> bool {
        if name.starts_with("mcp__") {
            return self.mcp_enabled;
        }
        match name {
            "save_memory" | "update_memory" | "forget_memory" | "memory_search" => {
                self.memory_enabled
            }
            "save_note" | "update_note" | "read_note" | "search_notes" | "list_notes"
            | "delete_note" => self.notes_enabled,
            "skill_view" | "skills_list" => self.skills_enabled,
            _ => true,
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
    /// Above this many tokens a fold runs before the request instead of behind it. `None` when
    /// the host did not report the model's window.
    ceiling: Option<usize>,
    /// Target percentage or count of turns.
    target: FoldTarget,
}

/// The provider and model that write summaries. A separate provider is what makes a failing
/// compaction reachable (point `agent.compaction_provider` at a provider with no credentials).
#[derive(Clone)]
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
    /// One increment of the model's visible reply text, emitted as it arrives so a TUI can
    /// stream the response live. Concatenating every delta yields the turn's final reply.
    ContentDelta(String),
    ToolExecuting {
        name: String,
        arguments: serde_json::Value,
    },
    /// A chunk of a running tool's output, emitted as it arrives.
    ///
    /// Only tools that override [`crate::traits::Tool::execute_with_progress`] produce this; the
    /// chunk boundaries are the reader's, not the tool's, so a consumer should buffer text rather
    /// than assume lines. The complete output still arrives with `ToolExecuted`.
    ToolOutput { name: String, chunk: String },
    ToolExecuted {
        name: String,
        output: String,
        is_error: bool,
    },
    /// A message arrived while a model call was streaming: the call was cut short, `text` is the
    /// user's message that continues the same turn, and `reasoning`/`partial` are what the model
    /// had produced before the cut. The partial reply is kept in the transcript, marked, so a
    /// resume shows what was said instead of pretending the call never happened.
    Interrupted {
        text: String,
        reasoning: String,
        partial: String,
    },
    /// A steering message arrived and was injected mid-turn into conversation context.
    Steered { text: String },
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
    /// The transcript's own estimate changed. `reset` is true only when a fold retired history and
    /// the figure describes the transcript that will be sent next — an explicit `/compact` between
    /// turns, or an automatic fold the running turn has just adopted. Those are the reasons the
    /// number may *fall*: an ordinary update can recompute a smaller figure at a turn boundary (the
    /// provider counts a prompt more tightly than the character heuristic, or the newest stored
    /// report does not cover the tail), and a gauge that fell there read as a reset rather than as
    /// a measurement. A UI that holds a high-water mark must still take a `reset` value even when
    /// it is lower.
    ContextTokensUpdated { tokens_used: u32, reset: bool },
    /// The background saver wrote one memory record.
    ///
    /// The pass runs detached after the reply, so this arrives while the session is idle. `name`
    /// is the model's friendly name when it gave one, otherwise the record's `subject.predicate`
    /// id; `status` says whether the write created a record or replaced an active one.
    MemorySaved {
        name: String,
        detail: Option<String>,
        status: MemorySaveStatus,
    },
    /// The background saver wrote, updated, archived, or merged one note.
    ///
    /// Like [`AgentEvent::MemorySaved`] this arrives after the reply, while the session is idle.
    /// `name` is the note's title (the human name), not its slug.
    NoteSaved {
        name: String,
        detail: Option<String>,
        status: SaveStatus,
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
    /// The soft-interrupt channel: stops one model call without ending the turn.
    interrupt: Arc<InterruptHandle>,
    /// The independent memory writer, when `memory.mode` is `background`.
    saver: Option<SaverHook>,
    /// Completed turns in this agent's lifetime; the saver's interval counts turns, not wall time.
    turns_completed: AtomicUsize,
    /// Fingerprint of the transcript the last successful automatic pass read.
    ///
    /// The interval pass and the idle pass see overlapping windows of the same conversation, so
    /// without this an unchanged tail is sent to the saver model twice. `/remember` ignores it:
    /// an explicit request always runs.
    saver_fingerprint: Arc<Mutex<Option<u64>>>,
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
            interrupt: Arc::new(InterruptHandle::new()),
            saver: None,
            turns_completed: AtomicUsize::new(0),
            saver_fingerprint: Arc::new(Mutex::new(None)),
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
        // A new saver has not seen any of the transcript, so the next automatic pass must run.
        self.saver_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
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

    /// The largest transcript the fold may wait for: the configured bar, tightened to
    /// [`FOLD_CONTEXT_FRACTION_NUMERATOR`]/[`FOLD_CONTEXT_FRACTION_DENOMINATOR`] of the model's
    /// context window when the host reported one.
    ///
    /// A configured bar can be above the window — one live config asks for 300k on a model with a
    /// 256k window — and a fold that waits for a request the model cannot accept is not a bound.
    fn bounded_threshold(&self, configured: usize) -> usize {
        fold_threshold(configured, self.config.context_window)
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
                threshold: self.bounded_threshold(
                    self.config
                        .auto_compact_threshold
                        .min(self.config.compact_at_tokens),
                ),
                ceiling: urgent_fold_threshold(self.config.context_window),
                target,
            }
        } else {
            FoldPolicy {
                threshold: self.bounded_threshold(self.config.compact_at_tokens),
                ceiling: urgent_fold_threshold(self.config.context_window),
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
            threshold: self.bounded_threshold(self.config.compact_at_tokens),
            ceiling: urgent_fold_threshold(self.config.context_window),
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
            .unwrap_or_else(|e| e.into_inner())
            .push_back(trimmed.to_string());
        true
    }

    /// Obtain a clone of the steering queue handle for mid-run submission without locking the outer Agent.
    pub fn steer_handle(&self) -> Arc<Mutex<VecDeque<String>>> {
        self.steer_queue.clone()
    }

    /// The soft-interrupt handle: a message handed to it stops the model call in flight and
    /// continues the same turn, keeping whatever the model had already produced.
    pub fn interrupt_handle(&self) -> Arc<InterruptHandle> {
        self.interrupt.clone()
    }

    /// Update the active model provider dynamically during a session.
    pub fn set_provider(&mut self, provider: Arc<P>) {
        self.provider = provider;
    }

    /// Update the reasoning effort setting dynamically during a session.
    pub fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffort>) {
        self.reasoning_effort = effort;
    }

    /// Tell the system prompt that this session is rendered by the full-screen TUI.
    ///
    /// Only the TUI sets this: the inline CLI, piped runs and headless execs have no rich renderer,
    /// so they must not be invited to emit tables or ANSI color.
    pub fn set_tui_output(&mut self, enabled: bool) {
        self.config.tui_output = enabled;
    }

    /// Update the context window bound when the model changes mid-session.
    ///
    /// The fold threshold is clamped to this, so a switch from a 1M-token model to a 256k one must
    /// carry the new number with it; otherwise the next turn folds against the old model's room.
    pub fn set_context_window(&mut self, context_window: Option<usize>) {
        self.config.context_window = context_window;
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

    /// Set or update the workspace root for the agent and its checkpoint controller.
    pub fn set_workspace_root(&mut self, root: PathBuf) {
        if let Some(cp) = &self.checkpoints {
            cp.set_workspace(root.clone());
        }
        self.config.workspace_root = Some(root);
    }

    /// Enable or disable saved memory for this session.
    ///
    /// Disabling it also stops the background saver: a feature that is off must not keep writing
    /// records behind the turn.
    pub fn set_memory_enabled(&mut self, enabled: bool) {
        self.config.memory_enabled = enabled;
        if let Some(hook) = self.saver.as_ref() {
            hook.saver.set_memory_enabled(enabled);
        }
    }

    /// Enable or disable note tools and the note protocol for this session.
    ///
    /// In background mode the saver writes notes too, so the runtime gate is forwarded to it.
    pub fn set_notes_enabled(&mut self, enabled: bool) {
        self.config.notes_enabled = enabled;
        if let Some(hook) = self.saver.as_ref() {
            hook.saver.set_notes_enabled(enabled);
        }
    }

    /// Enable or disable skill tools and skill commands for this session.
    pub fn set_skills_enabled(&mut self, enabled: bool) {
        self.config.skills_enabled = enabled;
    }

    /// Enable or disable MCP tools and the MCP prompt block for this session.
    pub fn set_mcp_enabled(&mut self, enabled: bool) {
        self.config.mcp_enabled = enabled;
    }

    /// Read-only access to current agent configuration.
    pub fn config(&self) -> &AgentConfig {
        &self.config
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

    /// Build a sibling [`Agent`] sharing this instance's shared runtime infrastructure (`memory`,
    /// `notes`, `registry`, `approver`, `permissions`, `approval`, `compaction`, `hooks`,
    /// `checkpoints`, `saver`) but bound to an independent session, provider, observer, steer
    /// queue, and interrupt handle.
    pub fn fork_for_session(
        &self,
        provider: Arc<P>,
        binding: SessionBinding,
        observer: AgentObserver,
        reasoning_effort: Option<ReasoningEffort>,
        context_window: Option<usize>,
    ) -> Self {
        let mut config = self.config.clone();
        config.context_window = context_window;
        Self {
            provider,
            memory: self.memory.clone(),
            notes: self.notes.clone(),
            registry: self.registry.clone(),
            config,
            approver: self.approver.clone(),
            permissions: self.permissions.clone(),
            approval: self.approval.clone(),
            compaction: self.compaction.clone(),
            session: Some(binding),
            hooks: self.hooks.clone(),
            checkpoints: self.checkpoints.clone(),
            observer: Some(observer),
            reasoning_effort,
            steer_queue: Arc::new(Mutex::new(VecDeque::new())),
            interrupt: Arc::new(InterruptHandle::new()),
            saver: self.saver.clone(),
            turns_completed: AtomicUsize::new(0),
            saver_fingerprint: Arc::new(Mutex::new(None)),
            fold_in_flight: Arc::new(AtomicBool::new(false)),
            fold_disabled_this_turn: Arc::new(AtomicBool::new(false)),
            completed_fold: Arc::new(Mutex::new(None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each toggle owns exactly its own tools; the mapping is what `/memory off` and friends
    /// rely on, so a new tool in one family must be added here rather than silently bypassing it.
    #[test]
    fn runtime_toggles_own_their_tools() {
        let mut config = AgentConfig::default();
        for name in [
            "save_memory",
            "update_memory",
            "forget_memory",
            "memory_search",
            "save_note",
            "update_note",
            "read_note",
            "search_notes",
            "list_notes",
            "delete_note",
            "skill_view",
            "skills_list",
            "mcp__server__tool",
            "read_file",
        ] {
            assert!(config.tool_enabled(name), "{name} starts enabled");
        }

        config.memory_enabled = false;
        assert!(!config.tool_enabled("save_memory"));
        assert!(!config.tool_enabled("update_memory"));
        assert!(!config.tool_enabled("forget_memory"));
        assert!(!config.tool_enabled("memory_search"));
        assert!(config.tool_enabled("save_note"));
        assert!(config.tool_enabled("read_file"));

        config.notes_enabled = false;
        assert!(!config.tool_enabled("save_note"));
        assert!(!config.tool_enabled("delete_note"));
        assert!(config.tool_enabled("skill_view"));

        config.skills_enabled = false;
        assert!(!config.tool_enabled("skill_view"));
        assert!(!config.tool_enabled("skills_list"));
        assert!(config.tool_enabled("mcp__server__tool"));

        config.mcp_enabled = false;
        assert!(!config.tool_enabled("mcp__server__tool"));
        assert!(config.tool_enabled("read_file"));
    }

    /// A configured bar above the model window is clamped below it; a bar that already fits, or a
    /// host that does not know the window, is left exactly as configured.
    #[test]
    fn the_fold_bar_never_sits_above_the_model_window() {
        assert_eq!(fold_threshold(300_000, Some(256_000)), 179_200);
        assert_eq!(fold_threshold(150_000, Some(1_000_000)), 150_000);
        assert_eq!(fold_threshold(300_000, None), 300_000);
        assert_eq!(fold_threshold(300_000, Some(0)), 300_000);
    }

    /// The urgent bar sits above the ordinary one, so a crossing still has the background path to
    /// fall back on and only a near-window transcript forces the turn to wait.
    #[test]
    fn the_urgent_bar_leaves_room_for_the_reply() {
        assert_eq!(urgent_fold_threshold(Some(256_000)), Some(230_400));
        assert_eq!(urgent_fold_threshold(None), None);
        assert_eq!(urgent_fold_threshold(Some(0)), None);
        assert!(urgent_fold_threshold(Some(256_000)).unwrap() > 179_200);
    }

    /// Replay is bounded below the fold bar: the request is assembled from history at three fifths
    /// of the window, leaving the system prompt, the user line and the reply their own room.
    #[test]
    fn replay_stays_below_the_fold_bar() {
        let budget = replay_token_budget(Some(256_000)).unwrap();
        assert_eq!(budget, 153_600);
        assert!(budget < fold_threshold(300_000, Some(256_000)));
        assert_eq!(replay_token_budget(None), None);
        assert_eq!(replay_token_budget(Some(0)), None);
    }
}
