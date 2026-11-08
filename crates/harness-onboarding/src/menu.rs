//! Shared panel and list primitives for the onboarding screens.
//!
//! The profile step is gone, so this module no longer owns a catalog or selection parser. What
//! remains is the frame every remaining step draws: a boxed header ([`panel_header`]), one
//! implementation of a numbered entry ([`entry_line`]), and the wrap/centre helpers they share.

use crate::screen::{self, Size};
use crate::style::{Line, Span, ACCENT_COLOR, BORDER_COLOR, CREST_COLOR, NAME_COLOR};

/// Marker on the option a blank answer takes.
pub const DEFAULT_MARKER: &str = "◂ default";

/// Widest the header box is allowed to grow, so it stays a header and not a banner.
pub const HEADER_MAX: usize = 78;
/// Narrowest the header box may be, however narrow the terminal.
pub const HEADER_MIN: usize = 24;
/// Rows the header always occupies: two borders and three content lines.
pub const HEADER_ROWS: u16 = 5;
/// Columns a grid cell spends on the selection number and the gap after it.
pub const ENTRY_INDENT: usize = 4;
/// Columns a name needs before the default marker is worth its space.
const MIN_NAME_COLUMNS: usize = 16;

/// The boxed panel above a list of choices: identity, section, question.
///
/// A screen is a `Panel` plus a list; only the words in it and the entries underneath differ, so
/// the box lives here once.
#[derive(Debug, Clone, Copy)]
pub struct Panel {
    /// The brand line, in the crest colour.
    pub title: &'static str,
    /// What this screen configures, in the name colour.
    pub subtitle: &'static str,
    /// The question the entries answer, muted.
    pub caption: &'static str,
}

/// The header box: the screen's identity, drawn once at the top.
///
/// `widest` is the widest line the caller's body will draw, measured before centring: a centred
/// line reports the terminal's width, not the width the box has to span.
pub fn panel_header(size: Size, panel: &Panel, widest: usize) -> Vec<Line> {
    let inner = widest
        .clamp(HEADER_MIN, HEADER_MAX)
        .min(screen::body_limit(size.columns));
    let mut lines = vec![Line::of(Span::colored(
        format!("╭{}╮", screen::rule(inner + 2, '─')),
        BORDER_COLOR,
    ))];
    for content in panel_content(panel, inner) {
        let pad = inner.saturating_sub(content.width());
        lines.push(
            Line::of(Span::colored("│ ", BORDER_COLOR))
                .then_spans(content)
                .then(format!("{} ", " ".repeat(pad)))
                .with(Span::colored("│", BORDER_COLOR)),
        );
    }
    lines.push(Line::of(Span::colored(
        format!("╰{}╯", screen::rule(inner + 2, '─')),
        BORDER_COLOR,
    )));
    screen::center_lines(lines, size.columns)
}

/// The three header lines: crest, title, caption.
fn panel_content(panel: &Panel, inner: usize) -> Vec<Line> {
    vec![
        Line::of(Span::strong(panel.title, CREST_COLOR)),
        Line::of(Span::strong(panel.subtitle, NAME_COLOR)),
        Line::of(Span::muted(screen::fit(panel.caption, inner))),
    ]
}

/// The widest line of a rendered body, for sizing a panel to it.
pub fn widest_of(lines: &[Line]) -> usize {
    lines.iter().map(Line::width).max().unwrap_or(0)
}

/// A muted paragraph, wrapped to the terminal and centred line by line.
pub fn muted_wrapped(text: &str, size: Size) -> Vec<Line> {
    screen::center_lines(
        screen::wrap(text, screen::body_limit(size.columns))
            .into_iter()
            .map(|line| Line::of(Span::muted(line)))
            .collect(),
        size.columns,
    )
}

/// Centre a block of already-built lines as a group, trimming each one's tail.
pub fn place_block(lines: Vec<Line>, size: Size) -> Vec<Line> {
    screen::center_group_lines(lines, size.columns)
        .into_iter()
        .map(Line::trim_end)
        .collect()
}

/// `  1  Name ◂ default`, clipped to `width` columns.
///
/// Shared by every numbered list in the flow: one implementation of the id column, the marker's
/// budget and the clipping rule. The marker is dropped before the name is clipped to nothing — a
/// long name in a narrow cell is worth more than the hint.
pub fn entry_line(id: u8, name: &str, marker: Option<&str>, width: usize) -> Line {
    let marker = marker.unwrap_or("");
    let marker_columns = if marker.is_empty() {
        0
    } else {
        1 + screen::display_width(marker)
    };
    let roomy = width.saturating_sub(ENTRY_INDENT + marker_columns);
    let show_marker = !marker.is_empty() && roomy >= MIN_NAME_COLUMNS;
    let name_columns = if show_marker {
        roomy
    } else {
        width.saturating_sub(ENTRY_INDENT)
    };

    let mut line = Line::of(Span::strong(format!("{:>2}", id), ACCENT_COLOR))
        .then("  ")
        .with(Span::strong(screen::fit(name, name_columns), NAME_COLOR));
    if show_marker {
        line = line.then(" ").then_muted(marker);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(columns: u16, rows: u16) -> Size {
        Size { columns, rows }
    }

    #[test]
    fn the_header_is_a_box_that_frames_the_title() {
        let panel = Panel {
            title: "🌙  LUNA",
            subtitle: "TEST SCREEN",
            caption: "Pick one",
        };
        let header = panel_header(size(100, 40), &panel, 40);
        let text = crate::style::text_of(&header);
        assert_eq!(text.len(), HEADER_ROWS as usize);
        let trimmed: Vec<String> = text.iter().map(|line| line.trim().to_string()).collect();
        assert!(trimmed[0].starts_with('╭') && trimmed[0].ends_with('╮'));
        assert!(trimmed[4].starts_with('╰') && trimmed[4].ends_with('╯'));
        assert!(trimmed[1].contains("LUNA"));
        assert!(trimmed[2].contains("TEST SCREEN"));
    }

    #[test]
    fn entries_align_and_clip() {
        let line = entry_line(2, "Terra", Some(DEFAULT_MARKER), 40);
        let text = line.text();
        assert!(text.contains("Terra"));
        assert!(text.contains(DEFAULT_MARKER));
        assert!(line.width() <= 40);

        let narrow = entry_line(9, "A very long profile name", Some(DEFAULT_MARKER), 12);
        assert!(narrow.width() <= 12, "{}", narrow.text());
    }

    #[test]
    fn a_block_is_centred_as_one_group() {
        let block = place_block(
            vec![Line::plain("one"), Line::plain("two")],
            Size::new(20, 10),
        );
        assert_eq!(block.len(), 2);
        assert!(block[0].text().starts_with(' '));
    }
}
