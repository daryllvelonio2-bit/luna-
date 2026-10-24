//! Phase 2c: automatic context compaction setup.
//!
//! Asks the user whether to enable auto-compaction, what token threshold to trigger at (100k-700k),
//! and how many oldest turns to compact.

use std::io::{BufRead, Write};

use harness_core::config::{
    AUTO_COMPACT_DEFAULT_THRESHOLD, AUTO_COMPACT_DEFAULT_TURNS, AUTO_COMPACT_MAX_THRESHOLD,
    AUTO_COMPACT_MIN_THRESHOLD,
};
use harness_core::error::HarnessError;

use crate::menu::{self, Panel};
use crate::screen::{self, Size};
use crate::store::AutoCompactSelection;
use crate::style::{Line, Span, ACCENT_COLOR, ERROR_COLOR, NAME_COLOR};
use crate::typewriter::{Pacer, Typewriter};

/// Panel for auto-compact onboarding step.
pub const PANEL: Panel = Panel {
    title: "🌙  LUNA",
    subtitle: "AUTO-COMPACT",
    caption: "Keep transcript context within bounds automatically?",
};

pub const PROMPT: &str = "Select 1-2 ▸ ";
pub const THRESHOLD_PROMPT: &str = "Threshold (100k-700k, default 150k) ▸ ";
pub const TURNS_PROMPT: &str = "Turns to compact (default 20) ▸ ";

pub const HINT: &str = "Enter 1 to enable auto-compact, 2 to disable — blank keeps enabled (1). Change it later with `/auto-compact`.";
pub const MAX_ATTEMPTS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub id: u8,
    pub name: &'static str,
    pub detail: &'static str,
    pub enabled: bool,
}

pub const CHOICES: [Choice; 2] = [
    Choice {
        id: 1,
        name: "Enable auto-compaction",
        detail: "Oldest conversation turns are trimmed when transcript tokens cross the threshold.",
        enabled: true,
    },
    Choice {
        id: 2,
        name: "Disable auto-compaction",
        detail: "Context runs unbounded without automatic turn compaction.",
        enabled: false,
    },
];

pub const DEFAULT_ID: u8 = 1;

pub fn parse_selection(answer: &str) -> Result<Choice, String> {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return Ok(CHOICES[0]);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "1" | "enable" | "y" | "yes" | "on" => Ok(CHOICES[0]),
        "2" | "disable" | "n" | "no" | "off" => Ok(CHOICES[1]),
        other => Err(format!("'{other}' is not an option: pick 1 or 2")),
    }
}

pub fn parse_threshold(input: &str) -> Result<usize, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(AUTO_COMPACT_DEFAULT_THRESHOLD);
    }
    let lower = trimmed.to_ascii_lowercase();
    let num: usize = if let Some(rest) = lower.strip_suffix('k') {
        let n: usize = rest
            .trim()
            .parse()
            .map_err(|_| format!("'{trimmed}' is not a valid number"))?;
        n.checked_mul(1_000)
            .ok_or_else(|| format!("'{trimmed}' is too large"))?
    } else {
        trimmed
            .parse()
            .map_err(|_| format!("'{trimmed}' is not a valid number"))?
    };

    if num < AUTO_COMPACT_MIN_THRESHOLD || num > AUTO_COMPACT_MAX_THRESHOLD {
        return Err(format!(
            "threshold must be between {} and {} (got {})",
            AUTO_COMPACT_MIN_THRESHOLD, AUTO_COMPACT_MAX_THRESHOLD, num
        ));
    }
    Ok(num)
}

pub fn parse_turns(input: &str) -> Result<usize, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(AUTO_COMPACT_DEFAULT_TURNS);
    }
    let n: usize = trimmed
        .parse()
        .map_err(|_| format!("'{trimmed}' is not a valid number of turns"))?;
    if n == 0 {
        return Err("turns must be positive (got 0)".to_string());
    }
    Ok(n)
}

pub fn summary(selection: &AutoCompactSelection) -> String {
    if selection.enabled {
        format!(
            "Enabled (threshold: {}k, turns: {})",
            selection.threshold / 1_000,
            selection.turns
        )
    } else {
        "Disabled".to_string()
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
) -> Result<AutoCompactSelection, HarnessError> {
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
            // EOF: default to enabled with standard defaults for non-interactive test harnesses
            return Ok(AutoCompactSelection::default());
        }
        match parse_selection(&answer) {
            Ok(choice) => {
                chosen_choice = Some(choice);
                break;
            }
            Err(error) => {
                writer.write_centered(&format!("✗ {}", error), Some(ERROR_COLOR))?;
                if attempt == MAX_ATTEMPTS {
                    return Err(HarnessError::ConfigurationError(format!(
                        "no option selected in {MAX_ATTEMPTS} attempts"
                    )));
                }
            }
        }
    }

    let choice = chosen_choice.unwrap_or(CHOICES[0]);
    if !choice.enabled {
        let selection = AutoCompactSelection {
            enabled: false,
            threshold: AUTO_COMPACT_DEFAULT_THRESHOLD,
            turns: AUTO_COMPACT_DEFAULT_TURNS,
        };
        writer.blank()?;
        writer.write_centered(&format!("✓ Auto-compact: {}", summary(&selection)), Some(NAME_COLOR))?;
        return Ok(selection);
    }

    // Ask for threshold
    let mut chosen_threshold = AUTO_COMPACT_DEFAULT_THRESHOLD;
    for attempt in 1..=MAX_ATTEMPTS {
        writer.write_prompt_line(&prompt_line(size, THRESHOLD_PROMPT))?;
        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            break;
        }
        match parse_threshold(&answer) {
            Ok(th) => {
                chosen_threshold = th;
                break;
            }
            Err(error) => {
                writer.write_centered(&format!("✗ {}", error), Some(ERROR_COLOR))?;
                if attempt == MAX_ATTEMPTS {
                    return Err(HarnessError::ConfigurationError(format!(
                        "invalid threshold in {MAX_ATTEMPTS} attempts"
                    )));
                }
            }
        }
    }

    // Ask for turns
    let mut chosen_turns = AUTO_COMPACT_DEFAULT_TURNS;
    for attempt in 1..=MAX_ATTEMPTS {
        writer.write_prompt_line(&prompt_line(size, TURNS_PROMPT))?;
        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            break;
        }
        match parse_turns(&answer) {
            Ok(t) => {
                chosen_turns = t;
                break;
            }
            Err(error) => {
                writer.write_centered(&format!("✗ {}", error), Some(ERROR_COLOR))?;
                if attempt == MAX_ATTEMPTS {
                    return Err(HarnessError::ConfigurationError(format!(
                        "invalid turns in {MAX_ATTEMPTS} attempts"
                    )));
                }
            }
        }
    }

    let selection = AutoCompactSelection {
        enabled: true,
        threshold: chosen_threshold,
        turns: chosen_turns,
    };
    writer.blank()?;
    writer.write_centered(&format!("✓ Auto-compact: {}", summary(&selection)), Some(NAME_COLOR))?;
    Ok(selection)
}
