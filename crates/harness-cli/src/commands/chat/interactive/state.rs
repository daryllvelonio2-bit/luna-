//! The borrowed session view and loop-local state the attended REPL threads through handlers.

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use harness_core::{Config, HarnessError, InterruptHandle, SqliteSessionStore, TurnOutcome};
use harness_providers::{CredentialManager, ModelDiscovery};

use super::super::commands::FeatureToggles;
use super::super::helpers::IdleMemoryPass;
use super::super::keys::AppKey;
use super::super::render::{TurnProcessItem, TurnRecord};
use super::super::setup::ChatRuntime;
use super::super::turn;
use super::super::ui::ChatUi;
use super::super::ChatAgent;

/// The key channel every prompt and picker reads from.
pub(super) type KeyRx = mpsc::UnboundedReceiver<AppKey>;

/// How a key handler tells the loop to keep going or to leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::commands::chat) enum Flow {
    Continue,
    Break,
}

/// The session the loop runs against.
///
/// The shared handles are cloned, not borrowed: the loop needs to hand them to the agent, the
/// screen and the turn while it also mutates the session's model choice and config. Only the
/// values that really change under the loop's feet — the config, the session id, the model
/// strings and the channel receivers — are borrowed.
pub(in crate::commands::chat) struct Env<'a> {
    pub(in crate::commands::chat) ui: Arc<ChatUi>,
    pub(in crate::commands::chat) cred_manager: Arc<CredentialManager>,
    pub(in crate::commands::chat) discovery: Arc<ModelDiscovery>,
    pub(in crate::commands::chat) store: Arc<SqliteSessionStore>,
    pub(in crate::commands::chat) agent: Arc<tokio::sync::Mutex<ChatAgent>>,
    pub(in crate::commands::chat) usage: Arc<crate::commands::usage::UsageRecorder>,
    pub(in crate::commands::chat) todo_scope: harness_core::tools::TodoScope,
    pub(in crate::commands::chat) feature_toggles: &'a FeatureToggles,
    pub(in crate::commands::chat) bg_queue: Arc<harness_core::tasks::BackgroundTaskQueue>,
    pub(in crate::commands::chat) delegate_tool: Arc<harness_core::tools::DelegateTaskTool>,
    pub(in crate::commands::chat) plan_mode: Arc<AtomicBool>,
    pub(in crate::commands::chat) allow_all_session: Arc<AtomicBool>,
    pub(in crate::commands::chat) compacting: Arc<AtomicBool>,
    pub(in crate::commands::chat) in_flight_tools: Arc<Mutex<Vec<TurnProcessItem>>>,
    pub(in crate::commands::chat) interruptions: Arc<Mutex<Vec<turn::InterruptNotice>>>,
    pub(in crate::commands::chat) steer_handle: Arc<Mutex<VecDeque<String>>>,
    pub(in crate::commands::chat) interrupt_handle: Arc<InterruptHandle>,
    pub(in crate::commands::chat) skills_arc: Option<Arc<Vec<harness_plugins::Skill>>>,
    pub(in crate::commands::chat) skills_readout: Option<String>,
    pub(in crate::commands::chat) approval_rx: &'a mut mpsc::UnboundedReceiver<(
        harness_core::ApprovalRequest,
        oneshot::Sender<harness_core::ApprovalDecision>,
    )>,
    pub(in crate::commands::chat) sudo_rx: &'a mut mpsc::UnboundedReceiver<(String, oneshot::Sender<Option<String>>)>,
    pub(in crate::commands::chat) clarify_rx: &'a mut mpsc::UnboundedReceiver<(
        crate::commands::clarify::ClarifyRequest,
        oneshot::Sender<crate::commands::clarify::ClarifyResponse>,
    )>,
    pub(in crate::commands::chat) job_rx: &'a mut mpsc::UnboundedReceiver<harness_core::tasks::JobCompletion>,
    pub(in crate::commands::chat) config: &'a mut Config,
    pub(in crate::commands::chat) session_id: &'a mut String,
    pub(in crate::commands::chat) current_provider: &'a mut String,
    pub(in crate::commands::chat) current_model: &'a mut String,
    pub(in crate::commands::chat) compact_provider: &'a mut Option<String>,
    pub(in crate::commands::chat) compact_model: &'a mut Option<String>,
    pub(in crate::commands::chat) display_settings: Arc<std::sync::RwLock<harness_core::DisplaySettings>>,
}

impl<'a> Env<'a> {
    pub(in crate::commands::chat) fn new(rt: &'a mut ChatRuntime) -> Self {
        Self {
            ui: rt.ui.clone(),
            cred_manager: rt.cred_manager.clone(),
            discovery: rt.discovery.clone(),
            store: rt.store.clone(),
            agent: rt.agent.clone(),
            usage: rt.usage.clone(),
            todo_scope: rt.todo_scope.clone(),
            feature_toggles: &rt.feature_toggles,
            bg_queue: rt.bg_queue.clone(),
            delegate_tool: rt.delegate_tool.clone(),
            plan_mode: rt.plan_mode.clone(),
            allow_all_session: rt.allow_all_session.clone(),
            compacting: rt.compacting.clone(),
            in_flight_tools: rt.in_flight_tools.clone(),
            interruptions: rt.interruptions.clone(),
            steer_handle: rt.steer_handle.clone(),
            interrupt_handle: rt.interrupt_handle.clone(),
            skills_arc: rt.skills_arc.clone(),
            skills_readout: rt.skills_readout.clone(),
            approval_rx: &mut rt.approval_rx,
            sudo_rx: &mut rt.sudo_rx,
            clarify_rx: &mut rt.clarify_rx,
            job_rx: &mut rt.job_rx,
            config: &mut rt.config,
            session_id: &mut rt.session_id,
            current_provider: &mut rt.current_provider,
            current_model: &mut rt.current_model,
            compact_provider: &mut rt.compact_provider,
            compact_model: &mut rt.compact_model,
            display_settings: rt.display_settings.clone(),
        }
    }
}

/// What the loop itself carries between ticks.
///
/// The timers stay out of here: `tokio::select!` builds every arm's future up front, so two
/// futures borrowing different fields of one struct would not coexist. Keeping `animation` and
/// `landing_animation` beside `st` in the loop keeps their borrows disjoint from `st.active_turn`.
pub(super) struct ReplState {
    pub(super) turns: Vec<TurnRecord>,
    pub(super) process_expanded: bool,
    pub(super) queue: VecDeque<String>,
    pub(super) current_cancel: CancellationToken,
    pub(super) active_turn: Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>>,
    pub(super) active_goal: Option<harness_core::GoalRunState>,
    pub(super) current_query: String,
    pub(super) turn_start: std::time::Instant,
    pub(super) prompt_history: Vec<String>,
    pub(super) history_cursor: Option<usize>,
    pub(super) draft_input: String,
    pub(super) quota_refreshed_at: std::time::Instant,
    pub(super) idle_pass: IdleMemoryPass,
    pub(super) pending_job_turn: Option<(String, std::time::Instant)>,
    pub(super) pending_images: Arc<Mutex<Vec<String>>>,
    /// Where typed lines are remembered between sessions.
    pub(super) history_path: Option<std::path::PathBuf>,
}
