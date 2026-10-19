use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub enum ThemeMode {
    #[default]
    Transparent,
    Dark,
    Light,
}

impl ThemeMode {
    #[allow(dead_code)]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Transparent => "Transparent",
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Transparent => Self::Dark,
            Self::Dark => Self::Light,
            Self::Light => Self::Transparent,
        }
    }
}

/// Global TUI theme structure supporting Transparent, Dark, and Light modes
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Theme {
    pub mode: ThemeMode,
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
        Self::for_mode(ThemeMode::default())
    }
}

impl Theme {
    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Transparent => Self::transparent(),
            ThemeMode::Dark => Self::dark(),
            ThemeMode::Light => Self::light(),
        }
    }

    /// Transparent mode: zero background fills, preserving terminal transparency and wallpaper
    pub fn transparent() -> Self {
        Self {
            mode: ThemeMode::Transparent,
            bg: Color::Reset,
            card_bg: Color::Reset,
            border: Style::default().fg(Color::Rgb(90, 90, 100)),
            border_focused: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            border_type: BorderType::Rounded,
            title: Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            text: Style::default().fg(Color::Reset),
            text_muted: Style::default().fg(Color::DarkGray),
            text_bold: Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            accent: Style::default().fg(Color::Cyan),
            accent_cyan: Style::default().fg(Color::Cyan),
            accent_purple: Style::default().fg(Color::Magenta),
            accent_green: Style::default().fg(Color::Green),
            accent_amber: Style::default().fg(Color::Yellow),
            active_item: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            inactive_item: Style::default().fg(Color::Reset),
            badge: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        }
    }

    /// Dark mode: deep obsidian and charcoal palette
    pub fn dark() -> Self {
        Self {
            mode: ThemeMode::Dark,
            bg: Color::Rgb(18, 18, 22),
            card_bg: Color::Rgb(28, 28, 34),
            border: Style::default().fg(Color::Rgb(60, 60, 70)),
            border_focused: Style::default()
                .fg(Color::Rgb(130, 130, 240))
                .add_modifier(Modifier::BOLD),
            border_type: BorderType::Rounded,
            title: Style::default().fg(Color::Rgb(230, 230, 240)).add_modifier(Modifier::BOLD),
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
                .bg(Color::Rgb(40, 40, 56))
                .add_modifier(Modifier::BOLD),
            inactive_item: Style::default().fg(Color::Rgb(180, 180, 190)),
            badge: Style::default()
                .fg(Color::Rgb(200, 200, 255))
                .bg(Color::Rgb(35, 35, 48)),
        }
    }

    /// Light mode: clean off-white canvas with dark slate text
    pub fn light() -> Self {
        Self {
            mode: ThemeMode::Light,
            bg: Color::Rgb(245, 245, 248),
            card_bg: Color::Rgb(255, 255, 255),
            border: Style::default().fg(Color::Rgb(205, 205, 215)),
            border_focused: Style::default()
                .fg(Color::Rgb(80, 80, 200))
                .add_modifier(Modifier::BOLD),
            border_type: BorderType::Rounded,
            title: Style::default().fg(Color::Rgb(25, 25, 35)).add_modifier(Modifier::BOLD),
            text: Style::default().fg(Color::Rgb(35, 35, 45)),
            text_muted: Style::default().fg(Color::Rgb(115, 115, 125)),
            text_bold: Style::default().fg(Color::Black).add_modifier(Modifier::BOLD),
            accent: Style::default().fg(Color::Rgb(80, 80, 200)),
            accent_cyan: Style::default().fg(Color::Rgb(0, 130, 160)),
            accent_purple: Style::default().fg(Color::Rgb(130, 60, 200)),
            accent_green: Style::default().fg(Color::Rgb(40, 140, 70)),
            accent_amber: Style::default().fg(Color::Rgb(180, 110, 20)),
            active_item: Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(225, 225, 240))
                .add_modifier(Modifier::BOLD),
            inactive_item: Style::default().fg(Color::Rgb(60, 60, 70)),
            badge: Style::default()
                .fg(Color::Rgb(50, 50, 160))
                .bg(Color::Rgb(230, 230, 245)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_mode_cycling() {
        assert_eq!(ThemeMode::Transparent.next(), ThemeMode::Dark);
        assert_eq!(ThemeMode::Dark.next(), ThemeMode::Light);
        assert_eq!(ThemeMode::Light.next(), ThemeMode::Transparent);
    }

    #[test]
    fn test_theme_color_invariants() {
        let transparent = Theme::transparent();
        assert_eq!(transparent.bg, Color::Reset);
        assert_eq!(transparent.card_bg, Color::Reset);

        let dark = Theme::dark();
        assert_ne!(dark.bg, Color::Reset);

        let light = Theme::light();
        assert_ne!(light.bg, Color::Reset);
    }
}
