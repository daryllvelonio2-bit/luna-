use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::app::AppState;

pub fn render_center(f: &mut Frame, app: &mut AppState, area: Rect) {
    let mut lines = Vec::new();
    for msg in &app.transcript {
        for line_content in msg.content.lines() {
            lines.push(Line::from(vec![Span::styled(line_content, app.theme.text)]));
        }
        lines.push(Line::from(""));
    }

    if !app.composer_input.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(&app.composer_input, app.theme.text_bold),
        ]));
    }

    let transcript_para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.transcript_scroll as u16, 0));
    f.render_widget(transcript_para, area);
}
