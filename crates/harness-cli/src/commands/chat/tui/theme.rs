//! Visual tokens, colors, and formatting helpers matching OpenCode's TUI design.

use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

use harness_core::ThemeMode;
use ratatui::style::Color;

/// Complete color palette for the full-screen TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    pub bg_canvas: Color,
    pub bg_surface: Color,
    pub bg_elevated: Color,
    pub bg_code: Color,
    pub bg_selected: Color,
    pub bg_selection: Color,
    pub bg_approval: Color,
    pub bg_approval_selected: Color,
    pub border_subtle: Color,
    pub accent_coral: Color,
    pub accent_plan: Color,
    pub accent_auto: Color,
    pub text_primary: Color,
    pub text_muted: Color,
    pub text_dim: Color,
    pub logo_gray: Color,
    pub green_ok: Color,
    pub red_err: Color,
    pub yellow_warn: Color,
    pub cyan_link: Color,
    pub code_inline: Color,
    pub code_keyword: Color,
    pub code_string: Color,
    pub code_number: Color,
    pub code_comment: Color,
    pub code_function: Color,
    pub code_key: Color,
    pub note_accent: Color,
    pub scanner_trail: [Color; 3],
}

pub const DARK_PALETTE: ThemePalette = ThemePalette {
    bg_canvas: Color::Rgb(5, 5, 7),
    bg_surface: Color::Rgb(15, 16, 20),
    bg_elevated: Color::Rgb(21, 23, 29),
    bg_code: Color::Rgb(10, 11, 14),
    bg_selected: Color::Rgb(30, 34, 46),
    bg_selection: Color::Rgb(52, 68, 108),
    bg_approval: Color::Rgb(26, 20, 12),
    bg_approval_selected: Color::Rgb(56, 42, 18),
    border_subtle: Color::Rgb(46, 50, 62),
    accent_coral: Color::Rgb(255, 116, 72),
    accent_plan: Color::Rgb(86, 168, 255),
    accent_auto: Color::Rgb(250, 188, 72),
    text_primary: Color::Rgb(246, 247, 250),
    text_muted: Color::Rgb(158, 166, 182),
    text_dim: Color::Rgb(104, 112, 128),
    logo_gray: Color::Rgb(152, 160, 176),
    green_ok: Color::Rgb(74, 214, 128),
    red_err: Color::Rgb(248, 92, 108),
    yellow_warn: Color::Rgb(245, 196, 88),
    cyan_link: Color::Rgb(82, 196, 246),
    code_inline: Color::Rgb(250, 188, 72),
    code_keyword: Color::Rgb(214, 126, 248),
    code_string: Color::Rgb(136, 214, 128),
    code_number: Color::Rgb(246, 190, 96),
    code_comment: Color::Rgb(118, 126, 144),
    code_function: Color::Rgb(92, 182, 255),
    code_key: Color::Rgb(72, 206, 220),
    note_accent: Color::Rgb(224, 134, 248),
    scanner_trail: [
        Color::Rgb(196, 82, 50),
        Color::Rgb(118, 48, 32),
        Color::Rgb(60, 26, 18),
    ],
};

pub const LIGHT_PALETTE: ThemePalette = ThemePalette {
    bg_canvas: Color::Rgb(252, 252, 254),
    bg_surface: Color::Rgb(236, 239, 245),
    bg_elevated: Color::Rgb(244, 246, 250),
    bg_code: Color::Rgb(230, 234, 242),
    bg_selected: Color::Rgb(204, 214, 234),
    bg_selection: Color::Rgb(166, 196, 242),
    bg_approval: Color::Rgb(254, 244, 224),
    bg_approval_selected: Color::Rgb(245, 222, 174),
    border_subtle: Color::Rgb(166, 174, 190),
    accent_coral: Color::Rgb(198, 62, 24),
    accent_plan: Color::Rgb(24, 92, 188),
    accent_auto: Color::Rgb(164, 96, 0),
    text_primary: Color::Rgb(12, 14, 18),
    text_muted: Color::Rgb(66, 72, 86),
    text_dim: Color::Rgb(106, 114, 128),
    logo_gray: Color::Rgb(92, 98, 112),
    green_ok: Color::Rgb(18, 122, 54),
    red_err: Color::Rgb(188, 28, 44),
    yellow_warn: Color::Rgb(154, 92, 0),
    cyan_link: Color::Rgb(12, 104, 168),
    code_inline: Color::Rgb(160, 74, 4),
    code_keyword: Color::Rgb(132, 34, 168),
    code_string: Color::Rgb(22, 116, 48),
    code_number: Color::Rgb(156, 82, 0),
    code_comment: Color::Rgb(92, 98, 112),
    code_function: Color::Rgb(18, 90, 176),
    code_key: Color::Rgb(8, 114, 130),
    note_accent: Color::Rgb(132, 34, 168),
    scanner_trail: [
        Color::Rgb(208, 108, 76),
        Color::Rgb(218, 152, 130),
        Color::Rgb(228, 192, 180),
    ],
};

/// Dark accents and no painted background: every full-area surface is [`Color::Reset`], so the
/// terminal's own background (a translucent or image-backed profile) shows through instead of the
/// charcoal canvas. Only the small selection surfaces keep a real color — a highlight drawn as
/// `Reset` would be invisible.
pub const TRANSPARENT_PALETTE: ThemePalette = ThemePalette {
    bg_canvas: Color::Reset,
    bg_surface: Color::Reset,
    bg_elevated: Color::Reset,
    bg_code: Color::Reset,
    bg_selected: Color::Rgb(44, 48, 62),
    bg_selection: Color::Rgb(64, 82, 124),
    bg_approval: Color::Reset,
    bg_approval_selected: Color::Rgb(56, 42, 18),
    border_subtle: Color::Rgb(96, 102, 120),
    accent_coral: Color::Rgb(255, 116, 72),
    accent_plan: Color::Rgb(86, 168, 255),
    accent_auto: Color::Rgb(250, 188, 72),
    text_primary: Color::Rgb(246, 247, 250),
    text_muted: Color::Rgb(158, 166, 182),
    text_dim: Color::Rgb(104, 112, 128),
    logo_gray: Color::Rgb(152, 160, 176),
    green_ok: Color::Rgb(74, 214, 128),
    red_err: Color::Rgb(248, 92, 108),
    yellow_warn: Color::Rgb(245, 196, 88),
    cyan_link: Color::Rgb(82, 196, 246),
    code_inline: Color::Rgb(250, 188, 72),
    code_keyword: Color::Rgb(214, 126, 248),
    code_string: Color::Rgb(136, 214, 128),
    code_number: Color::Rgb(246, 190, 96),
    code_comment: Color::Rgb(118, 126, 144),
    code_function: Color::Rgb(92, 182, 255),
    code_key: Color::Rgb(72, 206, 220),
    note_accent: Color::Rgb(224, 134, 248),
    scanner_trail: [
        Color::Rgb(196, 82, 50),
        Color::Rgb(118, 48, 32),
        Color::Rgb(60, 26, 18),
    ],
};

static ACTIVE_THEME: AtomicU8 = AtomicU8::new(0);

pub fn set_active_theme(mode: ThemeMode) {
    ACTIVE_THEME.store(mode.index() as u8, Ordering::Relaxed);
}

pub fn active_theme() -> ThemeMode {
    ThemeMode::from_index(ACTIVE_THEME.load(Ordering::Relaxed) as usize)
}

#[inline]
pub fn palette_for(mode: ThemeMode) -> &'static ThemePalette {
    match mode {
        ThemeMode::Light => &LIGHT_PALETTE,
        ThemeMode::Transparent => &TRANSPARENT_PALETTE,
        ThemeMode::Dark => &DARK_PALETTE,
    }
}

#[inline]
pub fn palette() -> &'static ThemePalette {
    palette_for(active_theme())
}

#[inline]
pub fn bg_canvas() -> Color {
    palette().bg_canvas
}
#[inline]
pub fn bg_surface() -> Color {
    palette().bg_surface
}
#[inline]
pub fn bg_elevated() -> Color {
    palette().bg_elevated
}
#[inline]
pub fn bg_code() -> Color {
    palette().bg_code
}
#[inline]
pub fn bg_selected() -> Color {
    palette().bg_selected
}
#[inline]
pub fn bg_selection() -> Color {
    palette().bg_selection
}
#[inline]
pub fn bg_approval() -> Color {
    palette().bg_approval
}
#[inline]
pub fn bg_approval_selected() -> Color {
    palette().bg_approval_selected
}
#[inline]
pub fn border_subtle() -> Color {
    palette().border_subtle
}
#[inline]
pub fn accent_coral() -> Color {
    palette().accent_coral
}
#[inline]
pub fn accent_plan() -> Color {
    palette().accent_plan
}
#[inline]
pub fn accent_auto() -> Color {
    palette().accent_auto
}
#[inline]
pub fn text_primary() -> Color {
    palette().text_primary
}
#[inline]
pub fn text_muted() -> Color {
    palette().text_muted
}
#[inline]
pub fn text_dim() -> Color {
    palette().text_dim
}
#[inline]
pub fn logo_gray() -> Color {
    palette().logo_gray
}
#[inline]
pub fn green_ok() -> Color {
    palette().green_ok
}
#[inline]
pub fn red_err() -> Color {
    palette().red_err
}
#[inline]
pub fn yellow_warn() -> Color {
    palette().yellow_warn
}
#[inline]
pub fn cyan_link() -> Color {
    palette().cyan_link
}
#[inline]
pub fn code_inline() -> Color {
    palette().code_inline
}
#[inline]
pub fn code_keyword() -> Color {
    palette().code_keyword
}
#[inline]
pub fn code_string() -> Color {
    palette().code_string
}
#[inline]
pub fn code_number() -> Color {
    palette().code_number
}
#[inline]
pub fn code_comment() -> Color {
    palette().code_comment
}
#[inline]
pub fn code_function() -> Color {
    palette().code_function
}
#[inline]
pub fn code_key() -> Color {
    palette().code_key
}
#[inline]
pub fn note_accent() -> Color {
    palette().note_accent
}
#[inline]
pub fn scanner_color(dist: usize) -> Color {
    let p = palette();
    match dist {
        0 => p.accent_coral,
        1 => p.scanner_trail[0],
        2 => p.scanner_trail[1],
        _ => p.scanner_trail[2],
    }
}

/// 3-row geometric pixel/half-block wordmark for `luna` styled like `opencode` in Screenshot 1:
/// `lu` in muted gray (`logo_gray()`), `na` in primary text (`text_primary()`).
///
/// Each character is 6 cells wide (except `l` = 4 cells) with 1 space between letters.
pub const LUNA_LOGO_LEFT: [&str; 3] = ["█     █   █ ", "█     █   █ ", "▀▀▀▀  ▀███▀ "];

pub const LUNA_LOGO_RIGHT: [&str; 3] = ["▄▀▀█▄  ▄▀▀▀█", "█   █  █   █", "▀   ▀  ▀▀▀▀▀"];

/// Nicer display title for the active agent mode.
pub fn mode_accent(mode_name: &str) -> Color {
    match mode_name {
        "Plan" => accent_plan(),
        "Auto" => accent_auto(),
        _ => accent_coral(),
    }
}

/// Format a raw provider id into a clean display label (e.g. `nvidia` -> `Nvidia`, `opencode` -> `OpenCode`).
pub fn friendly_provider_name(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "opencode" | "opencode-cli" => "OpenCode".to_string(),
        "openai" => "OpenAI".to_string(),
        "anthropic" => "Anthropic".to_string(),
        "gemini" => "Gemini".to_string(),
        "nvidia" => "Nvidia".to_string(),
        "groq" => "Groq".to_string(),
        "mistral" => "Mistral".to_string(),
        "codex" => "Codex".to_string(),
        "antigravity" => "Antigravity".to_string(),
        "openrouter" => "OpenRouter".to_string(),
        "deepseek" => "DeepSeek".to_string(),
        "kilocode" => "KiloCode".to_string(),
        "tokenharbor" => "TokenHarbor".to_string(),
        "cline" => "Cline".to_string(),
        "" => "Provider".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

/// Format a raw model slug into a readable display label while keeping version numbers crisp.
pub fn friendly_model_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "No model".to_string();
    }
    // Strip vendor prefix when present (e.g. `deepseek-ai/deepseek-v4.1-flash` -> `deepseek-v4.1-flash`)
    let leaf = trimmed.rsplit('/').next().unwrap_or(trimmed);
    // Strip `:free` tag into `Free` suffix or clean up `-` separators
    let (base, suffix) = if let Some(b) = leaf.strip_suffix(":free") {
        (b, " Free")
    } else if let Some(b) = leaf.strip_suffix("-free") {
        (b, " Free")
    } else {
        (leaf, "")
    };

    let words: Vec<String> = base
        .split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            match lower.as_str() {
                "gpt" => "GPT".to_string(),
                "oss" => "OSS".to_string(),
                "vl" => "VL".to_string(),
                "ai" => "AI".to_string(),
                "deepseek" => "DeepSeek".to_string(),
                "nemotron" => "Nemotron".to_string(),
                "claude" => "Claude".to_string(),
                "gemini" => "Gemini".to_string(),
                "qwen" => "Qwen".to_string(),
                "llama" => "Llama".to_string(),
                "mistral" => "Mistral".to_string(),
                "minimax" => "MiniMax".to_string(),
                "glm" => "GLM".to_string(),
                "kimi" => "Kimi".to_string(),
                _ if lower.starts_with('v')
                    && lower.chars().nth(1).is_some_and(|c| c.is_ascii_digit()) =>
                {
                    format!("V{}", &part[1..])
                }
                _ if lower.chars().all(|c| c.is_ascii_digit() || c == '.') => part.to_string(),
                _ if (lower.ends_with('b') || lower.ends_with("tb"))
                    && lower.chars().next().is_some_and(|c| c.is_ascii_digit()) =>
                {
                    part.to_ascii_uppercase()
                }
                _ => {
                    let mut chars = part.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => String::new(),
                    }
                }
            }
        })
        .collect();

    if words.is_empty() {
        trimmed.to_string()
    } else {
        format!("{}{}", words.join(" "), suffix)
    }
}

/// Shorten a workspace path for the bottom-left footer (`~` when under `$HOME`).
pub fn shorten_cwd(path: &Path) -> String {
    let raw = path.display().to_string();
    let home = harness_core::config::home_dir()
        .to_string_lossy()
        .into_owned();
    if !home.is_empty() {
        if raw == home {
            return "~".to_string();
        }
        if let Some(rest) = raw.strip_prefix(&format!("{home}/")) {
            return format!("~/{rest}");
        }
        // Windows paths separate with `\`, so the same shortening needs the native separator.
        let sep = std::path::MAIN_SEPARATOR;
        if let Some(rest) = raw.strip_prefix(&format!("{home}{sep}")) {
            return format!("~{sep}{rest}");
        }
    }
    raw
}

/// Compact token formatter matching OpenCode's `6.5K` / `1.2M` style.
pub fn humanize_tokens(tokens: u32) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_palette_paints_no_background_but_keeps_text_and_selection() {
        let t = palette_for(ThemeMode::Transparent);
        // Nothing full-area is painted, so the terminal's own background survives.
        assert_eq!(t.bg_canvas, Color::Reset);
        assert_eq!(t.bg_surface, Color::Reset);
        assert_eq!(t.bg_elevated, Color::Reset);
        assert_eq!(t.bg_code, Color::Reset);
        assert_eq!(t.bg_approval, Color::Reset);
        // A selection must stay visible, so it keeps a real color.
        assert_ne!(t.bg_selected, Color::Reset);
        assert_ne!(t.bg_selection, Color::Reset);
        // Text and accents are the dark palette's, so it stays legible on a dark terminal.
        assert_eq!(t.text_primary, DARK_PALETTE.text_primary);
        assert_eq!(t.accent_coral, DARK_PALETTE.accent_coral);
        assert_eq!(
            ThemeMode::from_index(ThemeMode::Transparent.index()),
            ThemeMode::Transparent
        );
    }

    #[test]
    fn dark_and_light_palettes_have_distinct_canvas_and_readable_text() {
        let dark = palette_for(ThemeMode::Dark);
        let light = palette_for(ThemeMode::Light);
        assert_ne!(dark.bg_canvas, light.bg_canvas);
        assert_ne!(dark.text_primary, light.text_primary);

        let luma = |c: Color| -> u32 {
            match c {
                Color::Rgb(r, g, b) => r as u32 + g as u32 + b as u32,
                _ => 0,
            }
        };
        assert!(luma(dark.bg_canvas) < luma(dark.text_primary));
        assert!(luma(light.bg_canvas) > luma(light.text_primary));
        assert_eq!(ThemeMode::default(), ThemeMode::Dark);
    }
}
