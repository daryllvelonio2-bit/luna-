//! Markdown and plain-text wrapping into `ratatui` styled lines.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::theme::{ACCENT_CORAL, BG_CODE, CYAN_LINK, TEXT_DIM, TEXT_MUTED, TEXT_PRIMARY};

/// Render Markdown into wrapped `ratatui` lines constrained to `max_width` terminal cells.
pub fn render_markdown(input: &str, max_width: usize) -> Vec<Line<'static>> {
    let width = max_width.max(16);
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);

    let parser = Parser::new_ext(input, opts);
    let mut out: Vec<Line<'static>> = Vec::new();

    let mut cur_spans: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default().fg(TEXT_PRIMARY)];
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut blockquote_depth: usize = 0;
    let mut pending_prefix: Option<Vec<Span<'static>>> = None;
    let mut ansi = AnsiLexer::default();

    let flush_line = |out: &mut Vec<Line<'static>>,
                      cur_spans: &mut Vec<Span<'static>>,
                      blockquote_depth: usize,
                      pending_prefix: &mut Option<Vec<Span<'static>>>| {
        if cur_spans.is_empty() && pending_prefix.is_none() {
            return;
        }
        let mut prefix = Vec::new();
        for _ in 0..blockquote_depth {
            prefix.push(Span::styled("│ ", Style::default().fg(TEXT_DIM)));
        }
        if let Some(p) = pending_prefix.take() {
            prefix.extend(p);
        }
        let mut combined = prefix;
        combined.append(cur_spans);
        let wrapped = wrap_spans(&combined, width);
        out.extend(wrapped);
    };

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    if !out.is_empty() && !out.last().is_some_and(|l| l.spans.is_empty()) {
                        out.push(Line::default());
                    }
                    let color = match level {
                        HeadingLevel::H1 => ACCENT_CORAL,
                        _ => TEXT_PRIMARY,
                    };
                    style_stack.push(Style::default().fg(color).add_modifier(Modifier::BOLD));
                }
                Tag::BlockQuote => {
                    blockquote_depth += 1;
                    style_stack.push(
                        Style::default()
                            .fg(TEXT_MUTED)
                            .add_modifier(Modifier::ITALIC),
                    );
                }
                Tag::CodeBlock(kind) => {
                    flush_line(
                        &mut out,
                        &mut cur_spans,
                        blockquote_depth,
                        &mut pending_prefix,
                    );
                    if !out.is_empty() && !out.last().is_some_and(|l| l.spans.is_empty()) {
                        out.push(Line::default());
                    }
                    in_code_block = true;
                    code_buf.clear();
                    code_lang = match kind {
                        CodeBlockKind::Fenced(lang) => lang.split_whitespace().next().unwrap_or("").to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                }
                Tag::List(start) => {
                    list_stack.push(start);
                }
                Tag::Item => {
                    flush_line(
                        &mut out,
                        &mut cur_spans,
                        blockquote_depth,
                        &mut pending_prefix,
                    );
                    let indent = "  ".repeat(list_stack.len().saturating_sub(1));
                    let marker = match list_stack.last_mut() {
                        Some(Some(num)) => {
                            let m = format!("{indent}{num}. ");
                            *num += 1;
                            m
                        }
                        _ => format!("{indent}• "),
                    };
                    pending_prefix = Some(vec![Span::styled(
                        marker,
                        Style::default().fg(ACCENT_CORAL),
                    )]);
                }
                Tag::Emphasis => {
                    let top = *style_stack.last().unwrap_or(&Style::default());
                    style_stack.push(top.add_modifier(Modifier::ITALIC));
                }
                Tag::Strong => {
                    let top = *style_stack.last().unwrap_or(&Style::default());
                    style_stack.push(top.add_modifier(Modifier::BOLD));
                }
                Tag::Strikethrough => {
                    let top = *style_stack.last().unwrap_or(&Style::default());
                    style_stack.push(top.add_modifier(Modifier::CROSSED_OUT));
                }
                Tag::Link { .. } => {
                    let top = *style_stack.last().unwrap_or(&Style::default());
                    style_stack.push(top.fg(CYAN_LINK).add_modifier(Modifier::UNDERLINED));
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    flush_line(
                        &mut out,
                        &mut cur_spans,
                        blockquote_depth,
                        &mut pending_prefix,
                    );
                    out.push(Line::default());
                }
                TagEnd::Heading(_) => {
                    style_stack.pop();
                    flush_line(
                        &mut out,
                        &mut cur_spans,
                        blockquote_depth,
                        &mut pending_prefix,
                    );
                    out.push(Line::default());
                }
                TagEnd::BlockQuote => {
                    flush_line(
                        &mut out,
                        &mut cur_spans,
                        blockquote_depth,
                        &mut pending_prefix,
                    );
                    blockquote_depth = blockquote_depth.saturating_sub(1);
                    style_stack.pop();
                    out.push(Line::default());
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    let code_width = width.saturating_sub(2);
                    if !code_lang.is_empty() {
                        out.push(Line::from(vec![
                            Span::styled("│ ", Style::default().fg(TEXT_DIM).bg(BG_CODE)),
                            Span::styled(
                                format!("{:<width$}", code_lang, width = code_width),
                                Style::default().fg(TEXT_MUTED).bg(BG_CODE),
                            ),
                        ]));
                    }
                    for raw_line in code_buf.lines() {
                        for chunk in wrap_plain_text(raw_line, code_width.max(12)) {
                            let pad = code_width.saturating_sub(chunk.width());
                            out.push(Line::from(vec![
                                Span::styled("│ ", Style::default().fg(TEXT_DIM).bg(BG_CODE)),
                                Span::styled(
                                    format!("{chunk}{}", " ".repeat(pad)),
                                    Style::default().fg(TEXT_PRIMARY).bg(BG_CODE),
                                ),
                            ]));
                        }
                    }
                    if code_buf.is_empty() {
                        out.push(Line::from(Span::styled(
                            "│ ",
                            Style::default().fg(TEXT_DIM).bg(BG_CODE),
                        )));
                    }
                    out.push(Line::default());
                }
                TagEnd::List(_) => {
                    flush_line(
                        &mut out,
                        &mut cur_spans,
                        blockquote_depth,
                        &mut pending_prefix,
                    );
                    list_stack.pop();
                    if list_stack.is_empty() {
                        out.push(Line::default());
                    }
                }
                TagEnd::Item => {
                    flush_line(
                        &mut out,
                        &mut cur_spans,
                        blockquote_depth,
                        &mut pending_prefix,
                    );
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                    style_stack.pop();
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_code_block {
                    code_buf.push_str(&strip_ansi(&text));
                } else {
                    let style = *style_stack.last().unwrap_or(&Style::default());
                    ansi.feed(&text, style, &mut cur_spans);
                }
            }
            Event::Code(code) => {
                cur_spans.push(Span::styled(
                    format!("`{}`", strip_ansi(&code)),
                    Style::default().fg(TEXT_PRIMARY).bg(BG_CODE),
                ));
            }
            Event::SoftBreak => {
                if in_code_block {
                    code_buf.push('\n');
                } else {
                    let style = *style_stack.last().unwrap_or(&Style::default());
                    cur_spans.push(Span::styled(" ", style));
                }
            }
            Event::HardBreak => {
                flush_line(
                    &mut out,
                    &mut cur_spans,
                    blockquote_depth,
                    &mut pending_prefix,
                );
            }
            Event::Rule => {
                flush_line(
                    &mut out,
                    &mut cur_spans,
                    blockquote_depth,
                    &mut pending_prefix,
                );
                out.push(Line::from(Span::styled(
                    "─".repeat(width.min(48)),
                    Style::default().fg(TEXT_DIM),
                )));
            }
            _ => {}
        }
    }

    flush_line(
        &mut out,
        &mut cur_spans,
        blockquote_depth,
        &mut pending_prefix,
    );

    // Trim trailing blank lines so metadata footers sit tightly below the reply.
    while out.last().is_some_and(|l| l.spans.is_empty()) {
        out.pop();
    }

    out
}

/// Wrap plain text into strings at most `max_width` display cells wide.
pub fn wrap_plain_text(text: &str, max_width: usize) -> Vec<String> {
    let width = max_width.max(1);
    let mut lines = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut cur_w = 0usize;
        for ch in raw_line.chars() {
            let cw = ch.width().unwrap_or(0);
            if cur_w + cw > width && !current.is_empty() {
                // Try breaking at last space for readability
                if let Some(space_idx) = current.rfind(' ') {
                    if space_idx > 0 {
                        let tail = current[space_idx + 1..].to_string();
                        current.truncate(space_idx);
                        lines.push(current);
                        current = tail;
                        cur_w = current.width();
                        if cur_w + cw <= width {
                            current.push(ch);
                            cur_w += cw;
                            continue;
                        }
                    }
                }
                lines.push(std::mem::take(&mut current));
                cur_w = 0;
            }
            current.push(ch);
            cur_w += cw;
        }
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Word/cell-wrap a list of styled spans to fit within `max_width` display cells.
pub fn wrap_spans(spans: &[Span<'static>], max_width: usize) -> Vec<Line<'static>> {
    let width = max_width.max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;

    for span in spans {
        let style = span.style;
        let mut buf = String::new();
        for token in split_keep_spaces(span.content.as_ref()) {
            let tw = token.width();
            if cur_w + tw > width && cur_w > 0 && !token.trim().is_empty() {
                if !buf.is_empty() {
                    current.push(Span::styled(std::mem::take(&mut buf), style));
                }
                lines.push(Line::from(std::mem::take(&mut current)));
                cur_w = 0;
            }
            if tw > width {
                // Break oversized token char-by-char
                for ch in token.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if cur_w + cw > width && cur_w > 0 {
                        if !buf.is_empty() {
                            current.push(Span::styled(std::mem::take(&mut buf), style));
                        }
                        lines.push(Line::from(std::mem::take(&mut current)));
                        cur_w = 0;
                    }
                    buf.push(ch);
                    cur_w += cw;
                }
            } else {
                // Skip leading space at start of wrapped line
                if cur_w == 0 && token == " " {
                    continue;
                }
                buf.push_str(token);
                cur_w += tw;
            }
        }
        if !buf.is_empty() {
            current.push(Span::styled(buf, style));
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

fn split_keep_spaces(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut in_space = None;
    for (idx, ch) in s.char_indices() {
        let is_sp = ch == ' ';
        match in_space {
            None => in_space = Some(is_sp),
            Some(prev) if prev != is_sp => {
                out.push(&s[start..idx]);
                start = idx;
                in_space = Some(is_sp);
            }
            _ => {}
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Strip ANSI escape codes from legacy command output so it renders cleanly in `ratatui`.
pub fn strip_ansi(s: &str) -> String {
    dialoguer::console::strip_ansi_codes(s).to_string()
}

/// The SGR state carried by an ANSI escape sequence in model prose.
#[derive(Debug, Clone, Copy, Default)]
struct AnsiStyle {
    fg: Option<Color>,
    bg: Option<Color>,
    modifiers: Modifier,
}

impl AnsiStyle {
    fn apply_to(self, base: Style) -> Style {
        let mut style = base;
        if let Some(fg) = self.fg {
            style = style.fg(fg);
        }
        if let Some(bg) = self.bg {
            style = style.bg(bg);
        }
        style.add_modifier(self.modifiers)
    }

    fn apply_sgr(&mut self, params: &str) {
        let values: Vec<u32> = if params.is_empty() {
            vec![0]
        } else {
            params
                .split(';')
                .map(|value| value.parse().unwrap_or(0))
                .collect()
        };
        let mut index = 0;
        while index < values.len() {
            match values[index] {
                0 => *self = Self::default(),
                1 => self.modifiers.insert(Modifier::BOLD),
                2 => self.modifiers.insert(Modifier::DIM),
                3 => self.modifiers.insert(Modifier::ITALIC),
                4 => self.modifiers.insert(Modifier::UNDERLINED),
                7 => self.modifiers.insert(Modifier::REVERSED),
                9 => self.modifiers.insert(Modifier::CROSSED_OUT),
                22 => {
                    self.modifiers.remove(Modifier::BOLD);
                    self.modifiers.remove(Modifier::DIM);
                }
                23 => self.modifiers.remove(Modifier::ITALIC),
                24 => self.modifiers.remove(Modifier::UNDERLINED),
                27 => self.modifiers.remove(Modifier::REVERSED),
                29 => self.modifiers.remove(Modifier::CROSSED_OUT),
                30..=37 => self.fg = Some(basic_color(values[index] - 30)),
                39 => self.fg = None,
                40..=47 => self.bg = Some(basic_color(values[index] - 40)),
                49 => self.bg = None,
                90..=97 => self.fg = Some(bright_color(values[index] - 90)),
                100..=107 => self.bg = Some(bright_color(values[index] - 100)),
                38 | 48 => {
                    if let Some((color, consumed)) = extended_color(&values[index + 1..]) {
                        if values[index] == 38 {
                            self.fg = Some(color);
                        } else {
                            self.bg = Some(color);
                        }
                        index += consumed;
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }
}

fn basic_color(index: u32) -> Color {
    match index {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        _ => Color::Gray,
    }
}

fn bright_color(index: u32) -> Color {
    match index {
        0 => Color::DarkGray,
        1 => Color::LightRed,
        2 => Color::LightGreen,
        3 => Color::LightYellow,
        4 => Color::LightBlue,
        5 => Color::LightMagenta,
        6 => Color::LightCyan,
        _ => Color::White,
    }
}

/// Parse the parameters after `38`/`48`: `5;n` for a palette index or `2;r;g;b` for true color.
fn extended_color(values: &[u32]) -> Option<(Color, usize)> {
    match values.first().copied()? {
        5 => {
            let index = *values.get(1)?;
            Some((Color::Indexed(u8::try_from(index).ok()?), 2))
        }
        2 => {
            let red = u8::try_from(*values.get(1)?).ok()?;
            let green = u8::try_from(*values.get(2)?).ok()?;
            let blue = u8::try_from(*values.get(3)?).ok()?;
            Some((Color::Rgb(red, green, blue), 4))
        }
        _ => None,
    }
}

/// Split model prose on ANSI escapes, keeping SGR color and dropping every other control sequence.
///
/// The lexer is stateful because the Markdown parser splits text at the escape bytes: a sequence
/// such as `\x1b[31m` can arrive as three separate text events, so the half-read sequence and the
/// current color have to survive across them.
#[derive(Debug, Default)]
struct AnsiLexer {
    state: AnsiStyle,
    escape: EscapeState,
}

#[derive(Debug, Default)]
enum EscapeState {
    #[default]
    None,
    /// Saw ESC and need the next character to know which sequence this is.
    Started,
    /// Inside `ESC [ ...`; the collected parameters, without the introducer.
    Csi(String),
    /// Inside `ESC ] ...`, skipped until BEL or ST.
    Osc { previous_is_esc: bool },
}

impl AnsiLexer {
    fn feed(&mut self, text: &str, base: Style, spans: &mut Vec<Span<'static>>) {
        let mut plain = String::new();
        for ch in text.chars() {
            match &mut self.escape {
                EscapeState::None => {
                    if ch == '\x1b' {
                        if !plain.is_empty() {
                            spans.push(Span::styled(
                                std::mem::take(&mut plain),
                                self.state.apply_to(base),
                            ));
                        }
                        self.escape = EscapeState::Started;
                    } else {
                        plain.push(ch);
                    }
                }
                EscapeState::Started => {
                    self.escape = match ch {
                        '[' => EscapeState::Csi(String::new()),
                        ']' => EscapeState::Osc {
                            previous_is_esc: false,
                        },
                        _ => EscapeState::None,
                    };
                }
                EscapeState::Csi(params) => {
                    if ('\u{40}'..='\u{7e}').contains(&ch) {
                        if ch == 'm' {
                            self.state.apply_sgr(params);
                        }
                        self.escape = EscapeState::None;
                    } else {
                        params.push(ch);
                    }
                }
                EscapeState::Osc { previous_is_esc } => {
                    if ch == '\u{7}' || (*previous_is_esc && ch == '\\') {
                        self.escape = EscapeState::None;
                    } else {
                        *previous_is_esc = ch == '\x1b';
                    }
                }
            }
        }
        if !plain.is_empty() {
            spans.push(Span::styled(plain, self.state.apply_to(base)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(input: &str) -> Vec<Span<'static>> {
        let mut lexer = AnsiLexer::default();
        let mut spans = Vec::new();
        for event in Parser::new(input) {
            if let Event::Text(text) = event {
                lexer.feed(&text, Style::default(), &mut spans);
            }
        }
        spans
    }

    #[test]
    fn ansi_color_becomes_a_ratatui_span_and_disappears_from_the_text() {
        let spans = feed_all("a \x1b[31mred\x1b[0m word");
        let content: String = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(content, "a red word");
        assert!(
            spans.iter().any(|span| span.style.fg == Some(Color::Red)),
            "the red run must keep its color"
        );
    }

    #[test]
    fn ansi_styles_combine_with_markdown_styles() {
        let mut lexer = AnsiLexer::default();
        let mut spans = Vec::new();
        let bold = Style::default().add_modifier(Modifier::BOLD);
        for event in Parser::new("**\x1b[32mgreen\x1b[0m**") {
            if let Event::Text(text) = event {
                lexer.feed(&text, bold, &mut spans);
            }
        }
        let green = spans
            .iter()
            .find(|span| span.style.fg == Some(Color::Green))
            .expect("green span");
        assert!(green.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn unknown_escape_sequences_do_not_reach_the_transcript() {
        let spans = feed_all("ok \x1b[2Kdone\x1b]8;;https://example.com\x07 link\x1b]8;;\x07");
        let content: String = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(content, "ok done link");
        assert!(!content.contains('\x1b'), "escape bytes leaked: {content:?}");
    }
}
