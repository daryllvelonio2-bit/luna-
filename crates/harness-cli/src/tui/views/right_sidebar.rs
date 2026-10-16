use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;

use crate::tui::app::{AppState, PanelFocus};

pub fn render_right_sidebar(f: &mut Frame, app: &mut AppState, area: Rect) {
    app.rect_right = area;
    let is_focused = app.focus == PanelFocus::RightSidebar;

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

    f.render_widget(block, area);
}
