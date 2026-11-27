//! `luna usage`: the turn ledger, recorded and reported.
//!
//! Recording is a by-product of every turn (see `harness_core::usage`); this module is the two ends
//! of it — the recorder a chat/exec/voice call site uses, and the command that reads it back.
//!
//! The report never hides unknown cost: a model without a price is counted in tokens and reported
//! as "unpriced", because a dollar figure that includes a guess is worse than no figure.

use std::sync::Arc;

use colored::Colorize;
use harness_core::usage::{
    GroupBy, PriceTable, UsageBucket, UsageFilter, UsageLedger, UsageRecord, UsageTotals,
    SOURCE_CHAT,
};
use harness_core::HarnessError;
use harness_providers::{CredentialManager, ModelDiscovery};

/// What a finished turn contributes, resolved once from the config.
pub struct UsageRecorder {
    ledger: Option<Arc<UsageLedger>>,
    prices: PriceTable,
}

impl UsageRecorder {
    pub fn from_config(config: &harness_core::Config) -> Self {
        let ledger = if config.usage.enabled {
            match UsageLedger::open(Some(config.paths.usage_db.clone())) {
                Ok(ledger) => Some(Arc::new(ledger)),
                // A ledger that cannot be opened must not take the session down with it: the turn
                // is still useful, and the accounting problem is reported once, on stderr.
                Err(error) => {
                    eprintln!("warning: usage recording is off ({error})");
                    None
                }
            }
        } else {
            None
        };
        Self {
            ledger,
            prices: config.usage.price_table(),
        }
    }

    /// Record one turn. Silent by design: accounting never interrupts a conversation, and a turn
    /// whose usage the provider did not report writes nothing rather than a zero.
    pub async fn record(
        &self,
        provider: &str,
        model: &str,
        session_id: Option<&str>,
        outcome: &harness_core::TurnOutcome,
        source: &str,
    ) {
        let Some(ledger) = self.ledger.as_ref() else {
            return;
        };
        if let Err(error) = ledger
            .record_turn(provider, model, session_id, outcome, source, &self.prices)
            .await
        {
            eprintln!("warning: could not record usage ({error})");
        }
    }

    /// The chat's default source tag, so call sites read as one call.
    pub async fn record_chat(
        &self,
        provider: &str,
        model: &str,
        session_id: Option<&str>,
        outcome: &harness_core::TurnOutcome,
    ) {
        self.record(provider, model, session_id, outcome, SOURCE_CHAT)
            .await;
    }
}

/// `luna usage` options.
pub struct UsageOptions {
    pub since: Option<String>,
    pub by: Option<String>,
    pub json: bool,
    pub limit: usize,
    pub recent: bool,
}

pub async fn run_usage(
    config: harness_core::Config,
    _cred_manager: Arc<CredentialManager>,
    _discovery: Arc<ModelDiscovery>,
    options: UsageOptions,
) -> Result<(), harness_core::HarnessError> {
    let ledger = UsageLedger::open(Some(config.paths.usage_db.clone()))?;
    let filter = match options.since.as_deref() {
        Some(text) => UsageFilter {
            since: Some(UsageFilter::parse_since(text)?),
            ..UsageFilter::default()
        },
        None => UsageFilter::default(),
    };

    if options.recent {
        // `--recent` prints the turns themselves and `--by` groups them: a run that asks for both
        // used to get the first and no word about the second, so the grouping the user asked for
        // silently did not happen.
        if options.by.is_some() {
            return Err(HarnessError::ConfigurationError(
                "--recent prints individual turns, so it cannot be grouped with --by; drop one of them"
                    .to_string(),
            ));
        }
        let records = ledger.recent(&filter, options.limit).await?;
        if options.json {
            for record in &records {
                println!("{}", serde_json::to_string(record).unwrap_or_default());
            }
        } else {
            print_recent(&records);
        }
        return Ok(());
    }

    if let Some(by) = options.by.as_deref() {
        let group = GroupBy::parse(by)?;
        let buckets = ledger.summarize(group, &filter).await?;
        let totals = ledger.totals(&filter).await?;
        if options.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "grouped_by": by,
                    "totals": totals,
                    "buckets": buckets,
                }))
                .unwrap_or_default()
            );
        } else {
            print_buckets(by, &buckets, &totals);
        }
        return Ok(());
    }

    let totals = ledger.totals(&filter).await?;
    let by_model = ledger.summarize(GroupBy::Model, &filter).await?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "totals": totals,
                "by_model": by_model,
            }))
            .unwrap_or_default()
        );
    } else {
        print_totals(&totals);
        if !by_model.is_empty() {
            println!();
            print_buckets("model", &by_model, &totals);
        }
    }
    Ok(())
}

/// Dots in a full share bar: how many of them are filled *is* the percentage, read before the
/// number. Ten is the widest bar that still fits beside a label in every terminal Luna draws for.
pub(crate) const BAR_DOTS: usize = 10;

/// The live context grid: four columns by twelve rows, the shape the pane draws.
pub(crate) const GRID_COLUMNS: usize = 4;
pub(crate) const GRID_ROWS: usize = 12;

/// `used` over `max`, clamped to `0.0..=1.0`. An unknown window (`max == 0`) reads as empty rather
/// than as a division by zero.
pub(crate) fn fill_ratio(used: u64, max: u64) -> f64 {
    if max == 0 {
        return 0.0;
    }
    (used as f64 / max as f64).clamp(0.0, 1.0)
}

/// How many cells a ratio fills: never zero for a non-zero amount, so "a little" is visible.
fn filled_cells(ratio: f64, total: usize) -> usize {
    if ratio <= 0.0 {
        return 0;
    }
    ((ratio * total as f64).round() as usize).clamp(1, total)
}

/// Colour of a filled cell by how full the thing is — the one signal that survives a screenshot.
fn fill_color(ratio: f64) -> colored::Color {
    if ratio < 0.60 {
        colored::Color::Green
    } else if ratio < 0.85 {
        colored::Color::Yellow
    } else {
        colored::Color::Red
    }
}

/// One filled cell.
pub(crate) fn filled_dot(ratio: f64) -> String {
    use colored::Colorize;
    "●".color(fill_color(ratio)).to_string()
}

/// One empty cell.
pub(crate) fn empty_dot() -> String {
    use colored::Colorize;
    "○".dimmed().to_string()
}

/// A left-to-right bar of [`BAR_DOTS`]: `ratio` of them filled, the rest empty.
pub(crate) fn dot_bar(ratio: f64) -> String {
    let filled = filled_cells(ratio, BAR_DOTS);
    let mut bar = String::new();
    for index in 0..BAR_DOTS {
        if index > 0 {
            bar.push(' ');
        }
        bar.push_str(if index < filled {
            &filled_dot(ratio)
        } else {
            &empty_dot()
        });
    }
    bar
}

/// The live context grid, one string per row, `ratio` of its cells filled in reading order.
pub(crate) fn dot_grid(ratio: f64) -> Vec<String> {
    let total = GRID_COLUMNS * GRID_ROWS;
    let filled = filled_cells(ratio, total);
    (0..GRID_ROWS)
        .map(|row| {
            (0..GRID_COLUMNS)
                .map(|column| {
                    if row * GRID_COLUMNS + column < filled {
                        filled_dot(ratio)
                    } else {
                        empty_dot()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// A horizontal rule, dimmed so it separates without competing with the numbers.
fn rule(width: usize) -> String {
    "─".repeat(width).dimmed().to_string()
}

/// The layout the report is read in: label column, value column, then whatever follows.
const LABEL: usize = 12;
const RULE: usize = 68;

fn print_totals(totals: &UsageTotals) {
    if totals.turns == 0 {
        println!(
            "No turns recorded yet ({}).",
            "`luna chat` and `luna exec` write here".dimmed()
        );
        return;
    }
    println!("{}", "Usage — all sessions".bold());
    println!("{}", rule(RULE));
    println!(
        "  {:<LABEL$}{:<22}{:<8}{}",
        "turns",
        totals.turns,
        "time",
        format_duration(totals.duration_ms)
    );
    println!(
        "  {:<LABEL$}{:<22}{:<8}{}",
        "tokens",
        format_tokens(totals.total_tokens),
        "in · out",
        format!(
            "{} · {}",
            format_tokens(totals.prompt_tokens),
            format_tokens(totals.completion_tokens)
        )
    );
    // The bar is the cached share: the one number here that is a *ratio* rather than a total, and
    // so the one a row of dots communicates better than the percentage does.
    if let Some(cache) = cache_text(totals) {
        let ratio = fill_ratio(totals.cached_tokens, totals.prompt_tokens);
        println!("  {:<LABEL$}{}  {cache}", "cache", dot_bar(ratio));
    }
    println!("  {:<LABEL$}{}", "cost", cost_text(totals));
    if totals.failed_turns > 0 || totals.denied_calls > 0 {
        println!(
            "  {}",
            format!(
                "{} turn(s) hit a tool error · {} denied call(s)",
                totals.failed_turns, totals.denied_calls
            )
            .yellow()
        );
    }
    if totals.unpriced_turns > 0 {
        println!(
            "  {}",
            format!(
                "{} turn(s) used a model with no price; set usage.prices to cost them",
                totals.unpriced_turns
            )
            .dimmed()
        );
    }
}

fn print_buckets(by: &str, buckets: &[UsageBucket], totals: &UsageTotals) {
    println!("{}", format!("By {by}").bold());
    println!("{}", rule(RULE));
    let width = buckets
        .iter()
        .map(|bucket| bucket.key.chars().count())
        .max()
        .unwrap_or(8)
        .clamp(8, 40);
    for bucket in buckets {
        // The bar is this row's share of the tokens the whole report covers, so the rows compare
        // to each other by eye before any number is read.
        let share = fill_ratio(bucket.totals.total_tokens, totals.total_tokens);
        println!(
            "  {:width$}  {:>4} turns  {:>8} tok  {:>9}  {:>7}  {}",
            truncate(&bucket.key, width),
            bucket.totals.turns,
            format_tokens(bucket.totals.total_tokens),
            cost_text(&bucket.totals),
            format_duration(bucket.totals.duration_ms),
            dot_bar(share),
            width = width
        );
    }
}

/// The key cut to the column width, so one long model id cannot push every other column right.
fn truncate(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Tokens the provider served from its prompt cache, and what share of the input that is.
///
/// `None` when no row reported a cache read: a line of zeroes would read as "caching was measured
/// and did not help", when what actually happened is that nothing was said about it.
pub(crate) fn cache_text(totals: &UsageTotals) -> Option<String> {
    if totals.cached_tokens == 0 {
        return None;
    }
    let share = if totals.prompt_tokens == 0 {
        0.0
    } else {
        totals.cached_tokens as f64 * 100.0 / totals.prompt_tokens as f64
    };
    Some(format!(
        "{} tok served from cache ({share:.0}% of input)",
        format_tokens(totals.cached_tokens)
    ))
}

/// A token count in the units a person reads at a glance: `812`, `48.2k`, `1.05M`.
///
/// The count is rounded for the eye, never for the ledger: the numbers behind it are exact, and
/// this is the line somebody reads while a session is still running.
pub(crate) fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    let thousands = tokens as f64 / 1_000.0;
    if thousands < 1_000.0 {
        if thousands.fract() == 0.0 {
            return format!("{thousands:.0}k");
        }
        return format!("{thousands:.1}k");
    }
    format!("{:.2}M", tokens as f64 / 1_000_000.0)
}

fn print_recent(records: &[UsageRecord]) {
    if records.is_empty() {
        println!("No turns recorded yet.");
        return;
    }
    println!("{}", "Recent turns".bold());
    println!("{}", rule(RULE));
    println!(
        "  {}",
        format!(
            "{:<17}{:>8}{:>10}{:>8}  {}",
            "when", "tokens", "cost", "time", "model"
        )
        .dimmed()
    );
    for record in records {
        // Only turns whose provider reported a cache read carry the marker, so the column stays
        // quiet on the providers that say nothing about caching.
        let cached = if record.cached_tokens > 0 {
            format!(" ({} cached)", format_tokens(record.cached_tokens))
        } else {
            String::new()
        };
        println!(
            "  {:<17}{:>8}{:>10}{:>8}  {}",
            record.ts.format("%Y-%m-%d %H:%M"),
            format!("{}{cached}", format_tokens(record.total_tokens)),
            record
                .cost_usd
                .map(|cost| format!("${cost:.4}"))
                .unwrap_or_else(|| "unpriced".to_string()),
            format_duration(record.duration_ms),
            format!("{}/{}", record.provider, record.model).dimmed()
        );
    }
}

/// What the rows cost: a dollar figure, or the truth that the price is not known.
///
/// A model with no price on file is counted in tokens and reported as unpriced, because a dollar
/// figure with a guess inside it is worse than no dollar figure at all.
pub(crate) fn cost_text(totals: &UsageTotals) -> String {
    if totals.turns == 0 {
        return "$0.0000".to_string();
    }
    if totals.unpriced_turns == totals.turns {
        return "unpriced".to_string();
    }
    let mut text = format!("${:.4}", totals.cost_usd);
    if totals.unpriced_turns > 0 {
        text.push_str(&format!(" (+{} unpriced)", totals.unpriced_turns));
    }
    text
}

pub(crate) fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bar is a percentage you can count: exactly [`BAR_DOTS`] cells, `ratio` of them filled.
    /// Colours are counted as glyphs, so this holds whatever the terminal does with SGR.
    #[test]
    fn a_bar_fills_the_share_it_is_given() {
        let cells = |bar: &str| -> (usize, usize) {
            (bar.matches('●').count(), bar.matches('○').count())
        };
        assert_eq!(cells(&dot_bar(0.0)), (0, BAR_DOTS));
        assert_eq!(cells(&dot_bar(1.0)), (BAR_DOTS, 0));
        assert_eq!(cells(&dot_bar(0.5)), (5, 5));
        // A sliver is still visible: a session that has spent anything shows a filled cell.
        assert_eq!(cells(&dot_bar(0.01)).0, 1);
    }

    /// An unknown window is not a division by zero, and does not claim space is free.
    #[test]
    fn an_unknown_window_fills_nothing() {
        assert_eq!(fill_ratio(1_000, 0), 0.0);
        assert_eq!(dot_bar(fill_ratio(1_000, 0)).matches('●').count(), 0);
    }

    /// The grid is the bar's shape, not a second layout: every cell is drawn once.
    #[test]
    fn the_grid_draws_every_cell_once() {
        let rows = dot_grid(0.5);
        assert_eq!(rows.len(), GRID_ROWS);
        let filled: usize = rows.iter().map(|row| row.matches('●').count()).sum();
        let empty: usize = rows.iter().map(|row| row.matches('○').count()).sum();
        assert_eq!(filled + empty, GRID_COLUMNS * GRID_ROWS);
        assert_eq!(filled, GRID_COLUMNS * GRID_ROWS / 2);
    }

    /// A long model key is cut, not allowed to shift every column after it.
    #[test]
    fn a_long_bucket_key_is_truncated() {
        assert_eq!(truncate("short", 10), "short");
        let cut = truncate("a-very-long-model-id", 8);
        assert_eq!(cut.chars().count(), 8);
        assert!(cut.ends_with('…'));
    }
}
