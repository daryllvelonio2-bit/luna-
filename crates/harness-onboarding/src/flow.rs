//! The first-run flow: briefing, settings, handshake.
//!
//! [`needs_onboarding`] is the gate the harness calls before its agent loop; [`run`] is the flow
//! itself, written against `BufRead`/`Write`/`Pacer`/a column-count closure so the whole sequence
//! can be driven from a test with a scripted answer and no terminal.
//!
//! The phases:
//!
//! 1. A centered, typed mission briefing (`🌙`, greeting, what Luna is for, the hand-off line).
//! 2. The settings screens — who writes memory, auto-compaction, the messaging gateway, where
//!    notes live, and whether the agent may search and read the web — each drawn and validated by
//!    its own module ([`crate::memory_step`], [`crate::compact_step`], [`crate::gateway_step`],
//!    [`crate::notes_step`], [`crate::web_step`]) against the same panel.
//! 3. The selections written to the config file, then a centered confirmation naming them and the
//!    file they were written to.
//!
//! There is no persona step. The only built-in profile is `default`, and bare `luna` already runs
//! it. Custom profiles are created later with `/profile create`; onboarding configures the
//! default install and never picks a voice on the user's behalf.
//!
//! The cursor is restored on every path: by the [`CursorGuard`] drop guard, and — because a signal
//! does not unwind — by the SIGINT/SIGTERM handlers [`run_interactive`] installs.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use harness_core::config::{self, Layers};
use harness_core::error::HarnessError;
use harness_core::profiles::DEFAULT_PROFILE;
use harness_core::MemoryMode;

use crate::screen::{self, Size};
use crate::signal::CursorGuard;
use crate::store;
use crate::style::{Line, Span, CREST_COLOR, MUTED_COLOR, NAME_COLOR};
use crate::typewriter::{Pacer, RealPacer, Typewriter};

/// The celestial crest, rendered with a glow.
pub const CREST: &str = "🌙";
/// The opening greeting.
pub const GREETING: &str = "Hi! I am Luna, your personal AI agent harness.";
/// The mission brief, one paragraph per entry.
pub const MISSION: [&str; 3] = [
    "I live directly in your terminal, watching over your environments and workflows.",
    "I retain persistent memory across sessions — recalling your project layouts, past mistakes, architectural decisions, and customized conventions.",
    "I don't just answer questions; I inspect, execute, and reflect.",
];
/// The line that hands the floor to the user.
pub const TRANSITION: &str = "── Let's tune my neural core to your style ──";
/// The confirmation headline.
pub const LINK_TITLE: &str = "🌙  Profile configured.";

/// What the flow produced.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The profile the install runs with. Always the built-in `default`.
    pub profile: &'static str,
    /// Who writes durable memory from now on.
    pub memory: MemoryMode,
    /// Where markdown notes live on disk.
    pub notes_dir: PathBuf,
    /// Automatic compaction settings.
    pub auto_compact: store::AutoCompactSelection,
    /// Messaging platform gateway settings.
    pub gateway: store::GatewaySelection,
    /// Web access settings (`web_search` / `web_fetch`).
    pub web: store::WebSelection,
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

    // Phase 2b: who writes memory. A separate screen, same box, same list language.
    let memory = select_memory(&mut writer, reader)?;

    // Phase 2c: auto-compact setup.
    let auto_compact = crate::compact_step::select(&mut writer, reader)?;

    // Phase 2d: messaging gateway setup.
    let gateway = crate::gateway_step::select(&mut writer, reader)?;

    // Phase 2e: where markdown notes are stored.
    let notes_dir = crate::notes_step::select(&mut writer, reader)?;

    // Phase 2f: whether the agent may search and read the web.
    let web = crate::web_step::select(&mut writer, reader)?;

    let config_path = store::persist(layers, memory, &notes_dir, &auto_compact, &gateway, &web)?;

    let mut writer = Typewriter::new(out, pacer, size);
    confirmation(
        &mut writer,
        memory,
        &notes_dir,
        &auto_compact,
        &gateway,
        &web,
        &config_path,
    )?;

    Ok(Outcome {
        profile: DEFAULT_PROFILE,
        memory,
        notes_dir,
        auto_compact,
        gateway,
        web,
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

/// Phase 2b: the memory-core screen, then its answer loop.
///
/// The step owns its own screen and retry loop ([`crate::memory_step::select`]); the flow only has
/// to place it between the two phases it already had.
fn select_memory<R: BufRead, W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    reader: &mut R,
) -> Result<MemoryMode, HarnessError> {
    crate::memory_step::select(writer, reader)
}

/// Phase 3: the handshake.
#[allow(clippy::too_many_arguments)]
fn confirmation<W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    memory: MemoryMode,
    notes_dir: &Path,
    auto_compact: &store::AutoCompactSelection,
    gateway: &store::GatewaySelection,
    web: &store::WebSelection,
    config_path: &Path,
) -> Result<(), HarnessError> {
    writer.clear_screen()?;
    writer.show_cursor()?;
    let lines = confirmation_lines(
        memory,
        notes_dir,
        auto_compact,
        gateway,
        web,
        config_path,
        writer.size(),
    );
    writer.write_block(&lines)?;
    Ok(())
}

/// The centered confirmation block, sized for the terminal it will land in.
pub fn confirmation_lines(
    memory: MemoryMode,
    notes_dir: &Path,
    auto_compact: &store::AutoCompactSelection,
    gateway: &store::GatewaySelection,
    web: &store::WebSelection,
    config_path: &Path,
    size: Size,
) -> Vec<Line> {
    let columns = size.columns;
    let limit = (columns as usize)
        .saturating_sub(2 * screen::MARGIN)
        .max(24);
    let mut lines = vec![
        Line::of(Span::strong(LINK_TITLE, CREST_COLOR)),
        Line::blank(),
        Line::of(Span::muted("Profile  ")).with(Span::strong(
            format!("{} (built-in)", DEFAULT_PROFILE),
            NAME_COLOR,
        )),
        Line::of(Span::muted(
            "Create another with `/profile create`; open it with `luna <name>`.",
        )),
        Line::blank(),
    ];
    // The hint above is a fixed string; rewrap it for the narrow terminal case so it obeys `limit`.
    let hint_start = 3usize;
    let hint = lines.remove(hint_start).text();
    for (offset, wrapped) in screen::wrap(&hint, limit).into_iter().enumerate() {
        lines.insert(hint_start + offset, Line::of(Span::muted(wrapped)));
    }
    // Same shape as the persona block above: label, then the value — on one line when it fits and
    // wrapped under the label when the terminal is too narrow for both.
    let label = "Memory writer";
    let summary = crate::memory_step::summary(memory);
    if label.len() + 2 + summary.len() <= limit {
        lines.push(
            Line::of(Span::muted(format!("{label}  "))).with(Span::strong(summary, NAME_COLOR)),
        );
    } else {
        lines.push(Line::of(Span::muted(label)));
        for wrapped in screen::wrap(summary, limit) {
            lines.push(Line::of(Span::strong(wrapped, NAME_COLOR)));
        }
    }
    lines.push(Line::blank());
    let nd_label = "Notes directory";
    let nd_summary = notes_dir.display().to_string();
    if nd_label.len() + 2 + nd_summary.len() <= limit {
        lines.push(
            Line::of(Span::muted(format!("{nd_label}  ")))
                .with(Span::strong(nd_summary, NAME_COLOR)),
        );
    } else {
        lines.push(Line::of(Span::muted(nd_label)));
        for wrapped in screen::wrap(&nd_summary, limit) {
            lines.push(Line::of(Span::strong(wrapped, NAME_COLOR)));
        }
    }
    lines.push(Line::blank());
    let ac_label = "Auto-compact";
    let ac_summary = crate::compact_step::summary(auto_compact);
    if ac_label.len() + 2 + ac_summary.len() <= limit {
        lines.push(
            Line::of(Span::muted(format!("{ac_label}  ")))
                .with(Span::strong(ac_summary, NAME_COLOR)),
        );
    } else {
        lines.push(Line::of(Span::muted(ac_label)));
        for wrapped in screen::wrap(&ac_summary, limit) {
            lines.push(Line::of(Span::strong(wrapped, NAME_COLOR)));
        }
    }
    lines.push(Line::blank());
    let gw_label = "Messaging gateway";
    let gw_summary = crate::gateway_step::summary(gateway);
    if gw_label.len() + 2 + gw_summary.len() <= limit {
        lines.push(
            Line::of(Span::muted(format!("{gw_label}  ")))
                .with(Span::strong(gw_summary, NAME_COLOR)),
        );
    } else {
        lines.push(Line::of(Span::muted(gw_label)));
        for wrapped in screen::wrap(&gw_summary, limit) {
            lines.push(Line::of(Span::strong(wrapped, NAME_COLOR)));
        }
    }
    lines.push(Line::blank());
    let web_label = "Web access";
    let web_summary = crate::web_step::summary(web);
    if web_label.len() + 2 + web_summary.len() <= limit {
        lines.push(
            Line::of(Span::muted(format!("{web_label}  ")))
                .with(Span::strong(web_summary, NAME_COLOR)),
        );
    } else {
        lines.push(Line::of(Span::muted(web_label)));
        for wrapped in screen::wrap(&web_summary, limit) {
            lines.push(Line::of(Span::strong(wrapped, NAME_COLOR)));
        }
    }
    lines.push(Line::blank());
    for wrapped in screen::wrap(
        &format!("Config written to {}", config_path.display()),
        limit,
    ) {
        lines.push(Line::of(Span::muted(wrapped)));
    }
    for wrapped in screen::wrap(
        "Verify with `luna config get memory.mode`, or change settings later with `luna config set`.",
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
    use std::path::PathBuf;

    #[test]
    fn the_confirmation_names_the_default_profile_and_the_file() {
        let path = std::env::temp_dir().join(".luna").join("config.yaml");
        let notes_dir = std::env::temp_dir().join(".luna").join("notes");
        let ac = store::AutoCompactSelection::default();
        let gw = store::GatewaySelection::default();
        let web = store::WebSelection::default();
        let block = confirmation_lines(
            MemoryMode::Background,
            &notes_dir,
            &ac,
            &gw,
            &web,
            &path,
            Size::new(90, 24),
        )
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");

        assert!(block.contains("Profile configured"));
        assert!(block.contains("default (built-in)"));
        assert!(block.contains(&path.display().to_string()));
        // Both answers are reported, so the screen states the whole outcome.
        assert!(block.contains("Background saver"), "{block}");
        assert!(block.contains("memory.mode"), "{block}");
        // And so is the web answer, including where a key would come from.
        assert!(block.contains("Web access"), "{block}");
        assert!(block.contains("DuckDuckGo"), "{block}");
    }

    #[test]
    fn the_confirmation_fits_a_narrow_terminal() {
        let notes_dir = PathBuf::from("~/.luna/notes");
        for columns in [40u16, 80, 120] {
            let ac = store::AutoCompactSelection::default();
            let gw = store::GatewaySelection::default();
            let web = store::WebSelection {
                enabled: true,
                backend: harness_core::config::SearchBackend::Brave,
                api_key_env: Some("A_VERY_LONG_ENVIRONMENT_VARIABLE_NAME".to_string()),
            };
            for line in confirmation_lines(
                MemoryMode::Session,
                &notes_dir,
                &ac,
                &gw,
                &web,
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
        assert!(MISSION[2].contains("inspect, execute"));
        assert!(TRANSITION.contains("tune my neural core"));
    }
}
