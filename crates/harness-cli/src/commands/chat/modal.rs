//! The screen an interactive prompt takes over.
//!
//! Approval, clarify and the sudo prompt are all asked mid-turn, which means every one of them used
//! to be drawn *under* whatever the turn had already printed — the process header, the streamed
//! reasoning, the composer. The question had to compete with the agent's own scratch work, and the
//! reader could not tell at a glance which text was being asked of them. A modal takes the whole
//! screen instead, centred, with nothing beside it.
//!
//! The screen is the alternate buffer, so this is not a redraw: the transcript is left untouched
//! and comes back — scrollback included — the moment the modal is done.

use std::io::{self, Write};

use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};

use super::ui::{get_terminal_height, get_terminal_width};

/// Holds the alternate screen for as long as a prompt is up.
///
/// Leaving restores the primary buffer rather than rebuilding it, which is the whole reason not to
/// clear the screen here: a rebuilt transcript would lose the scrollback above it.
pub(crate) struct ModalScreen {
    /// Whether the switch succeeded. A terminal that refuses it gets the prompt on the primary
    /// buffer, which is where it was drawn before this existed; what must never happen is
    /// *entering* the alternate screen on the way out.
    entered: bool,
}

impl ModalScreen {
    pub(crate) fn enter() -> Self {
        Self {
            entered: execute!(io::stdout(), EnterAlternateScreen, Hide).is_ok(),
        }
    }
}

impl Drop for ModalScreen {
    fn drop(&mut self) {
        if self.entered {
            let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
        }
    }
}

/// The frame that puts `rendered` in the middle of an otherwise empty screen.
///
/// The whole frame is written out rather than erased line by line: a box changes height as its
/// question wraps and as typing mode opens, and a partial repaint would leave the tail of the
/// previous frame behind it. The last row carries no newline, so a frame that fills the screen
/// cannot scroll it and shift itself off centre.
///
/// Centring is on the widest line, so a bordered box (whose rows are all the same width) is centred
/// as a box and a plain block of prompt text is centred as a block.
pub(crate) fn centred_frame(
    rendered: &str,
    terminal_width: usize,
    terminal_height: usize,
) -> String {
    let lines: Vec<&str> = rendered.lines().collect();
    // Cells, not chars: the borders land on the terminal, and a wide glyph in a question would
    // otherwise pull the frame off centre by a column.
    let block_width = lines
        .iter()
        .map(|line| dialoguer::console::measure_text_width(line))
        .max()
        .unwrap_or(0);
    let left = terminal_width.saturating_sub(block_width) / 2;
    let top = terminal_height.saturating_sub(lines.len()) / 2;

    let mut frame = String::from("\x1b[H\x1b[2J");
    for _ in 0..top {
        frame.push('\n');
    }
    for (index, line) in lines.iter().enumerate() {
        frame.push_str(&" ".repeat(left));
        frame.push_str(line);
        if index + 1 < lines.len() {
            frame.push('\n');
        }
    }
    frame
}

/// Draw `rendered` centred on the screen the modal owns, replacing whatever was there.
///
/// Called once for the first frame and again on every repaint. There is nothing to preserve on this
/// screen, so each frame is written whole.
pub(crate) fn paint_centred(rendered: &str) {
    print!(
        "{}",
        centred_frame(rendered, get_terminal_width(), get_terminal_height())
    );
    let _ = io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The swap is what keeps the transcript out of the question's way, and what makes leaving put
    /// back exactly what was there instead of a rebuilt screen.
    #[test]
    fn a_modal_is_drawn_on_the_alternate_screen() {
        let captured = crate::commands::chat::ui::test_support::capture_stdout(|| {
            let screen = ModalScreen::enter();
            drop(screen);
        });

        assert!(
            captured.contains("\x1b[?1049h"),
            "the alternate screen is entered: {captured:?}"
        );
        assert!(
            captured.contains("\x1b[?1049l"),
            "and left again on the way out: {captured:?}"
        );
        assert!(
            captured.find("\x1b[?1049h") < captured.find("\x1b[?1049l"),
            "in that order: {captured:?}"
        );
        assert!(
            captured.contains("\x1b[?25l") && captured.contains("\x1b[?25h"),
            "the cursor is hidden over the modal and shown again after it: {captured:?}"
        );
    }

    #[test]
    fn a_frame_is_centred_with_nothing_else_on_the_screen() {
        let block = "a line\nanother wider line\nshort";
        let frame = centred_frame(block, 40, 11);

        assert!(
            frame.starts_with("\x1b[H\x1b[2J"),
            "the screen is cleared before the frame is drawn: {frame:?}"
        );
        assert!(
            !frame.ends_with('\n'),
            "a trailing newline on the last row scrolls a full frame off centre"
        );

        let lines: Vec<&str> = frame
            .trim_start_matches("\x1b[H\x1b[2J")
            .split('\n')
            .collect();
        let blank_rows = lines.iter().take_while(|line| line.is_empty()).count();
        assert_eq!(blank_rows, (11 - 3) / 2, "the frame is centred vertically");
        assert_eq!(
            lines.len(),
            blank_rows + 3,
            "the screen holds the frame and nothing else"
        );

        // Centred on the widest line, so a block with ragged rows stays a block.
        let left = (40 - "another wider line".len()) / 2;
        for line in &lines[blank_rows..] {
            assert_eq!(
                line.chars().take_while(|c| *c == ' ').count(),
                left,
                "every row is indented to the same column: {line:?}"
            );
        }
        assert!(frame.contains("short"), "no line is cut to fit the screen");
    }

    /// A screen smaller than the frame must not panic or produce a negative indent: the frame is
    /// still written, flush left.
    #[test]
    fn a_frame_larger_than_the_screen_is_still_drawn() {
        let frame = centred_frame("wide content that does not fit", 10, 1);
        assert!(frame.contains("wide content"), "{frame:?}");
        assert!(
            !frame.trim_start_matches("\x1b[H\x1b[2J").starts_with(' '),
            "there is no room to indent: {frame:?}"
        );
    }
}
