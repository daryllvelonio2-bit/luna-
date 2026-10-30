//! `/usage`: what this session has spent, and how much of the context it fills.
//!
//! The ledger already knows every turn's tokens, its cached share and its price — `luna usage`
//! reads them from the terminal. The chat pane never showed them, so the cost of a session was
//! only visible by leaving the session. `/usage` reads the same ledger, filtered to the session on
//! screen: the same numbers, in the place the work is happening.
//!
//! Three things are reported, and they answer three different questions:
//!
//! - **tokens** — what the conversation cost, in and out of the model.
//! - **cache** — how much of the input the provider served from its own prompt cache, and what
//!   share that was. A provider that says nothing about caching reports nothing here: a line of
//!   zeroes would read as "caching was measured and did not help", when what happened is that
//!   nothing was said about it.
//! - **context** — how much of the model's window the live transcript fills, which is a different
//!   number from the tokens spent: the transcript is still there after the tokens are paid for.

use std::sync::Arc;

use colored::Colorize;
use harness_core::usage::{UsageFilter, UsageLedger, UsageTotals};
use harness_core::{Config, HarnessError};

use crate::commands::chat::ui::ChatUi;
use crate::commands::chat::ChatAgent;
use crate::commands::context_window::context_window_for;
use crate::commands::usage::{cache_text, cost_text, format_duration, format_tokens};

/// `/usage`: the session's usage, and the ledger's, in the chat pane.
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

    // The context gauge is the agent's own estimate of the live transcript. A turn in flight holds
    // the agent lock for its whole run, so the number is taken without waiting for it: a usage
    // report must never make a running turn wait.
    let context = match agent.try_lock() {
        Ok(agent) => {
            let used = agent.estimate_session_tokens().await.unwrap_or(0);
            let max = context_window_for(config, current_provider, current_model);
            Some((used, max))
        }
        Err(_) => None,
    };

    // The report is one block, emitted once: the composer is repainted after every `emit_above`,
    // and a report that repainted it eight times would arrive as eight separate notes.
    let mut report: Vec<String> = vec![format!(
        "{} {}",
        "Usage".bold(),
        format!("— session {}", session_id.dimmed())
    )];
    if session_totals.turns == 0 {
        report.push(format!(
            "  {}",
            "no turns recorded for this session yet".dimmed()
        ));
    } else {
        for line in totals_lines(&session_totals) {
            report.push(format!("  {}", line));
        }
    }
    match context {
        Some((used, max)) => report.push(format!("  {}", context_line(used, max))),
        None => report.push(format!(
            "  {}",
            "context          n/a (a turn is in flight)".dimmed()
        )),
    }

    // The whole ledger, so a session that has just started still shows what the machine has
    // spent. Skipped when this session *is* the ledger: a second copy of the same numbers would
    // read as more work than was done.
    if all_totals.turns > session_totals.turns {
        report.push(String::new());
        report.push(format!("{} {}", "All sessions".bold(), "so far".dimmed()));
        for line in totals_lines(&all_totals) {
            report.push(format!("  {}", line));
        }
    }

    ui.emit_above(&report.join("\n"));

    Ok(())
}

/// The block of numbers a set of rows is reported as.
///
/// Pure, so the report can be checked without a terminal: what the ledger holds and what the pane
/// shows are the same lines.
fn totals_lines(totals: &UsageTotals) -> Vec<String> {
    let mut lines = vec![
        format!("turns            {}", totals.turns),
        format!(
            "tokens           {} ({} in, {} out)",
            format_tokens(totals.total_tokens),
            format_tokens(totals.prompt_tokens),
            format_tokens(totals.completion_tokens)
        ),
    ];
    if let Some(cache) = cache_text(totals) {
        lines.push(format!("cache            {cache}"));
    }
    lines.push(format!("cost             {}", cost_text(totals)));
    lines.push(format!(
        "time in turns    {}",
        format_duration(totals.duration_ms)
    ));
    if totals.failed_turns > 0 || totals.denied_calls > 0 {
        lines.push(format!(
            "trouble          {} failed turn(s), {} denied call(s)",
            totals.failed_turns, totals.denied_calls
        ));
    }
    if totals.unpriced_turns > 0 {
        lines.push(format!(
            "note             {} turn(s) used a model with no price; set usage.prices to cost them",
            totals.unpriced_turns
        ));
    }
    lines
}

/// The live context: how much of the model's window the transcript fills.
fn context_line(used: u32, max: u32) -> String {
    if max == 0 {
        return format!("context          {} (window unknown)", format_tokens(used as u64));
    }
    let share = used as f64 * 100.0 / max as f64;
    format!(
        "context          {} / {} tokens ({share:.1}%)",
        format_tokens(used as u64),
        format_tokens(max as u64)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The report is the ledger, read back: turns, tokens, cache, cost and time, in that order.
    #[test]
    fn the_report_carries_the_numbers_the_ledger_holds() {
        let lines = totals_lines(&totals());
        assert!(lines[0].starts_with("turns            3"));
        assert!(lines[1].starts_with("tokens           1.1k (1k in, 100 out)"));
        assert!(lines[2].starts_with("cache            800 tok served from cache (80% of input)"));
        assert!(lines[3].starts_with("cost             $0.0123"));
        assert!(lines[4].starts_with("time in turns    2.5s"));
    }

    /// A provider that said nothing about caching gets no cache line: a zero would read as
    /// "caching was measured and did not help".
    #[test]
    fn an_unmeasured_cache_has_no_line() {
        let mut totals = totals();
        totals.cached_tokens = 0;
        let lines = totals_lines(&totals);
        assert!(
            !lines.iter().any(|line| line.starts_with("cache")),
            "a zero cache is not a measurement: {lines:?}"
        );
    }

    /// An unknown price stays unknown. The tokens are still counted; the dollars are not invented.
    #[test]
    fn an_unpriced_model_says_unpriced() {
        let mut totals = totals();
        totals.cost_usd = 0.0;
        totals.unpriced_turns = totals.turns;
        let lines = totals_lines(&totals);
        assert!(
            lines.iter().any(|line| line.contains("cost             unpriced")),
            "lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("no price")),
            "the note says what to do about it: {lines:?}"
        );
    }

    /// The context is a share of the window, not of the tokens spent.
    #[test]
    fn the_context_is_read_against_the_window() {
        let line = context_line(41_000, 128_000);
        assert!(line.contains("41k / 128k tokens"), "line: {line}");
        assert!(line.contains("32.0%"), "line: {line}");
    }

    /// A window nobody published is not a division by zero.
    #[test]
    fn an_unknown_window_is_reported_as_unknown() {
        let line = context_line(1_000, 0);
        assert!(line.contains("window unknown"), "line: {line}");
    }
}
