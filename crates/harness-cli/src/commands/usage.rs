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

fn print_totals(totals: &UsageTotals) {
    if totals.turns == 0 {
        println!(
            "No turns recorded yet ({}).",
            "luna chat` and `luna exec` write here".dimmed()
        );
        return;
    }
    println!("{}", "Usage".bold());
    println!("  turns            {}", totals.turns);
    println!(
        "  tokens           {} ({} in, {} out)",
        totals.total_tokens, totals.prompt_tokens, totals.completion_tokens
    );
    if let Some(cache) = cache_text(totals) {
        println!("  cache            {cache}");
    }
    println!("  cost             {}", cost_text(totals));
    println!("  time in turns    {}", format_duration(totals.duration_ms));
    if totals.failed_turns > 0 || totals.denied_calls > 0 {
        println!(
            "  trouble          {} turn(s) hit a tool error, {} denied call(s)",
            totals.failed_turns, totals.denied_calls
        );
    }
    if totals.unpriced_turns > 0 {
        println!(
            "{}",
            format!(
                "  note             {} turn(s) used a model with no price; set usage.prices to cost them",
                totals.unpriced_turns
            )
            .dimmed()
        );
    }
}

fn print_buckets(by: &str, buckets: &[UsageBucket], totals: &UsageTotals) {
    println!("{}", format!("By {by}").bold());
    let width = buckets
        .iter()
        .map(|bucket| bucket.key.chars().count())
        .max()
        .unwrap_or(8)
        .clamp(8, 40);
    for bucket in buckets {
        println!(
            "  {:width$}  {:>4} turns  {:>12} tok  {:>10}  {:>9}",
            bucket.key,
            bucket.totals.turns,
            bucket.totals.total_tokens,
            cost_text(&bucket.totals),
            format_duration(bucket.totals.duration_ms),
            width = width
        );
    }
    if buckets.len() > 1 {
        println!(
            "  {:width$}  {:>4} turns  {:>12} tok  {:>10}",
            "total",
            totals.turns,
            totals.total_tokens,
            cost_text(totals),
            width = width
        );
    }
    if let Some(cache) = cache_text(totals) {
        println!("  {}", cache.dimmed());
    }
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
    for record in records {
        // Only turns whose provider reported a cache read carry the marker, so the column stays
        // quiet on the providers that say nothing about caching.
        let cached = if record.cached_tokens > 0 {
            format!(" ({} cached)", record.cached_tokens)
        } else {
            String::new()
        };
        println!(
            "{}  {:>6} tok{}  {:>9}  {:>7}  {}",
            record.ts.format("%Y-%m-%d %H:%M:%S"),
            record.total_tokens,
            cached,
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
