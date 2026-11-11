use harness_providers::QuotaLine;

use super::tail::{Activity, ToolTail};

/// A delegated child that is still running, as far as the composer is concerned.
#[derive(Clone)]
pub(super) struct DelegateTaskView {
    pub(super) id: String,
    pub(super) goal: String,
    pub(super) activity: String,
}

/// A memory the background saver just wrote, as the composer announces it.
///
/// The notice is chrome, not transcript: the pass runs after the reply, and a permanent line per
/// record would grow the scrollback with bookkeeping. The next turn clears it, so it reads as a
/// glance at what was remembered rather than as history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MemoryNotice {
    /// `memory` or `note`: what kind of thing the row is about.
    pub(super) storage: String,
    pub(super) name: String,
    pub(super) detail: Option<String>,
    pub(super) status: String,
}

/// Longest friendly name a notice shows before the CLI trims it. Even on a wide terminal a long
/// generated title should not push the status off the row.
pub(super) const MAX_MEMORY_NOTICE_NAME_CELLS: usize = 36;

/// How many notices stack above the composer. A pass can write many records; the newest are the
/// ones still worth a glance.
pub(super) const MAX_MEMORY_NOTICES: usize = 5;

/// A fold of the oldest turns, as far as the animation is concerned.
///
/// Carried in the live region rather than printed once, because the summary call takes seconds and
/// the user is watching a transcript that is about to be rewritten underneath them.
#[derive(Clone)]
pub(super) struct Compaction {
    /// True for `/compact`, false for the automatic pass at the top of a turn: the row says which,
    /// so a fold the user did not ask for is never mistaken for one they did.
    pub(super) forced: bool,
    pub(super) started: std::time::Instant,
    pub(super) frame: usize,
}

/// The in-place region between the transcript and the composer: the tail of the reasoning as it
/// streams in, the animated line for a running tool, and the sweep shown while the transcript is
/// being folded.
#[derive(Default)]
pub(super) struct LiveRegion {
    pub(super) reasoning: String,
    pub(super) activity: Option<Activity>,
    /// The newest lines of the running tool's output, drawn under its activity row.
    pub(super) tool_output: ToolTail,
    /// Whether Ctrl+O asked for the whole buffered tail instead of the default eight rows.
    pub(super) tool_output_expanded: bool,
    /// A fold in flight. It outranks the activity row: for the seconds it runs, the fold is the
    /// only thing happening that the user cannot see the shape of.
    pub(super) compaction: Option<Compaction>,
    /// Limit on lines displayed per execution block while in progress.
    /// `None` means uncapped (display everything). `Some(1..=10)` caps output.
    pub(super) block_limit: Option<usize>,
    pub(super) rows: Vec<String>,
}

#[derive(Clone)]
pub(super) struct ChatStatus {
    pub(super) model: String,
    pub(super) account_name: Option<String>,
    /// The quota windows the provider reported, with their raw reset times. The countdown is
    /// rendered from these on every paint, so `(4h 59m)` is recomputed rather than frozen at the
    /// moment of the fetch.
    pub(super) quota: Vec<QuotaLine>,
    /// The quota text as it was last drawn. Only the idle tick reads it: it is what says a repaint
    /// is worth doing now that the countdown has moved a minute.
    pub(super) quota_drawn: String,
    pub(super) context_used: u32,
    pub(super) context_max: u32,
    /// A fold retired history since the last turn boundary, so the next boundary estimate is
    /// allowed to be lower than the gauge. Set by [`ChatUi::defer_context_drop`] and consumed by
    /// [`ChatUi::update_status`]: a fold that lands mid-turn describes a transcript the running
    /// turn has not adopted yet, and painting that size immediately made the gauge drop and jump
    /// back within one round.
    pub(super) allow_context_drop: bool,
    pub(super) yolo: bool,
    pub(super) plan_mode: bool,
    pub(super) session_name: String,
}

#[derive(Clone, Default, Debug)]
pub(super) struct BoxDrawnMetrics {
    pub(super) width: usize,
    pub(super) queued_line_len: usize,
    pub(super) status_line_len: usize,
    pub(super) divider_len: usize,
    pub(super) prompt_len: usize,
    pub(super) live_rows_lens: Vec<usize>,
    /// Rows of the todo panel drawn above the box. They are erased with the rest of
    /// the region, so the panel does not leave a trail behind it.
    pub(super) panel_rows_lens: Vec<usize>,
    /// Rows of running delegated tasks drawn above the composer.
    pub(super) delegate_rows_lens: Vec<usize>,
    /// Rows of background-saver notifications drawn above the status line.
    pub(super) memory_notice_rows_lens: Vec<usize>,
    /// The rows drawn between the transcript and the prompt, top to bottom.
    ///
    /// The repaint compares them with the next frame and rewrites only the rows whose text
    /// changed, so an unchanged status line is never cleared and redrawn.
    pub(super) block_rows: Vec<String>,
    /// The prompt row as it was drawn, so the repaint can leave it alone when it has not changed.
    pub(super) prompt_drawn: String,
}

/// The landing mark while its shower is still running.
///
/// The mark is printed once into the scrollback; this keeps the static rows so each animation
/// frame can be rendered from the same source, and counts the frames so the shower loops.
pub(super) struct LandingAnimation {
    /// The unabridged mark, before any stars are overlaid.
    pub(super) rows: Vec<String>,
    /// Frames since the mark was drawn. Only the modulo of this is used.
    pub(super) frame: usize,
}
