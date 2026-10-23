//! Styled text: the palette, and a line made of spans.
//!
//! A frame is a `Vec<Line>`, not a `Vec<String>`, because the layout has a hierarchy to express —
//! the crest, a title, the numbered ids, the names, the taglines — and one colour per line cannot
//! say any of it. Spans are the smallest unit the terminal can colour differently, so a line is a
//! run of them and every line reports its own *display* width for centering.
//!
//! Nothing here writes to a terminal; [`crate::typewriter`] does that. Keeping the palette and the
//! span model pure is what lets the layout be asserted in a test.

use crossterm::style::Color;

use crate::screen;

/// The crest's "soft glow": a pale lunar violet.
pub const CREST_COLOR: Color = Color::Rgb {
    r: 214,
    g: 190,
    b: 255,
};
/// Selection numbers, prompts, and the default marker.
pub const ACCENT_COLOR: Color = Color::Rgb {
    r: 160,
    g: 150,
    b: 255,
};
/// Primary text: persona names, headlines. Near-white, never pure white (it glares on dark themes).
pub const NAME_COLOR: Color = Color::Rgb {
    r: 244,
    g: 242,
    b: 255,
};
/// De-emphasised text: taglines, hints, paths, rules.
pub const MUTED_COLOR: Color = Color::Rgb {
    r: 128,
    g: 124,
    b: 146,
};
/// Frame borders. Dimmer than [`MUTED_COLOR`] so the box never competes with its contents.
pub const BORDER_COLOR: Color = Color::Rgb {
    r: 92,
    g: 88,
    b: 120,
};
/// Rejected input.
pub const ERROR_COLOR: Color = Color::Rgb {
    r: 255,
    g: 120,
    b: 140,
};

/// A run of text drawn with one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// What is drawn.
    pub text: String,
    /// Foreground colour, or `None` to inherit the terminal's.
    pub color: Option<Color>,
    /// Bold. Used for names and headlines — the terminal's cheapest emphasis.
    pub bold: bool,
}

impl Span {
    /// Unstyled text.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: None,
            bold: false,
        }
    }

    /// Text in `color`.
    pub fn colored(text: impl Into<String>, color: Color) -> Self {
        Self::new(text).color(color)
    }

    /// Muted text, the common case for secondary copy.
    pub fn muted(text: impl Into<String>) -> Self {
        Self::colored(text, MUTED_COLOR)
    }

    /// Bold text in `color`.
    pub fn strong(text: impl Into<String>, color: Color) -> Self {
        Self::colored(text, color).bold()
    }

    /// Builder: set the colour.
    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// Builder: make it bold.
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Columns this span occupies on screen.
    pub fn width(&self) -> usize {
        screen::display_width(&self.text)
    }

    /// Whether the span draws nothing.
    pub fn is_blank(&self) -> bool {
        self.text.is_empty()
    }
}

/// One line of styled spans. An empty line is a valid, useful thing: it is the gap between blocks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    /// The spans, drawn left to right.
    pub spans: Vec<Span>,
}

impl Line {
    /// An empty line.
    pub fn blank() -> Self {
        Self::default()
    }

    /// A single unstyled span.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            spans: vec![Span::new(text)],
        }
    }

    /// A single styled span.
    pub fn of(span: Span) -> Self {
        Self { spans: vec![span] }
    }

    /// Builder: append a span.
    pub fn with(mut self, span: Span) -> Self {
        self.spans.push(span);
        self
    }

    /// Builder: append unstyled text.
    pub fn then(self, text: impl Into<String>) -> Self {
        self.with(Span::new(text))
    }

    /// Builder: append muted text.
    pub fn then_muted(self, text: impl Into<String>) -> Self {
        self.with(Span::muted(text))
    }

    /// Builder: append another line's spans, keeping their styles.
    pub fn then_spans(mut self, other: Line) -> Self {
        self.spans.extend(other.spans);
        self
    }

    /// Prefix `columns` spaces. Used to indent a cell or a hanging tagline.
    pub fn indent(mut self, columns: usize) -> Self {
        if columns > 0 {
            self.spans.insert(0, Span::new(" ".repeat(columns)));
        }
        self
    }

    /// Columns the whole line occupies on screen.
    pub fn width(&self) -> usize {
        self.spans.iter().map(Span::width).sum()
    }

    /// Whether the line draws nothing at all.
    pub fn is_blank(&self) -> bool {
        self.spans.iter().all(Span::is_blank)
    }

    /// Drop trailing whitespace, so a padded grid row does not end in spaces on screen.
    ///
    /// Padding is how a column starts where it should; the *last* column has nothing to align to, so
    /// its padding is emitted as literal spaces unless it is trimmed here.
    pub fn trim_end(mut self) -> Self {
        while let Some(last) = self.spans.last_mut() {
            let trimmed = last.text.trim_end().len();
            if trimmed == last.text.len() {
                break;
            }
            last.text.truncate(trimmed);
            if !last.text.is_empty() || self.spans.len() == 1 {
                break;
            }
            self.spans.pop();
        }
        self
    }

    /// The text with the styling dropped, which is what a test asserts against.
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

/// Collect the plain text of a block, one entry per line. Test and snapshot helper.
pub fn text_of(lines: &[Line]) -> Vec<String> {
    lines.iter().map(Line::text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_is_measured_in_display_columns_not_chars() {
        let line = Line::plain("🌙").with(Span::new(" ok"));
        assert_eq!(line.width(), 5);
        assert_eq!(line.text(), "🌙 ok");
    }

    #[test]
    fn a_blank_line_is_blank_and_measures_zero() {
        assert!(Line::blank().is_blank());
        assert_eq!(Line::blank().width(), 0);
        assert!(!Line::plain("x").is_blank());
    }

    #[test]
    fn indenting_adds_columns_without_changing_the_text() {
        let line = Line::plain("hi").indent(3);
        assert_eq!(line.width(), 5);
        assert_eq!(line.text(), "   hi");
        assert_eq!(Line::plain("hi").indent(0).width(), 2);
    }

    #[test]
    fn trimming_drops_the_padding_of_the_last_column_only() {
        let line = Line::plain("1  name").with(Span::muted("     "));
        assert_eq!(line.clone().trim_end().text(), "1  name");
        assert_eq!(line.trim_end().width(), 7);

        // A line that is nothing but padding becomes empty rather than shrinking to one space.
        assert!(Line::plain("   ").trim_end().is_blank());
        assert_eq!(Line::plain("  x  ").trim_end().text(), "  x");
    }

    #[test]
    fn builders_keep_the_style_of_each_span() {
        let line = Line::of(Span::strong("Luna", CREST_COLOR))
            .then_muted(" · ")
            .with(Span::colored("1", ACCENT_COLOR));
        assert_eq!(line.spans.len(), 3);
        assert!(line.spans[0].bold);
        assert_eq!(line.spans[1].color, Some(MUTED_COLOR));
        assert_eq!(line.spans[2].color, Some(ACCENT_COLOR));
        assert_eq!(text_of(&[line]), vec!["Luna · 1"]);
    }
}
