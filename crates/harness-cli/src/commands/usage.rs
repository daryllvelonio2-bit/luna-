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
        let records = ledger.recent(&filter, options.limit).await?;
        if options.json {
            for record in &records {
                println!(
                    "{}",
                    serde_json::to_string(record).expect("record serializes")
                );
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
                .expect("summary serializes")
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
            .expect("summary serializes")
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
    println!("  cost             {}", cost_text(totals));
    println!("  time in turns    {}", format_duration(totals.duration_ms));
    if totals.failed_turns > 0 || totals.denied_calls > 0 {
        println!(
            "  trouble          {} failed turn(s), {} denied call(s)",
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
}

fn print_recent(records: &[UsageRecord]) {
    if records.is_empty() {
        println!("No turns recorded yet.");
        return;
    }
    for record in records {
        println!(
            "{}  {:>6} tok  {:>9}  {:>7}  {}",
            record.ts.format("%Y-%m-%d %H:%M:%S"),
            record.total_tokens,
            record
                .cost_usd
                .map(|cost| format!("${cost:.4}"))
                .unwrap_or_else(|| "unpriced".to_string()),
            format_duration(record.duration_ms),
            format!("{}/{}", record.provider, record.model).dimmed()
        );
    }
}

fn cost_text(totals: &UsageTotals) -> String {
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

fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}
