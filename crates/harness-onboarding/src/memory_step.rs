//! Phase 2b: who writes memory.
//!
//! The second question the first-run flow asks. Both answers write the same records through the same
//! tools; they differ in *when* the decision is made:
//!
//! - **Session** (the default) is the original design: the agent decides mid-turn and calls
//!   `save_memory` / `save_note` itself. Every record appears the moment it is written, and the
//!   agent can be told to remember something and do it before it answers.
//! - **Background** hands writing to an independent pass that reads each finished turn — a second
//!   reader deciding what was durable. Replies never wait on a memory write, and the session is not
//!   asked to spend tool calls on bookkeeping.
//!
//! The step reuses the menu's panel, entry line and wrappers ([`crate::menu`]) so it lands in the
//! same visual language as the persona menu; only the layout is simpler, because two options with
//! explanations are a list, not a grid.

use std::io::{BufRead, Write};

use harness_core::error::HarnessError;
use harness_core::MemoryMode;

use crate::menu::{self, Panel};
use crate::screen::{self, Size};
use crate::style::{Line, Span, ACCENT_COLOR, ERROR_COLOR, NAME_COLOR};
use crate::typewriter::{Pacer, Typewriter};

/// This step's panel: same brand, different section and question.
pub const PANEL: Panel = Panel {
    title: "🌙  LUNA",
    subtitle: "MEMORY CORE",
    caption: "Who writes what I remember between sessions?",
};

/// The question, in the accent colour.
pub const PROMPT: &str = "Select 1-2 ▸ ";
/// The footer: what to type, and where the choice is recorded.
pub const HINT: &str = "Enter a number, then press Enter — blank keeps the session writer (1). Change it later with `luna config set memory.mode background`.";
/// How many rejected answers are tolerated before the flow gives up.
pub const MAX_ATTEMPTS: usize = 10;

/// One option on this screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// What the user types.
    pub id: u8,
    /// The label, in the name colour.
    pub name: &'static str,
    /// What it means in practice, muted, wrapped under the label.
    pub detail: &'static str,
    /// The config value it writes.
    pub mode: MemoryMode,
}

/// The options, in the order they are shown. The first is the default, and the default is the
/// behaviour an install had before this question existed.
pub const CHOICES: [Choice; 2] = [
    Choice {
        id: 1,
        name: "Session writer",
        detail: "I save what matters as we talk, before I answer. Every memory is mine to place, \
                 and you can tell me to remember something and see it land in the same turn.",
        mode: MemoryMode::Session,
    },
    Choice {
        id: 2,
        name: "Background saver",
        detail: "Separate pass over each finished turn distils the same records afterwards. \
                 Replies never wait on a memory write, and my tool calls stay on your task.",
        mode: MemoryMode::Background,
    },
];

/// The default: keep the session as the writer.
pub const DEFAULT_ID: u8 = 1;

/// The option a user typed, or the reason it is not an option.
pub fn parse_selection(answer: &str) -> Result<Choice, String> {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return Ok(choice(DEFAULT_ID).expect("the default is one of the choices"));
    }
    let id: u8 = trimmed
        .parse()
        .map_err(|_| format!("'{trimmed}' is not a number: pick 1 or 2"))?;
    choice(id).ok_or_else(|| format!("there is no option {id}: pick 1 or 2"))
}

/// The option with this id, if it exists.
pub fn choice(id: u8) -> Option<Choice> {
    CHOICES.iter().copied().find(|choice| choice.id == id)
}

/// The one-line summary of a mode, for the confirmation screen.
pub fn summary(mode: MemoryMode) -> &'static str {
    match mode {
        MemoryMode::Session => "Session writer — I save memories as we talk",
        MemoryMode::Background => "Background saver — memories land after each reply",
    }
}

/// The body, placed as a centred block: one label line per option, then its wrapped explanation.
///
/// Placed here rather than in [`render`] because the interactive path draws the pieces separately
/// (header first, typed; body after) — anything that hands out a body must hand out the centred one.
pub fn body(size: Size) -> Vec<Line> {
    menu::place_block(body_lines(size), size)
}

/// The body before centring: the lines whose widest one the panel is measured against.
fn body_lines(size: Size) -> Vec<Line> {
    let width = screen::body_limit(size.columns);
    let limit = width.saturating_sub(menu::ENTRY_INDENT);
    let mut lines = Vec::new();
    for (index, option) in CHOICES.iter().enumerate() {
        if index > 0 {
            lines.push(Line::blank());
        }
        let mut label = menu::entry_line(
            option.id,
            option.name,
            (option.id == DEFAULT_ID).then_some(menu::DEFAULT_MARKER),
            width,
        );
        // The tag rides on the label line only when the whole line still fits: a hint that pushes
        // the row past the body width would break the block's alignment.
        let tag = option.detail_hint();
        if label.width() + 1 + screen::display_width(tag) <= width {
            label = label.then(" ").with(Span::muted(tag));
        }
        lines.push(label);
        for wrapped in screen::wrap(option.detail, limit) {
            lines.push(Line::of(Span::muted(wrapped)).indent(menu::ENTRY_INDENT));
        }
    }
    lines
}

impl Choice {
    /// The short tag that follows the label on the same line.
    fn detail_hint(&self) -> &'static str {
        match self.mode {
            MemoryMode::Session => "— writes during the turn",
            MemoryMode::Background => "— writes after the turn",
        }
    }
}

/// The header panel, sized to the widest line the body will draw.
pub fn header(size: Size) -> Vec<Line> {
    menu::panel_header(size, &PANEL, menu::widest_of(&body_lines(size)))
}

/// The prompt as it should be written: one short line, centred, cursor left after it.
pub fn prompt_line(size: Size) -> Line {
    let line = Line::of(Span::strong(PROMPT, ACCENT_COLOR));
    let pad = screen::center_pad(size.columns as usize, line.width());
    line.indent(pad)
}

/// The footer.
pub fn hint_lines(size: Size) -> Vec<Line> {
    menu::muted_wrapped(HINT, size)
}

/// The whole screen for a terminal of `size`.
pub fn render(size: Size) -> Vec<Line> {
    let mut frame = header(size);
    frame.push(Line::blank());
    frame.extend(body(size));
    frame.push(Line::blank());
    frame.push(prompt_line(size));
    frame.push(Line::blank());
    frame.extend(hint_lines(size));
    frame
}

/// The plain text of the screen, for tests.
pub fn render_text(size: Size) -> Vec<String> {
    crate::style::text_of(&render(size))
}

/// Draw the screen and return the chosen mode.
pub fn select<R: BufRead, W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    reader: &mut R,
) -> Result<MemoryMode, HarnessError> {
    writer.clear_screen()?;
    let size = writer.size();
    writer.type_block_quick(&header(size))?;
    writer.blank()?;
    writer.write_block(&body(size))?;
    writer.blank()?;
    writer.write_block(&hint_lines(size))?;
    writer.blank()?;

    for attempt in 1..=MAX_ATTEMPTS {
        writer.write_prompt_line(&prompt_line(size))?;
        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            writer.blank()?;
            return Err(HarnessError::Io(
                "input closed before a memory mode was chosen".to_string(),
            ));
        }
        match parse_selection(&answer) {
            Ok(choice) => {
                writer.blank()?;
                writer.write_centered(&format!("✓ {}", summary(choice.mode)), Some(NAME_COLOR))?;
                return Ok(choice.mode);
            }
            Err(error) => {
                writer.write_centered(&format!("✗ {}", error), Some(ERROR_COLOR))?;
                if attempt == MAX_ATTEMPTS {
                    break;
                }
            }
        }
    }

    Err(HarnessError::ConfigurationError(format!(
        "no memory mode was selected in {MAX_ATTEMPTS} attempts; run `luna` again when you are ready"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(columns: u16, rows: u16) -> Size {
        Size::new(columns, rows)
    }

    #[test]
    fn both_options_are_offered_and_the_session_writer_is_the_default() {
        assert_eq!(CHOICES.len(), 2);
        assert_eq!(default_choice().mode, MemoryMode::Session);
        assert_eq!(choice(2).unwrap().mode, MemoryMode::Background);
        assert_eq!(choice(1).unwrap().name, "Session writer");
        assert!(choice(3).is_none());
    }

    fn default_choice() -> Choice {
        parse_selection("").expect("blank picks the default")
    }

    #[test]
    fn a_blank_answer_keeps_the_session_writer_and_a_number_picks_one() {
        assert_eq!(parse_selection("\n").unwrap().mode, MemoryMode::Session);
        assert_eq!(
            parse_selection("  2  \n").unwrap().mode,
            MemoryMode::Background
        );
        assert_eq!(parse_selection("1").unwrap().mode, MemoryMode::Session);

        let error = parse_selection("7").unwrap_err();
        assert!(error.contains("pick 1 or 2"), "{error}");
        let error = parse_selection("background").unwrap_err();
        assert!(error.contains("not a number"), "{error}");
    }

    #[test]
    fn every_option_is_labelled_with_its_id_default_marker_and_what_it_does() {
        let text = render_text(size(100, 30)).join("\n");
        assert!(text.contains("1  Session writer"), "{text}");
        assert!(text.contains("2  Background saver"), "{text}");
        assert!(text.contains(crate::menu::DEFAULT_MARKER), "{text}");
        assert!(text.contains("MEMORY CORE"), "{text}");
        assert!(
            text.contains("writes during the turn") && text.contains("writes after the turn"),
            "{text}"
        );
    }

    #[test]
    fn both_details_survive_at_a_normal_width_and_the_hint_names_the_config_key() {
        let text = render_text(size(100, 30)).join("\n");
        assert!(text.contains("I save what matters as we talk"), "{text}");
        assert!(
            text.contains("Separate pass over each finished turn"),
            "{text}"
        );
        assert!(
            text.contains("luna config set memory.mode background"),
            "the escape hatch must be on screen: {text}"
        );
    }

    #[test]
    fn the_screen_fits_a_short_terminal_including_the_box_and_the_hint() {
        for rows in 20u16..=40 {
            let geometry = size(100, rows);
            let frame = render(geometry);
            assert!(
                frame.len() <= rows as usize,
                "{rows} rows: the memory screen is {} lines",
                frame.len()
            );
        }
    }

    #[test]
    fn the_panel_frames_the_choices_it_belongs_to() {
        for (columns, rows) in [(100u16, 30u16), (70, 30), (40, 24)] {
            let geometry = size(columns, rows);
            let frame_width = screen::display_width(header(geometry)[0].text().trim());
            let widest = body(geometry)
                .iter()
                .map(|line| screen::display_width(line.text().trim()))
                .max()
                .unwrap_or(0);
            assert!(
                frame_width.abs_diff(widest) <= 8,
                "{columns}x{rows}: the panel is {frame_width} over a {widest} wide body"
            );
        }
    }

    #[test]
    fn a_narrow_terminal_clips_instead_of_overflowing() {
        for columns in [24u16, 30, 40] {
            let geometry = size(columns, 40);
            for line in body_lines(geometry) {
                assert!(
                    line.width() <= screen::body_limit(columns),
                    "{columns} columns: a line is {} wide",
                    line.width()
                );
            }
        }
    }

    #[test]
    fn the_whole_body_is_placed_as_one_block() {
        // Every screen this module hands out is centred: the interactive path writes the body on its
        // own, so an unplaced body would land flush against the left edge of the terminal.
        for columns in [40u16, 70, 100] {
            let geometry = size(columns, 30);
            let placed = body(geometry);
            // The block's offset is derived from the widest *unplaced* line, which is what the panel
            // is measured against too.
            let expected =
                screen::center_pad(columns as usize, menu::widest_of(&body_lines(geometry)));
            let label = placed
                .iter()
                .find(|line| line.text().trim_start().starts_with("1  Session"))
                .expect("the first option is on screen");
            // `entry_line` right-aligns the id in two columns, so id 1's own text starts one column
            // in; that column is the entry's, not the block's.
            assert_eq!(
                leading(label) - 1,
                expected,
                "{columns} columns: the block is not centred"
            );
            for line in &placed {
                assert!(
                    line.width() <= columns as usize,
                    "{columns} columns: a line is {} wide",
                    line.width()
                );
            }
        }
    }

    /// Columns of padding in front of a line's text.
    fn leading(line: &Line) -> usize {
        let text = line.text();
        text.len() - text.trim_start().len()
    }

    #[test]
    fn the_summary_says_which_writer_was_chosen() {
        assert!(summary(MemoryMode::Session).contains("Session writer"));
        assert!(summary(MemoryMode::Background).contains("Background saver"));
    }
}
