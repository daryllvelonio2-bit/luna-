//! Turn-level usage accounting: what each turn cost, per provider and model, in one queryable file.
//!
//! The agent loop knows the provider's token report for the turn it just ran, and then loses it:
//! the figure is rendered once and the process exits. That is enough to watch a number go up and
//! not enough to answer "what did this week cost", "which model am I actually paying for", or "did
//! that refactor double my prompt size". So every turn writes one row here.
//!
//! **Cost is only claimed when it is known.** A price comes from `usage.prices` or the built-in
//! table; a model with no price records `cost_usd = NULL` and totals report it separately as
//! `unpriced_turns`. A blended per-token estimate would put a confident dollar figure on a number
//! nobody set — the wrong kind of wrong for a spending report.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, params_from_iter, types::Value, Connection};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::HarnessError;
use crate::types::TokenUsage;

/// Price for one model, in US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPrice {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl ModelPrice {
    /// What `usage` costs at this price. `None` when the price cannot be applied sensibly.
    fn cost(&self, usage: &TokenUsage) -> Option<f64> {
        if !self.input_per_million.is_finite() || !self.output_per_million.is_finite() {
            return None;
        }
        Some(
            (usage.prompt_tokens as f64 * self.input_per_million
                + usage.completion_tokens as f64 * self.output_per_million)
                / 1_000_000.0,
        )
    }
}

/// Prices by `provider/model` key, with `*/model` and `provider/*` fallbacks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PriceTable {
    prices: BTreeMap<String, ModelPrice>,
}

impl PriceTable {
    pub fn new(prices: BTreeMap<String, ModelPrice>) -> Self {
        Self { prices }
    }

    /// A small built-in table for the models the catalog defaults to.
    ///
    /// Deliberately short: a stale price here would be a wrong number in a spending report, so the
    /// table covers the models this harness actually defaults to and nothing else. Everything else
    /// is "unpriced" until the user sets `usage.prices`.
    pub fn builtin() -> Self {
        let mut prices = BTreeMap::new();
        let mut add = |key: &str, input: f64, output: f64| {
            prices.insert(
                key.to_string(),
                ModelPrice {
                    input_per_million: input,
                    output_per_million: output,
                },
            );
        };
        add("openai/gpt-4o", 2.50, 10.00);
        add("openai/gpt-4o-mini", 0.15, 0.60);
        add("openai/gpt-4.1", 2.00, 8.00);
        add("openai/o3", 2.00, 8.00);
        add("anthropic/claude-sonnet-4", 3.00, 15.00);
        add("anthropic/claude-opus-4", 15.00, 75.00);
        add("google/gemini-2.5-pro", 1.25, 10.00);
        add("google/gemini-2.5-flash", 0.30, 2.50);
        Self { prices }
    }

    /// `usage.prices` from the config layered over the built-in table, so a user override wins.
    pub fn with_overrides(overrides: &BTreeMap<String, ModelPrice>) -> Self {
        let mut table = Self::builtin();
        for (key, price) in overrides {
            table.prices.insert(key.clone(), *price);
        }
        table
    }

    /// The price for a `provider`/`model` pair: exact key, then `provider/*`, then `*/model`.
    pub fn lookup(&self, provider: &str, model: &str) -> Option<ModelPrice> {
        let exact = format!("{provider}/{model}");
        self.prices
            .get(&exact)
            .or_else(|| self.prices.get(&format!("{provider}/*")))
            .or_else(|| self.prices.get(&format!("*/{model}")))
            .copied()
    }

    pub fn cost(&self, provider: &str, model: &str, usage: &TokenUsage) -> Option<f64> {
        self.lookup(provider, model).and_then(|price| price.cost(usage))
    }

    pub fn len(&self) -> usize {
        self.prices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }
}

/// Where a turn ran. Kept as a string column so a new surface does not need a migration.
pub const SOURCE_CHAT: &str = "chat";
pub const SOURCE_EXEC: &str = "exec";
pub const SOURCE_VOICE: &str = "voice";
pub const SOURCE_DELEGATE: &str = "delegate";

/// One row: one turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub ts: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// `None` when the model has no price: unknown, not free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub duration_ms: u64,
    pub tool_calls: u32,
    pub denied_calls: u32,
    /// False when a tool failed or a call was denied: the tokens were still spent.
    pub ok: bool,
    pub source: String,
}

impl UsageRecord {
    /// Build a record for a finished turn, pricing it with `prices`.
    pub fn for_turn(
        provider: &str,
        model: &str,
        usage: &TokenUsage,
        duration_ms: u64,
        prices: &PriceTable,
    ) -> Self {
        Self {
            ts: Utc::now(),
            session_id: None,
            provider: provider.to_string(),
            model: model.to_string(),
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage
                .total_tokens
                .max(usage.prompt_tokens.saturating_add(usage.completion_tokens)),
            cost_usd: prices.cost(provider, model, usage),
            duration_ms,
            tool_calls: 0,
            denied_calls: 0,
            ok: true,
            source: SOURCE_CHAT.to_string(),
        }
    }
}

/// Which rows a query considers.
#[derive(Debug, Clone, Default)]
pub struct UsageFilter {
    pub since: Option<DateTime<Utc>>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub source: Option<String>,
}

impl UsageFilter {
    /// `--since 7d` / `30m` / `12h` / `2w` / an RFC 3339 timestamp.
    pub fn since_text(text: &str) -> Result<Self, HarnessError> {
        let text = text.trim();
        let (value, unit) = text.split_at(text.len().saturating_sub(1));
        let amount: i64 = value.parse().map_err(|_| {
            HarnessError::ConfigurationError(format!(
                "usage: '--since {text}' is not a duration like 30m, 12h, 7d, 2w"
            ))
        })?;
        let duration = match unit {
            "m" => Duration::minutes(amount),
            "h" => Duration::hours(amount),
            "d" => Duration::days(amount),
            "w" => Duration::weeks(amount),
            _ => {
                return Err(HarnessError::ConfigurationError(format!(
                    "usage: '--since {text}' needs one of m, h, d, w (or an RFC 3339 timestamp)"
                )))
            }
        };
        Ok(Self {
            since: Some(Utc::now() - duration),
            ..Self::default()
        })
    }

    /// Apply `--since` as either a duration or an RFC 3339 instant.
    pub fn parse_since(text: &str) -> Result<DateTime<Utc>, HarnessError> {
        if let Ok(instant) = DateTime::parse_from_rfc3339(text) {
            return Ok(instant.with_timezone(&Utc));
        }
        Ok(Self::since_text(text)?
            .since
            .expect("since_text always sets a timestamp"))
    }

    fn sql(&self) -> (String, Vec<Value>) {
        let mut clauses = Vec::new();
        let mut values = Vec::new();
        if let Some(since) = self.since {
            clauses.push("ts >= ?".to_string());
            values.push(Value::Text(since.to_rfc3339()));
        }
        for (column, value) in [
            ("provider", &self.provider),
            ("model", &self.model),
            ("session_id", &self.session_id),
            ("source", &self.source),
        ] {
            if let Some(value) = value {
                clauses.push(format!("{column} = ?"));
                values.push(Value::Text(value.clone()));
            }
        }
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        (where_clause, values)
    }
}

/// Totals for a set of rows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub turns: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// Sum over the priced rows only.
    pub cost_usd: f64,
    /// Rows with no known price; their tokens are counted above, their cost is not in `cost_usd`.
    pub unpriced_turns: u64,
    pub duration_ms: u64,
    pub failed_turns: u64,
    pub denied_calls: u64,
}

/// How `luna usage` groups its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Model,
    Session,
    Source,
    Day,
}

impl GroupBy {
    pub fn parse(text: &str) -> Result<Self, HarnessError> {
        match text {
            "model" => Ok(Self::Model),
            "session" => Ok(Self::Session),
            "source" => Ok(Self::Source),
            "day" => Ok(Self::Day),
            other => Err(HarnessError::ConfigurationError(format!(
                "usage: '--by {other}' must be model, session, source or day"
            ))),
        }
    }

    fn key_expression(self) -> &'static str {
        match self {
            Self::Model => "provider || '/' || model",
            Self::Session => "COALESCE(session_id, '(none)')",
            Self::Source => "source",
            Self::Day => "substr(ts, 1, 10)",
        }
    }
}

/// One group of rows: a model, a session, a source or a day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageBucket {
    pub key: String,
    pub totals: UsageTotals,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS usage (
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

CREATE INDEX IF NOT EXISTS usage_ts ON usage (ts);
CREATE INDEX IF NOT EXISTS usage_session ON usage (session_id);
CREATE INDEX IF NOT EXISTS usage_model ON usage (provider, model);
"#;

/// The sqlite-backed usage ledger. One process-wide handle, like the session store.
pub struct UsageLedger {
    conn: Arc<Mutex<Connection>>,
}

impl UsageLedger {
    /// Open (creating parents and the file). `None` opens an in-memory ledger, which is what tests
    /// and `--no-phase` style callers want.
    pub fn open(path: Option<PathBuf>) -> Result<Self, HarnessError> {
        let conn = match path {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        HarnessError::Io(format!("usage db {}: {}", parent.display(), e))
                    })?;
                }
                Connection::open(&path)
                    .map_err(|e| HarnessError::Io(format!("usage db {}: {}", path.display(), e)))?
            }
            None => Connection::open_in_memory()
                .map_err(|e| HarnessError::Io(format!("in-memory usage db: {e}")))?,
        };
        conn.execute_batch(SCHEMA)
            .map_err(|e| HarnessError::Io(format!("usage schema: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// The default location, so callers that only have a config do not repeat the path rule.
    pub fn open_at(path: &Path) -> Result<Self, HarnessError> {
        Self::open(Some(path.to_path_buf()))
    }

    pub async fn record(&self, record: &UsageRecord) -> Result<(), HarnessError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO usage (ts, session_id, provider, model, prompt_tokens, completion_tokens,
                                total_tokens, cost_usd, duration_ms, tool_calls, denied_calls, ok, source)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                record.ts.to_rfc3339(),
                record.session_id,
                record.provider,
                record.model,
                record.prompt_tokens,
                record.completion_tokens,
                record.total_tokens,
                record.cost_usd,
                record.duration_ms as i64,
                record.tool_calls,
                record.denied_calls,
                record.ok as i64,
                record.source,
            ],
        )
        .map_err(|e| HarnessError::Io(format!("usage insert: {e}")))?;
        Ok(())
    }

    /// Record a turn at a call site that has an outcome but no ledger conveniences.
    pub async fn record_turn(
        &self,
        provider: &str,
        model: &str,
        session_id: Option<&str>,
        outcome: &crate::agent::TurnOutcome,
        source: &str,
        prices: &PriceTable,
    ) -> Result<(), HarnessError> {
        let Some(usage) = outcome.usage.clone() else {
            return Ok(());
        };
        let mut record = UsageRecord::for_turn(provider, model, &usage, outcome.duration_ms, prices);
        record.session_id = session_id.map(|id| id.to_string());
        record.tool_calls = outcome.tool_calls.len() as u32;
        record.denied_calls = outcome.tool_calls.iter().filter(|call| call.denied).count() as u32;
        record.ok = outcome.tool_calls.iter().all(|call| !call.is_error);
        record.source = source.to_string();
        self.record(&record).await
    }

    pub async fn totals(&self, filter: &UsageFilter) -> Result<UsageTotals, HarnessError> {
        let (where_clause, values) = filter.sql();
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(prompt_tokens), 0), COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(total_tokens), 0), COALESCE(SUM(cost_usd), 0.0),
                    COALESCE(SUM(CASE WHEN cost_usd IS NULL THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(duration_ms), 0),
                    COALESCE(SUM(CASE WHEN ok = 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(denied_calls), 0)
             FROM usage{where_clause}"
        );
        let conn = self.conn.lock().await;
        let mut statement = conn
            .prepare(&sql)
            .map_err(|e| HarnessError::Io(format!("usage totals: {e}")))?;
        let totals = statement
            .query_row(params_from_iter(values), |row| {
                Ok(UsageTotals {
                    turns: row.get::<_, i64>(0)? as u64,
                    prompt_tokens: row.get::<_, i64>(1)? as u64,
                    completion_tokens: row.get::<_, i64>(2)? as u64,
                    total_tokens: row.get::<_, i64>(3)? as u64,
                    cost_usd: row.get::<_, f64>(4)?,
                    unpriced_turns: row.get::<_, i64>(5)? as u64,
                    duration_ms: row.get::<_, i64>(6)? as u64,
                    failed_turns: row.get::<_, i64>(7)? as u64,
                    denied_calls: row.get::<_, i64>(8)? as u64,
                })
            })
            .map_err(|e| HarnessError::Io(format!("usage totals: {e}")))?;
        Ok(totals)
    }

    /// Group the matching rows, largest spend first (then by tokens, so unpriced groups still sort).
    pub async fn summarize(
        &self,
        group_by: GroupBy,
        filter: &UsageFilter,
    ) -> Result<Vec<UsageBucket>, HarnessError> {
        let (where_clause, values) = filter.sql();
        let key = group_by.key_expression();
        let sql = format!(
            "SELECT {key} AS bucket, COUNT(*), COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0), COALESCE(SUM(total_tokens), 0),
                    COALESCE(SUM(cost_usd), 0.0),
                    COALESCE(SUM(CASE WHEN cost_usd IS NULL THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(duration_ms), 0),
                    COALESCE(SUM(CASE WHEN ok = 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(denied_calls), 0)
             FROM usage{where_clause}
             GROUP BY bucket
             ORDER BY 6 DESC, 5 DESC"
        );
        let conn = self.conn.lock().await;
        let mut statement = conn
            .prepare(&sql)
            .map_err(|e| HarnessError::Io(format!("usage summary: {e}")))?;
        let rows = statement
            .query_map(params_from_iter(values), |row| {
                Ok(UsageBucket {
                    key: row.get::<_, String>(0)?,
                    totals: UsageTotals {
                        turns: row.get::<_, i64>(1)? as u64,
                        prompt_tokens: row.get::<_, i64>(2)? as u64,
                        completion_tokens: row.get::<_, i64>(3)? as u64,
                        total_tokens: row.get::<_, i64>(4)? as u64,
                        cost_usd: row.get::<_, f64>(5)?,
                        unpriced_turns: row.get::<_, i64>(6)? as u64,
                        duration_ms: row.get::<_, i64>(7)? as u64,
                        failed_turns: row.get::<_, i64>(8)? as u64,
                        denied_calls: row.get::<_, i64>(9)? as u64,
                    },
                })
            })
            .map_err(|e| HarnessError::Io(format!("usage summary: {e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| HarnessError::Io(format!("usage summary: {e}")))
    }

    /// The newest rows, newest first.
    pub async fn recent(
        &self,
        filter: &UsageFilter,
        limit: usize,
    ) -> Result<Vec<UsageRecord>, HarnessError> {
        let (where_clause, values) = filter.sql();
        let sql = format!(
            "SELECT ts, session_id, provider, model, prompt_tokens, completion_tokens, total_tokens,
                    cost_usd, duration_ms, tool_calls, denied_calls, ok, source
             FROM usage{where_clause}
             ORDER BY ts DESC, id DESC
             LIMIT {limit}"
        );
        let conn = self.conn.lock().await;
        let mut statement = conn
            .prepare(&sql)
            .map_err(|e| HarnessError::Io(format!("usage recent: {e}")))?;
        let rows = statement
            .query_map(params_from_iter(values), |row| {
                let ts: String = row.get(0)?;
                Ok(UsageRecord {
                    ts: DateTime::parse_from_rfc3339(&ts)
                        .map(|t| t.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    session_id: row.get(1)?,
                    provider: row.get(2)?,
                    model: row.get(3)?,
                    prompt_tokens: row.get::<_, i64>(4)? as u32,
                    completion_tokens: row.get::<_, i64>(5)? as u32,
                    total_tokens: row.get::<_, i64>(6)? as u32,
                    cost_usd: row.get(7)?,
                    duration_ms: row.get::<_, i64>(8)? as u64,
                    tool_calls: row.get::<_, i64>(9)? as u32,
                    denied_calls: row.get::<_, i64>(10)? as u32,
                    ok: row.get::<_, i64>(11)? != 0,
                    source: row.get(12)?,
                })
            })
            .map_err(|e| HarnessError::Io(format!("usage recent: {e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| HarnessError::Io(format!("usage recent: {e}")))
    }
}
