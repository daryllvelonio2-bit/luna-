//! The first-run flow: briefing, selection, handshake.
//!
//! [`needs_onboarding`] is the gate the harness calls before its agent loop; [`run`] is the flow
//! itself, written against `BufRead`/`Write`/`Pacer`/a column-count closure so the whole three-phase
//! sequence can be driven from a test with a scripted answer and no terminal.
//!
//! The three phases:
//!
//! 1. A centered, typed mission briefing (`🌙`, greeting, what Luna is for, the hand-off line).
//! 2. The persona menu, with `1..=10` accepted, a blank line taking the default, and anything else
//!    rejected with a reason and re-prompted.
//! 3. The selection written to the config file, then a centered confirmation naming the persona and
//!    the file it was written to.
//!
//! The cursor is restored on every path: by the [`CursorGuard`] drop guard, and — because a signal
//! does not unwind — by the SIGINT/SIGTERM handlers [`run_interactive`] installs.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use harness_core::config::{self, Layers};
use harness_core::error::HarnessError;

use crate::menu;
use crate::persona::Persona;
use crate::screen::{self, Size};
use crate::signal::CursorGuard;
use crate::store;
use crate::style::{Line, Span, CREST_COLOR, ERROR_COLOR, MUTED_COLOR, NAME_COLOR};
use crate::typewriter::{Pacer, RealPacer, Typewriter};

/// The celestial crest, rendered with a glow.
pub const CREST: &str = "🌙";
/// The opening greeting.
pub const GREETING: &str = "Hi! I am Luna, your personal AI agent harness.";
/// The mission brief, one paragraph per entry.
pub const MISSION: [&str; 3] = [
    "I live directly in your terminal, watching over your environments and workflows.",
    "I retain persistent memory across sessions — recalling your project layouts, past mistakes, architectural decisions, and customized conventions.",
    "I don't just answer questions; I inspect, execute, reflect, and self-improve.",
];
/// The line that hands the floor to the user.
pub const TRANSITION: &str = "── Let's tune my neural core to your style ──";
/// The confirmation headline.
pub const LINK_TITLE: &str = "🌙  Neural link established.";
/// How many rejected answers are tolerated before the flow gives up instead of looping forever.
pub const MAX_ATTEMPTS: usize = 10;

/// What the flow produced.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The chosen personality core.
    pub persona: &'static Persona,
    /// The config file the choice was written to.
    pub config_path: PathBuf,
}

/// Whether the first-run flow should run: `true` until it has completed once.
///
/// Reads the resolved config, so a missing file (whose default is `first_run: true`), a file without
/// the key, and a file that still says `true` all take the tour — while `first_run: false`, whether
/// written by the flow or set with `luna config set first_run false`, skips it.
pub fn needs_onboarding(layers: &Layers) -> Result<bool, HarnessError> {
    Ok(config::resolve(layers)?.into_config().first_run)
}

/// Run the three phases against any reader/writer pair.
pub fn run<R, W, P>(
    layers: &Layers,
    reader: &mut R,
    out: &mut W,
    pacer: &mut P,
    size: &dyn Fn() -> Size,
) -> Result<Outcome, HarnessError>
where
    R: BufRead,
    W: Write,
    P: Pacer,
{
    // Defence in depth: the flow hides the cursor through the writer it was handed, and this guard
    // restores the real stdout even if the writer is something else and a step fails early.
    let _guard = CursorGuard::new();

    let mut writer = Typewriter::new(out, pacer, size);
    briefing(&mut writer)?;
    let persona = select_persona(&mut writer, reader)?;

    let config_path = store::persist(layers, persona)?;

    let mut writer = Typewriter::new(out, pacer, size);
    confirmation(&mut writer, persona, &config_path)?;

    Ok(Outcome {
        persona,
        config_path,
    })
}

/// Run the flow on the real terminal: real clock, real stdin/stdout, real geometry, and the signal
/// handlers that keep an early Ctrl+C from stranding an invisible cursor.
pub fn run_interactive(layers: &Layers) -> Result<Outcome, HarnessError> {
    crate::signal::install()?;
    let size = || match crossterm::terminal::size() {
        // A pty with no window size reports 0 columns; believing it would wrap the whole briefing at
        // `MIN_BODY` and centre it in nothing, so that answer counts as "unknown".
        Ok((columns, rows)) => Size::new(columns, rows),
        _ => Size::fallback(),
    };
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut pacer = RealPacer;
    run(layers, &mut reader, &mut out, &mut pacer, &size)
}

/// Phase 1: clear the screen, hide the cursor, stream the briefing.
fn briefing<W: Write, P: Pacer>(writer: &mut Typewriter<'_, W, P>) -> Result<(), HarnessError> {
    writer.clear_screen()?;
    writer.hide_cursor()?;

    writer.type_line(CREST, Some(CREST_COLOR))?;
    writer.blank()?;
    writer.pause_between_paragraphs();

    writer.type_paragraph(GREETING, Some(CREST_COLOR))?;
    writer.blank()?;
    for paragraph in MISSION {
        writer.type_paragraph(paragraph, None)?;
    }
    writer.blank()?;
    writer.type_line(TRANSITION, Some(MUTED_COLOR))?;

    // The cursor comes back before any input, so the user can see what they are typing.
    writer.show_cursor()?;
    Ok(())
}

/// Phase 2: the menu, then the answer loop.
fn select_persona<R: BufRead, W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    reader: &mut R,
) -> Result<&'static Persona, HarnessError> {
    writer.clear_screen()?;
    let size = writer.size();
    // The header draws itself at boot speed, then the choices land at once: the box gives the eye
    // somewhere to land before ten names arrive.
    writer.type_block_quick(&menu::header(size))?;
    writer.blank()?;
    writer.write_block(&menu::body(size))?;
    writer.blank()?;
    writer.write_block(&menu::hint_lines(size))?;
    writer.blank()?;

    for attempt in 1..=MAX_ATTEMPTS {
        let prompt = menu::prompt_lines(size);
        if let Some((last, leading)) = prompt.split_last() {
            for line in leading {
                writer.write_line(line)?;
            }
            writer.write_prompt_line(last)?;
        }
        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            // EOF: the input is gone (a piped stdin, a closed terminal). Say so rather than
            // persisting a persona the user never chose.
            writer.blank()?;
            return Err(HarnessError::Io(
                "input closed before a personality core was chosen".to_string(),
            ));
        }
        match menu::parse_selection(&answer) {
            Ok(persona) => return Ok(persona),
            Err(error) => {
                writer.write_centered(&format!("✗ {}", error), Some(ERROR_COLOR))?;
                if attempt == MAX_ATTEMPTS {
                    break;
                }
            }
        }
    }

    Err(HarnessError::ConfigurationError(format!(
        "no personality core was selected in {} attempts; run `luna` again when you are ready",
        MAX_ATTEMPTS
    )))
}

/// Phase 3: the handshake.
fn confirmation<W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    persona: &Persona,
    config_path: &Path,
) -> Result<(), HarnessError> {
    writer.clear_screen()?;
    writer.show_cursor()?;
    let lines = confirmation_lines(persona, config_path, writer.size());
    writer.write_block(&lines)?;
    Ok(())
}

/// The centered confirmation block, sized for the terminal it will land in.
pub fn confirmation_lines(persona: &Persona, config_path: &Path, size: Size) -> Vec<Line> {
    let columns = size.columns;
    let limit = (columns as usize)
        .saturating_sub(2 * screen::MARGIN)
        .max(24);
    let mut lines = vec![
        Line::of(Span::strong(LINK_TITLE, CREST_COLOR)),
        Line::blank(),
        Line::of(Span::muted("Personality core  "))
            .with(Span::strong(persona.labelled(), NAME_COLOR)),
    ];
    for wrapped in screen::wrap(&format!("“{}”", persona.tagline), limit) {
        lines.push(Line::of(Span::muted(wrapped)));
    }
    lines.push(Line::blank());
    for wrapped in screen::wrap(
        &format!("Config written to {}", config_path.display()),
        limit,
    ) {
        lines.push(Line::of(Span::muted(wrapped)));
    }
    for wrapped in screen::wrap(
        "Verify with `luna config get persona.id`, or switch cores later with `luna config set persona.id <1-10>`.",
        limit,
    ) {
        lines.push(Line::of(Span::muted(wrapped)));
    }
    screen::center_lines(lines, columns)
        .into_iter()
        .map(Line::trim_end)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persona;
    use std::path::PathBuf;

    #[test]
    fn the_confirmation_names_the_persona_and_the_file() {
        let persona = persona::by_id(3).expect("in the catalog");
        let path = PathBuf::from("/home/janelle/.luna/config.yaml");
        let block = confirmation_lines(persona, &path, Size::new(90, 24))
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(block.contains("Neural link established"));
        assert!(block.contains(&persona.labelled()));
        assert!(block.contains("/home/janelle/.luna/config.yaml"));
        assert!(block.contains(persona.tagline));
    }

    #[test]
    fn the_confirmation_fits_a_narrow_terminal() {
        let persona = persona::by_id(9).expect("in the catalog");
        for columns in [40u16, 80, 120] {
            for line in confirmation_lines(
                persona,
                &PathBuf::from("~/.luna/config.yaml"),
                Size::new(columns, 24),
            ) {
                assert!(
                    line.width() <= columns as usize,
                    "confirmation overflows {columns} columns: {:?}",
                    line.text()
                );
            }
        }
    }

    #[test]
    fn the_briefing_copy_is_the_promised_copy() {
        assert_eq!(GREETING, "Hi! I am Luna, your personal AI agent harness.");
        assert_eq!(MISSION.len(), 3);
        assert!(MISSION[2].contains("self-improve"));
        assert!(TRANSITION.contains("tune my neural core"));
    }
}
