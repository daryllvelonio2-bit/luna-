use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

/// Antigravity-inspired aesthetic theme for the Luna TUI
pub struct Theme {
    pub bg: Color,
    pub card_bg: Color,
    pub border: Style,
    pub border_focused: Style,
    pub border_type: BorderType,
    pub title: Style,
    pub text: Style,
    pub text_muted: Style,
    pub text_bold: Style,
    pub accent: Style,
    pub accent_cyan: Style,
    pub accent_purple: Style,
    pub accent_green: Style,
    pub accent_amber: Style,
    pub active_item: Style,
    pub inactive_item: Style,
    pub badge: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Color::Reset,
            card_bg: Color::Reset,
            border: Style::default().fg(Color::Rgb(60, 60, 65)),
            border_focused: Style::default().fg(Color::Rgb(130, 130, 240)).add_modifier(Modifier::BOLD),
            border_type: BorderType::Rounded,
            title: Style::default().fg(Color::Rgb(220, 220, 230)).add_modifier(Modifier::BOLD),
            text: Style::default().fg(Color::Rgb(220, 220, 225)),
            text_muted: Style::default().fg(Color::Rgb(120, 120, 130)),
            text_bold: Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            accent: Style::default().fg(Color::Rgb(130, 130, 240)),
            accent_cyan: Style::default().fg(Color::Cyan),
            accent_purple: Style::default().fg(Color::Rgb(180, 130, 240)),
            accent_green: Style::default().fg(Color::Rgb(80, 200, 120)),
            accent_amber: Style::default().fg(Color::Rgb(240, 180, 60)),
            active_item: Style::default()
                .fg(Color::White)
                .bg(Color::Rgb(40, 40, 50))
                .add_modifier(Modifier::BOLD),
            inactive_item: Style::default().fg(Color::Rgb(180, 180, 190)),
            badge: Style::default()
                .fg(Color::Rgb(200, 200, 255))
                .bg(Color::Rgb(35, 35, 48)),
        }
    }
}
