use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use harness_core::tools::todo::TodoSnapshot;

use super::super::render::OutputBlockKind;
use super::panels::{
    BoxDrawnMetrics, ChatStatus, DelegateTaskView, LandingAnimation, LiveRegion, MemoryNotice,
};
use super::terminal::PaintInFlight;

#[derive(Clone)]
pub struct ChatUi {
    pub input_buf: Arc<Mutex<String>>,
    pub(super) status: Arc<Mutex<Option<ChatStatus>>>,
    pub(super) live: Arc<Mutex<LiveRegion>>,
    pub(super) last_block: Arc<Mutex<OutputBlockKind>>,
    pub(super) transient_error: Arc<Mutex<Option<String>>>,
    /// The todo panel's source: the session's list, as it stands.
    ///
    /// The list is kept rather than the rows it was drawn into, so the panel is
    /// rendered at the width the terminal has now — a resize redraws it at the new
    /// width without the list having to be read again.
    pub(super) todo_snapshot: Arc<Mutex<Option<TodoSnapshot>>>,
    /// Whether the panel is folded to a single row.
    pub(super) todo_collapsed: Arc<AtomicBool>,
    /// Messages the user submitted while the agent could not take them yet, still waiting to be
    /// sent. Only the count lives here: the queue itself belongs to the loop that drains it.
    pub(super) queue_depth: Arc<AtomicUsize>,
    /// Delegated children that are still running. These stay visible above the composer after
    /// the main turn ends, so a background task never disappears without a trace.
    pub(super) delegate_tasks: Arc<Mutex<Vec<DelegateTaskView>>>,
    /// Memories the detached saver just wrote, shown above the status line until the next turn.
    pub(super) memory_notices: Arc<Mutex<Vec<MemoryNotice>>>,
    /// Whether a turn is in flight. Only the composer's hint reads it: the empty line says
    /// something different depending on whether Luna is idle or working.
    pub(super) turn_active: Arc<AtomicBool>,
    pub(super) box_drawn: Arc<AtomicBool>,
    /// The fresh-session mark while its shower is running. `None` once the transcript starts or
    /// the screen is cleared: the mark then stays in the scrollback as the last frame drawn.
    pub(super) landing: Arc<Mutex<Option<LandingAnimation>>>,
    pub(super) drawn_metrics: Arc<Mutex<Option<BoxDrawnMetrics>>>,
    /// Set while an interactive prompt owns the screen.
    ///
    /// Updates arrive from tasks the chat loop is not running — the provider's usage callback, a
    /// delegated child finishing — and a paint there would land on the alternate screen around the
    /// question instead of on the transcript. While this is set the state is still updated, but the
    /// composer is not drawn.
    pub(super) modal_active: Arc<AtomicBool>,
    /// Paints running right now, so a modal can wait for the last one before it takes the screen.
    ///
    /// The flag alone closes the common case: a paint that sees it returns without drawing. A
    /// paint that had already passed the check when the flag went up would still land on the
    /// modal's screen instead of the transcript, so entering the alternate buffer waits for this
    /// to fall to zero first.
    pub(super) paints_in_flight: Arc<AtomicUsize>,
    /// Transcript lines produced while a modal owned the screen, written once it is gone.
    pub(super) pending_above: Arc<Mutex<Vec<String>>>,
    /// Lines a slash command emitted while [`ChatUi::begin_command_capture`] had it open.
    ///
    /// The command's text is already printed above the composer as it runs; this copy is what
    /// lets the chat loop keep the command and its result in the transcript, where a Ctrl+O
    /// redraw can render them again instead of losing them.
    pub(super) command_capture: Arc<Mutex<Option<Vec<String>>>>,
    pub(super) tui_mode: Arc<AtomicBool>,
    pub(super) tui_notify: Arc<tokio::sync::Notify>,
    pub(super) tui_notices: Arc<Mutex<Vec<String>>>,
    pub is_attended: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TuiLiveSnapshot {
    pub reasoning: String,
    pub content: String,
    pub activity_call: Option<String>,
    pub activity_detail: Option<String>,
    pub activity_elapsed_secs: Option<f32>,
    pub tool_output_tail: Vec<String>,
    pub compacting: Option<(bool, f32)>,
    pub model: String,
    pub context_used: u32,
    pub context_max: u32,
    pub yolo: bool,
    pub plan_mode: bool,
    pub goal_progress: Option<(usize, usize)>,
    pub session_name: String,
    pub queue_depth: usize,
    pub transient_error: Option<String>,
    pub todo_snapshot: Option<TodoSnapshot>,
    pub todo_collapsed: bool,
    pub delegate_count: usize,
    pub delegate_last: Option<(String, String)>,
    pub memory_notices: Vec<(String, String, Option<String>, String)>,
}

impl ChatUi {
    pub fn new(is_attended: bool) -> Self {
        Self {
            input_buf: Arc::new(Mutex::new(String::new())),
            status: Arc::new(Mutex::new(None)),
            live: Arc::new(Mutex::new(LiveRegion::default())),
            last_block: Arc::new(Mutex::new(OutputBlockKind::None)),
            transient_error: Arc::new(Mutex::new(None)),
            todo_snapshot: Arc::new(Mutex::new(None)),
            todo_collapsed: Arc::new(AtomicBool::new(false)),
            queue_depth: Arc::new(AtomicUsize::new(0)),
            delegate_tasks: Arc::new(Mutex::new(Vec::new())),
            memory_notices: Arc::new(Mutex::new(Vec::new())),
            turn_active: Arc::new(AtomicBool::new(false)),
            box_drawn: Arc::new(AtomicBool::new(false)),
            landing: Arc::new(Mutex::new(None)),
            drawn_metrics: Arc::new(Mutex::new(None)),
            modal_active: Arc::new(AtomicBool::new(false)),
            paints_in_flight: Arc::new(AtomicUsize::new(0)),
            pending_above: Arc::new(Mutex::new(Vec::new())),
            command_capture: Arc::new(Mutex::new(None)),
            tui_mode: Arc::new(AtomicBool::new(false)),
            tui_notify: Arc::new(tokio::sync::Notify::new()),
            tui_notices: Arc::new(Mutex::new(Vec::new())),
            is_attended,
        }
    }
    pub fn set_tui_mode(&self, on: bool) {
        self.tui_mode.store(on, Ordering::SeqCst);
    }
    pub fn is_tui_mode(&self) -> bool {
        self.tui_mode.load(Ordering::SeqCst)
    }
    pub fn tui_notifier(&self) -> Arc<tokio::sync::Notify> {
        self.tui_notify.clone()
    }
    pub fn notify_tui(&self) {
        if self.is_tui_mode() {
            self.tui_notify.notify_one();
        }
    }
    pub fn take_tui_notices(&self) -> Vec<String> {
        std::mem::take(&mut *self.tui_notices.lock().unwrap_or_else(|e| e.into_inner()))
    }
    /// Tell the UI that a modal owns the screen, or that it has been given back.
    ///
    /// While a modal is up the composer must not be painted: updates can arrive from tasks the
    /// chat loop is not running, and a paint there would land on the alternate screen around the
    /// question. Giving the screen back flushes any transcript lines that arrived meanwhile; the
    /// caller repaints the composer if it needs to show state that changed under the modal.
    pub fn set_modal_active(&self, active: bool) {
        let was = self.modal_active.swap(active, Ordering::SeqCst);
        if was && !active {
            let pending: Vec<String> = {
                let mut pending = self.pending_above.lock().unwrap_or_else(|e| e.into_inner());
                std::mem::take(&mut *pending)
            };
            for text in pending {
                self.emit_above(&text);
            }
        }
    }
    /// Whether an interactive prompt currently owns the screen.
    pub fn modal_active(&self) -> bool {
        self.modal_active.load(Ordering::SeqCst)
    }
    /// Start recording everything [`Self::emit_above`] writes.
    ///
    /// Called around a slash command so the chat loop can turn what the command printed into a
    /// transcript turn. A capture already open is replaced: only the innermost command owns it.
    pub fn begin_command_capture(&self) {
        *self
            .command_capture
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Vec::new());
    }
    /// Stop recording and return the captured lines, or `None` when no capture was open.
    pub fn end_command_capture(&self) -> Option<Vec<String>> {
        self.command_capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
    /// Count one paint as running until the returned guard is dropped.
    ///
    /// Taken before [`Self::modal_active`] is read: a paint that started before the flag went up
    /// is then visible to [`Self::wait_for_paints`], and one that starts after it sees the flag.
    pub(super) fn paint_in_flight(&self) -> PaintInFlight {
        self.paints_in_flight.fetch_add(1, Ordering::SeqCst);
        PaintInFlight(self.paints_in_flight.clone())
    }
    /// Wait, briefly, for a paint that was already running when the modal went up.
    ///
    /// `set_modal_active(true)` must be called first: new paints then bail out at the flag while
    /// the ones already printing finish. The wait is bounded — a wedged paint must not keep the
    /// question off the screen forever.
    pub async fn wait_for_paints(&self) {
        const MAX_WAIT: std::time::Duration = std::time::Duration::from_millis(50);
        const POLL: std::time::Duration = std::time::Duration::from_millis(1);
        let start = std::time::Instant::now();
        while self.paints_in_flight.load(Ordering::SeqCst) > 0 && start.elapsed() < MAX_WAIT {
            tokio::time::sleep(POLL).await;
        }
    }
    pub fn set_transient_error(&self, err: Option<String>) {
        *self
            .transient_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = err;
        self.notify_tui();
    }
    pub fn transient_error(&self) -> Option<String> {
        self.transient_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn clear_transient_error(&self) {
        *self
            .transient_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.notify_tui();
    }
    pub fn has_status(&self) -> bool {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
    pub fn clear_live_state(&self) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.reasoning.clear();
        live.content.clear();
        live.rows.clear();
        live.activity = None;
        live.tool_output.clear();
        live.tool_output_expanded = false;
        live.compaction = None;
        drop(live);
        self.notify_tui();
    }
    pub fn tui_snapshot(&self, width: usize) -> TuiLiveSnapshot {
        let mut snap = TuiLiveSnapshot::default();
        {
            let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            snap.reasoning = live.reasoning.clone();
            snap.content = live.content.clone();
            if let Some(act) = &live.activity {
                snap.activity_call = Some(act.call.clone());
                snap.activity_detail = act.detail.clone();
                snap.activity_elapsed_secs = Some(act.started.elapsed().as_secs_f32());
                snap.tool_output_tail = live
                    .tool_output
                    .rows(width.max(20), if live.tool_output_expanded { 40 } else { 8 });
            }
            if let Some(comp) = &live.compaction {
                snap.compacting = Some((comp.forced, comp.started.elapsed().as_secs_f32()));
            }
        }
        if let Some(st) = self.status.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            snap.model = st.model.clone();
            snap.context_used = st.context_used;
            snap.context_max = st.context_max;
            snap.yolo = st.yolo;
            snap.plan_mode = st.plan_mode;
            snap.goal_progress = st.goal_progress;
            snap.session_name = st.session_name.clone();
        }
        snap.queue_depth = self.queue_depth.load(Ordering::SeqCst);
        snap.transient_error = self.transient_error();
        snap.todo_snapshot = self
            .todo_snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        snap.todo_collapsed = self.todo_collapsed.load(Ordering::SeqCst);
        {
            let tasks = self.delegate_tasks.lock().unwrap_or_else(|e| e.into_inner());
            snap.delegate_count = tasks.len();
            if let Some(last) = tasks.last() {
                snap.delegate_last = Some((last.goal.clone(), last.activity.clone()));
            }
        }
        {
            let notices = self.memory_notices.lock().unwrap_or_else(|e| e.into_inner());
            snap.memory_notices = notices
                .iter()
                .map(|n| {
                    (
                        n.storage.clone(),
                        n.name.clone(),
                        n.detail.clone(),
                        n.status.clone(),
                    )
                })
                .collect();
        }
        snap
    }
}
