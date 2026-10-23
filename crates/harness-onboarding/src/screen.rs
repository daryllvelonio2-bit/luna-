//! Layout geometry: display width, centering, wrapping.
//!
//! Pure functions over text and a column count — no terminal handle, no escape codes, no I/O — so
//! the interesting cases (a wide emoji, a line longer than the window, a width of zero) are unit
//! tested instead of eyeballed.
//!
//! Widths are *display* widths, not byte or char counts: `🌙` occupies two columns and a combining
//! mark occupies none, and mis-measuring either shifts the whole frame by a column.

use unicode_width::UnicodeWidthStr;

/// Used when the terminal refuses to report its size (a pipe, a CI log).
pub const FALLBACK_COLUMNS: u16 = 80;
/// Used when the terminal refuses to report its height.
pub const FALLBACK_ROWS: u16 = 24;
/// Columns kept free on each side of a wrapped body, so centered prose never touches the edge.
pub const MARGIN: usize = 4;
/// Shortest body a wrapped line is allowed to shrink to, however narrow the terminal claims to be.
pub const MIN_BODY: usize = 16;
/// A terminal narrower than this is reporting nonsense (a pty with no window size answers `0`), so
/// its answer is treated as "unknown" rather than believed.
pub const MIN_TERMINAL_COLUMNS: u16 = 20;
/// The ellipsis a truncated line ends with.
pub const ELLIPSIS: char = '…';

/// The terminal geometry a frame is laid out in.
///
/// Height matters as much as width: a menu that is one line too tall scrolls the terminal, which
/// pushes the header off the top and the prompt off the bottom — the "no header, everything
/// scrolled" failure. Layouts are chosen from this, not from a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    /// Usable columns.
    pub columns: u16,
    /// Usable rows.
    pub rows: u16,
}

impl Size {
    /// A size, with nonsense dimensions replaced by the fallbacks.
    pub fn new(columns: u16, rows: u16) -> Self {
        Self {
            columns: if columns >= MIN_TERMINAL_COLUMNS {
                columns
            } else {
                FALLBACK_COLUMNS
            },
            rows: if rows > 0 { rows } else { FALLBACK_ROWS },
        }
    }

    /// The default geometry, for a caller with no terminal.
    pub fn fallback() -> Self {
        Self {
            columns: FALLBACK_COLUMNS,
            rows: FALLBACK_ROWS,
        }
    }

    /// Rows left for a body once `chrome` lines are spent on headers, prompts and hints.
    pub fn body_rows(self, chrome: u16) -> usize {
        (self.rows.saturating_sub(chrome)).max(4) as usize
    }
}

impl Default for Size {
    fn default() -> Self {
        Self::fallback()
    }
}

/// The width a wrapped body line may use in a terminal of this size: the full width minus the
/// margins, never below [`MIN_BODY`] (a terminal that reports 5 columns is wrong, not narrow).
pub fn body_limit(columns: u16) -> usize {
    (columns as usize).saturating_sub(2 * MARGIN).max(MIN_BODY)
}

/// Columns `text` occupies on screen.
pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// `text`, cut to at most `limit` columns and ended with an ellipsis if anything was dropped.
///
/// A layout that reserves exactly two lines for a tagline needs this: wrapping alone would push the
/// entry three lines tall and scroll the frame, while hard truncation keeps the grid rectangular.
pub fn fit(text: &str, limit: usize) -> String {
    if display_width(text) <= limit {
        return text.to_string();
    }
    if limit == 0 {
        return String::new();
    }
    let budget = limit - display_width(&ELLIPSIS.to_string());
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = display_width(&ch.to_string());
        if used + width > budget {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.trim_end().to_string() + &ELLIPSIS.to_string()
}

/// A horizontal rule `width` columns wide, drawn with `glyph`.
pub fn rule(width: usize, glyph: char) -> String {
    glyph.to_string().repeat(width)
}

/// Columns of left padding that centre `text_width` inside `columns`.
pub fn center_pad(columns: usize, text_width: usize) -> usize {
    columns.saturating_sub(text_width) / 2
}

/// `text` with the padding that centres it in `columns` (no newline).
pub fn centered(text: &str, columns: u16) -> String {
    let width = columns as usize;
    format!(
        "{}{}",
        " ".repeat(center_pad(width, display_width(text))),
        text
    )
}

/// The display width of the widest line.
pub fn block_width<'a>(lines: impl IntoIterator<Item = &'a str>) -> usize {
    lines.into_iter().map(display_width).max().unwrap_or(0)
}

/// Centre each line of a block independently, preserving blank lines.
pub fn center_block<'a>(lines: impl IntoIterator<Item = &'a str>, columns: u16) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| centered(line, columns))
        .collect()
}

/// Centre a block *as a unit*: every non-empty line is shifted by the same amount, so columns that
/// are meant to line up (menu ids, prose under a title) keep their alignment.
///
/// The offset comes from the widest line and lines are not right-padded, so no trailing whitespace
/// is emitted. Blank lines stay blank.
pub fn center_group<'a>(lines: impl IntoIterator<Item = &'a str>, columns: u16) -> Vec<String> {
    let lines: Vec<&str> = lines.into_iter().collect();
    let offset = center_pad(columns as usize, block_width(lines.iter().copied()));
    lines
        .into_iter()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{}{}", " ".repeat(offset), line)
            }
        })
        .collect()
}

/// Centre each styled line by its own width, leaving blank lines blank.
///
/// The padding is prepended as a span so the caller keeps one `Line` type end to end: the writer
/// pads nothing, it only draws what it is handed.
pub fn center_lines(lines: Vec<crate::style::Line>, columns: u16) -> Vec<crate::style::Line> {
    lines
        .into_iter()
        .map(|line| {
            let pad = center_pad(columns as usize, line.width());
            if pad == 0 {
                line
            } else {
                line.indent(pad)
            }
        })
        .collect()
}

/// Centre styled lines *as a unit*: every line gets the same offset, taken from the widest one, so
/// a list keeps its left edge. Blank lines are left blank (no trailing whitespace).
pub fn center_group_lines(lines: Vec<crate::style::Line>, columns: u16) -> Vec<crate::style::Line> {
    let widest = lines
        .iter()
        .map(crate::style::Line::width)
        .max()
        .unwrap_or(0);
    let offset = center_pad(columns as usize, widest);
    if offset == 0 {
        return lines;
    }
    lines
        .into_iter()
        .map(|line| {
            if line.is_blank() {
                line
            } else {
                line.indent(offset)
            }
        })
        .collect()
}

/// Word-wrap `text` to at most `limit` columns, never splitting a word that fits on its own line.
///
/// A word longer than `limit` is hard-split, because the alternative is a line that overflows the
/// terminal and defeats the centering computed from it.
pub fn wrap(text: &str, limit: usize) -> Vec<String> {
    if limit == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        let candidate = if current.is_empty() {
            display_width(&word)
        } else {
            display_width(&current) + 1 + display_width(&word)
        };
        if candidate <= limit {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(&word);
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        while display_width(&word) > limit {
            let split = split_at_width(&word, limit);
            let rest = word[split..].to_string();
            word.truncate(split);
            lines.push(std::mem::take(&mut word));
            word = rest;
        }
        current = word;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Wrap `text` and centre every resulting line, capping the body at the terminal's body limit.
pub fn wrap_centered(text: &str, columns: u16) -> Vec<String> {
    center_block(
        wrap(text, body_limit(columns)).iter().map(String::as_str),
        columns,
    )
}

/// Byte index of the first char that would exceed `limit` display columns.
fn split_at_width(text: &str, limit: usize) -> usize {
    let mut used = 0;
    for (index, ch) in text.char_indices() {
        let width = display_width(&ch.to_string());
        if used + width > limit {
            return index;
        }
        used += width;
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_emoji_counts_as_two_columns() {
        assert_eq!(display_width("🌙"), 2);
        assert_eq!(display_width("Luna"), 4);
    }

    #[test]
    fn centering_splits_the_slack_and_never_underflows() {
        assert_eq!(center_pad(80, 10), 35);
        assert_eq!(center_pad(81, 10), 35);
        assert_eq!(center_pad(10, 10), 0);
        assert_eq!(center_pad(4, 10), 0);
        assert_eq!(centered("Luna", 10), "   Luna");
    }

    #[test]
    fn a_centered_line_is_symmetric_within_one_column() {
        let line = centered("Hi! I am Luna, your personal AI agent harness.", 80);
        let pad = line.len() - display_width("Hi! I am Luna, your personal AI agent harness.");
        assert_eq!(pad, 17);
        assert!(line.ends_with("harness."));
    }

    #[test]
    fn wrapping_keeps_words_intact_and_respects_the_budget() {
        let lines = wrap("I retain persistent memory across sessions", 20);
        assert_eq!(
            lines,
            vec!["I retain persistent", "memory across", "sessions"]
        );
        for line in &lines {
            assert!(display_width(line) <= 20);
        }
    }

    #[test]
    fn a_word_longer_than_the_budget_is_split_rather_than_overflowing() {
        let lines = wrap("supercalifragilisticexpialidocious", 10);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(display_width(line) <= 10);
        }
        assert_eq!(lines.concat(), "supercalifragilisticexpialidocious");
    }

    #[test]
    fn wrap_centered_centres_every_line_of_a_paragraph() {
        let lines = wrap_centered(
            "I live directly in your terminal, watching over your environments and workflows.",
            60,
        );
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(
                line.starts_with(' ') || line.starts_with("I live"),
                "line is not padded: {line:?}"
            );
            assert!(display_width(line) <= 60);
        }
    }

    #[test]
    fn an_empty_body_still_yields_one_line() {
        assert_eq!(wrap("", 20), vec![String::new()]);
        assert_eq!(wrap("anything", 0), vec!["anything".to_string()]);
    }

    #[test]
    fn a_group_shares_one_offset_so_its_columns_stay_aligned() {
        let lines = center_group([" 1  Classic Tsundere", "    tagline", "", "Select:"], 40);

        assert_eq!(lines[2], "", "a blank line stays blank");
        assert!(lines[0].starts_with(' '), "the group is not centered");
        let offset = lines[0].len() - " 1  Classic Tsundere".len();
        assert!(offset > 0);
        assert_eq!(offset, lines[1].len() - "    tagline".len());
        assert_eq!(offset, lines[3].len() - "Select:".len());
    }
}
