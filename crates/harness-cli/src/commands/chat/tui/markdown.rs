//! Markdown and plain-text wrapping into `ratatui` styled lines.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::theme::{
    ACCENT_AUTO, ACCENT_CORAL, BG_CODE, BORDER_SUBTLE, CODE_COMMENT, CODE_FUNCTION, CODE_INLINE,
    CODE_KEY, CODE_KEYWORD, CODE_NUMBER, CODE_STRING, CYAN_LINK, TEXT_DIM, TEXT_MUTED,
    TEXT_PRIMARY,
};

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
    // Tables are buffered cell-by-cell, measured once, then drawn with box borders so the
    // columns stay aligned regardless of the terminal width.
    let mut table: Option<TableBuffer> = None;

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
        // While inside a table every event feeds the current cell instead of the prose buffer.
        if matches!(&event, Event::End(TagEnd::Table)) {
            if let Some(buf) = table.take() {
                render_table(&mut out, &buf, width);
                out.push(Line::default());
            }
            continue;
        }
        if let Some(buf) = table.as_mut() {
            match &event {
                Event::Start(Tag::TableHead) => {}
                Event::End(TagEnd::TableHead) => {
                    if !buf.row.is_empty() {
                        let row = std::mem::take(&mut buf.row);
                        buf.rows.push(row);
                    }
                    buf.header_rows = buf.rows.len();
                }
                Event::Start(Tag::TableRow) => {}
                Event::End(TagEnd::TableRow) => {
                    let row = std::mem::take(&mut buf.row);
                    if !row.is_empty() {
                        buf.rows.push(row);
                    }
                }
                Event::Start(Tag::TableCell) => buf.cell.clear(),
                Event::End(TagEnd::TableCell) => {
                    let cell = buf.cell.trim().to_string();
                    buf.row.push(cell);
                    buf.cell.clear();
                }
                Event::Text(text) => buf.cell.push_str(&strip_ansi(text)),
                Event::Code(code) => buf.cell.push_str(&strip_ansi(code)),
                Event::SoftBreak | Event::HardBreak => buf.cell.push(' '),
                _ => {}
            }
            continue;
        }

        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    if !out.is_empty() && !out.last().is_some_and(|l| l.spans.is_empty()) {
                        out.push(Line::default());
                    }
                    let color = match level {
                        HeadingLevel::H1 => ACCENT_CORAL,
                        HeadingLevel::H2 => ACCENT_AUTO,
                        _ => TEXT_PRIMARY,
                    };
                    style_stack.push(Style::default().fg(color).add_modifier(Modifier::BOLD));
                }
                Tag::Table(alignments) => {
                    flush_line(
                        &mut out,
                        &mut cur_spans,
                        blockquote_depth,
                        &mut pending_prefix,
                    );
                    if !out.is_empty() && !out.last().is_some_and(|l| l.spans.is_empty()) {
                        out.push(Line::default());
                    }
                    table = Some(TableBuffer::new(alignments));
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
                        CodeBlockKind::Fenced(lang) => {
                            lang.split_whitespace().next().unwrap_or("").to_string()
                        }
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
                        let highlighted = highlight_code_line(&code_lang, raw_line);
                        for chunk in wrap_spans(&highlighted, code_width.max(12)) {
                            let pad = code_width.saturating_sub(chunk.width());
                            let mut spans = vec![Span::styled(
                                "│ ",
                                Style::default().fg(TEXT_DIM).bg(BG_CODE),
                            )];
                            for span in chunk.spans {
                                spans.push(Span::styled(span.content, span.style.bg(BG_CODE)));
                            }
                            if pad > 0 {
                                spans.push(Span::styled(
                                    " ".repeat(pad),
                                    Style::default().bg(BG_CODE),
                                ));
                            }
                            out.push(Line::from(spans));
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
                    Style::default().fg(CODE_INLINE).bg(BG_CODE),
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

/// Buffered Markdown table: rows of plain cell text plus the column alignments.
#[derive(Debug, Default)]
struct TableBuffer {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<String>>,
    header_rows: usize,
    row: Vec<String>,
    cell: String,
}

impl TableBuffer {
    fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            ..Default::default()
        }
    }
}

/// Draw the buffered table with box borders, clipped to `width` terminal cells.
fn render_table(out: &mut Vec<Line<'static>>, table: &TableBuffer, width: usize) {
    let cols = table.rows.iter().map(|row| row.len()).max().unwrap_or(0);
    if cols == 0 {
        return;
    }

    let natural: Vec<usize> = (0..cols)
        .map(|col| {
            table
                .rows
                .iter()
                .filter_map(|row| row.get(col))
                .map(|cell| cell.width())
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect();

    let avail = width.max(8);
    // Each column costs its content plus one space of padding on both sides; plus the outer walls.
    let max_content = avail.saturating_sub(3 * cols + 1);
    let mut widths = natural;
    while widths.iter().sum::<usize>() > max_content {
        let Some((widest, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, w)| **w > 3)
            .max_by_key(|(_, w)| **w)
        else {
            break;
        };
        widths[widest] -= 1;
    }

    let border = Style::default().fg(BORDER_SUBTLE);
    let rule = |left: &str, mid: &str, right: &str| {
        let mut s = String::from(left);
        for (i, w) in widths.iter().enumerate() {
            if i > 0 {
                s.push_str(mid);
            }
            s.push_str(&"─".repeat(w + 2));
        }
        s.push_str(right);
        s
    };

    let mut push_line = |spans: Vec<Span<'static>>| out.push(Line::from(spans));

    push_line(vec![Span::styled(rule("┌", "┬", "┐"), border)]);

    for (row_idx, row) in table.rows.iter().enumerate() {
        let is_header = row_idx < table.header_rows;
        let mut spans = vec![Span::styled("│", border)];
        for (col, w) in widths.iter().enumerate() {
            let text = row.get(col).map(String::as_str).unwrap_or("");
            let clipped = clip_cells(text, *w);
            let pad = w.saturating_sub(clipped.width());
            let align = table
                .alignments
                .get(col)
                .copied()
                .unwrap_or(Alignment::None);
            let (before, after) = match align {
                Alignment::Right => (pad, 0),
                Alignment::Center => (pad / 2, pad - pad / 2),
                _ => (0, pad),
            };
            let style = if is_header {
                Style::default()
                    .fg(ACCENT_CORAL)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT_PRIMARY)
            };
            spans.push(Span::styled(" ", border));
            spans.push(Span::styled(" ".repeat(before), style));
            spans.push(Span::styled(clipped, style));
            spans.push(Span::styled(" ".repeat(after), style));
            spans.push(Span::styled(" ", border));
            spans.push(Span::styled("│", border));
        }
        push_line(spans);

        if is_header && row_idx + 1 == table.header_rows && table.header_rows > 0 {
            push_line(vec![Span::styled(rule("├", "┼", "┤"), border)]);
        }
    }

    push_line(vec![Span::styled(rule("└", "┴", "┘"), border)]);
}

/// Clip a string to `max_cells` display cells, marking the cut with an ellipsis.
fn clip_cells(s: &str, max_cells: usize) -> String {
    if s.width() <= max_cells {
        return s.to_string();
    }
    if max_cells == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > max_cells {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Language family drives which tokens get colour; unknown tags fall back to strings/numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    Js,
    Go,
    CLike,
    Sql,
    Bash,
    Data,
    Generic,
}

impl Lang {
    fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "rust" | "rs" => Lang::Rust,
            "python" | "py" => Lang::Python,
            "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx" | "mjs" | "cjs" => Lang::Js,
            "go" | "golang" => Lang::Go,
            "c" | "h" | "cpp" | "c++" | "cc" | "hpp" | "java" | "cs" | "csharp" | "kt"
            | "kotlin" | "swift" => Lang::CLike,
            "sql" => Lang::Sql,
            "sh" | "bash" | "zsh" | "shell" | "console" => Lang::Bash,
            "json" | "toml" | "yaml" | "yml" | "ini" | "env" => Lang::Data,
            _ => Lang::Generic,
        }
    }

    fn keywords(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &[
                "fn", "let", "mut", "pub", "use", "mod", "struct", "enum", "impl", "trait", "for",
                "while", "loop", "if", "else", "match", "return", "self", "Self", "crate", "super",
                "const", "static", "async", "await", "move", "ref", "where", "dyn", "unsafe",
                "extern", "type", "in", "as", "break", "continue", "true", "false", "Some", "None",
                "Ok", "Err",
            ],
            Lang::Python => &[
                "def", "class", "import", "from", "return", "if", "elif", "else", "for", "while",
                "in", "not", "and", "or", "is", "None", "True", "False", "try", "except",
                "finally", "with", "as", "lambda", "yield", "async", "await", "pass", "raise",
                "global", "nonlocal", "assert", "del", "break", "continue",
            ],
            Lang::Js => &[
                "const",
                "let",
                "var",
                "function",
                "return",
                "if",
                "else",
                "for",
                "while",
                "do",
                "switch",
                "case",
                "break",
                "continue",
                "new",
                "class",
                "extends",
                "import",
                "export",
                "default",
                "from",
                "async",
                "await",
                "try",
                "catch",
                "finally",
                "throw",
                "typeof",
                "instanceof",
                "this",
                "null",
                "undefined",
                "true",
                "false",
                "interface",
                "type",
                "enum",
                "implements",
                "public",
                "private",
                "readonly",
                "static",
            ],
            Lang::Go => &[
                "func",
                "package",
                "import",
                "var",
                "const",
                "type",
                "struct",
                "interface",
                "map",
                "chan",
                "go",
                "defer",
                "return",
                "if",
                "else",
                "for",
                "range",
                "switch",
                "case",
                "break",
                "continue",
                "select",
                "nil",
                "true",
                "false",
            ],
            Lang::CLike => &[
                "int",
                "void",
                "char",
                "float",
                "double",
                "long",
                "short",
                "bool",
                "class",
                "struct",
                "public",
                "private",
                "protected",
                "static",
                "final",
                "const",
                "return",
                "if",
                "else",
                "for",
                "while",
                "switch",
                "case",
                "break",
                "continue",
                "new",
                "delete",
                "include",
                "namespace",
                "using",
                "template",
                "typename",
                "virtual",
                "override",
                "nullptr",
                "NULL",
                "true",
                "false",
                "auto",
            ],
            Lang::Sql => &[
                "select",
                "from",
                "where",
                "insert",
                "into",
                "values",
                "update",
                "set",
                "delete",
                "create",
                "table",
                "drop",
                "alter",
                "join",
                "left",
                "right",
                "inner",
                "outer",
                "on",
                "group",
                "by",
                "order",
                "having",
                "limit",
                "offset",
                "as",
                "and",
                "or",
                "not",
                "null",
                "is",
                "distinct",
                "count",
                "sum",
                "avg",
                "min",
                "max",
                "primary",
                "key",
                "foreign",
                "references",
                "index",
                "union",
                "all",
                "with",
            ],
            Lang::Bash => &[
                "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac",
                "function", "return", "export", "local", "echo", "cd", "source", "set", "unset",
                "in",
            ],
            Lang::Data => &["true", "false", "null"],
            Lang::Generic => &[],
        }
    }

    fn comment(self) -> Option<&'static str> {
        match self {
            Lang::Rust | Lang::Js | Lang::Go | Lang::CLike => Some("//"),
            Lang::Python | Lang::Bash | Lang::Data => Some("#"),
            Lang::Sql => Some("--"),
            Lang::Generic => None,
        }
    }

    fn template_strings(self) -> bool {
        matches!(self, Lang::Js | Lang::Bash)
    }

    fn keys_are_colored(self) -> bool {
        self == Lang::Data
    }
}

fn next_significant(chars: &[char], mut idx: usize) -> Option<char> {
    while idx < chars.len() && chars[idx].is_whitespace() {
        idx += 1;
    }
    chars.get(idx).copied()
}

/// Tokenize one line of a fenced code block into coloured spans.
fn highlight_code_line(lang_tag: &str, line: &str) -> Vec<Span<'static>> {
    let lang = Lang::from_tag(lang_tag);
    let chars: Vec<char> = line.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;

    let flush_plain = |plain: &mut String, spans: &mut Vec<Span<'static>>| {
        if !plain.is_empty() {
            spans.push(Span::styled(
                std::mem::take(plain),
                Style::default().fg(TEXT_PRIMARY),
            ));
        }
    };

    while i < chars.len() {
        if let Some(marker) = lang.comment() {
            let marker: Vec<char> = marker.chars().collect();
            if chars[i..].starts_with(&marker) {
                flush_plain(&mut plain, &mut spans);
                spans.push(Span::styled(
                    chars[i..].iter().collect::<String>(),
                    Style::default()
                        .fg(CODE_COMMENT)
                        .add_modifier(Modifier::ITALIC),
                ));
                break;
            }
        }

        let ch = chars[i];

        if ch == '"' || ch == '\'' || (ch == '`' && lang.template_strings()) {
            let quote = ch;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let literal: String = chars[start..i.min(chars.len())].iter().collect();
            flush_plain(&mut plain, &mut spans);
            let is_key = lang.keys_are_colored()
                && matches!(next_significant(&chars, i), Some(':') | Some('='));
            let color = if is_key { CODE_KEY } else { CODE_STRING };
            spans.push(Span::styled(literal, Style::default().fg(color)));
            continue;
        }

        if ch.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || chars[i] == '.'
                    || chars[i] == '_'
                    || chars[i] == 'x')
            {
                i += 1;
            }
            flush_plain(&mut plain, &mut spans);
            spans.push(Span::styled(
                chars[start..i].iter().collect::<String>(),
                Style::default().fg(CODE_NUMBER),
            ));
            continue;
        }

        if ch.is_alphabetic() || ch == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let mut probe = i;
            if lang == Lang::Rust && chars.get(probe) == Some(&'!') {
                probe += 1;
            }
            let style = if lang
                .keywords()
                .iter()
                .any(|keyword| keyword.eq_ignore_ascii_case(&word))
            {
                Style::default()
                    .fg(CODE_KEYWORD)
                    .add_modifier(Modifier::BOLD)
            } else if next_significant(&chars, probe) == Some('(') {
                Style::default().fg(CODE_FUNCTION)
            } else if lang.keys_are_colored()
                && matches!(next_significant(&chars, i), Some(':') | Some('='))
            {
                Style::default().fg(CODE_KEY)
            } else {
                Style::default().fg(TEXT_PRIMARY)
            };
            flush_plain(&mut plain, &mut spans);
            spans.push(Span::styled(word, style));
            continue;
        }

        plain.push(ch);
        i += 1;
    }

    flush_plain(&mut plain, &mut spans);
    spans
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
    if !s.contains('\x1b') {
        return s.to_string();
    }
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

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

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
    fn markdown_table_renders_aligned_borders_and_a_colored_header() {
        let md = "| Provider | Model | Tokens |\n|:---|---:|---:|\n| openai | gpt-5 | 1200 |\n| gemini | pro | 98000 |";
        let lines = render_markdown(md, 64);
        let joined = text_of(&lines);

        assert!(joined.contains("┌"), "missing table top border:\n{joined}");
        assert!(
            joined.contains("└"),
            "missing table bottom border:\n{joined}"
        );
        assert!(joined.contains("├"), "missing header separator:\n{joined}");
        assert!(joined.contains("Provider"));
        assert!(joined.contains("gemini"));
        assert!(
            lines.iter().all(|l| l.width() <= 64),
            "table overflowed:\n{joined}"
        );

        let header = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "Provider"))
            .expect("header row");
        assert!(
            header
                .spans
                .iter()
                .any(|s| s.style.fg == Some(ACCENT_CORAL)),
            "header cells should use the accent color"
        );
    }

    #[test]
    fn four_column_table_keeps_cells_on_their_own_rows() {
        let md = "| Tool | Status | Duration | Tokens |\n|---|---|---|---|\n| luna | ok | 0.12s | 842 |\n| cargo test | fail | 45.33s | 1240 |";
        let lines = render_markdown(md, 80);
        let joined = text_of(&lines);
        let flat = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>();

        assert!(joined.contains("┌"), "missing table top border:\n{joined}");
        assert!(joined.contains("└"), "missing table bottom border:\n{joined}");
        assert_eq!(
            flat.iter().filter(|line| line.starts_with('│')).count(),
            3,
            "expected a header row and two body rows:\n{joined}"
        );
        for row in ["luna", "cargo test"] {
            let line = flat
                .iter()
                .find(|line| line.contains(row))
                .unwrap_or_else(|| panic!("missing row {row}:\n{joined}"));
            assert!(
                line.contains("ok") || line.contains("fail"),
                "row {row} lost its cells:\n{joined}"
            );
        }
        assert!(
            !flat
                .iter()
                .any(|line| line.contains("Tokensluna") || line.contains("Toolsluna")),
            "cells ran together:\n{joined}"
        );
    }

    #[test]
    fn wide_table_clips_cells_instead_of_wrapping_outside_the_frame() {
        let md = "| Key | Description |\n|---|---|\n| a_very_long_key_name | a very long description that cannot possibly fit |";
        let lines = render_markdown(md, 32);
        let joined = text_of(&lines);
        assert!(
            lines.iter().all(|l| l.width() <= 32),
            "table overflowed:\n{joined}"
        );
        assert!(joined.contains('…'), "expected clipped cells:\n{joined}");
    }

    #[test]
    fn fenced_code_is_syntax_colored() {
        let md =
            "```rust\nfn main() {\n    let count = 42; // note\n    println!(\"hello\");\n}\n```";
        let lines = render_markdown(md, 64);
        let spans: Vec<&Span<'static>> = lines.iter().flat_map(|l| l.spans.iter()).collect();

        assert!(
            spans
                .iter()
                .any(|s| s.content.as_ref() == "fn" && s.style.fg == Some(CODE_KEYWORD)),
            "keyword `fn` should use the keyword color"
        );
        assert!(
            spans
                .iter()
                .any(|s| s.content.as_ref() == "42" && s.style.fg == Some(CODE_NUMBER)),
            "number literal should use the number color"
        );
        assert!(
            spans.iter().any(|s| s.content.as_ref().contains("// note")
                && s.style.fg == Some(CODE_COMMENT)),
            "comment should use the comment color"
        );
        assert!(
            spans
                .iter()
                .any(|s| s.content.as_ref().contains("hello") && s.style.fg == Some(CODE_STRING)),
            "string should use the string color"
        );
        assert!(
            spans
                .iter()
                .any(|s| s.content.as_ref() == "println" && s.style.fg == Some(CODE_FUNCTION)),
            "call name should use the function color"
        );
    }

    #[test]
    fn inline_code_and_headings_are_color_coded() {
        let lines = render_markdown("## Setup\n\nRun `cargo test` first.", 60);

        let heading = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "Setup"))
            .expect("heading line");
        assert!(heading
            .spans
            .iter()
            .any(|s| s.style.fg == Some(ACCENT_AUTO)
                && s.style.add_modifier.contains(Modifier::BOLD)));

        let inline = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.as_ref().contains("cargo test"))
            .expect("inline code span");
        assert_eq!(inline.style.fg, Some(CODE_INLINE));
        assert_eq!(inline.style.bg, Some(BG_CODE));
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
        assert!(
            !content.contains('\x1b'),
            "escape bytes leaked: {content:?}"
        );
    }
}
