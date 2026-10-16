use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;

use crate::tui::app::AppState;

pub fn render_header(f: &mut Frame, app: &mut AppState, area: Rect) {
    app.rect_header = area;
    let block = Block::default().borders(Borders::NONE);
    f.render_widget(block, area);
}
