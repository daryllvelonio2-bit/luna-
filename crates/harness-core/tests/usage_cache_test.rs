//! Cached-token accounting: what the ledger stores, what it prices, and what it refuses to invent.
//!
//! A cache read is a discount, and a discount nobody measured is worse than no discount: it makes
//! a spending report read low. So these tests pin three things — a reported cache read is stored
//! and totalled, a configured cache rate lowers the cost of exactly the cached tokens, and with no
//! rate on file a cached token costs the full input price instead of a guessed fraction of it.

use std::collections::BTreeMap;

use chrono::Utc;

use harness_core::usage::{ModelPrice, PriceTable, UsageFilter, UsageLedger, UsageRecord};
use harness_core::TokenUsage;

fn record(usage: &TokenUsage, cost: Option<f64>) -> UsageRecord {
    UsageRecord {
        ts: Utc::now(),
        session_id: Some("s1".to_string()),
        provider: "openai".to_string(),
        model: "gpt-4o".to_string(),
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        cached_tokens: usage.cached_tokens,
        cost_usd: cost,
        duration_ms: 10,
        tool_calls: 0,
        denied_calls: 0,
        ok: true,
        source: "chat".to_string(),
    }
}

#[tokio::test]
async fn a_reported_cache_read_is_stored_and_totalled() {
    let ledger = UsageLedger::open(None).expect("in-memory ledger");
    let usage = TokenUsage {
        prompt_tokens: 1_000,
        completion_tokens: 100,
        total_tokens: 1_100,
        cached_tokens: 800,
    };
    ledger
        .record(&record(&usage, Some(0.01)))
        .await
        .expect("record");

    let totals = ledger
        .totals(&UsageFilter::default())
        .await
        .expect("totals");
    assert_eq!(totals.prompt_tokens, 1_000);
    assert_eq!(totals.cached_tokens, 800);

    let recent = ledger
        .recent(&UsageFilter::default(), 10)
        .await
        .expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(
        recent[0].cached_tokens, 800,
        "the per-turn row keeps the cache count, not just the sum"
    );
}

/// A route that lists the cache read beside the prompt (OpenCode, Anthropic) must be folded into
/// the subset shape before pricing. The old code subtracted a cache count that had never been
/// added, so a 7,680-token cache hit over a 56-token remainder was priced as 56 input tokens.
#[test]
fn a_cache_read_reported_beside_the_prompt_is_folded_in_before_pricing() {
    let usage = TokenUsage::with_separate_cache(56, 21, 77, 7_680);
    assert_eq!(usage.prompt_tokens, 7_736, "fresh + cached is the prompt");
    assert_eq!(usage.total_tokens, 7_757);
    assert_eq!(usage.cached_tokens, 7_680);

    let mut prices = BTreeMap::new();
    prices.insert(
        "opencode/deepseek-v4.1-flash-free".to_string(),
        ModelPrice {
            input_per_million: 2.0,
            output_per_million: 8.0,
            cached_input_per_million: Some(0.5),
        },
    );
    let cost = PriceTable::new(prices)
        .cost("opencode", "deepseek-v4.1-flash-free", &usage)
        .expect("priced");
    // 56 fresh at $2 + 7,680 cached at $0.50 + 21 output at $8, all per million.
    let expected = (56.0 * 2.0 + 7_680.0 * 0.5 + 21.0 * 8.0) / 1_000_000.0;
    assert!((cost - expected).abs() < 1e-12);
}

#[tokio::test]
async fn a_configured_cache_rate_prices_only_the_cached_tokens() {
    let mut prices = BTreeMap::new();
    prices.insert(
        "openai/gpt-4o".to_string(),
        ModelPrice {
            input_per_million: 2.50,
            output_per_million: 10.00,
            cached_input_per_million: Some(1.25),
        },
    );
    let table = PriceTable::new(prices);

    let usage = TokenUsage {
        prompt_tokens: 1_000_000,
        completion_tokens: 0,
        total_tokens: 1_000_000,
        cached_tokens: 400_000,
    };
    let cost = table.cost("openai", "gpt-4o", &usage).expect("priced");

    // 600k fresh at $2.50 + 400k cached at $1.25.
    assert!((cost - (600_000.0 * 2.50 + 400_000.0 * 1.25) / 1_000_000.0).abs() < 1e-9);
}

#[tokio::test]
async fn without_a_cache_rate_a_cached_token_is_not_a_free_token() {
    let mut prices = BTreeMap::new();
    prices.insert("openai/gpt-4o".to_string(), ModelPrice::new(2.50, 10.00));
    let table = PriceTable::new(prices);

    let usage = TokenUsage {
        prompt_tokens: 1_000_000,
        completion_tokens: 0,
        total_tokens: 1_000_000,
        cached_tokens: 900_000,
    };
    let cost = table.cost("openai", "gpt-4o", &usage).expect("priced");

    // The full input rate, i.e. the same as if nothing had been cached: an upper bound.
    assert!((cost - 2.50).abs() < 1e-9);
}

#[tokio::test]
async fn a_cache_count_larger_than_the_prompt_cannot_make_tokens_negative() {
    let mut prices = BTreeMap::new();
    prices.insert(
        "openai/gpt-4o".to_string(),
        ModelPrice {
            input_per_million: 2.50,
            output_per_million: 10.00,
            cached_input_per_million: Some(0.0),
        },
    );
    let table = PriceTable::new(prices);

    // A provider that reported more cached tokens than prompt tokens: the prompt is the ceiling.
    let usage = TokenUsage {
        prompt_tokens: 1_000,
        completion_tokens: 0,
        total_tokens: 1_000,
        cached_tokens: 5_000,
    };
    let cost = table.cost("openai", "gpt-4o", &usage).expect("priced");
    assert!(cost >= 0.0);
    assert!((cost - 0.0).abs() < 1e-9);
}

/// The ledger is the one file a user will not throw away, so a database written before
/// `cached_tokens` existed is migrated in place rather than rejected.
#[tokio::test]
async fn a_ledger_written_before_the_cache_column_still_opens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("usage.db");

    {
        let conn = rusqlite::Connection::open(&path).expect("open old ledger");
        conn.execute_batch(
            "CREATE TABLE usage (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                ts                TEXT NOT NULL,
                session_id        TEXT,
                provider          TEXT NOT NULL,
                model             TEXT NOT NULL,
                prompt_tokens     INTEGER NOT NULL,
                completion_tokens INTEGER NOT NULL,
                total_tokens      INTEGER NOT NULL,
                cost_usd          REAL,
                duration_ms       INTEGER NOT NULL,
                tool_calls        INTEGER NOT NULL,
                denied_calls      INTEGER NOT NULL,
                ok                INTEGER NOT NULL,
                source            TEXT NOT NULL
            );
            INSERT INTO usage (ts, session_id, provider, model, prompt_tokens, completion_tokens,
                               total_tokens, cost_usd, duration_ms, tool_calls, denied_calls, ok,
                               source)
            VALUES ('2026-01-01T00:00:00+00:00', 's0', 'openai', 'gpt-4o', 500, 50, 550, 0.01,
                    10, 0, 0, 1, 'chat');",
        )
        .expect("old schema");
    }

    let ledger = UsageLedger::open(Some(path)).expect("old ledger opens");
    // The pre-existing row reads back with no cache claim...
    let recent = ledger
        .recent(&UsageFilter::default(), 10)
        .await
        .expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].cached_tokens, 0);

    // ...and a new row can be written, which is what the migration is for.
    let usage = TokenUsage {
        prompt_tokens: 200,
        completion_tokens: 20,
        total_tokens: 220,
        cached_tokens: 100,
    };
    ledger
        .record(&record(&usage, None))
        .await
        .expect("insert after migration");

    let totals = ledger
        .totals(&UsageFilter::default())
        .await
        .expect("totals");
    assert_eq!(totals.turns, 2);
    assert_eq!(totals.cached_tokens, 100);
}

/// Historical OpenCode rows stored the cache read beside the prompt. The repair is applied once:
/// reopening the ledger must not add the same tokens a second time.
#[tokio::test]
async fn opencode_rows_written_before_the_fix_are_repaired_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("usage.db");

    {
        let conn = rusqlite::Connection::open(&path).expect("old ledger");
        conn.execute_batch(
            "CREATE TABLE usage (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                ts                TEXT NOT NULL,
                session_id        TEXT,
                provider          TEXT NOT NULL,
                model             TEXT NOT NULL,
                prompt_tokens     INTEGER NOT NULL,
                completion_tokens INTEGER NOT NULL,
                total_tokens      INTEGER NOT NULL,
                cached_tokens     INTEGER NOT NULL DEFAULT 0,
                cost_usd          REAL,
                duration_ms       INTEGER NOT NULL,
                tool_calls        INTEGER NOT NULL,
                denied_calls      INTEGER NOT NULL,
                ok                INTEGER NOT NULL,
                source            TEXT NOT NULL
            );
            INSERT INTO usage (ts, session_id, provider, model, prompt_tokens, completion_tokens,
                               total_tokens, cached_tokens, cost_usd, duration_ms, tool_calls,
                               denied_calls, ok, source)
            VALUES
              ('2026-01-01T00:00:00+00:00', 's1', 'opencode', 'flint-40b-free',
               56, 21, 77, 7680, 0.5, 10, 0, 0, 1, 'chat'),
              ('2026-01-01T00:00:01+00:00', 's2', 'openai', 'gpt-4o',
               1000, 100, 1100, 800, 0.01, 10, 0, 0, 1, 'chat');",
        )
        .expect("old rows");
    }

    let ledger = UsageLedger::open(Some(path.clone())).expect("ledger opens");
    let recent = ledger
        .recent(&UsageFilter::default(), 10)
        .await
        .expect("recent");
    let opencode = recent
        .iter()
        .find(|row| row.provider == "opencode")
        .expect("opencode row");
    assert_eq!(opencode.prompt_tokens, 7_736, "fresh + cached");
    assert_eq!(opencode.total_tokens, 7_757);
    assert_eq!(opencode.cached_tokens, 7_680);
    assert_eq!(
        opencode.cost_usd, None,
        "the old cost was wrong, so it is dropped"
    );

    let openai = recent
        .iter()
        .find(|row| row.provider == "openai")
        .expect("openai row");
    assert_eq!(
        openai.prompt_tokens, 1_000,
        "already-subset rows are untouched"
    );
    assert_eq!(openai.total_tokens, 1_100);

    drop(ledger);
    let reopened = UsageLedger::open(Some(path)).expect("ledger reopens");
    let recent = reopened
        .recent(&UsageFilter::default(), 10)
        .await
        .expect("recent after reopen");
    let opencode = recent
        .iter()
        .find(|row| row.provider == "opencode")
        .expect("opencode row");
    assert_eq!(
        opencode.prompt_tokens, 7_736,
        "the repair is not applied twice"
    );
    assert_eq!(opencode.total_tokens, 7_757);
}

#[test]
fn an_estimated_usage_claims_no_cache() {
    let estimated = TokenUsage::estimated(1_000, 100);
    assert_eq!(estimated.total_tokens, 1_100);
    assert_eq!(
        estimated.cached_tokens, 0,
        "a count derived from characters knows nothing about caching"
    );
}

#[test]
fn usage_from_a_stored_session_without_the_field_still_loads() {
    // The transcript and the ledger both persist `TokenUsage`; a session written before the cache
    // field existed must deserialize rather than fail the resume.
    let stored = r#"{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}"#;
    let usage: TokenUsage = serde_json::from_str(stored).expect("older usage loads");
    assert_eq!(usage.cached_tokens, 0);
    assert_eq!(usage.total_tokens, 12);
}
