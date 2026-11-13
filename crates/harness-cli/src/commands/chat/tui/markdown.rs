//! Markdown and plain-text wrapping into `ratatui` styled lines.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
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
                    code_buf.push_str(&text);
                } else {
                    let style = *style_stack.last().unwrap_or(&Style::default());
                    cur_spans.push(Span::styled(text.into_string(), style));
                }
            }
            Event::Code(code) => {
                cur_spans.push(Span::styled(
                    format!("`{code}`"),
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
