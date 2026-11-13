//! Visual tokens, colors, and formatting helpers matching OpenCode's TUI design.

use std::path::Path;

use ratatui::style::Color;

/// Main deep charcoal canvas (`#0e0e0f`).
pub const BG_CANVAS: Color = Color::Rgb(14, 14, 15);
/// Card / composer / active-tab surface (`#1e1e20`).
pub const BG_SURFACE: Color = Color::Rgb(30, 30, 32);
/// Slightly elevated surface for modals & code blocks (`#26262a`).
pub const BG_ELEVATED: Color = Color::Rgb(38, 38, 42);
/// Fenced code block background (`#161618`).
pub const BG_CODE: Color = Color::Rgb(22, 22, 24);
/// Selected row highlight inside modals / popups (`#2f2f35`).
pub const BG_SELECTED: Color = Color::Rgb(47, 47, 53);
/// Mouse text-selection highlight (`#3c3c46`), brighter than `BG_SELECTED` so it reads over cards.
pub const BG_SELECTION: Color = Color::Rgb(60, 60, 70);

/// Subtle border gray (`#34343a`).
pub const BORDER_SUBTLE: Color = Color::Rgb(52, 52, 58);

/// OpenCode signature coral-orange accent (`#ea7652`).
pub const ACCENT_CORAL: Color = Color::Rgb(234, 118, 82);
/// Plan mode subtle blue-cyan accent (`#6aa6e8`).
pub const ACCENT_PLAN: Color = Color::Rgb(106, 166, 232);
/// Auto/Yolo mode warm amber accent (`#e5b567`).
pub const ACCENT_AUTO: Color = Color::Rgb(229, 181, 103);

/// Primary crisp white text (`#e6e6e8`).
pub const TEXT_PRIMARY: Color = Color::Rgb(230, 230, 232);
/// Secondary / muted label text (`#808088`).
pub const TEXT_MUTED: Color = Color::Rgb(128, 128, 136);
/// Very dim inactive tab / shadow text (`#4b4b52`).
pub const TEXT_DIM: Color = Color::Rgb(75, 75, 82);
/// Logo left-half gray (`#84848c`).
pub const LOGO_GRAY: Color = Color::Rgb(132, 132, 140);

/// Diff & status semantic colors.
pub const GREEN_OK: Color = Color::Rgb(104, 194, 130);
pub const RED_ERR: Color = Color::Rgb(224, 108, 117);
pub const YELLOW_WARN: Color = Color::Rgb(229, 192, 123);
pub const CYAN_LINK: Color = Color::Rgb(106, 176, 219);

/// Inline `code` payload: warm amber so it reads apart from prose.
pub const CODE_INLINE: Color = Color::Rgb(229, 181, 103);
/// Syntax tokens inside fenced code blocks.
pub const CODE_KEYWORD: Color = Color::Rgb(198, 120, 221);
pub const CODE_STRING: Color = Color::Rgb(152, 195, 121);
pub const CODE_NUMBER: Color = Color::Rgb(229, 192, 123);
pub const CODE_COMMENT: Color = Color::Rgb(106, 106, 116);
pub const CODE_FUNCTION: Color = Color::Rgb(97, 175, 239);
pub const CODE_KEY: Color = Color::Rgb(86, 182, 194);

/// 3-row geometric pixel/half-block wordmark for `luna` styled like `opencode` in Screenshot 1:
/// `lu` in muted gray (`LOGO_GRAY`), `na` in crisp white (`TEXT_PRIMARY`).
///
/// Each character is 6 cells wide (except `l` = 4 cells) with 1 space between letters.
pub const LUNA_LOGO_LEFT: [&str; 3] = ["█     █   █ ", "█     █   █ ", "▀▀▀▀  ▀███▀ "];

pub const LUNA_LOGO_RIGHT: [&str; 3] = ["▄▀▀█▄  ▄▀▀▀█", "█   █  █   █", "▀   ▀  ▀▀▀▀▀"];

/// Nicer display title for the active agent mode.
pub fn mode_accent(mode_name: &str) -> Color {
    match mode_name {
        "Plan" => ACCENT_PLAN,
        "Auto" => ACCENT_AUTO,
        _ => ACCENT_CORAL,
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
