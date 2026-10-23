//! The Phase-2 menu: the frame, and the rules for reading an answer.
//!
//! [`render`] is a pure function of the terminal geometry and the catalog, so the layout can be
//! asserted in a test; [`parse_selection`] owns the validation rules (`1..=10`, blank means the
//! default, anything else is rejected with a reason the caller can print). The read-echo loop
//! itself lives in [`crate::flow`], because it is interaction rather than data.
//!
//! The frame has a fixed shape — boxed header, persona body, prompt, hint — and the *body* picks
//! one of three layouts from the terminal height:
//!
//! - [`BodyLayout::Single`]: one persona per block with its full tagline. Needs a tall window.
//! - [`BodyLayout::Grid`]: two columns, taglines clipped to two lines. The usual case.
//! - [`BodyLayout::Compact`]: names only, two columns. For a short window.
//!
//! Choosing by height is the point: a body that is one line too tall scrolls the terminal, and the
//! scrolled-away header is exactly what makes a menu look broken.

use std::fmt;

use crate::persona::{self, Persona};
use crate::screen::{self, Size};
use crate::style::{self, Line, Span, ACCENT_COLOR, BORDER_COLOR, CREST_COLOR, NAME_COLOR};

/// The line the user answers on.
pub const PROMPT: &str = "Select 1-10 ▸ ";
/// Header line one.
pub const TITLE: &str = "🌙  LUNA";
/// Header line two: what this screen is for.
pub const SUBTITLE: &str = "NEURAL CORE CALIBRATION";
/// Header line three: what the user has to do.
pub const CAPTION: &str = "Pick the voice your agent speaks with";
/// Footer explaining what to type.
pub const HINT: &str = "Enter a number, then press Enter — blank picks the default (1). Your choice is written to the config file.";
/// Marker on the persona a blank answer would take.
pub const DEFAULT_MARKER: &str = "◂ default";
/// Widest the header box is allowed to grow, so it stays a header and not a banner.
pub const HEADER_MAX: usize = 78;
/// Narrowest the header box may be, however narrow the terminal.
pub const HEADER_MIN: usize = 24;
/// Rows the header always occupies: two borders and three content lines.
const HEADER_ROWS: u16 = 5;
/// Gap between the two grid columns.
const COLUMN_GAP: usize = 3;
/// Narrowest body that can hold two columns of a name and a tagline.
const GRID_MIN_WIDTH: usize = 46;
/// Columns a name needs before the default marker is worth its space. Below this the name wins.
const MIN_NAME_COLUMNS: usize = 16;
/// Columns a grid cell spends on the selection number and the gap after it.
const ID_COLUMNS: usize = 4;
/// Lines a tagline may occupy in a grid cell, so every row is the same height.
const GRID_TAGLINE_LINES: usize = 2;
/// Hanging indent for a tagline: aligns it under the name, not under the selection number.
const ENTRY_INDENT: usize = ID_COLUMNS;

/// Why an answer was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionError {
    /// The answer was not a number.
    NotANumber(String),
    /// The answer was a number outside the catalog.
    OutOfRange(u32),
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectionError::NotANumber(input) => write!(
                f,
                "'{}' is not a number. Enter 1-10, or press Enter for the default.",
                input.trim()
            ),
            SelectionError::OutOfRange(value) => write!(
                f,
                "'{}' is outside the catalog. Enter a number from 1 to {}.",
                value,
                persona::all().len()
            ),
        }
    }
}

impl std::error::Error for SelectionError {}

/// Read one answer: a blank line takes the default, `1..=10` picks that persona.
pub fn parse_selection(input: &str) -> Result<&'static Persona, SelectionError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(persona::by_id(persona::DEFAULT_ID).expect("the default id is in the catalog"));
    }
    let Ok(value) = trimmed.parse::<u32>() else {
        return Err(SelectionError::NotANumber(trimmed.to_string()));
    };
    if value == 0 || value > persona::all().len() as u32 {
        return Err(SelectionError::OutOfRange(value));
    }
    Ok(persona::by_id(value as u8).expect("range-checked against the catalog"))
}

/// How the persona body is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyLayout {
    /// One persona per block, full tagline.
    Single,
    /// Two columns, tagline clipped to [`GRID_TAGLINE_LINES`].
    Grid,
    /// Two columns, names only.
    Compact,
    /// One persona per line, names only: for a terminal too narrow for two columns.
    Stack,
}

/// The header box: the screen's identity, drawn once at the top.
///
/// It exists because the persona list alone gives the eye nowhere to land — the header states what
/// the screen is, and the box separates it from the choices underneath.
pub fn header(size: Size) -> Vec<Line> {
    let inner = header_inner_width(size);
    let mut lines = vec![Line::of(Span::colored(
        format!("╭{}╮", screen::rule(inner + 2, '─')),
        BORDER_COLOR,
    ))];
    for content in header_content(inner) {
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
    // A box has one width, so centring it as a unit and centring each line agree; do it here so the
    // caller (which types the header before drawing the body) does not have to think about it.
    screen::center_lines(lines, size.columns)
}

/// The interior width of the header box.
///
/// The box frames the choices, so it is measured from them: as wide as the widest body line, within
/// bounds. A fixed-width header over a wider body reads as two unrelated blocks.
fn header_inner_width(size: Size) -> usize {
    body_widest(size)
        .clamp(HEADER_MIN, HEADER_MAX)
        .min(screen::body_limit(size.columns))
}

/// The widest line the body will draw, which is what the header frames.
///
/// Measured before centring: a centred line reports the terminal's width, not the width the header
/// has to span.
fn body_widest(size: Size) -> usize {
    let layout = choose_layout(size, body_budget(size));
    render_layout(size, layout)
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
}

/// The three header lines: crest, title, caption.
fn header_content(inner: usize) -> Vec<Line> {
    vec![
        Line::of(Span::strong(TITLE, CREST_COLOR)),
        Line::of(Span::strong(SUBTITLE, NAME_COLOR)),
        Line::of(Span::muted(screen::fit(CAPTION, inner))),
    ]
}

/// The whole frame for a terminal of `size`.
///
/// The header box and the footer are centred on their own; the body is centred as a *group*, so the
/// ids line up in a column instead of every name drifting to its own middle.
pub fn render(size: Size) -> Vec<Line> {
    let mut frame = header(size);
    frame.push(Line::blank());
    frame.extend(body(size));
    frame.push(Line::blank());
    frame.extend(prompt_lines(size));
    frame.push(Line::blank());
    frame.extend(hint_lines(size));
    frame
}

/// The footer: what to type, muted so it never competes with the choices.
pub fn hint_lines(size: Size) -> Vec<Line> {
    screen::center_lines(
        screen::wrap(HINT, screen::body_limit(size.columns))
            .into_iter()
            .map(|line| Line::of(Span::muted(line)))
            .collect(),
        size.columns,
    )
}

/// The plain text of the frame, for tests and snapshots.
pub fn render_text(size: Size) -> Vec<String> {
    style::text_of(&render(size))
}

/// The persona body, laid out to fit the rows the header and footer leave behind.
pub fn body(size: Size) -> Vec<Line> {
    let layout = choose_layout(size, body_budget(size));
    // Centred as a group, so the ids form a column instead of every line finding its own middle.
    screen::center_group_lines(render_layout(size, layout), size.columns)
        .into_iter()
        .map(Line::trim_end)
        .collect()
}

/// Rows the body may use.
///
/// The floor is one line per persona: ten names is the least this menu can show, so a terminal
/// shorter than that scrolls rather than silently hiding half the catalog.
fn body_budget(size: Size) -> usize {
    size.body_rows(chrome_rows(size)).max(stack_height())
}

/// Rows spent on everything that is not the persona body: header box, gaps, prompt, hint.
///
/// The header height is a constant rather than a measurement: its width is derived from the body, so
/// measuring it here would be circular. `the_header_is_always_five_rows_wide_or_narrow` pins that.
fn chrome_rows(size: Size) -> u16 {
    let hint = hint_lines(size).len() as u16;
    // header + blank + blank + prompt + blank + hint
    HEADER_ROWS + hint + 4
}

/// The body for a layout, at this terminal size.
fn render_layout(size: Size, layout: BodyLayout) -> Vec<Line> {
    match layout {
        BodyLayout::Single => single_column(size),
        BodyLayout::Grid => grid(size, GRID_TAGLINE_LINES),
        BodyLayout::Compact => grid(size, 0),
        BodyLayout::Stack => stack(size),
    }
}

/// Pick the richest layout whose height fits `budget`.
///
/// Two columns need room for a name and a tagline side by side, so a narrow terminal is offered the
/// stacked layout instead — a clipped to two columns table is worse than a longer list.
pub fn choose_layout(size: Size, budget: usize) -> BodyLayout {
    let wide_enough = screen::body_limit(size.columns) >= GRID_MIN_WIDTH;
    let mut candidates = vec![(BodyLayout::Single, single_column_height(size))];
    if wide_enough {
        candidates.push((BodyLayout::Grid, grid_height(GRID_TAGLINE_LINES)));
        candidates.push((BodyLayout::Compact, grid_height(0)));
    }
    for (layout, height) in candidates {
        if height <= budget {
            return layout;
        }
    }
    if wide_enough {
        BodyLayout::Compact
    } else {
        BodyLayout::Stack
    }
}

/// Height of a body that is one name per line: the floor of this menu.
fn stack_height() -> usize {
    persona::all().len()
}

/// Height of the single-column body: every entry plus the gap between them.
fn single_column_height(size: Size) -> usize {
    let entries = persona::all().len();
    // Wrapping is measured per persona, because tagline lengths differ by a line.
    let limit = screen::body_limit(size.columns).saturating_sub(ENTRY_INDENT);
    let lines: usize = persona::all()
        .iter()
        .map(|persona| 1 + screen::wrap(persona.tagline, limit).len())
        .sum();
    lines + entries.saturating_sub(1)
}

/// Height of the grid body: the tallest cell in each row, plus one gap between rows.
fn grid_height(tagline_lines: usize) -> usize {
    let rows = persona::all().len().div_ceil(2);
    let cell = 1 + tagline_lines;
    rows * cell + rows.saturating_sub(1)
}

/// One persona per block: name line, then the full tagline hanging under it.
fn single_column(size: Size) -> Vec<Line> {
    let body = screen::body_limit(size.columns);
    let limit = body.saturating_sub(ENTRY_INDENT);
    let mut lines = Vec::new();
    for (index, persona) in persona::all().iter().enumerate() {
        if index > 0 {
            lines.push(Line::blank());
        }
        lines.push(name_line(persona, body));
        for wrapped in screen::wrap(persona.tagline, limit) {
            lines.push(Line::of(Span::muted(wrapped)).indent(ENTRY_INDENT));
        }
    }
    lines
}

/// One persona per line, names only: the layout a terminal too narrow for two columns can afford.
fn stack(size: Size) -> Vec<Line> {
    let body = screen::body_limit(size.columns);
    persona::all()
        .iter()
        .map(|persona| name_line(persona, body))
        .collect()
}

/// The width of one grid cell.
///
/// When the cells carry taglines they take half the body. When they are names only, the cell shrinks
/// to the widest name: a compact list stretched to the full half-width leaves two lonely columns at
/// opposite edges of the terminal.
fn grid_cell_width(size: Size, tagline_lines: usize) -> usize {
    let available =
        (screen::body_limit(size.columns).saturating_sub(COLUMN_GAP) / 2).max(screen::MIN_BODY / 2);
    if tagline_lines > 0 {
        return available;
    }
    let widest = persona::all()
        .iter()
        .map(|persona| name_line(persona, usize::MAX / 2).width())
        .max()
        .unwrap_or(0);
    widest.min(available)
}
fn grid(size: Size, tagline_lines: usize) -> Vec<Line> {
    let cell_width = grid_cell_width(size, tagline_lines);
    let tagline_limit = cell_width.saturating_sub(ENTRY_INDENT);

    let mut lines = Vec::new();
    for (row, pair) in persona::all().chunks(2).enumerate() {
        if row > 0 && tagline_lines > 0 {
            lines.push(Line::blank());
        }
        let mut name_row = Line::blank();
        let mut tagline_rows: Vec<Vec<Line>> = Vec::new();
        for (column, persona) in pair.iter().enumerate() {
            if column > 0 {
                name_row = name_row.then(" ".repeat(COLUMN_GAP));
            }
            name_row = name_row.then_spans(pad_to(name_line(persona, cell_width), cell_width));
            tagline_rows.push(cell_tagline(
                persona,
                tagline_limit,
                tagline_lines,
                cell_width,
            ));
        }
        lines.push(name_row);
        for depth in 0..tagline_lines {
            let mut row = Line::blank();
            for (column, cell) in tagline_rows.iter().enumerate() {
                if column > 0 {
                    row = row.then(" ".repeat(COLUMN_GAP));
                }
                row = row.then_spans(
                    cell.get(depth)
                        .cloned()
                        .unwrap_or_else(|| Line::plain(" ".repeat(cell_width))),
                );
            }
            lines.push(row);
        }
    }
    lines
}

/// A cell's tagline block: clipped to `tagline_lines` lines, the last one ellipsised.
fn cell_tagline(
    persona: &Persona,
    limit: usize,
    tagline_lines: usize,
    cell_width: usize,
) -> Vec<Line> {
    if tagline_lines == 0 {
        return Vec::new();
    }
    let mut wrapped = screen::wrap(persona.tagline, limit);
    // A clipped tagline must look clipped, or the reader assumes the pitch ends there.
    if wrapped.len() > tagline_lines {
        wrapped.truncate(tagline_lines);
        if let Some(last) = wrapped.last_mut() {
            *last = screen::fit(&format!("{} {}", last, screen::ELLIPSIS), limit);
        }
    }
    wrapped
        .into_iter()
        .map(|line| pad_to(Line::of(Span::muted(line)).indent(ENTRY_INDENT), cell_width))
        .collect()
}

/// The id and name of one persona, clipped to `width` columns, with the default marked.
///
/// The marker is dropped before the name is clipped to nothing: a long name in a narrow cell is
/// worth more than the hint, and an entry that overflows its column breaks the grid alignment.
fn name_line(persona: &Persona, width: usize) -> Line {
    let marker = if persona.id == persona::DEFAULT_ID {
        DEFAULT_MARKER
    } else {
        ""
    };
    let marker_columns = if marker.is_empty() {
        0
    } else {
        1 + screen::display_width(marker)
    };
    let roomy = width.saturating_sub(ID_COLUMNS + marker_columns);
    let show_marker = !marker.is_empty() && roomy >= MIN_NAME_COLUMNS;
    let name_columns = if show_marker {
        roomy
    } else {
        width.saturating_sub(ID_COLUMNS)
    };

    let mut line = Line::of(Span::strong(format!("{:>2}", persona.id), ACCENT_COLOR))
        .then("  ")
        .with(Span::strong(
            screen::fit(persona.name, name_columns),
            NAME_COLOR,
        ));
    if show_marker {
        line = line.then(" ").then_muted(DEFAULT_MARKER);
    }
    line
}

/// Right-pad `line` with spaces to `width` columns, so the next column starts where it should.
fn pad_to(line: Line, width: usize) -> Line {
    let pad = width.saturating_sub(line.width());
    if pad == 0 {
        line
    } else {
        line.then(" ".repeat(pad))
    }
}

/// The prompt as it should be written: wrapped to the terminal, the user's answer following the
/// last line.
pub fn prompt_lines(size: Size) -> Vec<Line> {
    let prompt = Line::of(Span::strong(PROMPT, ACCENT_COLOR));
    if prompt.width() <= screen::body_limit(size.columns) {
        return screen::center_lines(vec![prompt], size.columns);
    }
    screen::center_lines(
        screen::wrap(PROMPT, screen::body_limit(size.columns))
            .into_iter()
            .map(|line| Line::of(Span::strong(line, ACCENT_COLOR)))
            .collect(),
        size.columns,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(columns: u16, rows: u16) -> Size {
        Size { columns, rows }
    }

    #[test]
    fn a_blank_answer_takes_the_default() {
        assert_eq!(
            parse_selection("").expect("default").id,
            persona::DEFAULT_ID
        );
        assert_eq!(
            parse_selection("   \n").expect("default").id,
            persona::DEFAULT_ID
        );
    }

    #[test]
    fn every_id_in_range_resolves_to_its_persona() {
        for persona in persona::all() {
            assert_eq!(
                parse_selection(&persona.id.to_string()).expect("in range"),
                persona
            );
        }
        assert_eq!(
            parse_selection(" 10 \n")
                .expect("padded input is fine")
                .name,
            "Nostalgic 90s UNIX Daemon"
        );
    }

    #[test]
    fn zero_out_of_range_and_non_numbers_are_rejected_with_a_reason() {
        assert_eq!(parse_selection("0"), Err(SelectionError::OutOfRange(0)));
        assert_eq!(parse_selection("11"), Err(SelectionError::OutOfRange(11)));
        assert_eq!(
            parse_selection("banana"),
            Err(SelectionError::NotANumber("banana".to_string()))
        );
        assert_eq!(
            parse_selection("-3"),
            Err(SelectionError::NotANumber("-3".to_string()))
        );
        assert!(format!("{}", SelectionError::OutOfRange(11)).contains("1 to 10"));
    }

    #[test]
    fn the_frame_lists_every_persona_with_its_id_and_the_default_marked() {
        let frame = render_text(size(100, 40)).join("\n");
        for persona in persona::all() {
            assert!(
                frame.contains(persona.name),
                "{} missing from the menu",
                persona.name
            );
            assert!(frame.contains(&format!("{:>2}", persona.id)));
        }
        assert_eq!(
            frame.matches(DEFAULT_MARKER).count(),
            1,
            "exactly one persona is the default"
        );
        assert!(frame.contains("NEURAL CORE CALIBRATION"));
        assert!(frame.contains("Enter a number, then press Enter"));
    }

    /// The columns a line actually draws, with the centring padding and any column padding removed.
    fn ink(line: &Line) -> usize {
        screen::display_width(line.text().trim())
    }

    #[test]
    fn the_header_is_a_box_that_frames_the_title() {
        let header = header(size(100, 40));
        let text = style::text_of(&header);
        assert_eq!(text.len(), 5, "border, crest, title, caption, border");
        let trimmed: Vec<String> = text.iter().map(|line| line.trim().to_string()).collect();
        assert!(trimmed[0].starts_with('╭') && trimmed[0].ends_with('╮'));
        assert!(trimmed[4].starts_with('╰') && trimmed[4].ends_with('╯'));
        assert!(trimmed[1].contains("LUNA"));
        assert!(trimmed[2].contains("NEURAL CORE CALIBRATION"));
        // Every interior line is the same width as the border, or the box is ragged.
        for line in &header {
            assert_eq!(ink(line), ink(&header[0]));
        }
    }

    #[test]
    fn the_frame_fits_the_terminal_and_is_centered() {
        for (columns, rows) in [(80u16, 24u16), (100, 30), (120, 40), (60, 20)] {
            let lines = render(size(columns, rows));
            for line in &lines {
                assert!(
                    line.width() <= columns as usize,
                    "line overflows {columns} columns: {:?}",
                    line.text()
                );
            }
            let title = lines
                .iter()
                .find(|line| line.text().contains("NEURAL CORE"))
                .expect("title line");
            assert!(
                title.text().starts_with("  "),
                "title is not centered: {:?}",
                title.text()
            );
        }
    }

    #[test]
    fn a_tall_wide_terminal_gets_full_taglines_and_a_cramped_one_gets_a_compact_grid() {
        let full = size(100, 50);
        assert_eq!(choose_layout(full, body_budget(full)), BodyLayout::Single);

        let cramped = size(100, 24);
        assert_eq!(
            choose_layout(cramped, body_budget(cramped)),
            BodyLayout::Compact
        );

        let short = size(100, 20);
        assert_eq!(
            choose_layout(short, body_budget(short)),
            BodyLayout::Compact
        );
    }

    #[test]
    fn a_narrow_terminal_stacks_the_names_instead_of_clipping_two_columns() {
        let narrow = size(30, 40);
        assert_eq!(
            choose_layout(narrow, body_budget(narrow)),
            BodyLayout::Stack,
            "two clipped columns are worse than one readable one"
        );
        assert_eq!(stack(narrow).len(), persona::all().len());
    }

    #[test]
    fn the_chosen_body_actually_fits_the_rows_it_was_budgeted() {
        for rows in 8u16..=60 {
            let geometry = size(100, rows);
            let budget = body_budget(geometry);
            let body = body(geometry);
            assert!(
                body.len() <= budget,
                "{rows} rows: body is {} lines but only {budget} were budgeted",
                body.len()
            );
        }
    }

    #[test]
    fn the_header_is_always_five_rows_wide_or_narrow() {
        // `chrome_rows` budgets five rows for the header; every width must agree.
        for columns in [24u16, 30, 40, 70, 100, 200] {
            assert_eq!(
                header(Size::new(columns, 24)).len(),
                HEADER_ROWS as usize,
                "{columns} columns"
            );
        }
    }

    #[test]
    fn the_header_box_is_as_wide_as_the_choices_it_frames() {
        for (columns, rows) in [(100u16, 44u16), (100, 26), (70, 32)] {
            let geometry = Size::new(columns, rows);
            let frame_width = ink(&header(geometry)[0]);
            let body = body(geometry).iter().map(ink).max().unwrap_or(0);
            let difference = frame_width.abs_diff(body);
            assert!(
                difference <= 8,
                "{columns}x{rows}: the frame is {frame_width} wide over a {body} wide body"
            );
        }
    }

    #[test]
    fn the_whole_frame_fits_a_normal_terminal() {
        for rows in 20u16..=60 {
            let geometry = size(100, rows);
            let frame = render(geometry);
            assert!(
                frame.len() <= rows as usize,
                "{rows} rows: the frame is {} lines tall and would scroll the header away",
                frame.len()
            );
        }
    }

    #[test]
    fn a_compact_grid_keeps_two_columns_aligned() {
        let body = grid(size(100, 20), 0);
        assert_eq!(body.len(), 5, "ten personas in two columns is five rows");
        let widths: Vec<usize> = body.iter().map(Line::width).collect();
        assert!(
            widths.iter().all(|width| *width == widths[0]),
            "grid rows are ragged: {widths:?}"
        );
    }

    #[test]
    fn a_narrow_terminal_still_renders_every_entry() {
        let lines = render(size(30, 24));
        for line in &lines {
            assert!(line.width() <= 30, "overflow: {:?}", line.text());
        }
        let text = style::text_of(&lines).join("\n");
        assert!(text.contains("Silent Kuudere"));
    }
}
