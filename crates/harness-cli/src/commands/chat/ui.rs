use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use colored::Colorize;
use dialoguer::console::Term;

use super::render::{
    activity_line, needs_separator, reasoning_rows, tool_activity_detail,
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

/// A tool call currently in flight, as far as the animation is concerned.
#[derive(Clone)]
struct Activity {
    detail: Option<String>,
    started: std::time::Instant,
    frame: usize,
}

/// The in-place region between the transcript and the composer: the tail of the reasoning as it
/// streams in, and the animated line for a running tool.
#[derive(Default)]
struct LiveRegion {
    reasoning: String,
    activity: Option<Activity>,
    rows: Vec<String>,
}

#[derive(Clone)]
struct ChatStatus {
    model: String,
    context_used: u32,
    context_max: u32,
    yolo: bool,
    plan_mode: bool,
    session_name: String,
}

#[derive(Debug, Clone)]
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
    status_line_len: usize,
    divider_len: usize,
    prompt_len: usize,
    live_rows_lens: Vec<usize>,
}

pub fn visible_width(s: &str) -> usize {
    dialoguer::console::strip_ansi_codes(s).chars().count()
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
    box_drawn: Arc<AtomicBool>,
    drawn_metrics: Arc<Mutex<Option<BoxDrawnMetrics>>>,
    pub is_attended: bool,
}

/// Shortens a token count for the status line: 0→"0", 999→"999", 12_345→"12.3k", 272_000→"272k".
/// Sub-k counts stay exact because that is the range where the digits still matter.
fn humanize_tokens(count: u32) -> String {
    if count >= 10_000 {
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

    pub fn background_task(&self) -> Option<BackgroundTaskState> {
        self.bg_task.lock().unwrap().clone()
    }

    pub fn has_background_task(&self) -> bool {
        self.bg_task.lock().unwrap().is_some()
    }

    pub fn box_row_count(&self) -> usize {
        4 + if self.has_background_task() { 1 } else { 0 }
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

    pub fn start_activity(&self, name: &str, arguments: &serde_json::Value) {
        let mut live = self.live.lock().unwrap();
        live.activity = Some(Activity {
            detail: tool_activity_detail(name, arguments),
            started: std::time::Instant::now(),
            frame: 0,
        });
    }

    pub fn stop_activity(&self) {
        self.live.lock().unwrap().activity = None;
    }

    pub fn clear_live_state(&self) {
        let mut live = self.live.lock().unwrap();
        live.reasoning.clear();
        live.rows.clear();
        live.activity = None;
    }

    pub fn clear_live_and_box(&self) {
        self.clear_box_on_exit();
    }

    pub fn set_status(&self, model: &str, context_used: u32, context_max: u32) {
        let mut status = self.status.lock().unwrap();
        let (yolo, plan_mode, session_name) = match status.as_ref() {
            Some(s) => (s.yolo, s.plan_mode, s.session_name.clone()),
            None => (false, false, String::new()),
        };
        *status = Some(ChatStatus {
            model: model.to_string(),
            context_used,
            context_max,
            yolo,
            plan_mode,
            session_name,
        });
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
        let elapsed = format_elapsed(task.started_at.elapsed()).dimmed().to_string();
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
                    left_visible.chars().take(available.saturating_sub(1)).collect::<String>()
                );
                let spaces = width.saturating_sub(truncated.chars().count() + right_len);
                Some(format!("{}{}{}", truncated, " ".repeat(spaces), right_hint))
            } else {
                let cut = width.saturating_sub(1);
                Some(format!("{}…", left_visible.chars().take(cut).collect::<String>()))
            }
        }
    }

    pub fn format_status_line(&self, width: usize) -> Option<String> {
        let status = self.status.lock().unwrap();
        let s = status.as_ref()?;

        let left = s.model.clone();
        let center = if s.session_name.is_empty() {
            String::new()
        } else {
            format!("[{}]", s.session_name)
        };
        // A raw five-to-six-digit token count reads as noise mid-session; the percentage is the
        // glanceable fact ("how full is my window") and the pair keeps the absolute scale.
        let mut right = if s.context_max > 0 {
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
            let live_rows: usize = m
                .live_rows_lens
                .iter()
                .map(|&len| physical_line_count(len, current_width))
                .sum();

            let below = bottom_rows;
            let above = prompt_rows.saturating_sub(1)
                + top_divider_rows
                + status_rows
                + bg_rows
                + live_rows;
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
                    activity.detail.as_deref(),
                    activity.started.elapsed(),
                ));
            }
            rows
        };

        if rows.is_empty() && self.live.lock().unwrap().rows.is_empty() {
            return;
        }

        self.set_live(rows);
    }

    pub fn draw_box(&self, width: usize, buf: &str) {
        let divider = "─".repeat(width).dimmed().to_string();
        let bg_line = self.format_background_task_line(width);
        let status_str = self.format_status_line(width);
        let prompt_str = self.prompt_row(buf);

        if let Some(ref bg) = bg_line {
            println!("{}", bg);
        }
        if let Some(ref st) = status_str {
            println!("{}", st);
        }
        println!("{}", divider);
        println!();
        println!("{}", divider);
        print!("\x1b[2A\r{}", prompt_str);
        let _ = io::stdout().flush();

        let live_rows = self.live.lock().unwrap().rows.clone();
        let live_rows_lens = live_rows.iter().map(|r| visible_width(r)).collect();

        *self.drawn_metrics.lock().unwrap() = Some(BoxDrawnMetrics {
            width,
            bg_line_len: bg_line.as_ref().map(|s| visible_width(s)).unwrap_or(0),
            status_line_len: status_str.as_ref().map(|s| visible_width(s)).unwrap_or(0),
            divider_len: width,
            prompt_len: visible_width(&prompt_str),
            live_rows_lens,
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
                    activity.detail.as_deref(),
                    activity.started.elapsed(),
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
        if !self.is_attended || !self.box_drawn.load(Ordering::SeqCst) || !self.has_background_task() {
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
}
