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
    bg_canvas: Color::Rgb(14, 14, 15),
    bg_surface: Color::Rgb(30, 30, 32),
    bg_elevated: Color::Rgb(38, 38, 42),
    bg_code: Color::Rgb(22, 22, 24),
    bg_selected: Color::Rgb(47, 47, 53),
    bg_selection: Color::Rgb(92, 92, 110),
    border_subtle: Color::Rgb(52, 52, 58),
    accent_coral: Color::Rgb(234, 118, 82),
    accent_plan: Color::Rgb(106, 166, 232),
    accent_auto: Color::Rgb(229, 181, 103),
    text_primary: Color::Rgb(230, 230, 232),
    text_muted: Color::Rgb(128, 128, 136),
    text_dim: Color::Rgb(75, 75, 82),
    logo_gray: Color::Rgb(132, 132, 140),
    green_ok: Color::Rgb(104, 194, 130),
    red_err: Color::Rgb(224, 108, 117),
    yellow_warn: Color::Rgb(229, 192, 123),
    cyan_link: Color::Rgb(106, 176, 219),
    code_inline: Color::Rgb(229, 181, 103),
    code_keyword: Color::Rgb(198, 120, 221),
    code_string: Color::Rgb(152, 195, 121),
    code_number: Color::Rgb(229, 192, 123),
    code_comment: Color::Rgb(106, 106, 116),
    code_function: Color::Rgb(97, 175, 239),
    code_key: Color::Rgb(86, 182, 194),
    note_accent: Color::Rgb(210, 140, 230),
    scanner_trail: [
        Color::Rgb(168, 84, 60),
        Color::Rgb(96, 52, 42),
        Color::Rgb(54, 34, 30),
    ],
};

pub const LIGHT_PALETTE: ThemePalette = ThemePalette {
    bg_canvas: Color::Rgb(248, 248, 250),
    bg_surface: Color::Rgb(236, 237, 241),
    bg_elevated: Color::Rgb(242, 243, 246),
    bg_code: Color::Rgb(232, 234, 239),
    bg_selected: Color::Rgb(214, 219, 230),
    bg_selection: Color::Rgb(182, 204, 238),
    border_subtle: Color::Rgb(200, 204, 214),
    accent_coral: Color::Rgb(208, 84, 48),
    accent_plan: Color::Rgb(42, 110, 194),
    accent_auto: Color::Rgb(176, 114, 18),
    text_primary: Color::Rgb(28, 30, 36),
    text_muted: Color::Rgb(96, 100, 112),
    text_dim: Color::Rgb(148, 152, 164),
    logo_gray: Color::Rgb(128, 132, 144),
    green_ok: Color::Rgb(34, 134, 68),
    red_err: Color::Rgb(196, 48, 60),
    yellow_warn: Color::Rgb(166, 110, 14),
    cyan_link: Color::Rgb(26, 118, 176),
    code_inline: Color::Rgb(168, 92, 16),
    code_keyword: Color::Rgb(148, 56, 176),
    code_string: Color::Rgb(44, 128, 62),
    code_number: Color::Rgb(168, 98, 18),
    code_comment: Color::Rgb(118, 122, 134),
    code_function: Color::Rgb(32, 106, 184),
    code_key: Color::Rgb(20, 126, 140),
    note_accent: Color::Rgb(148, 56, 176),
    scanner_trail: [
        Color::Rgb(220, 132, 104),
        Color::Rgb(228, 176, 158),
        Color::Rgb(234, 210, 202),
    ],
};

static ACTIVE_THEME: AtomicU8 = AtomicU8::new(0);

pub fn set_active_theme(mode: ThemeMode) {
    let val = match mode {
        ThemeMode::Dark => 0,
        ThemeMode::Light => 1,
    };
    ACTIVE_THEME.store(val, Ordering::Relaxed);
}

pub fn active_theme() -> ThemeMode {
    match ACTIVE_THEME.load(Ordering::Relaxed) {
        1 => ThemeMode::Light,
        _ => ThemeMode::Dark,
    }
}

#[inline]
pub fn palette_for(mode: ThemeMode) -> &'static ThemePalette {
    if mode.is_light() {
        &LIGHT_PALETTE
    } else {
        &DARK_PALETTE
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
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            if raw == home {
                return "~".to_string();
            }
            if let Some(rest) = raw.strip_prefix(&format!("{home}/")) {
                return format!("~/{rest}");
            }
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
