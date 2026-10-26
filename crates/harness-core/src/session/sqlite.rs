//! The sqlite implementation of [`SessionStore`] — one file, WAL, no schema migration machinery.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use tokio::sync::Mutex;
use tracing::debug;

use super::{default_sessions_db, ReplayOutcome, SessionRecord, SessionStore, StoredMessage};
use crate::error::HarnessError;
use crate::types::{Message, Role, TokenUsage, ToolCall, ToolResult};

/// Longest title kept from the first user line.
const TITLE_MAX_CHARS: usize = 80;

/// Longest user-assigned session name kept (`/name`).
const NAME_MAX_CHARS: usize = 64;

/// Field separator inside the `sessions.model` column, which packs `provider` and `model` into
/// one string. U+001F is the unit-separator control character: it cannot appear in a provider id
/// or model id (model ids are URL-path segments and provider ids are catalog slugs), so it is an
/// unambiguous delimiter.
const MODEL_FIELD_SEPARATOR: char = '\u{1F}';

/// `sessions` + `messages`. `compacted_at` is what makes compaction non-destructive: folded rows
/// stay in the table and are simply excluded from replay.
const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    title       TEXT NOT NULL DEFAULT '',
    name        TEXT NOT NULL DEFAULT '',
    model       TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS messages (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq              INTEGER NOT NULL,
    role             TEXT NOT NULL,
    content          TEXT,
    reasoning        TEXT,
    tool_calls_json  TEXT,
    tool_result_json TEXT,
    usage_json       TEXT,
    is_summary       INTEGER NOT NULL DEFAULT 0,
    compacted_at     TEXT,
    created_at       TEXT NOT NULL,
    UNIQUE (session_id, seq)
);

CREATE INDEX IF NOT EXISTS messages_session_seq ON messages (session_id, seq);
"#;

/// One row of `messages`, before the JSON columns are parsed.
struct RawRow {
    id: i64,
    seq: i64,
    role: String,
    content: Option<String>,
    reasoning: Option<String>,
    tool_calls_json: Option<String>,
    tool_result_json: Option<String>,
    usage_json: Option<String>,
    is_summary: bool,
}

pub struct SqliteSessionStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteSessionStore {
    /// Open (creating parent directories and the file as needed) and apply the schema.
    pub fn new(path: Option<PathBuf>) -> Result<Self, HarnessError> {
        let db_path = path.unwrap_or_else(default_sessions_db);
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    HarnessError::SessionError(format!(
                        "failed to create the sessions db directory '{}': {}",
                        parent.display(),
                        error
                    ))
                })?;
            }
        }

        let conn = Connection::open(&db_path).map_err(|error| {
            HarnessError::SessionError(format!(
                "failed to open the sessions db '{}': {}",
                db_path.display(),
                error
            ))
        })?;
        // The schema is applied to the owned connection before it is shared behind the mutex: the
        // constructors are reachable from async callers, and a tokio lock taken there would panic
        // inside the runtime (see the memory/notes engines for the same ordering).
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// A session store that lives only for the process — what the tests use.
    pub fn in_memory() -> Result<Self, HarnessError> {
        let conn = Connection::open_in_memory()
            .map_err(|error| HarnessError::SessionError(error.to_string()))?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), HarnessError> {
        conn.execute_batch(SCHEMA).map_err(|error| {
            HarnessError::SessionError(format!("sessions schema initialization failed: {}", error))
        })?;
        Self::migrate(conn)
    }

    /// Databases written before `/name` existed lack the `name` column, and databases written
    /// before per-session model persistence existed lack `model`. `CREATE TABLE IF NOT EXISTS`
    /// cannot widen an existing table, so widen it here when a column is absent.
    fn migrate(conn: &Connection) -> Result<(), HarnessError> {
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(sessions)")
            .and_then(|mut stmt| {
                let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
                Ok(columns.collect::<Result<Vec<_>, _>>()?)
            })
            .map_err(|error| session_error("failed to inspect the sessions schema", error))?;

        let has_name = columns.iter().any(|column| column == "name");
        if !has_name {
            conn.execute(
                "ALTER TABLE sessions ADD COLUMN name TEXT NOT NULL DEFAULT ''",
                [],
            )
            .map_err(|error| session_error("failed to add the session name column", error))?;
        }

        let has_model = columns.iter().any(|column| column == "model");
        if !has_model {
            conn.execute(
                "ALTER TABLE sessions ADD COLUMN model TEXT NOT NULL DEFAULT ''",
                [],
            )
            .map_err(|error| session_error("failed to add the session model column", error))?;
        }

        // Reasoning is part of the conversation: replaying a session without it hands the model
        // a transcript missing half of what it said. Widen `messages` the same way.
        let message_columns: Vec<String> = conn
            .prepare("PRAGMA table_info(messages)")
            .and_then(|mut stmt| {
                let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
                Ok(columns.collect::<Result<Vec<_>, _>>()?)
            })
            .map_err(|error| session_error("failed to inspect the messages schema", error))?;
        let has_reasoning = message_columns.iter().any(|column| column == "reasoning");
        if !has_reasoning {
            conn.execute("ALTER TABLE messages ADD COLUMN reasoning TEXT", [])
                .map_err(|error| session_error("failed to add the reasoning column", error))?;
        }
        Ok(())
    }
}

fn session_error(context: &str, error: impl std::fmt::Display) -> HarnessError {
    HarnessError::SessionError(format!("{}: {}", context, error))
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn parse_time(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn role_to_text(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn text_to_role(raw: &str) -> Result<Role, HarnessError> {
    match raw {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        other => Err(HarnessError::SessionError(format!(
            "unknown message role '{}' in the sessions db",
            other
        ))),
    }
}

fn raw_to_stored(raw: RawRow) -> Result<StoredMessage, HarnessError> {
    let tool_calls: Vec<ToolCall> = match &raw.tool_calls_json {
        Some(json) if !json.is_empty() => serde_json::from_str(json)
            .map_err(|error| session_error("failed to parse tool_calls_json", error))?,
        _ => Vec::new(),
    };
    let tool_result: Option<ToolResult> = match &raw.tool_result_json {
        Some(json) if !json.is_empty() => Some(
            serde_json::from_str(json)
                .map_err(|error| session_error("failed to parse tool_result_json", error))?,
        ),
        _ => None,
    };
    let usage: Option<TokenUsage> = match &raw.usage_json {
        Some(json) if !json.is_empty() => Some(
            serde_json::from_str(json)
                .map_err(|error| session_error("failed to parse usage_json", error))?,
        ),
        _ => None,
    };

    // Rows written before the provider layer learned to drop DeepSeek's DSML scaffolding still
    // carry it, so the filter runs on the way out too: a resumed session cannot replay noise the
    // live turn never showed, and the model does not get it back as history either.
    let content = raw
        .content
        .map(|text| crate::text::strip_dsml_artifacts(&text))
        .filter(|text| !text.trim().is_empty());

    Ok(StoredMessage {
        id: raw.id,
        seq: raw.seq,
        is_summary: raw.is_summary,
        message: Message {
            role: text_to_role(&raw.role)?,
            content,
            reasoning: raw.reasoning.filter(|r| !r.is_empty()),
            tool_calls,
            tool_result,
            usage,
        },
    })
}

/// Query the messages of a session that match `filter`, oldest first.
///
/// `min_seq` bounds the window from below; the caller decides where the window starts.
fn query_rows(
    conn: &Connection,
    session_id: &str,
    filter: &str,
    min_seq: Option<i64>,
) -> Result<Vec<StoredMessage>, HarnessError> {
    let mut sql = format!(
        "SELECT id, seq, role, content, reasoning, tool_calls_json, tool_result_json, usage_json, is_summary
         FROM messages
         WHERE session_id = ?1 AND {filter}"
    );
    if let Some(min_seq) = min_seq {
        sql.push_str(&format!(" AND seq >= {}", min_seq));
    }
    sql.push_str(" ORDER BY seq ASC");

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|error| session_error("failed to read session messages", error))?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok(RawRow {
                id: row.get(0)?,
                seq: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                reasoning: row.get(4)?,
                tool_calls_json: row.get(5)?,
                tool_result_json: row.get(6)?,
                usage_json: row.get(7)?,
                is_summary: row.get::<_, i64>(8)? != 0,
            })
        })
        .map_err(|error| session_error("failed to read session messages", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| session_error("failed to read session messages", error))?;

    rows.into_iter().map(raw_to_stored).collect()
}

/// The synthetic summary rows: every one of them, whatever the bound is.
fn query_summary_rows(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<StoredMessage>, HarnessError> {
    query_rows(conn, session_id, "is_summary = 1", None)
}

/// The live transcript, bounded by `limit` rows.
///
/// The bound counts rows, but a row is not a turn: one long turn (a hundred tool calls) can fill
/// the whole window, and a window that holds no user row replays as nothing at all — the console
/// prints an empty transcript and the model is handed tool results whose calls are missing. A
/// window that would land inside a turn is therefore pulled back to the user message that anchors
/// that turn, so the turn comes in whole.
fn query_ordinary_rows(
    conn: &Connection,
    session_id: &str,
    limit: Option<usize>,
) -> Result<Vec<StoredMessage>, HarnessError> {
    let Some(start) = ordinary_window_start(conn, session_id, limit)? else {
        return Ok(Vec::new());
    };
    let mut stored = query_rows(
        conn,
        session_id,
        "is_summary = 0 AND compacted_at IS NULL",
        Some(start),
    )?;
    // If a limit boundary cuts in the middle of a turn, drop leading messages until
    // the first User message so replayed history always starts on a clean user turn.
    if let Some(first_user) = stored.iter().position(|m| m.message.role == Role::User) {
        if first_user > 0 {
            stored.drain(0..first_user);
        }
    }
    Ok(stored)
}

/// Where the bounded window starts: the seq of its oldest row, pulled back to the user message
/// that anchors the last turn when the bound would otherwise land inside one.
///
/// `None` means the session holds no live rows to replay.
fn ordinary_window_start(
    conn: &Connection,
    session_id: &str,
    limit: Option<usize>,
) -> Result<Option<i64>, HarnessError> {
    let Some(limit) = limit else {
        return first_live_seq(conn, session_id);
    };
    if limit == 0 {
        return Ok(None);
    }
    if limit >= i64::MAX as usize {
        return first_live_seq(conn, session_id);
    }
    // The seq of the oldest row the bound allows: the window is the newest `limit` rows.
    let boundary: Option<i64> = conn
        .query_row(
            "SELECT seq FROM messages
             WHERE session_id = ?1 AND is_summary = 0 AND compacted_at IS NULL
             ORDER BY seq DESC LIMIT 1 OFFSET ?2",
            params![session_id, (limit - 1) as i64],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| session_error("failed to read the session replay window", error))?;
    let Some(boundary) = boundary else {
        // Fewer live rows than the bound allows: the window is the whole transcript.
        return first_live_seq(conn, session_id);
    };
    // A window that already holds a user row starts on a turn; there is nothing to pull back.
    let anchored: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM messages
             WHERE session_id = ?1 AND is_summary = 0 AND compacted_at IS NULL
               AND role = 'user' AND seq >= ?2
             LIMIT 1",
            params![session_id, boundary],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| session_error("failed to read the replay window anchor", error))?;
    if anchored.is_some() {
        return Ok(Some(boundary));
    }
    // The window lies inside one turn: pull it back to the user row that anchors that turn.
    let anchor: Option<i64> = conn
        .query_row(
            "SELECT MAX(seq) FROM messages
             WHERE session_id = ?1 AND is_summary = 0 AND compacted_at IS NULL
               AND role = 'user' AND seq < ?2",
            params![session_id, boundary],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(|error| session_error("failed to read the replay window anchor", error))?;
    Ok(Some(anchor.unwrap_or(boundary)))
}

/// The seq of the first live row of a session, if it holds one.
fn first_live_seq(conn: &Connection, session_id: &str) -> Result<Option<i64>, HarnessError> {
    conn.query_row(
        "SELECT MIN(seq) FROM messages
         WHERE session_id = ?1 AND is_summary = 0 AND compacted_at IS NULL",
        params![session_id],
        |row| row.get::<_, Option<i64>>(0),
    )
    .map_err(|error| session_error("failed to read the session replay window", error))
}

fn count_live(conn: &Connection, session_id: &str) -> Result<usize, HarnessError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND compacted_at IS NULL",
            params![session_id],
            |row| row.get(0),
        )
        .map_err(|error| session_error("failed to count session messages", error))?;
    Ok(count as usize)
}

fn read_session(conn: &Connection, id: &str) -> Result<SessionRecord, HarnessError> {
    let (created_at, updated_at, title, name, model): (String, String, String, String, String) =
        conn.query_row(
            "SELECT created_at, updated_at, title, name, model FROM sessions WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|error| session_error(&format!("session '{}' not found", id), error))?;

    Ok(SessionRecord {
        id: id.to_string(),
        created_at: parse_time(&created_at),
        updated_at: parse_time(&updated_at),
        title,
        name,
        message_count: count_live(conn, id)?,
        // The column stores `provider\x1Fmodel` when the session has a model of its own and an
        // empty string otherwise; a legacy or never-switched row resumes on the config default.
        model: model
            .split_once(MODEL_FIELD_SEPARATOR)
            .filter(|(provider, m)| !provider.is_empty() && !m.is_empty())
            .map(|(provider, m)| (provider.to_string(), m.to_string())),
    })
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn ensure_session(&self, id: &str) -> Result<SessionRecord, HarnessError> {
        let conn = self.conn.lock().await;
        let timestamp = now();
        conn.execute(
            "INSERT OR IGNORE INTO sessions (id, created_at, updated_at, title) VALUES (?1, ?2, ?2, '')",
            params![id, timestamp],
        )
        .map_err(|error| session_error("failed to create the session", error))?;
        read_session(&conn, id)
    }

    async fn get(&self, id: &str) -> Result<SessionRecord, HarnessError> {
        let conn = self.conn.lock().await;
        read_session(&conn, id)
    }

    async fn set_name(&self, id: &str, name: &str) -> Result<(), HarnessError> {
        let conn = self.conn.lock().await;
        let name: String = name.trim().chars().take(NAME_MAX_CHARS).collect();
        let updated = conn
            .execute(
                "UPDATE sessions SET name = ?1 WHERE id = ?2",
                params![name, id],
            )
            .map_err(|error| session_error("failed to set the session name", error))?;
        if updated == 0 {
            return Err(HarnessError::SessionError(format!(
                "session '{}' not found",
                id
            )));
        }
        Ok(())
    }

    async fn set_model(&self, id: &str, provider: &str, model: &str) -> Result<(), HarnessError> {
        let conn = self.conn.lock().await;
        let packed = format!("{}{}{}", provider, MODEL_FIELD_SEPARATOR, model);
        let updated = conn
            .execute(
                "UPDATE sessions SET model = ?1 WHERE id = ?2",
                params![packed, id],
            )
            .map_err(|error| session_error("failed to set the session model", error))?;
        if updated == 0 {
            return Err(HarnessError::SessionError(format!(
                "session '{}' not found",
                id
            )));
        }
        Ok(())
    }

    async fn append(
        &self,
        session_id: &str,
        message: &Message,
        is_summary: bool,
    ) -> Result<i64, HarnessError> {
        let conn = self.conn.lock().await;
        let timestamp = now();
        // Defensive: an append can only land against an existing session (the row is a foreign
        // key), and losing a whole turn to a missing session row would be a bad trade for one
        // statement.
        conn.execute(
            "INSERT OR IGNORE INTO sessions (id, created_at, updated_at, title) VALUES (?1, ?2, ?2, '')",
            params![session_id, timestamp],
        )
        .map_err(|error| session_error("failed to create the session", error))?;

        let seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|error| session_error("failed to allocate a message sequence", error))?;

        let tool_calls_json = if message.tool_calls.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&message.tool_calls)
                    .map_err(|error| session_error("failed to serialize tool calls", error))?,
            )
        };
        let reasoning = message
            .reasoning
            .as_ref()
            .filter(|r| !r.is_empty())
            .cloned();
        let tool_result_json = match &message.tool_result {
            Some(result) => Some(
                serde_json::to_string(result)
                    .map_err(|error| session_error("failed to serialize the tool result", error))?,
            ),
            None => None,
        };
        let usage_json = match &message.usage {
            Some(usage) => Some(
                serde_json::to_string(usage)
                    .map_err(|error| session_error("failed to serialize token usage", error))?,
            ),
            None => None,
        };

        conn.execute(
            "INSERT INTO messages
                (session_id, seq, role, content, reasoning, tool_calls_json, tool_result_json,
                 usage_json, is_summary, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                session_id,
                seq,
                role_to_text(message.role),
                message.content,
                reasoning,
                tool_calls_json,
                tool_result_json,
                usage_json,
                if is_summary { 1 } else { 0 },
                timestamp,
            ],
        )
        .map_err(|error| session_error("failed to append the message", error))?;

        // The first user line becomes the title `luna sessions list` shows.
        if message.role == Role::User {
            if let Some(content) = &message.content {
                let title: String = content.trim().chars().take(TITLE_MAX_CHARS).collect();
                conn.execute(
                    "UPDATE sessions SET title = ?1 WHERE id = ?2 AND title = ''",
                    params![title, session_id],
                )
                .map_err(|error| session_error("failed to set the session title", error))?;
            }
        }

        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![timestamp, session_id],
        )
        .map_err(|error| session_error("failed to touch the session", error))?;

        Ok(conn.last_insert_rowid())
    }

    async fn replay(&self, session_id: &str, limit: usize) -> Result<ReplayOutcome, HarnessError> {
        let conn = self.conn.lock().await;
        let total = count_live(&conn, session_id)?;
        // Summaries are never dropped by the bound: they are the only trace of the turns they
        // replaced, and there is at most a handful of them.
        let mut history = query_summary_rows(&conn, session_id)?;
        history.extend(query_ordinary_rows(&conn, session_id, Some(limit))?);

        let replayed = history.len();
        Ok(ReplayOutcome {
            history,
            total,
            replayed,
        })
    }

    async fn record_compaction(
        &self,
        session_id: &str,
        folded: &[i64],
        summary: &str,
    ) -> Result<i64, HarnessError> {
        let mut conn = self.conn.lock().await;
        let transaction = conn
            .transaction()
            .map_err(|error| session_error("failed to open a compaction transaction", error))?;

        let seq: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|error| session_error("failed to allocate a summary sequence", error))?;

        let timestamp = now();
        transaction
            .execute(
                "INSERT INTO messages
                    (session_id, seq, role, content, is_summary, created_at)
                 VALUES (?1, ?2, 'system', ?3, 1, ?4)",
                params![session_id, seq, summary, timestamp],
            )
            .map_err(|error| session_error("failed to write the summary row", error))?;
        let summary_id = transaction.last_insert_rowid();

        if !folded.is_empty() {
            let placeholders = vec!["?"; folded.len()].join(",");
            let sql = format!(
                "UPDATE messages SET compacted_at = ? WHERE id IN ({}) AND compacted_at IS NULL",
                placeholders
            );
            let mut values = vec![Value::Text(timestamp.clone())];
            values.extend(folded.iter().map(|id| Value::Integer(*id)));
            transaction
                .execute(&sql, params_from_iter(values.iter()))
                .map_err(|error| session_error("failed to mark the compacted rows", error))?;
        }

        transaction
            .execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                params![timestamp, session_id],
            )
            .map_err(|error| session_error("failed to touch the session", error))?;
        transaction
            .commit()
            .map_err(|error| session_error("failed to commit the compaction", error))?;

        debug!(
            "Recorded a summary for session '{}' folding {} messages",
            session_id,
            folded.len()
        );
        Ok(summary_id)
    }

    async fn list(&self) -> Result<Vec<SessionRecord>, HarnessError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id FROM sessions ORDER BY updated_at DESC")
            .map_err(|error| session_error("failed to list sessions", error))?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| session_error("failed to list sessions", error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| session_error("failed to list sessions", error))?;

        ids.iter().map(|id| read_session(&conn, id)).collect()
    }

    async fn most_recent(&self) -> Result<Option<String>, HarnessError> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id FROM sessions ORDER BY updated_at DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(session_error(
                "failed to read the most recent session",
                other,
            )),
        })
    }

    async fn delete_session(&self, id: &str) -> Result<bool, HarnessError> {
        let conn = self.conn.lock().await;
        // `messages.session_id` is declared `REFERENCES sessions(id) ON DELETE CASCADE` and the
        // schema enables foreign keys, so removing the session row is enough to drop its history
        // without a second statement that could half-succeed.
        let removed = conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])
            .map_err(|error| session_error("failed to delete the session", error))?;
        Ok(removed > 0)
    }

    async fn delete_all_sessions(&self) -> Result<usize, HarnessError> {
        let conn = self.conn.lock().await;
        // One statement, one implicit transaction: the cascade drops every message row, so the
        // store cannot be left holding messages whose session is already gone.
        let removed = conn
            .execute("DELETE FROM sessions", [])
            .map_err(|error| session_error("failed to delete all sessions", error))?;
        Ok(removed)
    }

    async fn fork_session(
        &self,
        source_id: &str,
        new_title: Option<&str>,
    ) -> Result<String, HarnessError> {
        let conn = self.conn.lock().await;

        let (title, name, model) = conn
            .query_row(
                "SELECT title, name, model FROM sessions WHERE id = ?1",
                params![source_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|e| session_error(&format!("source session '{source_id}' not found"), e))?;

        let forked_title = new_title.map(str::to_string).unwrap_or_else(|| {
            if title.is_empty() {
                format!("Fork of {}", source_id)
            } else {
                format!("Fork of {}", title)
            }
        });

        let new_id = format!("s{:x}", uuid::Uuid::new_v4().as_fields().0);
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO sessions (id, created_at, updated_at, title, name, model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![new_id, now, now, forked_title, name, model],
        )
        .map_err(|e| session_error("failed to create forked session record", e))?;

        conn.execute(
            "INSERT INTO messages (session_id, seq, role, content, tool_calls_json, tool_result_json, usage_json, is_summary, compacted_at, created_at)
             SELECT ?1, seq, role, content, tool_calls_json, tool_result_json, usage_json, is_summary, compacted_at, created_at
             FROM messages WHERE session_id = ?2
             ORDER BY seq ASC",
            params![new_id, source_id],
        )
        .map_err(|e| session_error("failed to copy messages for forked session", e))?;

        Ok(new_id)
    }
}
