use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::app::{AppState, PanelFocus};

pub fn render_center(f: &mut Frame, app: &mut AppState, area: Rect) {
    app.rect_center = area;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),    // Transcript viewport
            Constraint::Length(3), // Bottom rounded Composer
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

    let mut transcript_block = Block::default()
        .borders(Borders::ALL)
        .border_type(app.theme.border_type)
        .border_style(transcript_border_style);
    if app.theme.card_bg != ratatui::style::Color::Reset {
        transcript_block = transcript_block.style(ratatui::style::Style::default().bg(app.theme.card_bg));
    }

    let mut lines = Vec::new();
    for msg in &app.transcript {
        for line_content in msg.content.lines() {
            lines.push(Line::from(vec![
                Span::styled(line_content, app.theme.text),
            ]));
        }
        lines.push(Line::from(""));
    }

    let transcript_para = Paragraph::new(lines)
        .block(transcript_block)
        .wrap(Wrap { trim: false })
        .scroll((app.transcript_scroll as u16, 0));
    f.render_widget(transcript_para, transcript_area);

    // --- 2. Bottom Anchored Rounded Composer ---
    let is_composer_focused = app.focus == PanelFocus::Composer;
    let composer_border_style = if is_composer_focused {
        app.theme.border_focused
    } else {
        app.theme.border
    };

    let mut composer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(app.theme.border_type)
        .border_style(composer_border_style);
    if app.theme.card_bg != ratatui::style::Color::Reset {
        composer_block = composer_block.style(ratatui::style::Style::default().bg(app.theme.card_bg));
    }

    let composer_inner = composer_block.inner(composer_area);
    f.render_widget(composer_block, composer_area);

    let input_para = Paragraph::new(Span::styled(&app.composer_input, app.theme.text_bold));
    f.render_widget(input_para, composer_inner);
}
