use colored::Colorize;

use super::super::render::truncate_to_cells;

/// A tool call currently in flight, as far as the animation is concerned.
#[derive(Clone)]
pub(super) struct Activity {
    /// The formatted call line, shown from the instant the tool is invoked — the transcript
    /// carries the same line once it finishes, so the user watches the call, not just its result.
    pub(super) call: String,
    pub(super) detail: Option<String>,
    pub(super) started: std::time::Instant,
    pub(super) frame: usize,
}

/// How many lines of a running command's output the live region shows by default.
///
/// A tail, not a log: the composer has to stay usable while a build prints thousands of lines. The
/// full output still reaches the model with the result; this is only the live view.
pub(super) const TOOL_TAIL_LINES: usize = 8;

/// How many lines the tail keeps in memory, so Ctrl+O can expand it without re-reading the child.
pub(super) const TOOL_TAIL_BUFFER_LINES: usize = 200;

/// Longest single line the tail keeps before forcing a break. A progress bar that never emits a
/// newline must not grow the buffer for the length of the command.
const TOOL_TAIL_LINE_CHARS: usize = 2048;

/// The newest lines of a running command's output.
///
/// Chunks arrive at arbitrary boundaries, so the line being written is held separately until a
/// newline completes it. A lone `\r` returns the cursor to column 0 without clearing the line:
/// the next printable character overwrites it, which is what makes a progress bar one row instead
/// of one row per update.
#[derive(Default)]
pub(super) struct ToolTail {
    lines: std::collections::VecDeque<String>,
    /// The line currently being written.
    current: String,
    /// A `\r` was seen and the next printable character should overwrite [`Self::current`].
    pending_cr: bool,
    /// Lines evicted from the front of the bounded buffer, for the "earlier lines" row.
    dropped: usize,
    escape: Escape,
}

/// Where a chunk sits inside an ANSI escape sequence.
///
/// A sequence can be split across two reads, so the state has to outlive a chunk: dropping only
/// the ESC byte would leave `[31m` behind and paint that into the composer as text.
#[derive(Default, Clone, Copy)]
enum Escape {
    #[default]
    None,
    Esc,
    Csi,
    Osc,
    OscEsc,
}

impl ToolTail {
    pub(super) fn push(&mut self, chunk: &str) {
        let mut chars = chunk.chars().peekable();
        while let Some(ch) = chars.next() {
            match self.escape {
                Escape::None => match ch {
                    '\x1b' => self.escape = Escape::Esc,
                    '\n' => self.break_line(),
                    '\r' => {
                        // CRLF is one break, not two. A lone `\r` leaves the line on screen until
                        // the next printable character overwrites it, matching the terminal.
                        if chars.peek() == Some(&'\n') {
                            chars.next();
                            self.break_line();
                        } else {
                            self.pending_cr = true;
                        }
                    }
                    '\t' => {
                        self.overwrite_if_pending();
                        self.current.push_str("    ");
                    }
                    c if c.is_control() => {}
                    c => {
                        self.overwrite_if_pending();
                        self.current.push(c);
                    }
                },
                Escape::Esc => {
                    self.escape = match ch {
                        '[' => Escape::Csi,
                        ']' => Escape::Osc,
                        _ => Escape::None,
                    };
                }
                Escape::Csi => {
                    // CSI ends on the first byte in the final-byte range (`m`, `K`, `H`, …).
                    if ('@'..='~').contains(&ch) {
                        self.escape = Escape::None;
                    }
                }
                Escape::Osc => match ch {
                    '\x07' => self.escape = Escape::None,
                    '\x1b' => self.escape = Escape::OscEsc,
                    _ => {}
                },
                Escape::OscEsc => {
                    self.escape = if ch == '\\' {
                        Escape::None
                    } else {
                        Escape::Osc
                    };
                }
            }
            if self.current.len() >= TOOL_TAIL_LINE_CHARS {
                self.break_line();
            }
        }
    }

    fn overwrite_if_pending(&mut self) {
        if self.pending_cr {
            self.current.clear();
            self.pending_cr = false;
        }
    }

    fn break_line(&mut self) {
        self.pending_cr = false;
        let line = std::mem::take(&mut self.current);
        let line = line.trim_end().to_string();
        // A run of blank lines (a CRLF split across chunks, a banner) costs rows and says nothing.
        if line.is_empty() && self.lines.back().is_none_or(|last| last.is_empty()) {
            return;
        }
        self.lines.push_back(line);
        while self.lines.len() > TOOL_TAIL_BUFFER_LINES {
            self.lines.pop_front();
            self.dropped += 1;
        }
    }

    pub(super) fn clear(&mut self) {
        self.lines.clear();
        self.current.clear();
        self.pending_cr = false;
        self.dropped = 0;
        self.escape = Escape::None;
    }

    /// The tail as composer rows, newest last, at most `limit` physical rows wide.
    ///
    /// When more lines exist than the limit allows, the oldest visible row is replaced by a count
    /// of what is hidden, so the tail never claims to be the whole story.
    pub(super) fn rows(&self, width: usize, limit: usize) -> Vec<String> {
        let budget = width.saturating_sub(4).max(8);
        let mut all: Vec<String> = self
            .lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| truncate_to_cells(line, budget))
            .collect();
        let current = self.current.trim_end();
        if !current.is_empty() {
            all.push(truncate_to_cells(current, budget));
        }

        let limit = limit.max(1);
        let content_limit = if all.len() > limit { limit - 1 } else { limit };
        let start = all.len().saturating_sub(content_limit);
        let hidden = self.dropped + start;

        let mut rows = Vec::with_capacity(all.len().saturating_sub(start) + 1);
        if hidden > 0 {
            rows.push(format!(
                "  {} {}",
                "…".dimmed(),
                format!(
                    "{hidden} earlier line{}",
                    if hidden == 1 { "" } else { "s" }
                )
                .dimmed()
            ));
        }
        for line in &all[start..] {
            rows.push(format!("  {} {}", "│".dimmed(), line.dimmed()));
        }
        rows
    }
}
