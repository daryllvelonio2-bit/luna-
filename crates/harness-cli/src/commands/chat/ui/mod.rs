use colored::Colorize;
use dialoguer::console::Term;
use harness_core::tools::todo::TodoSnapshot;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::render::{
    activity_line, compaction_line, fit_diff_line_to_width, needs_separator, reasoning_rows,
    tool_activity_detail, OutputBlockKind, PROMPT,
};

/// Fewest rows the live region gives the thought block. On a screen with no room left the block
/// still shows this much: below it the region stops saying anything at all.
pub const MIN_LIVE_REASONING_ROWS: usize = 4;

/// Rows of a live patch diff the region may show, at most. Bounded so a large patch cannot take
/// the screen the thought block is streaming into.
pub const MAX_LIVE_DIFF_ROWS: usize = 6;

/// Rows of a finished thought block that stay in the transcript.
///
/// While the model is thinking the block is shown whole in the live region; once thinking is done
/// it folds to this many rows, because the transcript is the record of what the agent did rather
/// than a second copy of its working. The rest is not lost: Ctrl+O expands the turn's process
/// header, which re-renders the block in full from the stored turn.
pub const FINISHED_REASONING_ROWS: usize = 5;

/// How often the live region advances a spinner frame and refreshes its elapsed time.
pub const ANIMATION_INTERVAL: std::time::Duration = std::time::Duration::from_millis(80);

/// What the empty composer says when nothing is in flight: the one line that teaches the way in.
const IDLE_HINT: &str = "Luna is inactive — type /help for commands";

/// What the empty composer says while a turn is in flight. The commands that only work mid-turn
/// are named here, because a running turn is exactly when they are worth knowing about.
const BUSY_HINT: &str = "Luna is thinking — /interrupt to stop · /steer or /queue to add";

/// Columns of room the prompt row must have before a hint is drawn at all. Below this the hint
/// would be a clipped fragment that teaches nothing, so the prompt is drawn bare instead.
const HINT_MIN_ROOM: usize = 12;

/// Holds the terminal in raw mode for the whole interactive session, restoring the original
/// settings when dropped.
pub struct RawTerminal {
    fd: libc::c_int,
    original: libc::termios,
}

impl RawTerminal {
    /// Returns `None` when stdin is not a terminal, or when termios is unavailable.
    pub fn acquire() -> Option<Self> {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        let fd = libc::STDIN_FILENO;
        let mut current = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, current.as_mut_ptr()) } != 0 {
            return None;
        }
        let original = unsafe { current.assume_init() };
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        // Keep output post-processing so `\n` still returns the carriage, and keep ISIG so
        // Ctrl-C stays a signal — the CLI routes it into the key channel itself.
        raw.c_oflag = original.c_oflag;
        raw.c_lflag |= libc::ISIG;
        if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &raw) } != 0 {
            return None;
        }
        Some(Self { fd, original })
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSADRAIN, &self.original);
        }
    }
}

pub fn get_terminal_width() -> usize {
    Term::stdout()
        .size_checked()
        .map(|(_, w)| w as usize)
        .filter(|&w| w > 0)
        .unwrap_or(80)
}

pub fn get_terminal_height() -> usize {
    Term::stdout()
        .size_checked()
        .map(|(h, _)| h as usize)
        .filter(|&h| h > 0)
        .unwrap_or(24)
}

/// A tool call currently in flight, as far as the animation is concerned.
#[derive(Clone)]
struct Activity {
    /// The formatted call line, shown from the instant the tool is invoked — the transcript
    /// carries the same line once it finishes, so the user watches the call, not just its result.
    call: String,
    detail: Option<String>,
    started: std::time::Instant,
    frame: usize,
}

/// A fold of the oldest turns, as far as the animation is concerned.
///
/// Carried in the live region rather than printed once, because the summary call takes seconds and
/// the user is watching a transcript that is about to be rewritten underneath them.
#[derive(Clone)]
struct Compaction {
    /// True for `/compact`, false for the automatic pass at the top of a turn: the row says which,
    /// so a fold the user did not ask for is never mistaken for one they did.
    forced: bool,
    started: std::time::Instant,
    frame: usize,
}

/// The in-place region between the transcript and the composer: the tail of the reasoning as it
/// streams in, the animated line for a running tool, and the sweep shown while the transcript is
/// being folded.
#[derive(Default)]
struct LiveRegion {
    reasoning: String,
    activity: Option<Activity>,
    /// A fold in flight. It outranks the activity row: for the seconds it runs, the fold is the
    /// only thing happening that the user cannot see the shape of.
    compaction: Option<Compaction>,
    patch_diff: Option<Vec<String>>,
    rows: Vec<String>,
}

#[derive(Clone)]
struct ChatStatus {
    model: String,
    account_name: Option<String>,
    quota_display: Option<String>,
    context_used: u32,
    context_max: u32,
    yolo: bool,
    plan_mode: bool,
    session_name: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct BackgroundTaskState {
    pub job_id: String,
    pub task: String,
    pub current_tool: String,
    pub started_at: std::time::Instant,
    pub status: String,
}

#[derive(Clone, Default, Debug)]
struct BoxDrawnMetrics {
    width: usize,
    bg_line_len: usize,
    queued_line_len: usize,
    status_line_len: usize,
    divider_len: usize,
    prompt_len: usize,
    live_rows_lens: Vec<usize>,
    /// Rows of the todo panel drawn above the box. They are erased with the rest of
    /// the region, so the panel does not leave a trail behind it.
    panel_rows_lens: Vec<usize>,
    /// The rows drawn between the transcript and the prompt, top to bottom.
    ///
    /// The repaint compares them with the next frame and rewrites only the rows whose text
    /// changed, so an unchanged status line is never cleared and redrawn.
    block_rows: Vec<String>,
    /// The prompt row as it was drawn, so the repaint can leave it alone when it has not changed.
    prompt_drawn: String,
}

pub fn visible_width(s: &str) -> usize {
    dialoguer::console::measure_text_width(s)
}

/// Rows of a live patch diff the region may show on a screen this tall.
fn max_live_diff_rows(height: usize) -> usize {
    height.saturating_sub(10).clamp(2, MAX_LIVE_DIFF_ROWS)
}

/// The thought rows the live region shows, given everything else the region carries and everything
/// the composer draws below it.
///
/// The whole block is shown while the model thinks: the live region is the only place reasoning is
/// visible at all, so a fixed tail is a fixed blind spot. The screen is the only bound, and when
/// the block cannot fit, the rows kept are the *newest* — the interesting end of a stream is its
/// end. `chrome` counts the rows drawn below the region, so a tall block pushes the transcript up
/// instead of pushing the prompt off the screen.
fn reasoning_window(live: &LiveRegion, width: usize, chrome: usize) -> Vec<String> {
    let all = if live.reasoning.is_empty() {
        Vec::new()
    } else {
        reasoning_rows(&live.reasoning, width)
    };
    let height = get_terminal_height();
    let reserved = usize::from(live.activity.is_some())
        + usize::from(live.compaction.is_some())
        + live
            .patch_diff
            .as_ref()
            .map(|diff| diff.len().min(max_live_diff_rows(height)))
            .unwrap_or(0);
    let budget = height
        .saturating_sub(chrome + reserved + 1)
        .max(MIN_LIVE_REASONING_ROWS);
    let start = all.len().saturating_sub(budget);
    all[start..].to_vec()
}

/// The transcript's form of a finished thought block: its last [`FINISHED_REASONING_ROWS`] rows,
/// behind a count of what the fold hides.
///
/// The tail is kept rather than the opening so the rows the user was just reading stay where they
/// are: the fold takes away what has already scrolled past, not what the thought arrived at.
pub(crate) fn folded_reasoning_rows(rows: Vec<String>) -> Vec<String> {
    if rows.len() <= FINISHED_REASONING_ROWS {
        return rows;
    }
    let hidden = rows.len() - FINISHED_REASONING_ROWS;
    let noun = if hidden == 1 { "line" } else { "lines" };
    let mut folded = vec![format!(
        "  {}",
        format!("… ({hidden} earlier {noun})").dimmed()
    )];
    folded.extend(rows[hidden..].iter().cloned());
    folded
}

pub fn physical_line_count(visible_len: usize, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    if visible_len == 0 {
        1
    } else {
        (visible_len + width - 1) / width
    }
}

#[derive(Clone)]
pub struct ChatUi {
    pub input_buf: Arc<Mutex<String>>,
    status: Arc<Mutex<Option<ChatStatus>>>,
    live: Arc<Mutex<LiveRegion>>,
    last_block: Arc<Mutex<OutputBlockKind>>,
    transient_error: Arc<Mutex<Option<String>>>,
    bg_task: Arc<Mutex<Option<BackgroundTaskState>>>,
    /// The todo panel's source: the session's list, as it stands.
    ///
    /// The list is kept rather than the rows it was drawn into, so the panel is
    /// rendered at the width the terminal has now — a resize redraws it at the new
    /// width without the list having to be read again.
    todo_snapshot: Arc<Mutex<Option<TodoSnapshot>>>,
    /// Whether the panel is folded to a single row.
    todo_collapsed: Arc<AtomicBool>,
    /// Messages the user submitted while the agent could not take them yet, still waiting to be
    /// sent. Only the count lives here: the queue itself belongs to the loop that drains it.
    queue_depth: Arc<AtomicUsize>,
    /// Whether a turn is in flight. Only the composer's hint reads it: the empty line says
    /// something different depending on whether Luna is idle or working.
    turn_active: Arc<AtomicBool>,
    box_drawn: Arc<AtomicBool>,
    drawn_metrics: Arc<Mutex<Option<BoxDrawnMetrics>>>,
    pub is_attended: bool,
}

/// Shortens a token count for the status line: 0→"0", 999→"999", 12_345→"12.3k", 272_000→"272k".
/// Sub-k counts stay exact because that is the range where the digits still matter.
fn humanize_tokens(count: u32) -> String {
    if count >= 1_000_000 {
        let m = count as f64 / 1_000_000.0;
        if count % 1_000_000 == 0 {
            format!("{}M", (count / 1_000_000))
        } else {
            let rounded = (m * 10.0).round() / 10.0;
            if rounded.fract() == 0.0 {
                format!("{}M", rounded as u64)
            } else {
                format!("{:.1}M", rounded)
            }
        }
    } else if count >= 10_000 {
        let k = count as f64 / 1_000.0;
        if k >= 100.0 || (count % 1_000 == 0) {
            format!("{}k", k.round() as u64)
        } else {
            let rounded = (k * 10.0).round() / 10.0;
            if rounded.fract() == 0.0 {
                format!("{}k", rounded as u64)
            } else {
                format!("{:.1}k", rounded)
            }
        }
    } else {
        count.to_string()
    }
}

fn format_elapsed(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

impl ChatUi {
    pub fn new(is_attended: bool) -> Self {
        Self {
            input_buf: Arc::new(Mutex::new(String::new())),
            status: Arc::new(Mutex::new(None)),
            live: Arc::new(Mutex::new(LiveRegion::default())),
            last_block: Arc::new(Mutex::new(OutputBlockKind::None)),
            transient_error: Arc::new(Mutex::new(None)),
            bg_task: Arc::new(Mutex::new(None)),
            todo_snapshot: Arc::new(Mutex::new(None)),
            todo_collapsed: Arc::new(AtomicBool::new(false)),
            queue_depth: Arc::new(AtomicUsize::new(0)),
            turn_active: Arc::new(AtomicBool::new(false)),
            box_drawn: Arc::new(AtomicBool::new(false)),
            drawn_metrics: Arc::new(Mutex::new(None)),
            is_attended,
        }
    }

    pub fn set_transient_error(&self, err: Option<String>) {
        *self.transient_error.lock().unwrap() = err;
    }

    pub fn transient_error(&self) -> Option<String> {
        self.transient_error.lock().unwrap().clone()
    }

    pub fn clear_transient_error(&self) {
        *self.transient_error.lock().unwrap() = None;
    }

    pub fn has_status(&self) -> bool {
        self.status.lock().unwrap().is_some()
    }

    pub fn set_background_task(&self, state: Option<BackgroundTaskState>) {
        {
            let mut bg = self.bg_task.lock().unwrap();
            *bg = state;
        }
        if self.is_attended && self.box_drawn.load(Ordering::SeqCst) {
            self.redraw_box();
        }
    }

    pub fn update_background_task_tool(&self, current_tool: &str) {
        {
            let mut bg = self.bg_task.lock().unwrap();
            if let Some(ref mut task) = *bg {
                task.current_tool = current_tool.to_string();
            }
        }
        if self.is_attended && self.box_drawn.load(Ordering::SeqCst) {
            self.redraw_background_task_line();
        }
    }

    #[allow(dead_code)]
    pub fn background_task(&self) -> Option<BackgroundTaskState> {
        self.bg_task.lock().unwrap().clone()
    }

    pub fn has_background_task(&self) -> bool {
        self.bg_task.lock().unwrap().is_some()
    }

    /// The rows the todo panel is drawn from, empty when the list is.
    pub fn todo_panel_rows(&self, width: usize) -> Vec<String> {
        let snapshot = self.todo_snapshot.lock().unwrap();
        match snapshot.as_ref() {
            Some(snapshot) => super::todo::render_panel(snapshot, self.todo_collapsed(), width),
            None => Vec::new(),
        }
    }

    /// Replace the session's list and redraw the composer, so the new list shows at
    /// once.
    ///
    /// The panel is drawn above the composer, so a change to the list is a change to
    /// the box rather than a line in the transcript: it appears in place instead of
    /// scrolling up with the conversation.
    pub fn set_todo_panel(&self, snapshot: Option<TodoSnapshot>) {
        let changed = {
            let mut panel = self.todo_snapshot.lock().unwrap();
            if *panel == snapshot {
                false
            } else {
                *panel = snapshot;
                true
            }
        };
        if !changed {
            return;
        }
        if !self.is_attended {
            // A headless run has no box to redraw, so the list is simply printed.
            for row in self.todo_panel_rows(get_terminal_width()) {
                println!("{}", row);
            }
            return;
        }
        if self.box_drawn.load(Ordering::SeqCst) {
            self.redraw_box();
        }
    }

    /// Whether a todo panel snapshot is active.
    pub fn has_todo_panel(&self) -> bool {
        self.todo_snapshot.lock().unwrap().is_some()
    }

    /// Whether the todo panel is folded to a single row.
    pub fn todo_collapsed(&self) -> bool {
        self.todo_collapsed.load(Ordering::SeqCst)
    }

    /// Fold or unfold the todo panel, and say which way it went.
    pub fn toggle_todo_collapsed(&self) -> bool {
        let folded = !self.todo_collapsed.load(Ordering::SeqCst);
        self.todo_collapsed.store(folded, Ordering::SeqCst);
        if self.box_drawn.load(Ordering::SeqCst) {
            self.redraw_box();
        }
        folded
    }

    pub fn box_row_count(&self) -> usize {
        4 + if self.has_background_task() { 1 } else { 0 }
            + if self.queue_depth() > 0 { 1 } else { 0 }
            + self.todo_panel_rows(get_terminal_width()).len()
    }

    pub fn begin_block(&self, kind: OutputBlockKind) {
        let mut last = self.last_block.lock().unwrap();
        if needs_separator(*last, kind) {
            self.emit_above("");
        }
        *last = kind;
    }

    pub fn push_reasoning(&self, text: &str) {
        self.live.lock().unwrap().reasoning.push_str(text);
    }

    pub fn finish_reasoning(&self) {
        let rows = {
            let mut live = self.live.lock().unwrap();
            if live.reasoning.is_empty() {
                return;
            }
            let text = std::mem::take(&mut live.reasoning);
            reasoning_rows(&text, get_terminal_width())
        };
        if !rows.is_empty() {
            self.begin_block(OutputBlockKind::Thinking);
            self.emit_above(&folded_reasoning_rows(rows).join("\n"));
        }
    }

    pub fn start_activity(&self, call: &str, name: &str, arguments: &serde_json::Value) {
        let mut live = self.live.lock().unwrap();
        live.activity = Some(Activity {
            call: call.to_string(),
            detail: tool_activity_detail(name, arguments),
            started: std::time::Instant::now(),
            frame: 0,
        });
    }

    pub fn stop_activity(&self) {
        self.live.lock().unwrap().activity = None;
    }

    /// A fold of the oldest turns has started: animate it until [`Self::finish_compaction`].
    ///
    /// Nothing is printed here in either mode. In the TUI the row is drawn by the animation tick,
    /// and an unattended run says what is happening with the summary line that follows — a static
    /// "compacting…" line with no way to ever update it would be worse than silence.
    pub fn start_compaction(&self, forced: bool) {
        let mut live = self.live.lock().unwrap();
        live.compaction = Some(Compaction {
            forced,
            started: std::time::Instant::now(),
            frame: 0,
        });
    }

    /// The fold is over, whatever it produced: stop animating it. Reporting what was folded is the
    /// caller's job, because the same event carries the failure to report.
    pub fn finish_compaction(&self) {
        let had = {
            let mut live = self.live.lock().unwrap();
            live.compaction.take().is_some()
        };
        if had {
            // One final draw without the row, so the sweep does not stay frozen on screen while the
            // transcript below it is rewritten.
            self.animate_tick();
        }
    }

    #[allow(dead_code)]
    pub fn is_compacting(&self) -> bool {
        self.live.lock().unwrap().compaction.is_some()
    }

    /// How many submitted messages are waiting to be sent.
    #[allow(dead_code)]
    pub fn set_queue_depth(&self, depth: usize) {
        self.queue_depth.store(depth, Ordering::SeqCst);
        self.redraw_queue_line();
    }

    pub fn queue_depth(&self) -> usize {
        self.queue_depth.load(Ordering::SeqCst)
    }

    pub fn clear_live_state(&self) {
        let mut live = self.live.lock().unwrap();
        live.reasoning.clear();
        live.rows.clear();
        live.activity = None;
        live.compaction = None;
        live.patch_diff = None;
    }

    pub fn show_patch_diff(&self, diff: Vec<String>) {
        {
            let mut live = self.live.lock().unwrap();
            live.patch_diff = Some(diff);
        }
        self.animate_tick();
    }

    pub fn clear_patch_diff(&self) {
        let had_diff = {
            let mut live = self.live.lock().unwrap();
            live.patch_diff.take().is_some()
        };
        if had_diff {
            self.animate_tick();
        }
    }

    pub fn set_status(&self, model: &str, context_used: u32, context_max: u32) {
        let mut status = self.status.lock().unwrap();
        let (yolo, plan_mode, session_name, account_name, quota_display) = match status.as_ref() {
            Some(s) => (
                s.yolo,
                s.plan_mode,
                s.session_name.clone(),
                s.account_name.clone(),
                s.quota_display.clone(),
            ),
            None => (false, false, String::new(), None, None),
        };
        *status = Some(ChatStatus {
            model: model.to_string(),
            account_name,
            quota_display,
            context_used,
            context_max,
            yolo,
            plan_mode,
            session_name,
        });
    }

    pub fn set_account_name(&self, account: Option<String>) {
        {
            let mut status = self.status.lock().unwrap();
            if let Some(ref mut s) = *status {
                s.account_name = account;
            }
        }
        self.redraw_status_line();
    }

    pub fn set_quota_display(&self, quota: Option<String>) {
        {
            let mut status = self.status.lock().unwrap();
            if let Some(ref mut s) = *status {
                s.quota_display = quota;
            }
        }
        self.redraw_status_line();
    }

    pub fn set_model(&self, model: &str, context_used: u32, context_max: u32) {
        let mut status = self.status.lock().unwrap();
        if let Some(ref mut s) = *status {
            s.model = model.to_string();
            s.context_used = context_used;
            s.context_max = context_max;
        }
    }

    pub fn set_session_name(&self, name: &str) {
        {
            let mut status = self.status.lock().unwrap();
            if let Some(ref mut s) = *status {
                s.session_name = name.to_string();
            }
        }
        self.redraw_status_line();
    }

    pub fn update_status(&self, context_used: u32, context_max: u32) {
        {
            let mut status = self.status.lock().unwrap();
            if let Some(ref mut s) = *status {
                s.context_used = context_used;
                s.context_max = context_max;
            }
        }
        self.redraw_status_line();
    }

    pub fn update_context_used(&self, context_used: u32) {
        {
            let mut status = self.status.lock().unwrap();
            if let Some(ref mut s) = *status {
                s.context_used = context_used;
            }
        }
        self.redraw_status_line();
    }

    pub fn set_yolo(&self, on: bool) {
        {
            let mut status = self.status.lock().unwrap();
            if let Some(ref mut s) = *status {
                s.yolo = on;
            }
        }
        self.redraw_status_line();
    }

    pub fn set_plan_mode(&self, on: bool) {
        {
            let mut status = self.status.lock().unwrap();
            if let Some(ref mut s) = *status {
                s.plan_mode = on;
            }
        }
        self.redraw_status_line();
    }

    pub fn format_background_task_line(&self, width: usize) -> Option<String> {
        let bg = self.bg_task.lock().unwrap();
        let task = bg.as_ref()?;

        let icon = "⚙".cyan().bold();
        let tag = "evolver-harness".cyan().bold();
        let elapsed = format_elapsed(task.started_at.elapsed())
            .dimmed()
            .to_string();
        let right_hint = "[Ctrl+B]".dimmed().bold().to_string();
        let right_visible = dialoguer::console::strip_ansi_codes(&right_hint).to_string();
        let right_len = right_visible.chars().count();

        let tool_str = if task.current_tool.is_empty() {
            "running".to_string()
        } else {
            task.current_tool.clone()
        };

        // Format: ⚙ evolver-harness 24m read chats.rs                         [Ctrl+B]
        let left_part = format!("{} {} {} {}", icon, tag, elapsed, tool_str.dimmed());
        let left_visible = dialoguer::console::strip_ansi_codes(&left_part).to_string();
        let left_len = left_visible.chars().count();

        if left_len + right_len + 2 <= width {
            let spaces = width.saturating_sub(left_len + right_len);
            Some(format!("{}{}{}", left_part, " ".repeat(spaces), right_hint))
        } else if left_len <= width {
            Some(left_part)
        } else {
            let available = width.saturating_sub(right_len + 2);
            if available > 2 {
                let truncated = format!(
                    "{}…",
                    left_visible
                        .chars()
                        .take(available.saturating_sub(1))
                        .collect::<String>()
                );
                let spaces = width.saturating_sub(truncated.chars().count() + right_len);
                Some(format!("{}{}{}", truncated, " ".repeat(spaces), right_hint))
            } else {
                let cut = width.saturating_sub(1);
                Some(format!(
                    "{}…",
                    left_visible.chars().take(cut).collect::<String>()
                ))
            }
        }
    }

    /// The line that says messages are waiting, drawn above the status line.
    ///
    /// A message the user submitted while the agent could not take it is acknowledged here instead
    /// of the transcript: it has not been sent, so it does not belong in the conversation yet, but
    /// silently swallowing it would look like the keystroke was lost.
    pub fn format_queue_line(&self, width: usize) -> Option<String> {
        let queued = self.queue_depth();
        if queued == 0 {
            return None;
        }
        let left = format!(
            "{} {} queued — sent when the agent is free",
            "⏸".yellow().bold(),
            queued
        );
        let left_visible = dialoguer::console::strip_ansi_codes(&left).to_string();
        if dialoguer::console::measure_text_width(&left_visible) <= width {
            return Some(left);
        }
        // Narrower than the sentence: keep the count, drop the explanation.
        let short_visible = format!("⏸ {} queued", queued);
        if dialoguer::console::measure_text_width(&short_visible) <= width {
            return Some(short_visible.yellow().to_string());
        }
        None
    }

    pub fn format_status_line(&self, width: usize) -> Option<String> {
        let status = self.status.lock().unwrap();
        let s = status.as_ref()?;

        let left = if let Some(ref account) = s.account_name {
            if !account.is_empty() {
                format!("{} · {}", s.model, account)
            } else {
                s.model.clone()
            }
        } else {
            s.model.clone()
        };
        let center = if s.session_name.is_empty() {
            String::new()
        } else {
            format!("[{}]", s.session_name)
        };
        // If a provider quota display is available, format as:
        // "<used>/<max> · 5h <pct>% (<time>) · w <pct>% (<time>)"
        // Otherwise, fall back to the standard context percentage and humanized tokens.
        let mut right = if let Some(ref q) = s.quota_display {
            if !q.is_empty() {
                if s.context_max > 0 {
                    format!(
                        "{}/{} · {}",
                        humanize_tokens(s.context_used),
                        humanize_tokens(s.context_max),
                        q
                    )
                } else {
                    format!("{} · {}", s.context_used, q)
                }
            } else if s.context_max > 0 {
                let percent = (s.context_used as u64) * 100 / s.context_max as u64;
                format!(
                    "ctx: {}% ({}/{})",
                    percent,
                    humanize_tokens(s.context_used),
                    humanize_tokens(s.context_max)
                )
            } else {
                format!("ctx: {}/{}", s.context_used, s.context_max)
            }
        } else if s.context_max > 0 {
            let percent = (s.context_used as u64) * 100 / s.context_max as u64;
            format!(
                "ctx: {}% ({}/{})",
                percent,
                humanize_tokens(s.context_used),
                humanize_tokens(s.context_max)
            )
        } else {
            format!("ctx: {}/{}", s.context_used, s.context_max)
        };
        if s.plan_mode {
            right = format!("{} {}", "plan".cyan().bold(), right);
        }
        if s.yolo {
            right = format!("{} {}", "yolo".yellow().bold(), right);
        }

        let right_visible = dialoguer::console::strip_ansi_codes(&right).to_string();
        let right_len = right_visible.chars().count();
        let left_len = left.chars().count();
        let center_len = center.chars().count();

        // If right is already wider than available width, shorten it so it never wraps.
        let (right_str, right_len) = if right_len > width {
            let cut = width.saturating_sub(1);
            let truncated = format!("{}…", right_visible.chars().take(cut).collect::<String>());
            let t_len = truncated.chars().count();
            (truncated.dimmed().to_string(), t_len)
        } else {
            (right.dimmed().to_string(), right_len)
        };

        if left_len + right_len + 4 > width {
            let available = width.saturating_sub(right_len + 2);
            let left_truncated = if left_len > available {
                if available == 0 {
                    String::new()
                } else {
                    let cut = available.saturating_sub(1);
                    format!("{}…", left.chars().take(cut).collect::<String>())
                }
            } else {
                left
            };
            let left_len = left_truncated.chars().count();
            let spaces = width.saturating_sub(left_len + right_len);
            Some(format!(
                "{}{}{}",
                left_truncated.dimmed(),
                " ".repeat(spaces),
                right_str
            ))
        } else if left_len + right_len + center_len + 8 > width {
            let spaces = width.saturating_sub(left_len + right_len);
            Some(format!(
                "{}{}{}",
                left.dimmed(),
                " ".repeat(spaces),
                right_str
            ))
        } else {
            let free = width.saturating_sub(left_len + right_len);
            let left_gap = ((free.saturating_sub(center_len)) / 2).max(1);
            let right_gap = free.saturating_sub(center_len + left_gap);
            Some(format!(
                "{}{}{}{}{}",
                left.dimmed(),
                " ".repeat(left_gap),
                center.dimmed().bold(),
                " ".repeat(right_gap),
                right_str
            ))
        }
    }

    pub fn erase_live_and_box(&self, previous_live_rows: usize) {
        if !self.is_attended {
            return;
        }
        self.box_drawn.store(false, Ordering::SeqCst);

        let metrics_opt = self.drawn_metrics.lock().unwrap().take();
        let (rows_below, rows_above) = if let Some(m) = metrics_opt {
            let current_width = get_terminal_width().max(1);
            let bottom_rows = physical_line_count(m.divider_len, current_width);
            let prompt_rows = physical_line_count(m.prompt_len, current_width);
            let top_divider_rows = physical_line_count(m.divider_len, current_width);
            let status_rows = if m.status_line_len > 0 {
                physical_line_count(m.status_line_len, current_width)
            } else {
                0
            };
            let bg_rows = if m.bg_line_len > 0 {
                physical_line_count(m.bg_line_len, current_width)
            } else {
                0
            };
            let queued_rows = if m.queued_line_len > 0 {
                physical_line_count(m.queued_line_len, current_width)
            } else {
                0
            };
            let live_rows: usize = m
                .live_rows_lens
                .iter()
                .map(|&len| physical_line_count(len, current_width))
                .sum();
            // The panel sits above the box, so it is erased with the region above it.
            let panel_rows: usize = m
                .panel_rows_lens
                .iter()
                .map(|&len| physical_line_count(len, current_width))
                .sum();

            let below = bottom_rows;
            let above = prompt_rows.saturating_sub(1)
                + top_divider_rows
                + status_rows
                + bg_rows
                + queued_rows
                + live_rows
                + panel_rows;
            (below, above)
        } else {
            let total = previous_live_rows + self.box_row_count();
            (1, total.saturating_sub(1))
        };

        if rows_below > 0 {
            print!("\x1b[{}B", rows_below);
        }

        let term_height = get_terminal_height();
        let total_rows = (rows_below + rows_above + 1).min(term_height);
        for i in 0..total_rows {
            print!("\r\x1b[2K");
            if i + 1 < total_rows {
                print!("\x1b[1A");
            }
        }
        print!("\r\x1b[J");
        let _ = io::stdout().flush();
    }

    pub fn emit_above(&self, text: &str) {
        if !self.is_attended {
            println!("{}", text);
            return;
        }
        let buf = self.input_buf.lock().unwrap();
        let width = get_terminal_width();
        let previous = {
            let mut live = self.live.lock().unwrap();
            let prev = live.rows.len();
            live.rows.clear();
            prev
        };
        self.erase_live_and_box(previous);
        println!("{}", text);
        self.draw_box(width, &buf);
    }

    pub fn set_live(&self, rows: Vec<String>) {
        if !self.is_attended {
            for row in &rows {
                println!("{}", row);
            }
            return;
        }
        let buf = self.input_buf.lock().unwrap();
        let width = get_terminal_width();
        let previous = {
            let mut live = self.live.lock().unwrap();
            let prev = live.rows.len();
            live.rows = rows.clone();
            prev
        };
        // The fast path: the block keeps its height, so it is rewritten where it stands and the
        // screen never blanks. This is what stops the status line from flickering on every tick.
        if let Some(sequence) = self.repaint_block_sequence(width, &buf) {
            print!("{sequence}");
            let _ = io::stdout().flush();
            return;
        }
        self.erase_live_and_box(previous);
        for row in &rows {
            println!("{}", row);
        }
        self.draw_box(width, &buf);
    }

    /// The escape sequence that repaints the region between the transcript and the prompt in place.
    ///
    /// The animation tick changes the spinner frame and the elapsed time twelve times a second.
    /// Erasing that region *and* the composer to redraw it made the status line, the rules and the
    /// prompt flash on every tick, which reads as a broken display rather than as activity.
    ///
    /// When the region keeps its height — the common case: the same rows, a new frame — only the
    /// rows whose text actually changed are rewritten, where they stand. The status line, the
    /// rules and the prompt are not touched at all unless they changed, so nothing blinks. A
    /// height change (the live tail grew, a tool started, the queue line appeared) means the
    /// composer has to move, and the caller falls back to the full erase-and-redraw.
    fn repaint_block_sequence(&self, width: usize, buf: &str) -> Option<String> {
        if !self.box_drawn.load(Ordering::SeqCst) {
            return None;
        }
        let metrics = self.drawn_metrics.lock().unwrap().clone()?;
        if metrics.width != width {
            // A resize redraws everything anyway.
            return None;
        }

        let live_rows = self.live.lock().unwrap().rows.clone();
        let panel_rows = self.todo_panel_rows(width);
        let bg_line = self.format_background_task_line(width);
        let queued_line = self.format_queue_line(width);
        let status_line = self.format_status_line(width);

        // The block is everything between the transcript and the prompt, top to bottom.
        let mut block: Vec<String> = Vec::with_capacity(live_rows.len() + panel_rows.len() + 4);
        block.extend(live_rows.iter().cloned());
        block.extend(panel_rows.iter().cloned());
        if let Some(line) = &bg_line {
            block.push(line.clone());
        }
        if let Some(line) = &queued_line {
            block.push(line.clone());
        }
        if let Some(line) = &status_line {
            block.push(line.clone());
        }
        block.push("─".repeat(width).dimmed().to_string());

        // A row wider than the terminal would occupy two physical rows, and the cursor arithmetic
        // below counts one row per block row.
        let old_block = metrics.block_rows.as_slice();
        if block
            .iter()
            .chain(old_block.iter())
            .any(|row| visible_width(row) > width)
        {
            return None;
        }
        // A different height means the composer moves: that is the full redraw's job.
        if block.len() != old_block.len() {
            return None;
        }
        // Reaching above the top of the screen scrolls the terminal and desyncs every row below.
        if old_block.len() + 2 > get_terminal_height() {
            return None;
        }

        let (prompt, hint_width) = self.prompt_row_drawn(buf);
        let mut sequence = String::new();
        // Tracks the cursor's current vertical offset above the prompt row (0 = on prompt row).
        let mut current_offset = 0usize;
        for (i, row) in block.iter().enumerate() {
            if old_block.get(i).map(|old| old == row).unwrap_or(false) {
                continue;
            }
            // Block row `i` sits this many rows above the prompt row.
            let target_offset = old_block.len() - i;
            if target_offset > current_offset {
                let up = target_offset - current_offset;
                sequence.push_str(&format!("\r\x1b[{up}A\x1b[2K{row}"));
            } else if target_offset < current_offset {
                let down = current_offset - target_offset;
                sequence.push_str(&format!("\r\x1b[{down}B\x1b[2K{row}"));
            } else {
                sequence.push_str(&format!("\r\x1b[2K{row}"));
            }
            current_offset = target_offset;
        }
        // The cursor belongs back on the prompt row: the caret lives there, and the next keypress
        // is written there.
        if current_offset > 0 {
            sequence.push_str(&format!("\r\x1b[{current_offset}B"));
        }
        if prompt != metrics.prompt_drawn {
            sequence.push_str("\r\x1b[2K");
            sequence.push_str(&prompt);
        }
        // The caret belongs where typing happens: at the end of the row, less the hint it walks
        // back over. An unchanged prompt row is not rewritten, only re-seated.
        let column = visible_width(&prompt).saturating_sub(hint_width);
        if column > 0 {
            sequence.push_str(&format!("\r\x1b[{column}C"));
        } else {
            sequence.push('\r');
        }

        // Keep the measurements honest: the rows changed, the height did not.
        {
            let mut guard = self.drawn_metrics.lock().unwrap();
            if let Some(m) = guard.as_mut() {
                m.live_rows_lens = live_rows.iter().map(|r| visible_width(r)).collect();
                m.panel_rows_lens = panel_rows.iter().map(|r| visible_width(r)).collect();
                m.bg_line_len = bg_line.as_ref().map(|s| visible_width(s)).unwrap_or(0);
                m.queued_line_len = queued_line.as_ref().map(|s| visible_width(s)).unwrap_or(0);
                m.status_line_len = status_line.as_ref().map(|s| visible_width(s)).unwrap_or(0);
                m.prompt_len = visible_width(&prompt);
                m.block_rows = block;
                m.prompt_drawn = prompt;
            }
        }
        Some(sequence)
    }

    pub fn animate_tick(&self) {
        if !self.is_attended {
            return;
        }
        let width = get_terminal_width();
        // Rows the composer below the region draws: the block may fill the rest of the screen, and
        // it is measured before the live lock is taken so the two locks are never held together.
        let chrome = self.box_row_count() + 2;

        let rows = {
            let mut live = self.live.lock().unwrap();
            let mut rows = reasoning_window(&live, width, chrome);

            if let Some(activity) = &mut live.activity {
                activity.frame = activity.frame.wrapping_add(1);
                rows.push(activity_line(
                    activity.frame,
                    &activity.call,
                    activity.detail.as_deref(),
                    activity.started.elapsed(),
                    width,
                ));
            }

            if let Some(compaction) = &mut live.compaction {
                compaction.frame = compaction.frame.wrapping_add(1);
                rows.push(compaction_line(
                    compaction.frame,
                    compaction.forced,
                    compaction.started.elapsed(),
                    width,
                ));
            }

            if let Some(diff) = &live.patch_diff {
                let count = diff.len();
                let display_count = count.min(max_live_diff_rows(get_terminal_height()));
                for line in &diff[..display_count] {
                    rows.push(fit_diff_line_to_width(line, width));
                }
                if count > display_count {
                    rows.push(
                        format!(
                            "    ... ({} more changed lines, Ctrl+O to expand)",
                            count - display_count
                        )
                        .dimmed()
                        .to_string(),
                    );
                }
            }
            rows
        };

        if rows.is_empty() && self.live.lock().unwrap().rows.is_empty() {
            return;
        }

        self.set_live(rows);
    }

    /// The composer as it stands, top to bottom: the todo panel, the background
    /// task line, the status line, and the two rules around the prompt.
    ///
    /// The prompt sits between the rules, so the row the rules leave blank is the
    /// prompt's own row.
    pub fn composer_rows(&self, width: usize, buf: &str) -> Vec<String> {
        let divider = "─".repeat(width).dimmed().to_string();
        let mut rows = Vec::new();
        // The panel is chrome: it is drawn above the composer, so it stays put while
        // the conversation scrolls under it.
        rows.extend(self.todo_panel_rows(width));
        if let Some(bg) = self.format_background_task_line(width) {
            rows.push(bg);
        }
        if let Some(queued) = self.format_queue_line(width) {
            rows.push(queued);
        }
        if let Some(status) = self.format_status_line(width) {
            rows.push(status);
        }
        rows.push(divider.clone());
        rows.push(self.prompt_row_drawn(buf).0);
        rows.push(divider);
        rows
    }

    pub fn draw_box(&self, width: usize, buf: &str) {
        let rows = self.composer_rows(width, buf);
        let prompt_str = rows[rows.len() - 2].clone();
        let bottom_rule = rows[rows.len() - 1].clone();
        // The hint is drawn after a caret that belongs before it, so the cursor walks back over
        // it: the next keypress has to land right after the prompt, not after the hint.
        let hint_width = self
            .hint_for(width, buf)
            .map(|hint| visible_width(&hint))
            .unwrap_or(0);

        for row in &rows[..rows.len() - 2] {
            println!("{}", row);
        }
        // The rules are printed around a blank line, and the prompt is written over
        // that blank line: the cursor moves back up to it.
        println!();
        println!("{}", bottom_rule);
        print!("\x1b[2A\r{}", prompt_str);
        if hint_width > 0 {
            print!("\x1b[{hint_width}D");
        }
        let _ = io::stdout().flush();

        let live_rows = self.live.lock().unwrap().rows.clone();
        let live_rows_lens = live_rows.iter().map(|r| visible_width(r)).collect();

        let panel = self.todo_panel_rows(width);
        let bg_line = self.format_background_task_line(width);
        let queue_line = self.format_queue_line(width);
        let status_str = self.format_status_line(width);

        // The block is everything between the transcript and the prompt: the live region, then the
        // composer's own rows, ending with the rule above the prompt.
        let mut block_rows = live_rows.clone();
        block_rows.extend(rows[..rows.len() - 2].iter().cloned());

        *self.drawn_metrics.lock().unwrap() = Some(BoxDrawnMetrics {
            width,
            bg_line_len: bg_line.as_ref().map(|s| visible_width(s)).unwrap_or(0),
            queued_line_len: queue_line.as_ref().map(|s| visible_width(s)).unwrap_or(0),
            status_line_len: status_str.as_ref().map(|s| visible_width(s)).unwrap_or(0),
            divider_len: width,
            prompt_len: visible_width(&prompt_str),
            live_rows_lens,
            panel_rows_lens: panel.iter().map(|r| visible_width(r)).collect(),
            block_rows,
            prompt_drawn: prompt_str.clone(),
        });

        self.box_drawn.store(true, Ordering::SeqCst);
    }

    pub fn visible_input(&self, buf: &str) -> String {
        let room = get_terminal_width()
            .saturating_sub(PROMPT.chars().count())
            .saturating_sub(1);
        let len = buf.chars().count();
        if len <= room {
            return buf.to_string();
        }
        let keep = room.saturating_sub(1);
        format!("…{}", buf.chars().skip(len - keep).collect::<String>())
    }

    pub fn prompt_row(&self, buf: &str) -> String {
        format!("{}{}", PROMPT.bold().yellow(), self.visible_input(buf))
    }

    /// Mark whether a turn is in flight, so the empty composer's hint can say what is happening.
    ///
    /// Only the hint reads this: it decides between the idle line and the working one. The chat
    /// loop sets it when it spawns a turn and clears it on every path a turn can end by.
    pub fn set_turn_active(&self, active: bool) {
        self.turn_active.store(active, Ordering::SeqCst);
    }

    /// Whether a turn is in flight, as far as the composer's hint is concerned.
    pub fn turn_active(&self) -> bool {
        self.turn_active.load(Ordering::SeqCst)
    }

    /// The hint an empty composer shows in a terminal `width` columns wide, if it fits at all.
    ///
    /// `None` when the composer holds text (the line belongs to the user then) or when there is no
    /// room for it: the hint is drawn on the prompt row, so a hint that made the row wrap would
    /// desync every cursor move the in-place repaint makes.
    fn hint_for(&self, width: usize, buf: &str) -> Option<String> {
        if !buf.is_empty() {
            return None;
        }
        let room = width
            .saturating_sub(PROMPT.chars().count())
            .saturating_sub(1);
        if room < HINT_MIN_ROOM {
            return None;
        }
        let text = if self.turn_active() {
            BUSY_HINT
        } else {
            IDLE_HINT
        };
        if text.chars().count() <= room {
            return Some(text.to_string());
        }
        let kept: String = text.chars().take(room.saturating_sub(1)).collect();
        Some(format!("{}…", kept))
    }

    /// The prompt row as the composer paints it, plus the visible width of the hint it carries.
    ///
    /// The hint is drawn *after* the caret on purpose: the caller walks the cursor back over it, so
    /// typing starts right after the prompt and the first character simply overwrites the hint.
    /// The width comes back with the row because only the caller knows what it is about to print.
    pub fn prompt_row_drawn(&self, buf: &str) -> (String, usize) {
        let prompt = self.prompt_row(buf);
        match self.hint_for(get_terminal_width(), buf) {
            Some(hint) => {
                let width = visible_width(&hint);
                (format!("{}{}", prompt, hint.dimmed()), width)
            }
            None => (prompt, 0),
        }
    }

    pub fn redraw_input_line(&self) {
        if !self.is_attended || !self.box_drawn.load(Ordering::SeqCst) {
            return;
        }
        let width = get_terminal_width();
        let drawn_width = self
            .drawn_metrics
            .lock()
            .unwrap()
            .as_ref()
            .map(|m| m.width)
            .unwrap_or(0);
        if drawn_width != 0 && drawn_width != width {
            self.handle_resize();
            return;
        }
        let buf = self.input_buf.lock().unwrap();
        let (row, hint_width) = self.prompt_row_drawn(&buf);
        print!("\r\x1b[K{}", row);
        if hint_width > 0 {
            print!("\x1b[{hint_width}D");
        }
        let _ = io::stdout().flush();
    }

    pub fn draw_initial_box(&self) {
        if !self.is_attended {
            print!("{}", PROMPT.bold().yellow());
            let _ = io::stdout().flush();
            return;
        }
        let width = get_terminal_width();
        let buf = self.input_buf.lock().unwrap();
        self.draw_box(width, &buf);
    }

    pub fn handle_resize(&self) {
        if !self.is_attended {
            return;
        }
        let width = get_terminal_width();
        let buf = self.input_buf.lock().unwrap().clone();
        let chrome = self.box_row_count() + 2;

        let rows = {
            let mut live = self.live.lock().unwrap();
            let mut rows = reasoning_window(&live, width, chrome);

            if let Some(activity) = &mut live.activity {
                rows.push(activity_line(
                    activity.frame,
                    &activity.call,
                    activity.detail.as_deref(),
                    activity.started.elapsed(),
                    width,
                ));
            }
            rows
        };

        if self.box_drawn.load(Ordering::SeqCst) {
            let previous = {
                let mut live = self.live.lock().unwrap();
                let prev = live.rows.len();
                live.rows = rows.clone();
                prev
            };
            self.erase_live_and_box(previous);
            for row in &rows {
                println!("{}", row);
            }
            self.draw_box(width, &buf);
        } else if !rows.is_empty() {
            self.set_live(rows);
        }
    }

    pub fn redraw_box(&self) {
        if !self.is_attended || !self.box_drawn.load(Ordering::SeqCst) {
            return;
        }
        let width = get_terminal_width();
        let buf = self.input_buf.lock().unwrap().clone();
        let previous = {
            let mut live = self.live.lock().unwrap();
            let prev = live.rows.len();
            live.rows.clear();
            prev
        };
        self.erase_live_and_box(previous);
        self.draw_box(width, &buf);
    }

    pub fn redraw_status_line(&self) {
        if !self.is_attended || !self.box_drawn.load(Ordering::SeqCst) {
            return;
        }
        self.redraw_box();
    }

    pub fn redraw_background_task_line(&self) {
        if !self.is_attended
            || !self.box_drawn.load(Ordering::SeqCst)
            || !self.has_background_task()
        {
            return;
        }
        self.redraw_box();
    }

    pub fn redraw_queue_line(&self) {
        if !self.is_attended || !self.box_drawn.load(Ordering::SeqCst) {
            return;
        }
        self.redraw_box();
    }

    pub fn clear_box_on_exit(&self) {
        if !self.is_attended {
            return;
        }
        let previous = {
            let mut live = self.live.lock().unwrap();
            let prev = live.rows.len();
            live.rows.clear();
            live.activity = None;
            prev
        };
        self.erase_live_and_box(previous);
        let _ = io::stdout().flush();
    }
}

/// Helpers shared by this crate's test modules.
///
/// The interfaces under test here draw with cursor moves rather than by writing a whole screen, so
/// the only way to see what they put where is to read the bytes they wrote.
#[cfg(test)]
pub(crate) mod test_support {
    /// Capture what something writes to stdout while it runs.
    ///
    /// The work runs on its own thread: a fresh thread has no output capture of its own, so it
    /// writes to the real stdout, which is the redirected file.
    ///
    /// This only sees writes that go to [`std::io::stdout`] directly. `print!` and `println!` go
    /// through the harness's own capture first and never reach the redirected descriptor, so a test
    /// that captures a `println!` gets an empty string and asserts nothing — check the returned
    /// bytes are non-empty when the assertion matters.
    ///
    pub(crate) fn capture_stdout<F: FnOnce() + Send + 'static>(f: F) -> String {
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;

        /// Serialises the captures: every capture swaps the process's stdout, so two running at
        /// once write into each other's file and each test reads the other's bytes.
        static CAPTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

        let _guard = CAPTURE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out");
        let file = std::fs::File::create(&path).unwrap();
        let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
        assert!(saved >= 0, "cannot save stdout");
        assert!(unsafe { libc::dup2(file.as_raw_fd(), libc::STDOUT_FILENO) } >= 0);

        std::thread::spawn(move || {
            f();
            let _ = std::io::stdout().flush();
        })
        .join()
        .expect("the captured work panicked");
        let _ = std::io::stdout().flush();

        unsafe { libc::dup2(saved, libc::STDOUT_FILENO) };
        unsafe { libc::close(saved) };

        let mut out = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        out
    }
}

#[cfg(test)]
mod tests;
