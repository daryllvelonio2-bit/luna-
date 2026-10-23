//! The animation driver.
//!
//! Rendering and *cadence* are separate concerns: this module knows how to emit a centered line one
//! character at a time, and a [`Pacer`] decides how long each character waits. Production passes
//! [`RealPacer`] (a thread sleep); the tests pass [`InstantPacer`] or a recording pacer, so the real
//! code path is exercised without spending three seconds per assertion.
//!
//! Cadence is jittered inside a fixed window rather than sampled from an RNG: a small linear
//! congruential sequence seeded from a constant makes the typing feel human while keeping the
//! timing — and therefore the test that bounds it — deterministic.
//!
//! The column count is re-read through a closure on every line, so a window resize mid-briefing is
//! picked up instead of leaving the rest of the frame centred on the old geometry.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{queue, QueueableCommand};

use crate::screen::{self, Size};
use crate::style::{Line, Span};

/// Shortest per-character delay.
pub const MIN_TYPE_MS: u64 = 12;
/// Longest per-character delay.
pub const MAX_TYPE_MS: u64 = 22;
/// Per-character delay for chrome (frame borders, headers), which should read as a boot sequence
/// rather than as prose.
pub const QUICK_TYPE_MS: u64 = 4;
/// Shortest pause between paragraphs.
pub const MIN_PARAGRAPH_MS: u64 = 180;
/// Longest pause between paragraphs.
pub const MAX_PARAGRAPH_MS: u64 = 380;
/// A clock. The only thing that makes an animation slow, which is exactly why it is injectable.
pub trait Pacer {
    fn wait(&mut self, delay: Duration);
}

/// Sleeps for real. Used by the interactive entry point.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealPacer;

impl Pacer for RealPacer {
    fn wait(&mut self, delay: Duration) {
        std::thread::sleep(delay);
    }
}

/// Sleeps not at all. Used by tests and by any non-interactive caller.
#[derive(Debug, Default, Clone, Copy)]
pub struct InstantPacer;

impl Pacer for InstantPacer {
    fn wait(&mut self, _delay: Duration) {}
}

/// Types centered lines into a writer, one character at a time, and draws styled blocks.
pub struct Typewriter<'a, W: Write, P: Pacer> {
    out: &'a mut W,
    pacer: &'a mut P,
    size: &'a dyn Fn() -> Size,
    seed: u64,
}

impl<'a, W: Write, P: Pacer> Typewriter<'a, W, P> {
    pub fn new(out: &'a mut W, pacer: &'a mut P, size: &'a dyn Fn() -> Size) -> Self {
        Self {
            out,
            pacer,
            size,
            // Any odd constant works; the sequence only has to look irregular, not be random.
            seed: 0x2545_F491_4F6C_DD1D,
        }
    }

    /// The terminal geometry, re-read through the closure the caller supplied so a resize mid-flow
    /// is picked up instead of leaving the rest of the frame centred on the old geometry.
    pub fn size(&self) -> Size {
        (self.size)()
    }

    /// The current terminal width.
    pub fn columns(&self) -> u16 {
        self.size().columns
    }

    /// Blank the screen and return the cursor to the top-left.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        queue!(self.out, Clear(ClearType::All), MoveTo(0, 0))?;
        self.out.flush()
    }

    /// Hide the cursor, so a character-at-a-time redraw does not flicker.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        queue!(self.out, Hide)?;
        self.out.flush()
    }

    /// Restore the cursor.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        queue!(self.out, Show)?;
        self.out.flush()
    }

    /// Type one centered line, then end the line.
    pub fn type_line(&mut self, text: &str, color: Option<Color>) -> io::Result<()> {
        let pad = screen::center_pad(self.columns() as usize, screen::display_width(text));
        self.type_line_at(text, pad, color)
    }

    /// Type one line at a fixed left padding, then end the line.
    ///
    /// A paragraph types its lines through this so they share one offset: centring each line of a
    /// wrapped paragraph on its own would give the block a ragged left edge and read as a mistake.
    pub fn type_line_at(&mut self, text: &str, pad: usize, color: Option<Color>) -> io::Result<()> {
        if pad > 0 {
            write!(self.out, "{}", " ".repeat(pad))?;
        }
        if let Some(color) = color {
            self.out.queue(SetForegroundColor(color))?;
        }
        for ch in text.chars() {
            write!(self.out, "{}", ch)?;
            self.out.flush()?;
            let delay = self.char_delay();
            self.pacer.wait(delay);
        }
        if color.is_some() {
            self.out.queue(ResetColor)?;
        }
        writeln!(self.out)?;
        self.out.flush()
    }

    /// Type a paragraph: every line wrapped and centred as one block, then a dramatic pause.
    pub fn type_paragraph(&mut self, text: &str, color: Option<Color>) -> io::Result<()> {
        let lines = screen::wrap(text, screen::body_limit(self.columns()));
        let pad = screen::center_pad(
            self.columns() as usize,
            screen::block_width(lines.iter().map(String::as_str)),
        );
        for line in &lines {
            self.type_line_at(line, pad, color)?;
        }
        self.pause_between_paragraphs();
        Ok(())
    }

    /// Print one centered line with no typing delay: a confirmation must not make the user wait.
    pub fn write_centered(&mut self, text: &str, color: Option<Color>) -> io::Result<()> {
        self.pad_line(text)?;
        if let Some(color) = color {
            self.out.queue(SetForegroundColor(color))?;
        }
        writeln!(self.out, "{}", text)?;
        if color.is_some() {
            self.out.queue(ResetColor)?;
        }
        self.out.flush()
    }

    /// Print a centered prompt and leave the cursor on the same line, so the answer is typed after
    /// it. No trailing newline.
    pub fn write_prompt(&mut self, text: &str) -> io::Result<()> {
        self.pad_line(text)?;
        write!(self.out, "{}", text)?;
        self.out.flush()
    }

    /// Print a block of already-laid-out lines.
    pub fn write_lines(&mut self, lines: &[String], color: Option<Color>) -> io::Result<()> {
        for line in lines {
            self.write_centered(line, color)?;
        }
        Ok(())
    }

    /// Draw one styled line, ending the line.
    ///
    /// The line is written *as placed*: centring is the layout's job ([`crate::menu`] returns lines
    /// that already carry their own offset), and a writer that re-centred them would shrink a grid
    /// whose last column was trimmed back to its own middle.
    pub fn write_line(&mut self, line: &Line) -> io::Result<()> {
        self.emit_spans(&line.spans)?;
        writeln!(self.out)?;
        self.out.flush()
    }

    /// Draw a block of styled lines.
    pub fn write_block(&mut self, lines: &[Line]) -> io::Result<()> {
        for line in lines {
            self.write_line(line)?;
        }
        Ok(())
    }

    /// Draw a styled block one character at a time at [`QUICK_TYPE_MS`]: chrome, not prose.
    pub fn type_block_quick(&mut self, lines: &[Line]) -> io::Result<()> {
        for line in lines {
            for span in &line.spans {
                self.style_on(span)?;
                for ch in span.text.chars() {
                    write!(self.out, "{}", ch)?;
                    self.out.flush()?;
                    self.pacer.wait(Duration::from_millis(QUICK_TYPE_MS));
                }
                self.style_off(span)?;
            }
            writeln!(self.out)?;
            self.out.flush()?;
        }
        Ok(())
    }

    /// Draw a placed prompt and leave the cursor on the same line, so the answer is typed after it.
    pub fn write_prompt_line(&mut self, line: &Line) -> io::Result<()> {
        self.emit_spans(&line.spans)?;
        self.out.flush()
    }

    /// Write a span run, honouring each span's colour and weight.
    fn emit_spans(&mut self, spans: &[Span]) -> io::Result<()> {
        for span in spans {
            self.style_on(span)?;
            write!(self.out, "{}", span.text)?;
            self.style_off(span)?;
        }
        Ok(())
    }

    /// Apply a span's style.
    fn style_on(&mut self, span: &Span) -> io::Result<()> {
        if let Some(color) = span.color {
            self.out.queue(SetForegroundColor(color))?;
        }
        if span.bold {
            self.out.queue(SetAttribute(Attribute::Bold))?;
        }
        Ok(())
    }

    /// Undo a span's style. Both halves are guarded by the same conditions as [`Self::style_on`],
    /// so an unstyled span never emits an escape sequence at all.
    fn style_off(&mut self, span: &Span) -> io::Result<()> {
        if span.bold {
            self.out.queue(SetAttribute(Attribute::NormalIntensity))?;
        }
        if span.color.is_some() {
            self.out.queue(ResetColor)?;
        }
        Ok(())
    }

    /// An empty line.
    pub fn blank(&mut self) -> io::Result<()> {
        writeln!(self.out)?;
        self.out.flush()
    }

    /// A pause between paragraphs.
    pub fn pause_between_paragraphs(&mut self) {
        let delay = Duration::from_millis(self.jitter(MIN_PARAGRAPH_MS, MAX_PARAGRAPH_MS));
        self.pacer.wait(delay);
    }

    /// Write the left padding that centers this line, refreshing the width first.
    fn pad_line(&mut self, text: &str) -> io::Result<()> {
        self.pad_width(screen::display_width(text))
    }

    /// Write the padding that centres a line `width` columns wide.
    ///
    /// A blank line is not padded: centring nothing would emit half a screen of spaces.
    fn pad_width(&mut self, width: usize) -> io::Result<()> {
        if width == 0 {
            return Ok(());
        }
        let pad = screen::center_pad(self.columns() as usize, width);
        if pad > 0 {
            write!(self.out, "{}", " ".repeat(pad))?;
        }
        Ok(())
    }

    /// Next per-character delay, inside `MIN_TYPE_MS..=MAX_TYPE_MS`.
    fn char_delay(&mut self) -> Duration {
        Duration::from_millis(self.jitter(MIN_TYPE_MS, MAX_TYPE_MS))
    }

    /// A deterministic value in `min..=max` from the internal LCG.
    fn jitter(&mut self, min: u64, max: u64) -> u64 {
        self.seed = self
            .seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let spread = max - min;
        min + ((self.seed >> 33) % (spread + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{ACCENT_COLOR, BORDER_COLOR, CREST_COLOR};

    /// Records every delay instead of sleeping, so a test can bound the cadence.
    #[derive(Debug, Default)]
    struct RecordingPacer {
        delays: Vec<Duration>,
    }

    impl Pacer for RecordingPacer {
        fn wait(&mut self, delay: Duration) {
            self.delays.push(delay);
        }
    }

    fn width(columns: u16) -> impl Fn() -> Size {
        move || Size { columns, rows: 24 }
    }

    /// Drop ANSI sequences, leaving what the user actually sees.
    fn strip_escapes(bytes: &[u8]) -> String {
        let text = String::from_utf8_lossy(bytes);
        let mut out = String::new();
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\u{1b}' && chars.peek() == Some(&'[') {
                chars.next();
                for escape in chars.by_ref() {
                    if escape.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            out.push(ch);
        }
        out
    }

    #[test]
    fn typing_emits_the_text_centered() {
        let mut out = Vec::new();
        let columns = width(40);
        let mut pacer = InstantPacer;
        let mut writer = Typewriter::new(&mut out, &mut pacer, &columns);
        writer.type_line("Luna", None).expect("writes");

        assert_eq!(strip_escapes(&out), "                  Luna\n");
    }

    #[test]
    fn every_character_waits_inside_the_declared_window() {
        let mut out = Vec::new();
        let columns = width(80);
        let mut pacer = RecordingPacer::default();
        let text = "Hi! I am Luna.";
        {
            let mut writer = Typewriter::new(&mut out, &mut pacer, &columns);
            writer.type_line(text, None).expect("writes");
        }

        assert_eq!(pacer.delays.len(), text.chars().count());
        for delay in &pacer.delays {
            let ms = delay.as_millis() as u64;
            assert!(
                (MIN_TYPE_MS..=MAX_TYPE_MS).contains(&ms),
                "delay outside the typing window: {ms}ms"
            );
        }
    }

    #[test]
    fn the_cadence_is_irregular_but_deterministic() {
        let delays = |()| {
            let mut out = Vec::new();
            let columns = width(80);
            let mut pacer = RecordingPacer::default();
            {
                let mut writer = Typewriter::new(&mut out, &mut pacer, &columns);
                writer
                    .type_line("a steady cadence would be uncanny", None)
                    .expect("writes");
            }
            pacer.delays
        };

        assert_ne!(delays(()), Vec::new());
        assert_ne!(
            delays(()).iter().map(|d| d.as_millis()).collect::<Vec<_>>(),
            vec![MIN_TYPE_MS as u128; 36],
            "every delay is the minimum: the jitter is not applied"
        );
        assert_eq!(
            delays(()),
            delays(()),
            "the same seed must replay identically"
        );
    }

    #[test]
    fn paragraph_pauses_land_in_the_dramatic_window() {
        let mut out = Vec::new();
        let columns = width(80);
        let mut pacer = RecordingPacer::default();
        {
            let mut writer = Typewriter::new(&mut out, &mut pacer, &columns);
            for _ in 0..50 {
                writer.pause_between_paragraphs();
            }
        }

        assert_eq!(pacer.delays.len(), 50);
        for delay in &pacer.delays {
            let ms = delay.as_millis() as u64;
            assert!(
                (MIN_PARAGRAPH_MS..=MAX_PARAGRAPH_MS).contains(&ms),
                "pause outside the paragraph window: {ms}ms"
            );
        }
    }

    #[test]
    fn a_styled_line_is_centered_by_display_width_and_reset() {
        let mut out = Vec::new();
        let columns = width(20);
        let mut pacer = InstantPacer;
        let mut writer = Typewriter::new(&mut out, &mut pacer, &columns);
        writer.type_line("🌙", Some(CREST_COLOR)).expect("writes");

        assert_eq!(strip_escapes(&out), "         🌙\n");
        let raw = String::from_utf8_lossy(&out);
        assert!(raw.contains("\u{1b}[38;"), "the crest is not styled");
        // Whatever sequence the terminal layer picks, the styled run must be closed by an SGR reset
        // (the terminal would otherwise keep the crest's colour for everything after it).
        assert!(
            raw.trim_end_matches('\n').ends_with('m'),
            "the style is not reset: {raw:?}"
        );
    }

    #[test]
    fn a_styled_block_keeps_each_span_and_resets_it() {
        let mut out = Vec::new();
        let columns = width(30);
        let mut pacer = InstantPacer;
        let mut writer = Typewriter::new(&mut out, &mut pacer, &columns);
        let line = Line::of(Span::strong("Luna", CREST_COLOR))
            .then_muted(" · ")
            .with(Span::colored("1", ACCENT_COLOR));
        writer.write_line(&line).expect("writes");

        assert_eq!(strip_escapes(&out), "Luna · 1\n");
        let raw = String::from_utf8_lossy(&out);
        assert!(raw.contains("\u{1b}[1m"), "the name is not bold");
        // Two colours on one line: both must be opened, so two resets are the minimum.
        assert!(
            raw.matches("\u{1b}[0m").count() >= 2,
            "styles leak between spans: {raw:?}"
        );
    }

    #[test]
    fn a_placed_line_is_written_where_the_layout_put_it() {
        // The layout centres; the writer must not centre again, or a trimmed grid row collapses
        // back to its own middle.
        let mut out = Vec::new();
        let columns = width(40);
        let mut pacer = InstantPacer;
        let mut writer = Typewriter::new(&mut out, &mut pacer, &columns);
        writer
            .write_line(&Line::of(Span::new("  placed")))
            .expect("writes");
        assert_eq!(strip_escapes(&out), "  placed\n");
    }

    #[test]
    fn a_prompt_line_leaves_the_cursor_on_the_same_line() {
        let mut out = Vec::new();
        let columns = width(20);
        let mut pacer = InstantPacer;
        let mut writer = Typewriter::new(&mut out, &mut pacer, &columns);
        writer
            .write_prompt_line(&Line::of(Span::colored("1-10 ▸ ", ACCENT_COLOR)))
            .expect("writes");

        let text = strip_escapes(&out);
        assert_eq!(text, "1-10 ▸ ");
        assert!(!text.ends_with('\n'), "the prompt ended the line");
    }

    #[test]
    fn quick_typed_chrome_waits_far_less_than_prose() {
        let mut out = Vec::new();
        let columns = width(40);
        let mut pacer = RecordingPacer::default();
        {
            let mut writer = Typewriter::new(&mut out, &mut pacer, &columns);
            writer
                .type_block_quick(&[Line::of(Span::colored("╭────╮", BORDER_COLOR))])
                .expect("writes");
        }
        assert_eq!(pacer.delays.len(), 6);
        for delay in &pacer.delays {
            assert_eq!(delay.as_millis() as u64, QUICK_TYPE_MS);
        }
        const { assert!(QUICK_TYPE_MS < MIN_TYPE_MS) };
    }

    #[test]
    fn a_wrapped_paragraph_keeps_one_left_edge() {
        let mut out = Vec::new();
        let columns = width(60);
        let mut pacer = InstantPacer;
        {
            let mut writer = Typewriter::new(&mut out, &mut pacer, &columns);
            writer
                .type_paragraph(
                    "I retain persistent memory across sessions, recalling your project layouts, past mistakes and decisions.",
                    None,
                )
                .expect("writes");
        }

        let text = strip_escapes(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() > 1, "the paragraph did not wrap: {text:?}");
        let pads: Vec<usize> = lines
            .iter()
            .map(|line| line.len() - line.trim_start().len())
            .collect();
        assert!(
            pads.iter().all(|pad| *pad == pads[0]),
            "the paragraph has a ragged left edge: {pads:?}"
        );
    }

    #[test]
    fn clearing_hides_and_restores_the_cursor() {
        let mut out = Vec::new();
        let columns = width(20);
        let mut pacer = InstantPacer;
        let mut writer = Typewriter::new(&mut out, &mut pacer, &columns);
        writer.hide_cursor().expect("hide");
        writer.clear_screen().expect("clear");
        writer.show_cursor().expect("show");

        let raw = String::from_utf8_lossy(&out);
        assert!(raw.contains("\u{1b}[?25l"), "cursor not hidden");
        assert!(raw.contains("\u{1b}[2J"), "screen not cleared");
        assert!(raw.contains("\u{1b}[?25h"), "cursor not restored");
    }
}
