use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::tui::app::AppState;

pub fn render_header(f: &mut Frame, app: &mut AppState, area: Rect) {
    app.rect_header = area;

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(28),
            Constraint::Min(20),
            Constraint::Length(48),
        ])
        .split(area);

    // Left brand & navigation
    let brand_spans = Line::from(vec![
        Span::styled("☾ Luna ", app.theme.accent_purple.add_modifier(Modifier::BOLD)),
        Span::styled("File  View  Window", app.theme.text_muted),
    ]);
    let left_para = Paragraph::new(brand_spans)
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(left_para, chunks[0]);

    // Center breadcrumb: Workspace / Current File
    let breadcrumb = Line::from(vec![
        Span::styled(&app.workspace_name, app.theme.text_muted),
        Span::styled(" / ", app.theme.text_muted),
        Span::styled(&app.current_file, app.theme.title),
    ]);
    let center_para = Paragraph::new(breadcrumb)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(center_para, chunks[1]);

    // Right shortcut actions
    let theme_label = format!("[Ctrl+T] {} ", app.theme.mode.name());
    let left_state = if app.left_collapsed { "[Ctrl+S] +Left " } else { "[Ctrl+S] -Left " };
    let right_state = if app.right_collapsed { "[Ctrl+B] +Right " } else { "[Ctrl+B] -Right " };
    let actions = Line::from(vec![
        Span::styled("[Tab] Focus ", app.theme.text_muted),
        Span::styled(left_state, app.theme.text_muted),
        Span::styled(right_state, app.theme.text_muted),
        Span::styled(theme_label, app.theme.accent_purple),
        Span::styled("[q] Quit", app.theme.accent_amber),
    ]);
    let right_para = Paragraph::new(actions)
        .alignment(Alignment::Right)
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(right_para, chunks[2]);
}
