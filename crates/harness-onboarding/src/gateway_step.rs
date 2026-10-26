//! Phase 2d: messaging platform gateway setup.
//!
//! Asks the user whether to connect Luna to external messaging platforms (Telegram, Discord, WhatsApp)
//! or keep Luna purely terminal-based.

use std::io::{BufRead, Write};

use harness_core::error::HarnessError;

use crate::menu::{self, Panel};
use crate::screen::{self, Size};
use crate::store::GatewaySelection;
use crate::style::{Line, Span, ACCENT_COLOR, ERROR_COLOR};
use crate::typewriter::{Pacer, Typewriter};

/// Panel for gateway onboarding step.
pub const PANEL: Panel = Panel {
    title: "🌙  LUNA",
    subtitle: "MESSAGING GATEWAY",
    caption: "Connect Luna to your chat platforms (Telegram, Discord, WhatsApp)?",
};

pub const PROMPT: &str = "Select 1-4 ▸ ";
pub const TELEGRAM_TOKEN_PROMPT: &str = "Telegram Bot Token (blank to configure later) ▸ ";
pub const DISCORD_TOKEN_PROMPT: &str = "Discord Bot Token (blank to configure later) ▸ ";
pub const WHATSAPP_TOKEN_PROMPT: &str = "WhatsApp Token (blank to configure later) ▸ ";

pub const HINT: &str = "Enter 1 for Terminal only, 2 for Telegram, 3 for Discord, 4 for WhatsApp — blank keeps Terminal only (1). Change it later with `luna gateway` or `luna config`.";
pub const MAX_ATTEMPTS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub id: u8,
    pub name: &'static str,
    pub detail: &'static str,
    pub platform: &'static str,
    pub enabled: bool,
}

pub const CHOICES: [Choice; 4] = [
    Choice {
        id: 1,
        name: "Terminal only (CLI)",
        detail: "Run Luna locally in your terminal. Messaging gateway disabled (default).",
        platform: "none",
        enabled: false,
    },
    Choice {
        id: 2,
        name: "Telegram Bot",
        detail: "Connect Luna to Telegram via long-polling bot (no public IP or webhook required).",
        platform: "telegram",
        enabled: true,
    },
    Choice {
        id: 3,
        name: "Discord Bot",
        detail: "Connect Luna to Discord server channels or direct messages using a bot token.",
        platform: "discord",
        enabled: true,
    },
    Choice {
        id: 4,
        name: "WhatsApp Cloud",
        detail: "Connect Luna to Meta WhatsApp Cloud API via inbound webhook.",
        platform: "whatsapp",
        enabled: true,
    },
];

pub const DEFAULT_ID: u8 = 1;

pub fn parse_selection(answer: &str) -> Result<Choice, String> {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return Ok(CHOICES[0]);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "1" | "terminal" | "none" | "cli" | "off" => Ok(CHOICES[0]),
        "2" | "telegram" | "tg" => Ok(CHOICES[1]),
        "3" | "discord" | "dc" => Ok(CHOICES[2]),
        "4" | "whatsapp" | "wa" => Ok(CHOICES[3]),
        other => Err(format!("'{other}' is not an option: pick 1, 2, 3, or 4")),
    }
}

pub fn summary(selection: &GatewaySelection) -> String {
    if !selection.enabled {
        "Terminal only (disabled)".to_string()
    } else {
        match selection.platform.as_str() {
            "telegram" => "Telegram Bot (enabled)".to_string(),
            "discord" => "Discord Bot (enabled)".to_string(),
            "whatsapp" => "WhatsApp Cloud (enabled)".to_string(),
            other => format!("{other} (enabled)"),
        }
    }
}

fn body_lines(size: Size) -> Vec<Line> {
    let width = screen::body_limit(size.columns);
    let limit = width.saturating_sub(menu::ENTRY_INDENT);
    let mut lines = Vec::new();
    for (index, option) in CHOICES.iter().enumerate() {
        if index > 0 {
            lines.push(Line::blank());
        }
        let label = menu::entry_line(
            option.id,
            option.name,
            (option.id == DEFAULT_ID).then_some(menu::DEFAULT_MARKER),
            width,
        );
        lines.push(label);
        for wrapped in screen::wrap(option.detail, limit) {
            lines.push(Line::of(Span::muted(wrapped)).indent(menu::ENTRY_INDENT));
        }
    }
    lines
}

pub fn header(size: Size) -> Vec<Line> {
    menu::panel_header(size, &PANEL, menu::widest_of(&body_lines(size)))
}

pub fn body(size: Size) -> Vec<Line> {
    menu::place_block(body_lines(size), size)
}

pub fn prompt_line(size: Size, text: &str) -> Line {
    let line = Line::of(Span::strong(text, ACCENT_COLOR));
    let pad = screen::center_pad(size.columns as usize, line.width());
    line.indent(pad)
}

pub fn hint_lines(size: Size) -> Vec<Line> {
    menu::muted_wrapped(HINT, size)
}

pub fn select<R: BufRead, W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    reader: &mut R,
) -> Result<GatewaySelection, HarnessError> {
    writer.clear_screen()?;
    let size = writer.size();
    writer.type_block_quick(&header(size))?;
    writer.blank()?;
    writer.write_block(&body(size))?;
    writer.blank()?;
    writer.write_block(&hint_lines(size))?;
    writer.blank()?;

    let mut chosen_choice = None;
    for attempt in 1..=MAX_ATTEMPTS {
        writer.write_prompt_line(&prompt_line(size, PROMPT))?;
        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            // EOF: default to Terminal only
            return Ok(GatewaySelection::default());
        }
        match parse_selection(&answer) {
            Ok(choice) => {
                chosen_choice = Some(choice);
                break;
            }
            Err(reason) => {
                if attempt == MAX_ATTEMPTS {
                    return Err(HarnessError::ConfigurationError(format!(
                        "gateway setup aborted after {MAX_ATTEMPTS} attempts: {reason}"
                    )));
                }
                let line = Line::of(Span::strong(format!("✗ {reason}"), ERROR_COLOR));
                let pad = screen::center_pad(size.columns as usize, line.width());
                writer.write_line(&line.indent(pad))?;
                writer.blank()?;
            }
        }
    }

    let choice = chosen_choice.unwrap_or(CHOICES[0]);
    if !choice.enabled {
        return Ok(GatewaySelection {
            enabled: false,
            platform: "none".to_string(),
            token: None,
        });
    }

    // Platform enabled: ask for token
    let token_prompt_str = match choice.platform {
        "telegram" => TELEGRAM_TOKEN_PROMPT,
        "discord" => DISCORD_TOKEN_PROMPT,
        "whatsapp" => WHATSAPP_TOKEN_PROMPT,
        _ => "Bot Token (blank to configure later) ▸ ",
    };

    writer.blank()?;
    writer.write_prompt_line(&prompt_line(size, token_prompt_str))?;
    let mut token_input = String::new();
    let _ = reader.read_line(&mut token_input);
    let trimmed_token = token_input.trim();
    let token = if trimmed_token.is_empty() {
        None
    } else {
        Some(trimmed_token.to_string())
    };

    Ok(GatewaySelection {
        enabled: true,
        platform: choice.platform.to_string(),
        token,
    })
}
