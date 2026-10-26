//! Phase 2e: notes directory setup.
//!
//! Asks the user where markdown notes should be stored on disk.
//! Blank / Enter keeps the default (`~/.luna/notes`).

use std::io::{BufRead, Write};
use std::path::PathBuf;

use harness_core::error::HarnessError;

use crate::menu::{self, Panel};
use crate::screen::{self, Size};
use crate::style::{Line, Span, ACCENT_COLOR, ERROR_COLOR, NAME_COLOR};
use crate::typewriter::{Pacer, Typewriter};

pub const PANEL: Panel = Panel {
    title: "🌙  LUNA",
    subtitle: "NOTES DIRECTORY",
    caption: "Where should your markdown notes live on disk?",
};

pub const PROMPT: &str = "Directory path (default: ~/.luna/notes) ▸ ";
pub const HINT: &str = "Enter an absolute or relative path, or press Enter to keep ~/.luna/notes. Change it later with `luna config set paths.notes_dir <path>`.";
pub const MAX_ATTEMPTS: usize = 10;

pub fn default_notes_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".luna").join("notes")
}

pub fn parse_path(answer: &str) -> PathBuf {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return default_notes_dir();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    } else if trimmed == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(trimmed)
}

pub fn summary(dir: &std::path::Path) -> String {
    dir.display().to_string()
}

fn body_lines(size: Size) -> Vec<Line> {
    let width = screen::body_limit(size.columns);
    let limit = width.saturating_sub(menu::ENTRY_INDENT);
    let mut lines = Vec::new();
    let label = Line::of(Span::strong("Markdown file notes", NAME_COLOR));
    lines.push(label);
    let detail = "Notes are stored as standalone .md files with YAML front-matter metadata. You can inspect, edit, or commit them directly.";
    for wrapped in screen::wrap(detail, limit) {
        lines.push(Line::of(Span::muted(wrapped)).indent(menu::ENTRY_INDENT));
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
) -> Result<PathBuf, HarnessError> {
    writer.clear_screen()?;
    let size = writer.size();
    writer.type_block_quick(&header(size))?;
    writer.blank()?;
    writer.write_block(&body(size))?;
    writer.blank()?;
    writer.write_block(&hint_lines(size))?;
    writer.blank()?;

    for attempt in 1..=MAX_ATTEMPTS {
        writer.write_prompt_line(&prompt_line(size, PROMPT))?;
        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            // EOF: default to standard location
            return Ok(default_notes_dir());
        }
        let path = parse_path(&answer);
        match validate_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) => {
                writer.write_centered(&format!("✗ {}", error), Some(ERROR_COLOR))?;
                if attempt == MAX_ATTEMPTS {
                    return Err(HarnessError::ConfigurationError(format!(
                        "no usable notes directory was chosen: {}",
                        error
                    )));
                }
            }
        }
    }

    Ok(default_notes_dir())
}

/// Checks that the folder can actually hold notes: it is created if missing, and it has to be a
/// directory rather than a file.
///
/// The check is the difference between a config value that works and one that fails at the first
/// `save_note` with an opaque I/O error.
pub fn validate_dir(path: &std::path::Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("the notes directory path is empty".to_string());
    }
    if path.exists() && !path.is_dir() {
        return Err(format!("'{}' is a file, not a directory", path.display()));
    }
    std::fs::create_dir_all(path)
        .map_err(|error| format!("could not create '{}': {}", path.display(), error))?;
    Ok(())
}
