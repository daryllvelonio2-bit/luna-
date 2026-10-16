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
    session_name: String,
}

#[derive(Debug, Clone)]
pub struct BackgroundTaskState {
    pub job_id: String,
    pub task: String,
    pub current_tool: String,
    pub status: String,
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
    pub is_attended: bool,
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
        let (yolo, session_name) = match status.as_ref() {
            Some(s) => (s.yolo, s.session_name.clone()),
            None => (false, String::new()),
        };
        *status = Some(ChatStatus {
            model: model.to_string(),
            context_used,
            context_max,
            yolo,
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

    pub fn set_yolo(&self, on: bool) {
        {
            let mut status = self.status.lock().unwrap();
            if let Some(ref mut s) = *status {
                s.yolo = on;
            }
        }
        self.redraw_status_line();
    }

    pub fn format_background_task_line(&self, width: usize) -> Option<String> {
        let bg = self.bg_task.lock().unwrap();
        let task = bg.as_ref()?;

        let icon = "⚙".cyan().bold();
        let job_tag = format!("[{}]", task.job_id).cyan();
        let right_hint = "[Ctrl+B]".dimmed().bold().to_string();
        let right_visible = dialoguer::console::strip_ansi_codes(&right_hint).to_string();
        let right_len = right_visible.chars().count();

        let bullet = "•".dimmed();
        let tool = if task.current_tool.is_empty() {
            "running".dimmed().to_string()
        } else {
            task.current_tool.dimmed().to_string()
        };

        let left_prefix = format!("{} {} {}", icon, job_tag, task.task.bold());
        let prefix_visible = dialoguer::console::strip_ansi_codes(&left_prefix).to_string();
        let tool_visible = dialoguer::console::strip_ansi_codes(&tool).to_string();

        let fixed_len = prefix_visible.chars().count() + 3 + right_len;
        if fixed_len + tool_visible.chars().count() + 2 <= width {
            let spaces = width.saturating_sub(fixed_len + tool_visible.chars().count());
            Some(format!(
                "{} {} {}{}{}",
                left_prefix,
                bullet,
                tool,
                " ".repeat(spaces),
                right_hint
            ))
        } else if fixed_len + 4 <= width {
            let available_tool = width.saturating_sub(fixed_len + 2);
            let truncated_tool = if available_tool > 3 {
                format!(
                    "{}…",
                    tool_visible.chars().take(available_tool - 1).collect::<String>()
                )
                .dimmed()
                .to_string()
            } else {
                String::new()
            };
            let truncated_vis = dialoguer::console::strip_ansi_codes(&truncated_tool).to_string();
            let used = prefix_visible.chars().count()
                + (if truncated_vis.is_empty() {
                    0
                } else {
                    3 + truncated_vis.chars().count()
                });
            let spaces = width.saturating_sub(used + right_len);
            if truncated_vis.is_empty() {
                Some(format!("{}{}{}", left_prefix, " ".repeat(spaces), right_hint))
            } else {
                Some(format!(
                    "{} {} {}{}{}",
                    left_prefix,
                    bullet,
                    truncated_tool,
                    " ".repeat(spaces),
                    right_hint
                ))
            }
        } else {
            let available = width.saturating_sub(right_len + 1);
            let truncated = format!(
                "{}…",
                prefix_visible.chars().take(available.saturating_sub(1)).collect::<String>()
            );
            let spaces = width.saturating_sub(truncated.chars().count() + right_len);
            Some(format!("{}{}{}", truncated, " ".repeat(spaces), right_hint))
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
        let mut right = format!("ctx: {}/{}", s.context_used, s.context_max);
        if s.yolo {
            right = format!("{} {}", "yolo".yellow().bold(), right);
        }

        let right_visible = dialoguer::console::strip_ansi_codes(&right).to_string();
        let left_len = left.chars().count();
        let right_len = right_visible.chars().count();
        let center_len = center.chars().count();

        if left_len + right_len + 4 > width {
            let available = width.saturating_sub(right_len + 4);
            let left_truncated = if left_len > available {
                let cut = available.saturating_sub(1);
                format!("{}…", left.chars().take(cut).collect::<String>())
            } else {
                left
            };
            let left_len = left_truncated.chars().count();
            let spaces = width.saturating_sub(left_len + right_len);
            Some(format!(
                "{}{}{}",
                left_truncated.dimmed(),
                " ".repeat(spaces),
                right.dimmed()
            ))
        } else if left_len + right_len + center_len + 8 > width {
            let spaces = width - left_len - right_len;
            Some(format!(
                "{}{}{}",
                left.dimmed(),
                " ".repeat(spaces),
                right.dimmed()
            ))
        } else {
            let free = width - left_len - right_len;
            let left_gap = ((free - center_len) / 2).max(1);
            let right_gap = free - center_len - left_gap;
            Some(format!(
                "{}{}{}{}{}",
                left.dimmed(),
                " ".repeat(left_gap),
                center.dimmed().bold(),
                " ".repeat(right_gap),
                right.dimmed()
            ))
        }
    }

    pub fn erase_live_and_box(&self, previous_live_rows: usize) {
        if !self.is_attended {
            return;
        }
        self.box_drawn.store(false, Ordering::SeqCst);
        let total = previous_live_rows + self.box_row_count();
        print!("\x1b[1B");
        for i in 0..total {
            print!("\r\x1b[2K");
            if i + 1 < total {
                print!("\x1b[1A");
            }
        }
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
        if let Some(bg_line) = self.format_background_task_line(width) {
            println!("{}", bg_line);
        }
        if let Some(status_str) = self.format_status_line(width) {
            println!("{}", status_str);
        }
        println!("{}", divider);
        println!();
        println!("{}", divider);
        print!("\x1b[2A\r{}", self.prompt_row(buf));
        let _ = io::stdout().flush();
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
        let width = get_terminal_width();
        if let Some(status_str) = self.format_status_line(width) {
            let buf = self.input_buf.lock().unwrap();
            print!("\x1b[2A\r\x1b[2K{}\x1b[2B\r{}", status_str, self.prompt_row(&buf));
            let _ = io::stdout().flush();
        }
    }

    pub fn redraw_background_task_line(&self) {
        if !self.is_attended || !self.box_drawn.load(Ordering::SeqCst) || !self.has_background_task() {
            return;
        }
        let width = get_terminal_width();
        if let Some(bg_str) = self.format_background_task_line(width) {
            let buf = self.input_buf.lock().unwrap();
            print!("\x1b[3A\r\x1b[2K{}\x1b[3B\r{}", bg_str, self.prompt_row(&buf));
            let _ = io::stdout().flush();
        }
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
