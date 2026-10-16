use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
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

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(app.theme.border_type)
        .border_style(border_style)
        .title(Span::styled(" Navigation ", app.theme.title));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // New Conversation action
            Constraint::Length(7), // Pinned Conversations
            Constraint::Min(6),    // Projects Tree
            Constraint::Length(2), // Settings at bottom
        ])
        .split(inner);

    // 1. New Conversation button
    let new_chat_line = Line::from(vec![
        Span::styled("+ ", app.theme.accent_cyan.add_modifier(Modifier::BOLD)),
        Span::styled("New Conversation", app.theme.text_bold),
    ]);
    let new_chat_block = Block::default()
        .borders(Borders::ALL)
        .border_type(app.theme.border_type)
        .border_style(app.theme.border);
    let new_chat_para = Paragraph::new(new_chat_line).block(new_chat_block);
    f.render_widget(new_chat_para, chunks[0]);

    // 2. Pinned / Recent Conversations list
    let session_items: Vec<ListItem> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let is_selected = i == app.selected_session_idx;
            let dot = if s.active {
                Span::styled("● ", app.theme.accent_cyan)
            } else {
                Span::styled("○ ", app.theme.text_muted)
            };

            let name_span = if is_selected {
                Span::styled(&s.name, app.theme.active_item)
            } else {
                Span::styled(&s.name, app.theme.inactive_item)
            };

            ListItem::new(Line::from(vec![dot, name_span]))
        })
        .collect();

    let sessions_list = List::new(session_items)
        .block(Block::default().borders(Borders::NONE).title(Span::styled("Pinned Chats", app.theme.text_muted)));
    f.render_widget(sessions_list, chunks[1]);

    // 3. Projects & Files Tree
    let file_items: Vec<ListItem> = app
        .workspace_files
        .iter()
        .enumerate()
        .map(|(i, file)| {
            let is_selected = i == app.selected_file_idx;
            let icon = if file.ends_with('/') {
                Span::styled("📁 ", app.theme.accent_purple)
            } else {
                Span::styled("📄 ", app.theme.text_muted)
            };

            let name_span = if is_selected && is_focused {
                Span::styled(file, app.theme.active_item)
            } else {
                Span::styled(file, app.theme.text)
            };

            ListItem::new(Line::from(vec![icon, name_span]))
        })
        .collect();

    let files_list = List::new(file_items)
        .block(Block::default().borders(Borders::TOP).border_style(app.theme.border).title(Span::styled("Projects", app.theme.text_muted)));
    f.render_widget(files_list, chunks[2]);

    // 4. Settings at bottom
    let settings_line = Line::from(vec![
        Span::styled("⚙ ", app.theme.text_muted),
        Span::styled("Settings", app.theme.text_muted),
    ]);
    f.render_widget(Paragraph::new(settings_line), chunks[3]);
}
