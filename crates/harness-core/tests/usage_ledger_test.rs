//! The usage ledger: pricing, storage, aggregation, and the rule that a turn's tokens are counted
//! once and a model without a price is reported as unknown rather than free.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use tokio_util::sync::CancellationToken;

use harness_core::usage::{
    GroupBy, ModelPrice, PriceTable, UsageFilter, UsageLedger, UsageRecord, SOURCE_CHAT,
    SOURCE_EXEC,
};
use harness_core::*;

fn price(input: f64, output: f64) -> ModelPrice {
    ModelPrice::new(input, output)
}

fn usage(prompt: u32, completion: u32) -> TokenUsage {
    TokenUsage::estimated(prompt, completion)
}

fn at(offset: Duration) -> chrono::DateTime<Utc> {
    Utc::now() - offset
}

fn record(
    provider: &str,
    model: &str,
    tokens: u32,
    cost: Option<f64>,
    source: &str,
) -> UsageRecord {
    UsageRecord {
        ts: Utc::now(),
        session_id: Some("s1".to_string()),
        provider: provider.to_string(),
        model: model.to_string(),
        prompt_tokens: tokens / 2,
        completion_tokens: tokens - tokens / 2,
        total_tokens: tokens,
        cached_tokens: 0,
        cost_usd: cost,
        duration_ms: 1000,
        tool_calls: 0,
        denied_calls: 0,
        ok: true,
        source: source.to_string(),
    }
}

// ---- pricing ------------------------------------------------------------------------------

#[test]
fn the_builtin_table_prices_the_default_models() {
    let table = PriceTable::builtin();
    let cost = table
        .cost("openai", "gpt-4o", &usage(1_000_000, 0))
        .expect("gpt-4o is priced");
    assert!((cost - 2.50).abs() < 1e-9, "got {cost}");

    let cost = table
        .cost("openai", "gpt-4o", &usage(0, 1_000_000))
        .expect("output tokens are priced");
    assert!((cost - 10.00).abs() < 1e-9, "got {cost}");
}

#[test]
fn a_model_with_no_price_costs_none_rather_than_zero() {
    let table = PriceTable::builtin();
    assert_eq!(table.cost("local", "llama-3", &usage(1000, 1000)), None);
}

#[test]
fn an_override_beats_the_builtin_price() {
    let mut overrides = BTreeMap::new();
    overrides.insert("openai/gpt-4o".to_string(), price(1.0, 2.0));
    let table = PriceTable::with_overrides(&overrides);
    let cost = table
        .cost("openai", "gpt-4o", &usage(1_000_000, 1_000_000))
        .unwrap();
    assert!((cost - 3.0).abs() < 1e-9, "got {cost}");
}

#[test]
fn wildcards_cover_a_whole_provider_or_a_model_everywhere() {
    let mut overrides = BTreeMap::new();
    overrides.insert("local/*".to_string(), price(0.0, 0.0));
    overrides.insert("*/gpt-4o-mini".to_string(), price(0.5, 1.5));
    let table = PriceTable::with_overrides(&overrides);

    assert_eq!(
        table.cost("local", "anything", &usage(1_000_000, 0)),
        Some(0.0)
    );
    let cost = table
        .cost("azure", "gpt-4o-mini", &usage(1_000_000, 0))
        .expect("the model wildcard applies");
    assert!((cost - 0.5).abs() < 1e-9, "got {cost}");
    // The exact provider key still wins over the model wildcard.
    assert!(
        (table
            .cost("openai", "gpt-4o-mini", &usage(1_000_000, 0))
            .unwrap()
            - 0.15)
            .abs()
            < 1e-9
    );
}

#[test]
fn a_record_prices_the_turn_at_construction() {
    let table = PriceTable::builtin();
    let record = UsageRecord::for_turn("openai", "gpt-4o", &usage(1000, 500), 42, &table);
    let expected = (1000.0 * 2.50 + 500.0 * 10.00) / 1_000_000.0;
    assert!((record.cost_usd.unwrap() - expected).abs() < 1e-12);
    assert_eq!(record.total_tokens, 1500);
    assert_eq!(record.duration_ms, 42);
    assert!(record.ok);
    assert_eq!(record.source, SOURCE_CHAT);
}

// ---- storage ------------------------------------------------------------------------------

#[tokio::test]
async fn totals_add_up_and_unpriced_rows_are_counted_separately() {
    let ledger = UsageLedger::open(None).expect("in-memory ledger");
    ledger
        .record(&record("openai", "gpt-4o", 100, Some(0.5), SOURCE_CHAT))
        .await
        .unwrap();
    ledger
        .record(&record("openai", "gpt-4o", 200, Some(1.5), SOURCE_EXEC))
        .await
        .unwrap();
    ledger
        .record(&record("local", "llama-3", 300, None, SOURCE_CHAT))
        .await
        .unwrap();

    let totals = ledger.totals(&UsageFilter::default()).await.unwrap();
    assert_eq!(totals.turns, 3);
    assert_eq!(totals.total_tokens, 600);
    assert!((totals.cost_usd - 2.0).abs() < 1e-9, "priced rows only");
    assert_eq!(
        totals.unpriced_turns, 1,
        "the unpriced row is not counted as free"
    );
}

#[tokio::test]
async fn filters_narrow_by_source_and_session() {
    let ledger = UsageLedger::open(None).expect("in-memory ledger");
    ledger
        .record(&record("openai", "gpt-4o", 100, Some(0.5), SOURCE_CHAT))
        .await
        .unwrap();
    ledger
        .record(&record("openai", "gpt-4o", 200, Some(1.5), SOURCE_EXEC))
        .await
        .unwrap();

    let exec_only = ledger
        .totals(&UsageFilter {
            source: Some(SOURCE_EXEC.to_string()),
            ..UsageFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(exec_only.turns, 1);
    assert_eq!(exec_only.total_tokens, 200);

    let other_session = ledger
        .totals(&UsageFilter {
            session_id: Some("nope".to_string()),
            ..UsageFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(other_session.turns, 0);
}

#[tokio::test]
async fn a_since_filter_excludes_older_turns() {
    let ledger = UsageLedger::open(None).expect("in-memory ledger");
    let mut old = record("openai", "gpt-4o", 100, Some(0.5), SOURCE_CHAT);
    old.ts = at(Duration::days(30));
    ledger.record(&old).await.unwrap();
    ledger
        .record(&record("openai", "gpt-4o", 200, Some(1.5), SOURCE_CHAT))
        .await
        .unwrap();

    let recent = ledger
        .totals(&UsageFilter {
            since: Some(at(Duration::days(1))),
            ..UsageFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(recent.turns, 1);
    assert_eq!(recent.total_tokens, 200);
}

#[tokio::test]
async fn the_summary_groups_largest_spend_first() {
    let ledger = UsageLedger::open(None).expect("in-memory ledger");
    ledger
        .record(&record("openai", "gpt-4o", 100, Some(0.1), SOURCE_CHAT))
        .await
        .unwrap();
    ledger
        .record(&record("openai", "gpt-4o", 100, Some(0.1), SOURCE_CHAT))
        .await
        .unwrap();
    ledger
        .record(&record(
            "anthropic",
            "claude-opus-4",
            900,
            Some(9.0),
            SOURCE_CHAT,
        ))
        .await
        .unwrap();

    let buckets = ledger
        .summarize(GroupBy::Model, &UsageFilter::default())
        .await
        .unwrap();
    assert_eq!(buckets.len(), 2);
    assert_eq!(buckets[0].key, "anthropic/claude-opus-4");
    assert_eq!(buckets[1].key, "openai/gpt-4o");
    assert_eq!(buckets[1].totals.turns, 2);
    assert_eq!(buckets[1].totals.total_tokens, 200);
}

#[tokio::test]
async fn grouping_by_day_and_source_works() {
    let ledger = UsageLedger::open(None).expect("in-memory ledger");
    ledger
        .record(&record("openai", "gpt-4o", 100, Some(0.5), SOURCE_CHAT))
        .await
        .unwrap();
    ledger
        .record(&record("openai", "gpt-4o", 100, Some(0.5), SOURCE_EXEC))
        .await
        .unwrap();

    let by_source = ledger
        .summarize(GroupBy::Source, &UsageFilter::default())
        .await
        .unwrap();
    assert_eq!(by_source.len(), 2);

    let by_day = ledger
        .summarize(GroupBy::Day, &UsageFilter::default())
        .await
        .unwrap();
    assert_eq!(by_day.len(), 1, "both rows are today");
    assert_eq!(by_day[0].key.len(), 10, "a day key is YYYY-MM-DD");
}

#[tokio::test]
async fn recent_returns_newest_first_with_the_limit_applied() {
    let ledger = UsageLedger::open(None).expect("in-memory ledger");
    for tokens in [100, 200, 300] {
        ledger
            .record(&record("openai", "gpt-4o", tokens, Some(0.1), SOURCE_CHAT))
            .await
            .unwrap();
    }
    let rows = ledger.recent(&UsageFilter::default(), 2).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].total_tokens, 300, "newest first");
}

#[tokio::test]
async fn a_ledger_reopens_with_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.db");
    {
        let ledger = UsageLedger::open(Some(path.clone())).unwrap();
        ledger
            .record(&record("openai", "gpt-4o", 100, Some(0.5), SOURCE_CHAT))
            .await
            .unwrap();
    }
    let ledger = UsageLedger::open(Some(path)).unwrap();
    let totals = ledger.totals(&UsageFilter::default()).await.unwrap();
    assert_eq!(totals.turns, 1, "the file is the ledger, not the handle");
}

#[test]
fn since_accepts_durations_and_timestamps() {
    let filter = UsageFilter::since_text("7d").expect("7d parses");
    let since = filter.since.expect("a timestamp");
    let age = Utc::now() - since;
    assert!(
        age >= Duration::days(7) - Duration::minutes(1)
            && age <= Duration::days(7) + Duration::minutes(1)
    );

    let instant = UsageFilter::parse_since("2024-01-01T00:00:00Z").expect("RFC 3339 parses");
    assert_eq!(instant.to_rfc3339(), "2024-01-01T00:00:00+00:00");

    assert!(UsageFilter::since_text("7x").is_err());
    assert!(UsageFilter::since_text("week").is_err());
}

// ---- the agent's contribution -------------------------------------------------------------

/// Two model calls in one turn: the ledger must see one row with both calls' tokens.
struct TwoCallProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for TwoCallProvider {
    fn provider_id(&self) -> &str {
        "mock"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["mock-1".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let mut message = if call == 0 {
            Message::assistant_with_tools(vec![ToolCall {
                id: "call_1".to_string(),
                name: "noop".to_string(),
                arguments: serde_json::json!({}),
                signature: None,
            }])
        } else {
            Message::assistant("done")
        };
        // 10 in / 5 out per call, reported the way a real provider does.
        message.usage = Some(usage(10, 5));
        Ok(message)
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

struct NoopTool {
    runs: AtomicUsize,
}

#[async_trait]
impl Tool for NoopTool {
    fn name(&self) -> &str {
        "noop"
    }
    fn description(&self) -> &str {
        "does nothing"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn is_mutating(&self) -> bool {
        false
    }
    async fn execute(
        &self,
        call_id: &str,
        _arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::success(call_id, "ok"))
    }
}

struct NoMemory;
#[async_trait]
impl MemoryEngine for NoMemory {
    async fn retrieve(&self, _q: &str, _l: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
        Ok(vec![])
    }
    async fn save(&self, _r: &MemoryRecord) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn update(&self, _i: &str, _r: &MemoryRecord) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn forget(&self, _i: &str) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn add_note_pointer(&self, _i: &str, _p: &str) -> Result<(), HarnessError> {
        Ok(())
    }
    fn format_as_system_prompt(&self, _m: &[MemoryRecord]) -> String {
        String::new()
    }
}

struct NoNotes;
#[async_trait]
impl NotesEngine for NoNotes {
    async fn save_note(&self, _t: &str, _c: &str, _tags: &[String]) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn search_notes(&self, _q: &str, _l: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn list_notes(&self, _l: usize, _o: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn read_note(&self, _i: &str) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn update_note(
        &self,
        _i: &str,
        _t: Option<&str>,
        _c: Option<&str>,
        _tags: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn delete_note(&self, _i: &str) -> Result<bool, HarnessError> {
        unreachable!("not used by this test")
    }
}

#[tokio::test]
async fn a_turns_usage_sums_every_model_call_and_lands_in_the_ledger() {
    let registry = Arc::new(DefaultToolRegistry::new());
    let tool = Arc::new(NoopTool {
        runs: AtomicUsize::new(0),
    });
    registry.register(tool.clone()).await.unwrap();

    let agent = Agent::new(
        Arc::new(TwoCallProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(NoMemory),
        Arc::new(NoNotes),
        registry,
        AgentConfig::default(),
    )
    .with_approval(
        Arc::new(AllowAll),
        harness_core::ApprovalSettings::default(),
    );

    let outcome = agent
        .run_turn("do it", CancellationToken::new())
        .await
        .expect("turn completes");

    let usage = outcome.usage.clone().expect("usage is reported");
    assert_eq!(usage.prompt_tokens, 20, "two calls at 10 in each");
    assert_eq!(usage.completion_tokens, 10, "two calls at 5 out each");
    assert_eq!(usage.total_tokens, 30);
    assert_eq!(outcome.tool_calls.len(), 1);
    let _ = outcome.duration_ms; // wall-clock is whatever the run took; it must simply exist

    let ledger = UsageLedger::open(None).unwrap();
    let prices = PriceTable::builtin();
    ledger
        .record_turn(
            "openai",
            "gpt-4o",
            Some("s1"),
            &outcome,
            SOURCE_EXEC,
            &prices,
        )
        .await
        .unwrap();

    let totals = ledger.totals(&UsageFilter::default()).await.unwrap();
    assert_eq!(totals.turns, 1, "one turn is one row, not one row per call");
    assert_eq!(totals.total_tokens, 30);
    assert_eq!(totals.unpriced_turns, 0);

    let rows = ledger.recent(&UsageFilter::default(), 1).await.unwrap();
    assert_eq!(rows[0].tool_calls, 1);
    assert_eq!(rows[0].session_id.as_deref(), Some("s1"));
    assert_eq!(rows[0].source, SOURCE_EXEC);
    assert!(rows[0].ok);
}

#[tokio::test]
async fn a_turn_the_provider_did_not_measure_writes_no_row() {
    // No usage reported → nothing to record. A zero row would claim the turn was free.
    let ledger = UsageLedger::open(None).unwrap();
    let outcome = TurnOutcome {
        reply: "hi".to_string(),
        reasoning: None,
        tool_calls: vec![],
        replayed: None,
        usage: None,
        duration_ms: 5,
    };
    ledger
        .record_turn(
            "openai",
            "gpt-4o",
            None,
            &outcome,
            SOURCE_CHAT,
            &PriceTable::builtin(),
        )
        .await
        .unwrap();
    assert_eq!(
        ledger.totals(&UsageFilter::default()).await.unwrap().turns,
        0
    );
}

/// A denied call is a decision, not a malfunction: it is reported in `denied_calls`, and the turn
/// is not counted among "turns that hit a tool error". Counting it there is what made the summary
/// read as a harness failure when the user had simply refused a prompt.
#[tokio::test]
async fn a_denied_call_does_not_make_the_turn_a_tool_error() {
    let ledger = UsageLedger::open(None).unwrap();
    let outcome = TurnOutcome {
        reply: "ok".to_string(),
        reasoning: None,
        tool_calls: vec![TurnToolCall {
            name: "terminal".to_string(),
            arguments: serde_json::json!({ "command": "rm -rf build" }),
            output: "approval denied: denied at the prompt".to_string(),
            is_error: true,
            denied: true,
        }],
        replayed: None,
        usage: Some(TokenUsage::estimated(10, 5)),
        duration_ms: 10,
    };
    ledger
        .record_turn(
            "openai",
            "gpt-4o",
            Some("s1"),
            &outcome,
            SOURCE_CHAT,
            &PriceTable::builtin(),
        )
        .await
        .unwrap();

    let totals = ledger.totals(&UsageFilter::default()).await.unwrap();
    assert_eq!(totals.denied_calls, 1);
    assert_eq!(
        totals.failed_turns, 0,
        "a refusal is not a failed tool call"
    );

    let rows = ledger.recent(&UsageFilter::default(), 1).await.unwrap();
    assert!(rows[0].ok, "the turn itself was clean");
}

#[tokio::test]
async fn a_failed_tool_call_still_marks_the_turn() {
    let ledger = UsageLedger::open(None).unwrap();
    let outcome = TurnOutcome {
        reply: "ok".to_string(),
        reasoning: None,
        tool_calls: vec![TurnToolCall {
            name: "read_file".to_string(),
            arguments: serde_json::json!({ "path": "missing.rs" }),
            output: "File not found: 'missing.rs'".to_string(),
            is_error: true,
            denied: false,
        }],
        replayed: None,
        usage: Some(TokenUsage::estimated(10, 5)),
        duration_ms: 10,
    };
    ledger
        .record_turn(
            "openai",
            "gpt-4o",
            Some("s1"),
            &outcome,
            SOURCE_CHAT,
            &PriceTable::builtin(),
        )
        .await
        .unwrap();

    let totals = ledger.totals(&UsageFilter::default()).await.unwrap();
    assert_eq!(totals.failed_turns, 1);
    assert_eq!(totals.denied_calls, 0);
}
