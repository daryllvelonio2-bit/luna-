//! `/usage`: what this session has spent, and how much of the context it fills.
//!
//! The ledger already knows every turn's tokens, its cached share and its price — `luna usage`
//! reads them from the terminal. The chat pane never showed them, so the cost of a session was
//! only visible by leaving the session. `/usage` reads the same ledger, filtered to the session on
//! screen: the same numbers, in the place the work is happening.
//!
//! The layout is a report, not a table. A context window is a *share*, and a share is read as a
//! picture before it is read as a number: the grid of cells fills left to right, then the rows
//! beside it say what filled it — system prompt, the user's turns, the model's replies, its
//! reasoning, and the tool calls. The empty cells are the free space, which is the one number a
//! reader usually wants and the one a total of "tokens used" never shows.
//!
//! Three things are reported, and they answer three different questions:
//!
//! - **context** — what the next request carries, split by what put it there.
//! - **tokens** — what the conversation cost, in and out of the model.
//! - **cache** — how much of the input the provider served from its own prompt cache. A provider
//!   that says nothing about caching reports nothing here: a line of zeroes would read as
//!   "caching was measured and did not help", when what happened is that nothing was said.
//!
//! The report is emitted as one block into the transcript. It is not a modal: a modal would cover
//! the conversation the numbers are about, and `/usage` is a question asked *about* the pane, not
//! a place to go.

use std::sync::Arc;

use colored::Colorize;
use harness_core::session::ContextParts;
use harness_core::usage::{UsageFilter, UsageLedger, UsageTotals};
use harness_core::{Config, HarnessError};

use crate::commands::chat::ui::{get_terminal_width, ChatUi};
use crate::commands::chat::ChatAgent;
use crate::commands::context_window::context_window_for;
use crate::commands::usage::{
    cache_text, cost_text, dot_bar, dot_grid, empty_dot, fill_ratio, filled_dot, format_duration,
    format_tokens,
};

/// Where the right-hand column starts when the grid is drawn beside it. The grid is
/// `GRID_COLUMNS * 2 - 1` cells wide (`4*2-1 = 7`), two spaces of indent, then a gap.
const RIGHT_COLUMN: usize = 14;

/// Below this width the grid and the rows stack instead of sitting side by side.
const SIDE_BY_SIDE_MIN: usize = 60;

/// `/usage`: the session's context, its spend, and the ledger's, in the chat pane.
pub(crate) async fn handle_usage_command(
    ui: &ChatUi,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &str,
    current_provider: &str,
    current_model: &str,
) -> Result<(), HarnessError> {
    if !config.usage.enabled {
        ui.emit_above(&format!(
            "{} Usage recording is off — set {} to turn it on",
            "•".cyan(),
            "usage.enabled = true".bold()
        ));
        return Ok(());
    }

    let ledger = match UsageLedger::open(Some(config.paths.usage_db.clone())) {
        Ok(ledger) => ledger,
        Err(error) => {
            ui.emit_above(&format!(
                "{} Could not open the usage ledger: {}",
                "✖".red().bold(),
                error
            ));
            return Ok(());
        }
    };

    let session_filter = UsageFilter {
        session_id: Some(session_id.to_string()),
        ..UsageFilter::default()
    };
    let session_totals = ledger.totals(&session_filter).await?;
    let all_totals = ledger.totals(&UsageFilter::default()).await?;

    // A turn in flight holds the agent lock for its whole run, so the parts are taken without
    // waiting for it: a usage report must never make a running turn wait. `None` says so below.
    let parts = match agent.try_lock() {
        Ok(agent) => Some(agent.context_parts().await.unwrap_or_default()),
        Err(_) => None,
    };

    let max = context_window_for(config, current_provider, current_model);
    let mut report = context_report(
        &format!("{current_provider}/{current_model}"),
        parts.as_ref(),
        max,
        &compact_note(config),
        get_terminal_width(),
    );
    report.push(String::new());
    report.extend(totals_report("Session totals", &session_totals));

    // The whole ledger, so a session that has just started still shows what the machine has
    // spent. Skipped when this session *is* the ledger: a second copy of the same numbers would
    // read as more work than was done.
    if all_totals.turns > session_totals.turns {
        report.push(String::new());
        report.extend(totals_report("All sessions", &all_totals));
    }

    // One block: the composer is repainted after every `emit_above`, and a report that repainted
    // it ten times would arrive as ten separate notes.
    ui.emit_above(&report.join("\n"));

    Ok(())
}

/// How the fold is configured, in the words the report uses. The pane and `luna usage` both say
/// exactly what the config says rather than a rounded idea of it.
fn compact_note(config: &Config) -> String {
    if !config.agent.auto_compact_enabled {
        return "disabled (agent.auto_compact_enabled)".to_string();
    }
    match config.agent.auto_compact_percent {
        Some(percent) => format!("at {percent}% of the window"),
        None => format!(
            "at {} tokens",
            format_tokens(config.agent.auto_compact_threshold as u64)
        ),
    }
}

fn rule(width: usize) -> String {
    "─".repeat(width.clamp(20, 78)).dimmed().to_string()
}

fn heading(title: &str) -> String {
    format!("{} {}", "▸".dimmed(), title.bold())
}

/// One row of the context split: the bullet carries the share as colour, the text as a number.
fn part_row(parts: &ContextParts, max: u32, label: &str, tokens: u32) -> String {
    // Without a published window there is no share to draw, and a `0.0%` row would read as
    // empty — so the row states the tokens and nothing else.
    if max == 0 {
        return format!(
            "{} {label}: {} tokens",
            "•".dimmed(),
            format_tokens(tokens as u64)
        );
    }
    let share = parts.percent(tokens, max);
    format!(
        "{} {label}: {} tokens ({share:.1}%)",
        filled_dot(fill_ratio(tokens as u64, max as u64)),
        format_tokens(tokens as u64)
    )
}

/// The context section: the filled grid, then what filled it.
///
/// Pure, so the report is checked without a terminal: `parts` is `None` while a turn holds the
/// agent, and the section then says the measurement is unavailable instead of inventing zeros.
pub(crate) fn context_report(
    model_label: &str,
    parts: Option<&ContextParts>,
    max: u32,
    compact_note: &str,
    width: usize,
) -> Vec<String> {
    let mut lines = vec![rule(width), heading("Context Usage")];

    let Some(parts) = parts else {
        lines.push(format!(
            "  {}",
            "a turn is in flight — context is measured when it finishes".dimmed()
        ));
        return lines;
    };

    let used = parts.used();
    let ratio = fill_ratio(used as u64, max as u64);
    let headline = if max == 0 {
        format!(
            "{} · {} tokens · {}",
            model_label.bold(),
            format_tokens(used as u64),
            "window unknown".dimmed()
        )
    } else {
        format!(
            "{} · {}/{} tokens ({:.1}%)",
            model_label.bold(),
            format_tokens(used as u64),
            format_tokens(max as u64),
            ratio * 100.0
        )
    };

    let mut rows = vec![
        headline,
        format!("{} Auto-compact {compact_note}", "·".dimmed()),
        format!(
            "{}",
            "Estimated usage (4 chars ≈ 1 token over the transcript)".dimmed()
        ),
        part_row(parts, max, "System prompt", parts.system),
        part_row(parts, max, "User messages", parts.user),
        part_row(parts, max, "Agent responses", parts.assistant),
        part_row(parts, max, "Reasoning", parts.reasoning),
        part_row(
            parts,
            max,
            "Tool calls",
            parts.tool_calls + parts.tool_results,
        ),
    ];
    if max > 0 {
        rows.push(format!(
            "{} Free space: {} tokens ({:.1}%)",
            empty_dot(),
            format_tokens(parts.free(max) as u64),
            parts.percent(parts.free(max), max)
        ));
    }

    let grid = dot_grid(ratio);
    if width >= SIDE_BY_SIDE_MIN {
        let pad = " ".repeat(RIGHT_COLUMN.saturating_sub(2 + 7));
        for index in 0..grid.len().max(rows.len()) {
            let left = grid.get(index).cloned().unwrap_or_default();
            match rows.get(index) {
                Some(right) => lines.push(format!("  {left}{pad}{right}")),
                None => lines.push(format!("  {left}")),
            }
        }
    } else {
        lines.extend(grid.into_iter().map(|row| format!("  {row}")));
        lines.extend(rows.into_iter().map(|row| format!("  {row}")));
    }

    lines
}

/// One block of spend: what a set of rows cost, and the shared cache read as a bar.
pub(crate) fn totals_report(title: &str, totals: &UsageTotals) -> Vec<String> {
    let mut lines = vec![heading(title)];
    if totals.turns == 0 {
        lines.push(format!("  {}", "no turns recorded yet".dimmed()));
        return lines;
    }
    let mut first = format!(
        "  {} turns · {} · {} tokens (in {}, out {})",
        totals.turns,
        format_duration(totals.duration_ms),
        format_tokens(totals.total_tokens),
        format_tokens(totals.prompt_tokens),
        format_tokens(totals.completion_tokens)
    );
    first.push_str(&format!(" · {}", cost_text(totals)));
    lines.push(first);

    if let Some(cache) = cache_text(totals) {
        let ratio = fill_ratio(totals.cached_tokens, totals.prompt_tokens);
        // The bar is padded by hand: the colored cells carry SGR escapes, so `{:width$}` would
        // count bytes instead of visible cells and misalign the column.
        lines.push(format!("  {}  cache {cache}", dot_bar(ratio)));
    }
    if totals.failed_turns > 0 || totals.denied_calls > 0 {
        let note = format!(
            "{} turn(s) hit a tool error · {} denied call(s)",
            totals.failed_turns, totals.denied_calls
        );
        lines.push(format!("  {}", note.yellow()));
    }
    if totals.unpriced_turns > 0 {
        let note = format!(
            "{} turn(s) used a model with no price; set usage.prices to cost them",
            totals.unpriced_turns
        );
        lines.push(format!("  {}", note.dimmed()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts() -> ContextParts {
        ContextParts {
            system: 6_000,
            user: 1_000,
            assistant: 800,
            reasoning: 200,
            tool_calls: 1_200,
            tool_results: 300,
        }
    }

    fn totals() -> UsageTotals {
        UsageTotals {
            turns: 3,
            prompt_tokens: 1_000,
            completion_tokens: 100,
            total_tokens: 1_100,
            cached_tokens: 800,
            cost_usd: 0.0123,
            unpriced_turns: 0,
            duration_ms: 2_500,
            failed_turns: 0,
            denied_calls: 0,
        }
    }

    /// Every part is its own row, so a reader can see what filled the window rather than only how
    /// much of it is gone.
    #[test]
    fn each_part_of_the_context_gets_a_row() {
        let report =
            context_report("p/m", Some(&parts()), 100_000, "at 80% of the window", 100).join("\n");
        for label in [
            "System prompt",
            "User messages",
            "Agent responses",
            "Reasoning",
            "Tool calls",
        ] {
            assert!(report.contains(label), "missing {label}: {report}");
        }
        assert!(
            report.contains("Free space"),
            "free space is shown: {report}"
        );
        assert!(report.contains("9.5k/100k tokens"), "headline: {report}");
    }

    /// The free space is the window minus the parts, and the parts sum to the total: a reader who
    /// adds the rows up gets the headline.
    #[test]
    fn the_rows_add_up_to_the_headline() {
        let parts = parts();
        assert_eq!(parts.used(), 6_000 + 1_000 + 800 + 200 + 1_200 + 300);
        assert_eq!(parts.used() + parts.free(100_000), 100_000);
    }

    /// A turn in flight is not a zero: the section says why it cannot measure instead of drawing
    /// an empty grid that reads as "the context is empty".
    #[test]
    fn a_running_turn_says_the_measurement_is_unavailable() {
        let report = context_report("p/m", None, 100_000, "at 80% of the window", 100).join("\n");
        assert!(report.contains("in flight"), "{report}");
        assert!(!report.contains('○'), "no grid: {report}");
    }

    /// A narrow pane stacks the grid above the rows rather than wrapping them into nonsense.
    #[test]
    fn a_narrow_pane_stacks_the_sections() {
        let wide = context_report("p/m", Some(&parts()), 100_000, "at 80%", 100);
        let narrow = context_report("p/m", Some(&parts()), 100_000, "at 80%", 40);
        assert!(wide.len() < narrow.len(), "stacked is taller");
        assert!(narrow.iter().all(|line| !line.contains("\t")));
    }

    /// An unknown window is stated, not divided by.
    #[test]
    fn an_unknown_window_is_reported_as_unknown() {
        let report = context_report("p/m", Some(&parts()), 0, "at 80%", 100).join("\n");
        assert!(report.contains("window unknown"), "{report}");
        // No row may claim a share: "0.0%" of an unknown window would read as "empty".
        assert!(
            !report.contains("tokens ("),
            "no share without a window: {report}"
        );
    }

    /// The cache is a bar and a share of the input, and it disappears when nothing measured it.
    #[test]
    fn the_cache_is_a_bar_when_it_was_measured() {
        let report = totals_report("Session totals", &totals()).join("\n");
        assert!(report.contains("cache"), "{report}");
        assert_eq!(report.matches('●').count(), 8, "800 of 1000: {report}");

        let mut unmeasured = totals();
        unmeasured.cached_tokens = 0;
        let report = totals_report("Session totals", &unmeasured).join("\n");
        assert!(
            !report.contains("cache"),
            "no measurement, no line: {report}"
        );
    }

    /// A session with nothing recorded says so instead of drawing an empty block.
    #[test]
    fn an_empty_ledger_says_so() {
        let mut empty = totals();
        empty.turns = 0;
        let report = totals_report("Session totals", &empty).join("\n");
        assert!(report.contains("no turns recorded yet"), "{report}");
    }
}
