use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::tui::app::{AppState, PanelFocus};

pub fn render_left_sidebar(f: &mut Frame, app: &mut AppState, area: Rect) {
    app.rect_left = area;
    let is_focused = app.focus == PanelFocus::LeftSidebar;

    let border_style = if is_focused {
        app.theme.border_focused
    } else {
        app.theme.border
    };

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(app.theme.border_type)
        .border_style(border_style);
    if app.theme.card_bg != ratatui::style::Color::Reset {
        block = block.style(ratatui::style::Style::default().bg(app.theme.card_bg));
    }

    let inner = block.inner(area);
    f.render_widget(block, area);

    if !app.sessions.is_empty() {
        let items: Vec<ListItem> = app
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let is_selected = i == app.selected_session_idx;
                let span = if is_selected {
                    Span::styled(&s.name, app.theme.active_item)
                } else {
                    Span::styled(&s.name, app.theme.inactive_item)
                };
                ListItem::new(Line::from(vec![span]))
            })
            .collect();
        f.render_widget(List::new(items), inner);
    }
}
