use colored::Colorize;
use dialoguer::console::Term;
use harness_core::tools::todo::TodoSnapshot;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::render::{
    activity_line, compaction_line, needs_separator, reasoning_rows, tool_activity_detail,
    OutputBlockKind, PROMPT,
};

/// Rows of reasoning kept in the live region before the oldest roll into the transcript. Keeping
/// a shallow tail is what makes long reasoning stream smoothly instead of scrolling the screen.
pub const LIVE_REASONING_ROWS: usize = 4;

/// How often the live region advances a spinner frame and refreshes its elapsed time.
pub const ANIMATION_INTERVAL: std::time::Duration = std::time::Duration::from_millis(80);

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
}

pub fn visible_width(s: &str) -> usize {
    dialoguer::console::measure_text_width(s)
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
            self.emit_above(&rows.join("\n"));
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

    pub fn clear_live_and_box(&self) {
        self.clear_box_on_exit();
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

        let total_rows = rows_below + rows_above + 1;
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
        self.erase_live_and_box(previous);
        for row in &rows {
            println!("{}", row);
        }
        self.draw_box(width, &buf);
    }

    pub fn animate_tick(&self) {
        if !self.is_attended {
            return;
        }
        let width = get_terminal_width();

        let rows = {
            let mut live = self.live.lock().unwrap();
            let all = if live.reasoning.is_empty() {
                Vec::new()
            } else {
                reasoning_rows(&live.reasoning, width)
            };
            let start = all.len().saturating_sub(LIVE_REASONING_ROWS);
            let mut rows = all[start..].to_vec();

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
                const MAX_LIVE_DIFF: usize = 20;
                if diff.len() <= MAX_LIVE_DIFF {
                    rows.extend(diff.iter().cloned());
                } else {
                    rows.extend(diff[..MAX_LIVE_DIFF - 1].iter().cloned());
                    rows.push(
                        format!(
                            "    ... ({} more changed lines)",
                            diff.len() - (MAX_LIVE_DIFF - 1)
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
        rows.push(self.prompt_row(buf));
        rows.push(divider);
        rows
    }

    pub fn draw_box(&self, width: usize, buf: &str) {
        let rows = self.composer_rows(width, buf);
        let prompt_str = rows[rows.len() - 2].clone();
        let bottom_rule = rows[rows.len() - 1].clone();

        for row in &rows[..rows.len() - 2] {
            println!("{}", row);
        }
        // The rules are printed around a blank line, and the prompt is written over
        // that blank line: the cursor moves back up to it.
        println!();
        println!("{}", bottom_rule);
        print!("\x1b[2A\r{}", prompt_str);
        let _ = io::stdout().flush();

        let live_rows = self.live.lock().unwrap().rows.clone();
        let live_rows_lens = live_rows.iter().map(|r| visible_width(r)).collect();

        let panel = self.todo_panel_rows(width);
        let bg_line = self.format_background_task_line(width);
        let queue_line = self.format_queue_line(width);
        let status_str = self.format_status_line(width);

        *self.drawn_metrics.lock().unwrap() = Some(BoxDrawnMetrics {
            width,
            bg_line_len: bg_line.as_ref().map(|s| visible_width(s)).unwrap_or(0),
            queued_line_len: queue_line.as_ref().map(|s| visible_width(s)).unwrap_or(0),
            status_line_len: status_str.as_ref().map(|s| visible_width(s)).unwrap_or(0),
            divider_len: width,
            prompt_len: visible_width(&prompt_str),
            live_rows_lens,
            panel_rows_lens: panel.iter().map(|r| visible_width(r)).collect(),
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
        print!("\r\x1b[K{}", self.prompt_row(&buf));
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

        let rows = {
            let mut live = self.live.lock().unwrap();
            let all = if live.reasoning.is_empty() {
                Vec::new()
            } else {
                reasoning_rows(&live.reasoning, width)
            };
            let start = all.len().saturating_sub(LIVE_REASONING_ROWS);
            let mut rows = all[start..].to_vec();

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

    #[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture what the UI writes to stdout while `f` runs.
    ///
    /// The composer draws with cursor moves rather than by rewriting the screen, so
    /// the only way to see what it put where is to read the bytes it wrote. The work
    /// runs on its own thread: the test harness captures `println!` per thread, and
    /// a fresh thread writes to the real stdout, which is the redirected file.
    fn capture_stdout<F: FnOnce() + Send + 'static>(f: F) -> String {
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out");
        let file = std::fs::File::create(&path).unwrap();
        let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
        assert!(saved >= 0, "cannot save stdout");
        assert!(unsafe { libc::dup2(file.as_raw_fd(), libc::STDOUT_FILENO) } >= 0);

        std::thread::spawn(f)
            .join()
            .expect("the captured work panicked");
        let _ = io::stdout().flush();

        unsafe { libc::dup2(saved, libc::STDOUT_FILENO) };
        unsafe { libc::close(saved) };

        let mut out = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        out
    }

    fn a_snapshot() -> TodoSnapshot {
        TodoSnapshot {
            items: vec![harness_core::tools::todo::TodoItem {
                id: 0,
                text: "a task".to_string(),
                status: harness_core::tools::todo::TodoStatus::Pending,
            }],
        }
    }

    #[test]
    fn the_todo_panel_is_drawn_above_the_composer() {
        let ui = ChatUi::new(true);
        ui.set_todo_panel(Some(a_snapshot()));

        let rows = ui.composer_rows(80, "hello");
        let panel_at = rows
            .iter()
            .position(|r| r.contains("Todo"))
            .expect("the panel is drawn");
        let prompt_at = rows
            .iter()
            .position(|r| r.contains("hello"))
            .expect("the composer is drawn");
        assert!(
            panel_at < prompt_at,
            "the panel belongs above the composer: {:?}",
            rows
        );
        // The panel is the first thing in the composer: nothing is drawn over the
        // top of the terminal, so the list stays in front of the user.
        assert_eq!(panel_at, 0, "the panel is at the top: {:?}", rows);
    }

    #[test]
    fn the_panel_rows_are_counted_with_the_box() {
        let ui = ChatUi::new(true);
        ui.set_todo_panel(Some(a_snapshot()));
        // A header, one task and a footer.
        assert_eq!(ui.box_row_count(), 4 + 3);

        let boxed = ui.clone();
        capture_stdout(move || boxed.draw_box(80, ""));
        let metrics = ui.drawn_metrics.lock().unwrap();
        let drawn = metrics.as_ref().expect("the box was drawn");
        assert_eq!(drawn.panel_rows_lens.len(), 3);
    }

    #[test]
    fn a_fresh_list_replaces_the_old_one() {
        let ui = ChatUi::new(true);
        ui.set_todo_panel(Some(a_snapshot()));
        assert_eq!(ui.todo_panel_rows(80).len(), 3);

        let mut second = a_snapshot();
        second.items.push(harness_core::tools::todo::TodoItem {
            id: 1,
            text: "another task".to_string(),
            status: harness_core::tools::todo::TodoStatus::Done,
        });
        ui.set_todo_panel(Some(second));
        assert_eq!(ui.todo_panel_rows(80).len(), 4);

        // An empty list has no panel at all.
        ui.set_todo_panel(None);
        assert!(ui.todo_panel_rows(80).is_empty());
        assert_eq!(ui.box_row_count(), 4);
    }

    #[test]
    fn the_panel_folds_and_unfolds() {
        let ui = ChatUi::new(true);
        assert!(!ui.todo_collapsed(), "the panel starts open");
        assert!(
            ui.toggle_todo_collapsed(),
            "folding says the panel is folded"
        );
        assert!(ui.todo_collapsed());
        assert!(!ui.toggle_todo_collapsed(), "unfolding says it is open");
        assert!(!ui.todo_collapsed());
    }

    /// Replays the shape of a turn: the composer is drawn, the agent answers, the
    /// agent opens a task list, and the answer comes again.
    ///
    /// Ignored by default: it is a scenario to render in a terminal, not an
    /// assertion. Run it with `--ignored --nocapture` and read the screen.
    #[test]
    #[ignore]
    fn the_panel_stays_pinned_while_the_conversation_scrolls() {
        let ui = ChatUi::new(true);
        ui.set_status("test-model", 1200, 128_000);
        ui.draw_initial_box();
        ui.emit_above("Luna: working on it");
        ui.set_todo_panel(Some(TodoSnapshot {
            items: vec![
                harness_core::tools::todo::TodoItem {
                    id: 0,
                    text: "read the panel code".to_string(),
                    status: harness_core::tools::todo::TodoStatus::Done,
                },
                harness_core::tools::todo::TodoItem {
                    id: 1,
                    text: "pin it above the composer".to_string(),
                    status: harness_core::tools::todo::TodoStatus::Pending,
                },
            ],
        }));
        ui.emit_above("Luna: the panel is pinned above the composer");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    #[test]
    fn test_format_background_task_line() {
        let ui = ChatUi::new(true);
        ui.set_background_task(Some(BackgroundTaskState {
            job_id: "job_0d24ea53".to_string(),
            task: "Fix search_notes".to_string(),
            current_tool: "read chat.rs".to_string(),
            started_at: std::time::Instant::now() - std::time::Duration::from_secs(1440),
            status: "running".to_string(),
        }));

        let line = ui.format_background_task_line(80).expect("bg line");
        let stripped = dialoguer::console::strip_ansi_codes(&line);
        assert!(stripped.contains("evolver-harness"));
        assert!(stripped.contains("24m"));
        assert!(stripped.contains("read chat.rs"));
        assert!(stripped.contains("[Ctrl+B]"));
    }

    #[test]
    fn status_line_context_is_humanized() {
        let ui = ChatUi::new(true);
        ui.set_status("test-model", 68_000, 272_000);

        let line = ui.format_status_line(80).expect("status line");
        let stripped = dialoguer::console::strip_ansi_codes(&line).to_string();
        assert!(stripped.contains("ctx: 25% (68k/272k)"), "{stripped}");
        assert!(!stripped.contains("68000"), "{stripped}");
    }

    #[test]
    fn humanize_tokens_stays_exact_below_10k() {
        assert_eq!(humanize_tokens(0), "0");
        assert_eq!(humanize_tokens(999), "999");
        assert_eq!(humanize_tokens(9_999), "9999");
        assert_eq!(humanize_tokens(10_000), "10k");
        assert_eq!(humanize_tokens(12_345), "12.3k");
        assert_eq!(humanize_tokens(272_000), "272k");
        assert_eq!(humanize_tokens(1_000_000), "1M");
        assert_eq!(humanize_tokens(2_000_000), "2M");
    }

    #[test]
    fn status_line_displays_antigravity_account_and_quota_format() {
        let ui = ChatUi::new(true);
        ui.set_status("gemini-3.8-flash", 0, 1_000_000);
        ui.set_account_name(Some("daryllvelonio9@gmail.com".to_string()));
        ui.set_quota_display(Some("5h 100% (4h 59m) · w 100% (6d 23h)".to_string()));

        let line = ui.format_status_line(120).expect("status line");
        let stripped = dialoguer::console::strip_ansi_codes(&line).to_string();
        assert!(
            stripped.contains("gemini-3.8-flash · daryllvelonio9@gmail.com"),
            "{stripped}"
        );
        assert!(
            stripped.contains("0/1M · 5h 100% (4h 59m) · w 100% (6d 23h)"),
            "{stripped}"
        );
    }

    #[test]
    fn status_line_handles_zero_context_max() {
        let ui = ChatUi::new(true);
        ui.set_status("test-model", 0, 0);

        let line = ui.format_status_line(80).expect("status line");
        let stripped = dialoguer::console::strip_ansi_codes(&line).to_string();
        assert!(stripped.contains("ctx: 0/0"), "{stripped}");
    }

    #[test]
    fn physical_line_count_calculates_wrapped_rows() {
        assert_eq!(physical_line_count(0, 80), 1);
        assert_eq!(physical_line_count(80, 80), 1);
        assert_eq!(physical_line_count(81, 80), 2);
        assert_eq!(physical_line_count(100, 70), 2);
        assert_eq!(physical_line_count(140, 70), 2);
        assert_eq!(physical_line_count(141, 70), 3);
    }

    #[test]
    fn status_line_never_exceeds_terminal_width_even_when_narrow() {
        let ui = ChatUi::new(true);
        ui.set_status("z-ai/glm-5.3-flash", 3902, 1_000_000);
        ui.set_session_name("long-session-name-here");

        for width in [15, 20, 30, 45, 60, 80, 120] {
            let line = ui.format_status_line(width).expect("status line");
            let stripped = dialoguer::console::strip_ansi_codes(&line);
            assert!(
                stripped.chars().count() <= width,
                "width {width}: line length {} exceeds width: {:?}",
                stripped.chars().count(),
                stripped
            );
        }
    }

    #[test]
    fn background_task_line_never_exceeds_terminal_width_even_when_narrow() {
        let ui = ChatUi::new(true);
        ui.set_background_task(Some(BackgroundTaskState {
            job_id: "job_0d24ea53".to_string(),
            task: "Fix search_notes".to_string(),
            current_tool: "read chat.rs".to_string(),
            started_at: std::time::Instant::now(),
            status: "running".to_string(),
        }));

        for width in [15, 20, 30, 45, 60, 80, 120] {
            let line = ui.format_background_task_line(width).expect("bg line");
            let stripped = dialoguer::console::strip_ansi_codes(&line);
            assert!(
                stripped.chars().count() <= width,
                "width {width}: bg line length {} exceeds width: {:?}",
                stripped.chars().count(),
                stripped
            );
        }
    }

    #[test]
    fn patch_diff_lifecycle_in_live_region() {
        let ui = ChatUi::new(false);
        assert!(ui.live.lock().unwrap().patch_diff.is_none());

        let diff = vec!["- old".to_string(), "+ new".to_string()];
        ui.show_patch_diff(diff.clone());
        assert_eq!(
            ui.live.lock().unwrap().patch_diff.as_deref(),
            Some(diff.as_slice())
        );

        ui.clear_patch_diff();
        assert!(ui.live.lock().unwrap().patch_diff.is_none());
    }
}
