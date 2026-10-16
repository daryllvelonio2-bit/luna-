use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::app::{AppState, MessageRole, PanelFocus};

pub fn render_center(f: &mut Frame, app: &mut AppState, area: Rect) {
    app.rect_center = area;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),     // Transcript viewport
            Constraint::Length(4),  // Bottom rounded Composer
        ])
        .split(area);

    let transcript_area = chunks[0];
    let composer_area = chunks[1];
    app.rect_composer = composer_area;

    // --- 1. Transcript Area ---
    let is_transcript_focused = app.focus == PanelFocus::CenterTranscript;
    let transcript_border_style = if is_transcript_focused {
        app.theme.border_focused
    } else {
        app.theme.border
    };

    let transcript_block = Block::default()
        .borders(Borders::ALL)
        .border_type(app.theme.border_type)
        .border_style(transcript_border_style)
        .title(Span::styled(" Workspace & Transcript ", app.theme.title));

    let mut lines = Vec::new();

    for msg in &app.transcript {
        let (prefix, prefix_style) = match msg.role {
            MessageRole::User => ("You", app.theme.accent_cyan.add_modifier(Modifier::BOLD)),
            MessageRole::Assistant => ("Luna", app.theme.accent_purple.add_modifier(Modifier::BOLD)),
            MessageRole::Tool => ("Tool", app.theme.accent_green),
            MessageRole::System => ("System", app.theme.text_muted),
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", prefix), prefix_style),
            Span::styled(format!("({})", msg.timestamp), app.theme.text_muted),
        ]));

        for line_content in msg.content.lines() {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(line_content, app.theme.text),
            ]));
        }
        lines.push(Line::from("")); // Spacer between turns
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No messages yet. Ask anything below.",
            app.theme.text_muted,
        )));
    }

    let transcript_para = Paragraph::new(lines)
        .block(transcript_block)
        .wrap(Wrap { trim: false })
        .scroll((app.transcript_scroll as u16, 0));
    f.render_widget(transcript_para, transcript_area);

    // --- 2. Bottom Anchored / Floating Rounded Composer ---
    let is_composer_focused = app.focus == PanelFocus::Composer;
    let composer_border_style = if is_composer_focused {
        app.theme.border_focused
    } else {
        app.theme.border
    };

    let composer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(app.theme.border_type)
        .border_style(composer_border_style);

    let composer_inner = composer_block.inner(composer_area);
    f.render_widget(composer_block, composer_area);

    let composer_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((app.model_name.len() + 6) as u16), // Model Badge
            Constraint::Min(20),                                   // Input text
            Constraint::Length(8),                                 // Submit hint
        ])
        .split(composer_inner);

    // Model Selector Badge (e.g. [ Gemini 3.8 Flash High ▾ ])
    let model_badge = Paragraph::new(Line::from(vec![
        Span::styled(format!(" {} ▾ ", app.model_name), app.theme.badge),
    ]));
    f.render_widget(model_badge, composer_chunks[0]);

    // Text Input or Placeholder
    let input_para = if app.composer_input.is_empty() {
        Paragraph::new(Span::styled(
            "Ask anything, @ to mention, / for actions...",
            app.theme.text_muted,
        ))
    } else {
        Paragraph::new(Span::styled(&app.composer_input, app.theme.text_bold))
    };
    f.render_widget(input_para, composer_chunks[1]);

    // Right action hint (⏎ Send)
    let send_hint = Paragraph::new(Line::from(vec![
        Span::styled("⏎ Send", if is_composer_focused { app.theme.accent_cyan } else { app.theme.text_muted }),
    ]));
    f.render_widget(send_hint, composer_chunks[2]);
}
