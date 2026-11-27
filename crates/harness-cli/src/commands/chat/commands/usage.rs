//! `/usage`: what this session has spent, and how much of the context it fills.
//!
//! The ledger already knows every turn's tokens, its cached share and its price — `luna usage`
//! reads them from the terminal. The chat pane never showed them, so the cost of a session was
//! only visible by leaving the session. `/usage` reads the same ledger, filtered to the session on
//! screen: the same numbers, in the place the work is happening.
//!
//! The layout is a two-column report: a 10×10 grid of square cells (`■` / `□`, 1% each) on the
//! left, and an aligned list of facts on the right — model, headline, auto-compact setting,
//! category breakdown, and free space.
//!
//! Three things are reported, and they answer three different questions:
//!
//! - **context** — what the next request carries, split by what put it there.
//! - **tokens** — what the conversation cost, in and out of the model.
//! - **cache** — how much of the input the provider served from its own prompt cache. A provider
//!   that says nothing about caching reports nothing here: a line of zeroes would read as
//!   "caching was measured and did not help", when what happened is that nothing was said.
//!
//! The report is emitted as one block into the transcript and can be dismissed with `Esc`.

use std::sync::Arc;

use colored::Colorize;
use harness_core::session::ContextParts;
use harness_core::usage::{UsageFilter, UsageLedger, UsageTotals};
use harness_core::{Config, HarnessError};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::commands::chat::ui::{get_terminal_width, ChatUi};
use crate::commands::chat::ChatAgent;
use crate::commands::context_window::context_window_for;
use crate::commands::usage::{
    cache_text, cost_text, dot_bar, fill_ratio, format_duration, format_tokens,
};

const GRID_COLS: usize = 10;
const GRID_ROWS: usize = 10;
/// Width of one 10-cell grid row (`10 * 2 - 1 = 19` visible cells).
const GRID_WIDTH: usize = GRID_COLS * 2 - 1;
/// Where the right-hand column starts: 2-space indent + 19-cell grid + 3-space gap = 24.
const RIGHT_COLUMN: usize = 24;
/// Below this width the grid and the rows stack instead of sitting side by side.
const SIDE_BY_SIDE_MIN: usize = 50;

const CATEGORIES: [(&str, (u8, u8, u8)); 5] = [
    ("System prompt", (137, 180, 250)),
    ("User messages", (166, 227, 161)),
    ("Agent responses", (116, 199, 236)),
    ("Reasoning", (203, 166, 247)),
    ("Tool calls", (148, 226, 213)),
];

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
        get_terminal_width().saturating_sub(8),
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

fn heading(title: &str) -> String {
    format!("{} {}", "└".dimmed(), title.bold())
}

fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let target = max_width - 1;
    let mut out = String::new();
    let mut w = 0;
    for ch in text.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > target {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// Build the 10×10 square-cell grid: each cell is 1% of the window, colored by category.
fn context_grid(tokens_list: &[u32; 5], max: u32) -> Vec<String> {
    let total_cells = GRID_COLS * GRID_ROWS;
    let mut counts = [0usize; 5];
    if max > 0 {
        for (i, &tokens) in tokens_list.iter().enumerate() {
            if tokens > 0 {
                counts[i] = ((tokens as f64 / max as f64) * 100.0).round().max(1.0) as usize;
            }
        }
        while counts.iter().sum::<usize>() > total_cells {
            if let Some((idx, _)) = counts.iter().enumerate().max_by_key(|(_, &c)| c) {
                if counts[idx] > 0 {
                    counts[idx] -= 1;
                } else {
                    break;
                }
            }
        }
    }

    let mut cells = Vec::with_capacity(total_cells);
    for (idx, &count) in counts.iter().enumerate() {
        let (_, (r, g, b)) = CATEGORIES[idx];
        for _ in 0..count {
            cells.push("■".truecolor(r, g, b).to_string());
        }
    }
    while cells.len() < total_cells {
        cells.push("□".dimmed().to_string());
    }

    cells.chunks(GRID_COLS).map(|row| row.join(" ")).collect()
}

/// One row of the context split: the bullet carries the category colour, the text the number.
fn part_row(parts: &ContextParts, max: u32, label: &str, rgb: (u8, u8, u8), tokens: u32) -> String {
    let bullet = "●".truecolor(rgb.0, rgb.1, rgb.2);
    // Without a published window there is no share to draw, and a `0.0%` row would read as
    // empty — so the row states the tokens and nothing else.
    if max == 0 {
        return format!("{bullet} {label}: {} tokens", format_tokens(tokens as u64));
    }
    let share = parts.percent(tokens, max);
    format!(
        "{bullet} {label}: {} tokens ({share:.1}%)",
        format_tokens(tokens as u64)
    )
}

/// The context section: the filled grid on the left, then what filled it on the right.
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
    let mut lines = vec![heading("Context Usage")];

    let Some(parts) = parts else {
        lines.push(format!(
            "  {}",
            "a turn is in flight — context is measured when it finishes".dimmed()
        ));
        return lines;
    };

    let side_by_side = width >= SIDE_BY_SIDE_MIN;
    let right_budget = if side_by_side {
        width.saturating_sub(RIGHT_COLUMN)
    } else {
        width.saturating_sub(2)
    };

    let used = parts.used();
    let ratio = fill_ratio(used as u64, max as u64);
    let headline = if max == 0 {
        format!(
            "{} tokens · {}",
            format_tokens(used as u64),
            "window unknown".dimmed()
        )
    } else {
        format!(
            "{}/{} tokens ({:.1}%)",
            format_tokens(used as u64),
            format_tokens(max as u64),
            ratio * 100.0
        )
    };

    let tokens_list = [
        parts.system,
        parts.user,
        parts.assistant,
        parts.reasoning,
        parts.tool_calls + parts.tool_results,
    ];

    let mut rows = vec![
        truncate_with_ellipsis(model_label, right_budget)
            .bold()
            .to_string(),
        headline,
        format!("Auto-compact {compact_note}"),
        "Estimated usage (≈4 chars per token)".dimmed().to_string(),
    ];
    for (idx, (label, rgb)) in CATEGORIES.iter().enumerate() {
        rows.push(part_row(parts, max, label, *rgb, tokens_list[idx]));
    }
    if max > 0 {
        rows.push(format!(
            "{} Free space: {} tokens ({:.1}%)",
            "□".dimmed(),
            format_tokens(parts.free(max) as u64),
            parts.percent(parts.free(max), max)
        ));
    }

    let grid = context_grid(&tokens_list, max);
    if side_by_side {
        let pad_left = " ".repeat(GRID_WIDTH);
        let gap = " ".repeat(RIGHT_COLUMN - 2 - GRID_WIDTH);
        for index in 0..grid.len().max(rows.len()) {
            let left = grid.get(index).map(String::as_str).unwrap_or(&pad_left);
            match rows.get(index) {
                Some(right) => lines.push(format!("  {left}{gap}{right}")),
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
        assert!(!report.contains('□'), "no grid: {report}");
        assert!(!report.contains('■'), "no grid: {report}");
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

    /// The grid always renders 100 square cells (1% each) alongside the breakdown.
    #[test]
    fn grid_has_100_cells_with_expected_fill() {
        let report =
            context_report("p/m", Some(&parts()), 100_000, "at 80% of the window", 100).join("\n");
        // 6000->6, 1000->1, 800->1, 200->max(1,0)=1, 1500->2 => 11 filled cells.
        let filled = report.matches('■').count();
        // 89 empty cells in the grid + 1 '□' bullet on the Free space row = 90.
        let empty = report.matches('□').count();
        assert_eq!(filled, 11, "filled cells: {report}");
        assert_eq!(empty, 90, "empty cells + free bullet: {report}");
    }

    /// Long model names are truncated so no line in the wide report exceeds the width budget.
    #[test]
    fn wide_report_respects_width_budget_with_long_model_name() {
        let long_model = "provider/".to_string() + &"m".repeat(61);
        let lines = context_report(
            &long_model,
            Some(&parts()),
            100_000,
            "at 80% of the window",
            80,
        );
        for line in &lines {
            let plain = dialoguer::console::strip_ansi_codes(line);
            assert!(
                plain.width() <= 80,
                "line exceeded 80 cols ({}): {plain:?}",
                plain.width()
            );
        }
    }
}
