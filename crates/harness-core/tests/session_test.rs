//! Session persistence (`sessions.db`), replay bounds, and bounded context (compaction).
//!
//! The store tests run against a real sqlite file in a temp dir and then count rows with `rusqlite`
//! directly, because the acceptance criteria are stated as `sqlite3` row counts — a `:memory:`
//! database has no rows to count from outside. Compaction runs against fake providers: the
//! summarising call is a plain `ModelProvider::complete`, so a canned provider pins the boundary
//! and a failing one proves the non-fatal path.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rusqlite::Connection;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use harness_core::*;

// ---------------------------------------------------------------- fakes

/// Answers every completion with one canned summary; counts the calls so a retry is visible.
struct CannedProvider {
    summary: String,
    calls: AtomicUsize,
}

impl CannedProvider {
    fn new(summary: &str) -> Self {
        Self {
            summary: summary.to_string(),
            calls: AtomicUsize::new(0),
        }
    }
}

/// Always fails, the way a provider with no credentials fails.
struct FailingProvider {
    calls: AtomicUsize,
}

impl FailingProvider {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

/// Returns a fixed reply and remembers every request it was sent.
struct RecordingProvider {
    reply: String,
    received: Mutex<Vec<Vec<Message>>>,
}

impl RecordingProvider {
    fn new(reply: &str) -> Self {
        Self {
            reply: reply.to_string(),
            received: Mutex::new(Vec::new()),
        }
    }

    fn last_request(&self) -> Vec<Message> {
        self.received
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }

    fn call_count(&self) -> usize {
        self.received.lock().unwrap().len()
    }
}

#[async_trait]
impl ModelProvider for CannedProvider {
    fn provider_id(&self) -> &str {
        "canned"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["canned-model".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Message::assistant(self.summary.clone()))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl ModelProvider for FailingProvider {
    fn provider_id(&self) -> &str {
        "unconfigured"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(Vec::new())
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(HarnessError::ProviderError {
            provider: "unconfigured".to_string(),
            message: "no credential for this provider".to_string(),
        })
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    fn provider_id(&self) -> &str {
        "recording"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["recording-model".to_string()])
    }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        self.received.lock().unwrap().push(messages.to_vec());
        Ok(Message::assistant(self.reply.clone()))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(Vec::new())
    }
}

/// A reasoning model that spent its whole budget on reasoning and wrote no text: the
/// first call answers with the provider layer's "no text response" marker, the second
/// (the retry with a bigger budget) answers with a real summary. It records the budget
/// of every call, so a test can see that the retry asked for more room.
struct ReasoningProvider {
    calls: AtomicUsize,
    budgets: Mutex<Vec<u32>>,
}

impl ReasoningProvider {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            budgets: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ModelProvider for ReasoningProvider {
    fn provider_id(&self) -> &str {
        "reasoning"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["reasoning-model".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.budgets
            .lock()
            .unwrap()
            .push(options.max_tokens.unwrap_or_default());
        if self.calls.load(Ordering::SeqCst) == 1 {
            Ok(Message::assistant(NO_TEXT_RESPONSE))
        } else {
            Ok(Message::assistant("the summary, with room to write it"))
        }
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(Vec::new())
    }
}

/// A model that never writes a summary: every call comes back with the marker.
struct SilentProvider {
    calls: AtomicUsize,
}

impl SilentProvider {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ModelProvider for SilentProvider {
    fn provider_id(&self) -> &str {
        "silent"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["silent-model".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Message::assistant(NO_TEXT_RESPONSE))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(Vec::new())
    }
}

struct MockMemory;

#[async_trait]
impl MemoryEngine for MockMemory {
    async fn retrieve(
        &self,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<MemoryRecord>, HarnessError> {
        Ok(Vec::new())
    }
    async fn save(&self, _record: &MemoryRecord) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn update(&self, _id: &str, _updated: &MemoryRecord) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn forget(&self, _id: &str) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn add_note_pointer(&self, _note_id: &str, _pointer: &str) -> Result<(), HarnessError> {
        Ok(())
    }
    fn format_as_system_prompt(&self, _memories: &[MemoryRecord]) -> String {
        String::new()
    }
}

struct MockNotes;

#[async_trait]
impl NotesEngine for MockNotes {
    async fn save_note(
        &self,
        title: &str,
        content: &str,
        _tags: &[String],
    ) -> Result<Note, HarnessError> {
        Ok(Note {
            id: "n1".to_string(),
            title: title.to_string(),
            content: content.to_string(),
            summary_pointer: String::new(),
            tags: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        })
    }
    async fn search_notes(&self, _query: &str, _limit: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(Vec::new())
    }
    async fn list_notes(&self, _limit: usize, _offset: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(Vec::new())
    }
    async fn read_note(&self, _id: &str) -> Result<Note, HarnessError> {
        Err(HarnessError::NotesError(
            "no notes in this test".to_string(),
        ))
    }
    async fn update_note(
        &self,
        _id: &str,
        _title: Option<&str>,
        _content: Option<&str>,
        _tags: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        Err(HarnessError::NotesError(
            "no notes in this test".to_string(),
        ))
    }
    async fn delete_note(&self, _id: &str) -> Result<bool, HarnessError> {
        Err(HarnessError::NotesError(
            "no notes in this test".to_string(),
        ))
    }
}

struct MockEvolver;

#[async_trait]
impl EvolverEngine for MockEvolver {
    async fn inspect_source(&self, _path: &str) -> Result<String, HarnessError> {
        Ok(String::new())
    }
    async fn propose_patch(&self, _description: &str, _diff: &str) -> Result<String, HarnessError> {
        Ok("patch_1".to_string())
    }
    async fn test_patch_sandbox(&self, _patch_id: &str) -> Result<bool, HarnessError> {
        Ok(true)
    }
    async fn apply_patch(&self, _patch_id: &str) -> Result<(), HarnessError> {
        Ok(())
    }
}

// ---------------------------------------------------------------- helpers

fn temp_store() -> (TempDir, std::path::PathBuf, SqliteSessionStore) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("sessions.db");
    let store = SqliteSessionStore::new(Some(path.clone())).expect("open the sessions db");
    (dir, path, store)
}

/// Rows in `messages` straight out of the file, the way `sqlite3` would count them.
fn count_rows(path: &std::path::Path, filter: &str) -> i64 {
    let conn = Connection::open(path).expect("open the sessions db read-only for counting");
    conn.query_row(
        &format!("SELECT COUNT(*) FROM messages WHERE {}", filter),
        [],
        |row| row.get(0),
    )
    .expect("count rows")
}

/// A history of `turns` user/assistant pairs, one turn per index.
fn history(turns: usize) -> Vec<StoredMessage> {
    let mut out = Vec::new();
    for turn in 0..turns {
        for (role, body) in [
            (Role::User, format!("question {}", turn)),
            (Role::Assistant, format!("answer {}", turn)),
        ] {
            out.push(StoredMessage {
                id: out.len() as i64 + 1,
                seq: out.len() as i64 + 1,
                is_summary: false,
                message: Message {
                    role,
                    content: Some(body),
                    reasoning: None,
                    tool_calls: vec![],
                    tool_result: None,
                    usage: None,
                },
            });
        }
    }
    out
}

/// One stored message, with the fields a test needs to set.
fn stored(role: Role, content: String) -> StoredMessage {
    stored_with(role, content, vec![], None)
}

/// One stored message, with tool calls and a tool result.
fn stored_with(
    role: Role,
    content: String,
    tool_calls: Vec<ToolCall>,
    tool_result: Option<ToolResult>,
) -> StoredMessage {
    StoredMessage {
        id: 1,
        seq: 1,
        is_summary: false,
        message: Message {
            role,
            content: Some(content),
            reasoning: None,
            tool_calls,
            tool_result,
            usage: None,
        },
    }
}

// ---------------------------------------------------------------- store

#[tokio::test]
async fn append_assigns_sequence_and_replay_returns_it_in_order() {
    let (_dir, _path, store) = temp_store();
    store
        .ensure_session("s1")
        .await
        .expect("create the session");

    store
        .append("s1", &Message::user("first"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("second"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::user("third"), false)
        .await
        .unwrap();

    let replay = store.replay("s1", 200).await.unwrap();
    assert_eq!(replay.total, 3);
    assert_eq!(replay.replayed, 3);
    assert!(!replay.truncated());

    let seqs: Vec<i64> = replay.history.iter().map(|stored| stored.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    let bodies: Vec<Option<String>> = replay
        .history
        .iter()
        .map(|stored| stored.message.content.clone())
        .collect();
    assert_eq!(
        bodies,
        vec![
            Some("first".to_string()),
            Some("second".to_string()),
            Some("third".to_string())
        ]
    );
    assert_eq!(replay.history[0].message.role, Role::User);
}

/// A tool call's provider signature is part of the record, not scratch state: Gemini validates it
/// against the call when the history is replayed on the next request, so a store that dropped it
/// would break the conversation it just persisted.
#[tokio::test]
async fn a_tool_calls_signature_survives_the_round_trip_through_the_store() {
    let (_dir, _path, store) = temp_store();
    store
        .ensure_session("s1")
        .await
        .expect("create the session");

    let assistant = Message {
        role: Role::Assistant,
        content: None,
        reasoning: None,
        tool_calls: vec![ToolCall {
            id: "call_1".to_string(),
            name: "list_files".to_string(),
            arguments: serde_json::json!({ "path": "." }),
            signature: Some("sig-from-gemini".to_string()),
        }],
        tool_result: None,
        usage: None,
    };
    store.append("s1", &assistant, false).await.unwrap();

    let replay = store.replay("s1", 200).await.unwrap();
    let replayed = &replay.history[0].message.tool_calls[0];
    assert_eq!(replayed.name, "list_files");
    assert_eq!(
        replayed.signature.as_deref(),
        Some("sig-from-gemini"),
        "the signature must come back verbatim, or the next request 400s"
    );
}

#[tokio::test]
async fn replay_bound_keeps_the_newest_and_reports_the_total() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    for index in 0..10 {
        let message = Message::user(format!("line {}", index));
        store.append("s1", &message, false).await.unwrap();
    }

    let replay = store.replay("s1", 3).await.unwrap();
    assert_eq!(replay.total, 10);
    assert_eq!(replay.replayed, 3);
    assert!(
        replay.truncated(),
        "the bound dropped messages, so it must say so"
    );

    let bodies: Vec<Option<String>> = replay
        .history
        .iter()
        .map(|stored| stored.message.content.clone())
        .collect();
    assert_eq!(
        bodies,
        vec![
            Some("line 7".to_string()),
            Some("line 8".to_string()),
            Some("line 9".to_string())
        ]
    );
}

#[tokio::test]
async fn record_compaction_writes_a_summary_and_keeps_the_folded_rows() {
    let (_dir, path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    store
        .append("s1", &Message::user("one"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("two"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::user("three"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("four"), false)
        .await
        .unwrap();

    let summary_id = store
        .record_compaction("s1", &[1, 2], "the user asked one")
        .await
        .unwrap();
    assert!(summary_id > 4, "the summary is a new row, not a rewrite");

    let replay = store.replay("s1", 200).await.unwrap();
    assert!(replay.history[0].is_summary);
    assert_eq!(
        replay.history[0].message.content.as_deref(),
        Some("the user asked one")
    );
    assert_eq!(replay.history[0].message.role, Role::System);
    assert_eq!(
        replay
            .history
            .iter()
            .filter(|stored| !stored.is_summary)
            .count(),
        2,
        "the two unfolded rows are what is left to replay"
    );

    // Nothing is destroyed: all four originals are still rows, the folded two are marked.
    assert_eq!(count_rows(&path, "1 = 1"), 5);
    assert_eq!(count_rows(&path, "compacted_at IS NOT NULL"), 2);
    assert_eq!(count_rows(&path, "is_summary = 1"), 1);
}

#[tokio::test]
async fn list_reports_the_first_user_line_and_the_most_recent_session() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    store
        .append(
            "s1",
            &Message::user("  what is a compaction boundary?  "),
            false,
        )
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("a boundary"), false)
        .await
        .unwrap();

    let sessions = store.list().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "s1");
    assert_eq!(sessions[0].title, "what is a compaction boundary?");
    assert_eq!(sessions[0].name, "", "a fresh session has no name");
    assert_eq!(sessions[0].message_count, 2);
    assert_eq!(store.most_recent().await.unwrap().as_deref(), Some("s1"));
}

#[tokio::test]
async fn delete_session_removes_the_session_and_its_messages() {
    let (_dir, path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    store.ensure_session("s2").await.unwrap();
    store
        .append("s1", &Message::user("keep me"), false)
        .await
        .unwrap();
    store
        .append("s2", &Message::user("delete me"), false)
        .await
        .unwrap();
    store
        .append("s2", &Message::assistant("gone"), false)
        .await
        .unwrap();
    assert_eq!(count_rows(&path, "1 = 1"), 3);

    assert!(store.delete_session("s2").await.unwrap());

    // The foreign key cascade takes the messages with the session.
    assert_eq!(count_rows(&path, "1 = 1"), 1);
    assert!(
        store.get("s2").await.is_err(),
        "the deleted session is gone"
    );

    let sessions = store.list().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "s1");
    assert_eq!(store.most_recent().await.unwrap().as_deref(), Some("s1"));

    // Deleting what is already gone is not an error: it reports that nothing was removed.
    assert!(!store.delete_session("s2").await.unwrap());
}

#[tokio::test]
async fn delete_all_sessions_empties_the_store() {
    let (_dir, path, store) = temp_store();
    for id in ["s1", "s2", "s3"] {
        store.ensure_session(id).await.unwrap();
        store.append(id, &Message::user("hi"), false).await.unwrap();
        store
            .append(id, &Message::assistant("hello"), false)
            .await
            .unwrap();
    }
    assert_eq!(count_rows(&path, "1 = 1"), 6);

    assert_eq!(store.delete_all_sessions().await.unwrap(), 3);

    // The cascade takes every message with its session, in the same statement.
    assert_eq!(count_rows(&path, "1 = 1"), 0);
    assert!(store.list().await.unwrap().is_empty());
    assert_eq!(store.most_recent().await.unwrap(), None);

    // An empty store is not an error: it reports that nothing was removed.
    assert_eq!(store.delete_all_sessions().await.unwrap(), 0);
}

#[tokio::test]
async fn set_name_is_persisted_and_reported_by_list() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();

    store.set_name("s1", "  release triage  ").await.unwrap();
    assert_eq!(store.get("s1").await.unwrap().name, "release triage");

    let sessions = store.list().await.unwrap();
    assert_eq!(sessions[0].name, "release triage");

    // An empty name clears it again.
    store.set_name("s1", "").await.unwrap();
    assert_eq!(store.get("s1").await.unwrap().name, "");
}

#[tokio::test]
async fn naming_tolerates_databases_written_before_the_column_existed() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");

    // A pre-`/name` database: the sessions table has no `name` column yet.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id         TEXT PRIMARY KEY,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 title      TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO sessions (id, created_at, updated_at, title)
             VALUES ('s1', '2020-01-01T00:00:00+00:00', '2020-01-01T00:00:00+00:00', 'old');",
        )
        .unwrap();
    }

    let store = SqliteSessionStore::new(Some(path)).expect("opening migrates the table");
    assert_eq!(
        store.get("s1").await.unwrap().name,
        "",
        "backfilled as empty"
    );
    store.set_name("s1", "migrated").await.unwrap();
    assert_eq!(store.get("s1").await.unwrap().name, "migrated");
}

#[tokio::test]
async fn set_model_is_persisted_and_reported_by_get_and_list() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();

    // A session that has not picked a model reports none, not a zero pair.
    assert_eq!(store.get("s1").await.unwrap().model, None);

    store.set_model("s1", "deepseek", "glm-4.6").await.unwrap();
    assert_eq!(
        store.get("s1").await.unwrap().model,
        Some(("deepseek".to_string(), "glm-4.6".to_string()))
    );

    let sessions = store.list().await.unwrap();
    assert_eq!(
        sessions[0].model,
        Some(("deepseek".to_string(), "glm-4.6".to_string()))
    );

    // A second switch overwrites: the row holds the latest pair.
    store.set_model("s1", "openai", "gpt-5").await.unwrap();
    assert_eq!(
        store.get("s1").await.unwrap().model,
        Some(("openai".to_string(), "gpt-5".to_string()))
    );

    // Setting on a missing session is an error, same contract as set_name.
    assert!(store.set_model("nope", "openai", "gpt-5").await.is_err());
}

#[tokio::test]
async fn model_persistence_survives_reopening_the_database() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");

    {
        let store = SqliteSessionStore::new(Some(path.clone())).unwrap();
        store.ensure_session("s1").await.unwrap();
        store.set_model("s1", "zhipu", "glm-4.6").await.unwrap();
    }

    // A new process opens the same file and reads the pair back — this is the resume path.
    let reopened = SqliteSessionStore::new(Some(path)).unwrap();
    assert_eq!(
        reopened.get("s1").await.unwrap().model,
        Some(("zhipu".to_string(), "glm-4.6".to_string()))
    );
}

#[tokio::test]
async fn model_tolerates_databases_written_before_the_column_existed() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");

    // A pre-model-persistence database: the sessions table has neither `name` nor `model`.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id         TEXT PRIMARY KEY,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 title      TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO sessions (id, created_at, updated_at, title)
             VALUES ('s1', '2020-01-01T00:00:00+00:00', '2020-01-01T00:00:00+00:00', 'old');",
        )
        .unwrap();
    }

    let store = SqliteSessionStore::new(Some(path)).expect("opening migrates the table");
    assert_eq!(
        store.get("s1").await.unwrap().model,
        None,
        "backfilled as unset"
    );
    store.set_model("s1", "deepseek", "glm-4.6").await.unwrap();
    assert_eq!(
        store.get("s1").await.unwrap().model,
        Some(("deepseek".to_string(), "glm-4.6".to_string()))
    );
}

// ---------------------------------------------------------------- estimator

/// Characters per token for the estimator's fallback. The provider-reported count for
/// `fixtures/token_sample.txt` was measured once, against the harness model
/// (`nvidia/deepseek-ai/deepseek-v4.1-flash`), with the sample as a single user message and no system
/// prompt; the command that prints it is `python3 scripts/measure-token-sample.py`. The tolerance
/// asserted below is [`HEURISTIC_TOLERANCE`].
const REPORTED_PROMPT_TOKENS: u32 = 397;

#[tokio::test]
async fn heuristic_and_reported_estimates_agree_on_a_fixed_sample() {
    let sample = include_str!("fixtures/token_sample.txt");
    let messages = vec![Message::user(sample)];

    let heuristic = heuristic_tokens(&messages);
    assert_eq!(heuristic as usize, sample.len().div_ceil(CHARS_PER_TOKEN));

    let reported = REPORTED_PROMPT_TOKENS as f64;
    let relative = (heuristic as f64 - reported).abs() / reported;
    assert!(
        relative <= HEURISTIC_TOLERANCE,
        "heuristic {} vs reported {} is {:.1}% off, tolerance is {:.0}%",
        heuristic,
        REPORTED_PROMPT_TOKENS,
        relative * 100.0,
        HEURISTIC_TOLERANCE * 100.0
    );
}

#[tokio::test]
async fn estimate_prefers_reported_usage_and_adds_the_heuristic_tail() {
    let mut with_usage = Message::assistant("an answer");
    with_usage.usage = Some(TokenUsage {
        prompt_tokens: 1_000,
        completion_tokens: 200,
        total_tokens: 1_200,
    });
    let tail = Message::user("x".repeat(400));

    let estimate = estimate_tokens(&[with_usage, tail]);
    assert_eq!(estimate.source, EstimateSource::Reported);
    assert_eq!(
        estimate.tokens,
        1_200 + 100,
        "reported (prompt+completion) plus the 400-char tail"
    );

    let heuristic_only = estimate_tokens(&[Message::user("x".repeat(400))]);
    assert_eq!(heuristic_only.source, EstimateSource::Heuristic);
    assert_eq!(heuristic_only.tokens, 100);
    assert_eq!(estimate_tokens(&[]).tokens, 0);
}

// ---------------------------------------------------------------- compaction

#[tokio::test]
async fn compact_folds_the_oldest_turns_and_bounds_the_vector() {
    let history = history(6);
    let provider = CannedProvider::new("four questions were answered");

    let outcome = compact(&provider, "canned-model", &history, Some(2))
        .await
        .unwrap()
        .expect("six turns with two kept is four folded");

    assert_eq!(outcome.folded_turns, 4);
    assert_eq!(outcome.folded_ids.len(), 8);
    assert_eq!(outcome.kept.len(), 4, "the last two turns stay verbatim");

    let bounded = outcome.bounded_messages("SYSTEM".to_string());
    assert_eq!(bounded.len(), 6, "system + summary + 4 kept turns");
    assert_eq!(bounded[0].role, Role::System);
    assert_eq!(bounded[0].content.as_deref(), Some("SYSTEM"));
    assert_eq!(bounded[1].role, Role::System);
    assert_eq!(
        bounded[1].content.as_deref(),
        Some("four questions were answered"),
        "the summary is the second message, right after the system prompt"
    );
    assert_eq!(bounded[2].content.as_deref(), Some("question 4"));
    assert!(bounded.len() < history.len(), "the sent vector is bounded");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn compact_folds_nothing_when_the_history_is_short_enough() {
    let history = history(2);
    let provider = CannedProvider::new("unused");

    assert_eq!(fold_boundary(&history, 2), 0);
    assert_eq!(fold_boundary(&history, 8), 0);
    let outcome = compact(&provider, "canned-model", &history, Some(2))
        .await
        .unwrap();
    assert!(outcome.is_none());
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "no foldable turn means no summarising call at all"
    );
}

#[tokio::test]
async fn compact_returns_an_error_when_the_provider_fails() {
    let history = history(6);
    let provider = FailingProvider::new();

    let result = compact(&provider, "unconfigured", &history, Some(2)).await;
    assert!(
        result.is_err(),
        "the caller decides what a failed summary means"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

/// A transcript that holds fewer turns than we keep is still foldable. A turn is not a
/// fixed size: one turn carrying a megabyte of inline image is as big as a hundred short
/// ones, so the turn rule can have nothing to fold while the transcript is far over
/// budget. Then the boundary moves to a message boundary — otherwise the user is left
/// with a context they cannot shrink, and `/compact` says there is nothing to compact.
#[tokio::test]
async fn a_big_transcript_with_few_turns_is_folded_at_a_message_boundary() {
    let mut big = history(2);
    big[0].message.content = Some("x".repeat(500_000));
    let provider = RecordingProvider::new("FOLDED");

    let outcome = compact(&provider, "canned-model", &big, Some(8))
        .await
        .unwrap()
        .expect("a transcript over budget is foldable, whatever its turn count");

    assert_eq!(
        outcome.folded_ids,
        vec![big[0].id],
        "the huge message is what gets folded"
    );
    assert_eq!(
        outcome.kept.len(),
        3,
        "the most recent messages stay verbatim"
    );
    assert_eq!(
        outcome.folded_turns, 1,
        "the fold took the turn the huge message was in"
    );
    assert_eq!(outcome.folded_messages, 1);
}

/// The summarising call must not carry the megabytes. A provider rejects a prompt that
/// long, and the fold would fail for the very transcript that needed it.
#[tokio::test]
async fn the_summarising_call_carries_a_bounded_transcript() {
    let mut big = history(2);
    big[0].message.content = Some("x".repeat(2_000_000));
    let provider = RecordingProvider::new("FOLDED");

    compact(&provider, "canned-model", &big, Some(8))
        .await
        .unwrap()
        .expect("foldable");

    let request = provider.last_request();
    let sent = request
        .iter()
        .map(|message| message.content.clone().unwrap_or_default())
        .collect::<String>();
    assert!(
        sent.len() < 100_000,
        "the transcript that goes to the summariser is bounded, got {} chars",
        sent.len()
    );
    assert!(
        sent.contains("more characters cut"),
        "the cut is marked, so the summary can still say what was cut"
    );
}

/// The folded region is budgeted from its newest end.
///
/// The turns immediately before the kept tail are the working state the next turn continues
/// from. A head-first cut — render the oldest messages and stop at the budget — replaces
/// exactly those with a truncation marker, so the summary describes the opening of the session
/// and says nothing about what was happening when the fold ran. That reads as the model
/// forgetting its own next step right after a compaction.
#[tokio::test]
async fn the_summarising_call_keeps_the_newest_folded_turns() {
    let mut transcript = history(40);
    for stored in transcript.iter_mut() {
        if let Some(content) = stored.message.content.as_mut() {
            content.push_str(&"f".repeat(4_000));
        }
    }
    // The compressed head of an earlier fold: a handful of lines, never the thing to cut.
    let mut prior = stored(Role::System, "PRIOR SUMMARY LAYER".to_string());
    prior.is_summary = true;
    transcript.insert(0, prior);
    // Turn 37 is the last folded turn (turns 38 and 39 are kept). Its user line is the state
    // the summary must carry forward.
    transcript[75].message.content =
        Some(format!("THE MOST RECENT FOLDED TURN {}", "g".repeat(4_000)));

    let provider = RecordingProvider::new("FOLDED");
    compact(&provider, "canned-model", &transcript, Some(2))
        .await
        .unwrap()
        .expect("foldable");

    let request = provider.last_request();
    let sent = request
        .iter()
        .map(|message| message.content.clone().unwrap_or_default())
        .collect::<String>();
    assert!(
        sent.contains("THE MOST RECENT FOLDED TURN"),
        "the newest folded turn is in the summariser's transcript"
    );
    assert!(
        sent.contains("PRIOR SUMMARY LAYER"),
        "a previous summary survives the cut: it is the only trace of the oldest layers"
    );
    assert!(
        sent.contains("older messages cut"),
        "the cut is marked, and it is the older messages that were cut"
    );
    assert!(
        sent.len() < 100_000,
        "the transcript that goes to the summariser is still bounded, got {} chars",
        sent.len()
    );
    assert!(
        !sent.contains("transcript truncated"),
        "the tail of the folded region is never what gets thrown away"
    );
}

/// A tool result may not stand alone: the assistant message that called it has to be in
/// the kept tail as well, or the provider rejects the vector. So a boundary that lands
/// on a tool result moves forward.
#[tokio::test]
async fn the_kept_tail_never_starts_with_a_tool_result() {
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path": "a.rs"}),
        signature: None,
    };
    let mut transcript = vec![
        stored(Role::User, "look at this".to_string()),
        stored_with(Role::Assistant, String::new(), vec![call], None),
        stored_with(
            Role::Tool,
            String::new(),
            vec![],
            Some(ToolResult::success("call-1", "y".repeat(400_000))),
        ),
        stored(Role::Assistant, "here it is".to_string()),
    ];
    transcript[0].message.content = Some("x".repeat(500_000));

    let provider = RecordingProvider::new("FOLDED");
    let outcome = compact(&provider, "canned-model", &transcript, Some(8))
        .await
        .unwrap()
        .expect("foldable");

    assert!(
        outcome.kept.iter().all(|m| m.role != Role::Tool),
        "no orphan tool result survives in the kept tail"
    );
    assert_eq!(
        outcome.kept.first().map(|m| m.role),
        Some(Role::Assistant),
        "the kept tail opens on a message that can start a conversation"
    );
    assert!(
        outcome.folded_messages >= 2,
        "the huge message and the huge tool result are folded, got {}",
        outcome.folded_messages
    );
}

/// An assistant message issuing tool calls cannot open the kept tail: providers like Gemini
/// require a user turn before any function call turn, and tool calls without the user prompt
/// are orphaned. The boundary must advance forward past the tool loop.
#[tokio::test]
async fn the_kept_tail_never_starts_with_an_assistant_tool_call() {
    let call1 = ToolCall {
        id: "call-1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path": "a.rs"}),
        signature: None,
    };
    let call2 = ToolCall {
        id: "call-2".to_string(),
        name: "search_files".to_string(),
        arguments: serde_json::json!({"pattern": "foo"}),
        signature: None,
    };
    let mut transcript = vec![
        stored(Role::User, "initial prompt".to_string()),
        stored_with(Role::Assistant, String::new(), vec![call1], None),
        stored_with(
            Role::Tool,
            String::new(),
            vec![],
            Some(ToolResult::success("call-1", "file content")),
        ),
        stored_with(Role::Assistant, String::new(), vec![call2], None),
        stored_with(
            Role::Tool,
            String::new(),
            vec![],
            Some(ToolResult::success("call-2", "y".repeat(400_000))),
        ),
        stored(Role::User, "next user task".to_string()),
        stored(Role::Assistant, "completed".to_string()),
    ];
    transcript[0].message.content = Some("x".repeat(500_000));

    let provider = RecordingProvider::new("FOLDED");
    let outcome = compact(&provider, "canned-model", &transcript, Some(8))
        .await
        .unwrap()
        .expect("foldable");

    let first = outcome.kept.first().expect("kept tail not empty");
    assert!(
        !(first.role == Role::Assistant && !first.tool_calls.is_empty()),
        "first kept message cannot be an assistant issuing tool calls"
    );
    assert_ne!(
        first.role,
        Role::Tool,
        "first kept message cannot be a tool result"
    );
    assert_eq!(
        first.role,
        Role::User,
        "kept tail advances to the clean user turn"
    );
    assert_eq!(first.content.as_deref(), Some("next user task"));
}

/// A transcript that fits the budget has nothing to fold, and must not spend a call.
#[tokio::test]
async fn a_small_transcript_with_few_turns_is_left_alone() {
    let history = history(2);
    let provider = CannedProvider::new("unused");

    assert_eq!(fold_boundary(&history, 8), 0);
    let outcome = compact(&provider, "canned-model", &history, Some(8))
        .await
        .unwrap();
    assert!(outcome.is_none());
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "a transcript under the budget needs no summarising call at all"
    );
}

/// A reasoning model can spend its whole budget on reasoning and answer with nothing.
/// The provider layer then hands back a marker that says so, and a compaction that took
/// it for a summary would have replaced the session with a sentence about reasoning.
/// So the call is retried with more room.
#[tokio::test]
async fn an_empty_summary_is_retried_with_a_bigger_budget() {
    let mut big = history(2);
    big[0].message.content = Some("x".repeat(500_000));
    let provider = ReasoningProvider::new();

    let outcome = compact(&provider, "reasoning-model", &big, Some(8))
        .await
        .unwrap()
        .expect("foldable");

    assert_eq!(
        outcome.summary, "the summary, with room to write it",
        "the retry's summary is the one that counts"
    );
    let budgets = provider.budgets.lock().unwrap().clone();
    assert_eq!(budgets.len(), 2, "one call, one retry");
    assert!(
        budgets[1] > budgets[0],
        "the retry asks for more room: {} then {}",
        budgets[0],
        budgets[1]
    );
}

/// A model that never writes a summary is a failure, not a summary: the fold must not
/// go through with the marker in place of one.
#[tokio::test]
async fn a_summary_that_never_arrives_is_an_error() {
    let mut big = history(2);
    big[0].message.content = Some("x".repeat(500_000));
    let provider = SilentProvider::new();

    let result = compact(&provider, "silent-model", &big, Some(8)).await;

    assert!(
        result.is_err(),
        "a fold with no summary must not go through"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "one call, one retry"
    );
}

// ---------------------------------------------------------------- the agent loop

/// The explicit fold policy: `compact_at_tokens` and `keep_recent_turns`, which is what `/compact`
/// and the default turn loop use. Auto-compact is off here because it moves both numbers, and the
/// tests built on this helper assert *these* two.
fn agent_config() -> AgentConfig {
    AgentConfig {
        compact_at_tokens: 1,
        keep_recent_turns: 2,
        auto_compact_enabled: false,
        ..AgentConfig::default()
    }
}

async fn seeded_store(turns: usize) -> (TempDir, std::path::PathBuf, Arc<SqliteSessionStore>) {
    let (dir, path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    for turn in 0..turns {
        store
            .append("s1", &Message::user(format!("question {}", turn)), false)
            .await
            .unwrap();
        store
            .append("s1", &Message::assistant(format!("answer {}", turn)), false)
            .await
            .unwrap();
    }
    (dir, path, Arc::new(store))
}

/// An observer that fires when a fold reports itself finished, plus the notification to await.
///
/// A fold runs off the turn's path, so a test that wants to look at what it wrote waits for the
/// announcement instead of for `run_turn` to return.
fn fold_finished() -> (AgentObserver, Arc<tokio::sync::Notify>) {
    let notify = Arc::new(tokio::sync::Notify::new());
    let signal = notify.clone();
    let observer: AgentObserver = Arc::new(move |event| {
        if matches!(event, AgentEvent::CompactionFinished { .. }) {
            signal.notify_one();
        }
    });
    (observer, notify)
}

#[tokio::test]
async fn run_turn_does_not_wait_for_the_fold_and_the_next_turn_reaps_it() {
    let (_dir, path, store) = seeded_store(6).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED"));

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    });
    let (observer, folded) = fold_finished();
    agent.set_observer(observer);

    let outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome.reply, "done");

    // The request this turn sent is the transcript as it stood: the fold did not hold it back.
    let sent = main.last_request();
    assert_eq!(
        sent.len(),
        14,
        "system + the full 12-message transcript + the new user line"
    );
    assert_eq!(sent[0].role, Role::System);
    assert_eq!(sent[1].content.as_deref(), Some("question 0"));
    assert_eq!(sent[sent.len() - 1].content.as_deref(), Some("hello"));

    // The fold ran off that path, and it announced itself when it was done.
    folded.notified().await;
    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "one summarising call"
    );
    assert_eq!(main.call_count(), 1);

    // What it wrote is a bounded transcript: the kept turns plus the summary.
    assert_eq!(count_rows(&path, "compacted_at IS NOT NULL"), 8);
    let replay = store.replay("s1", 200).await.unwrap();
    assert!(replay.history.iter().any(|stored| stored.is_summary));

    // The next turn is the one that sends the bounded transcript.
    let second = agent
        .run_turn("again", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(second.reply, "done");
    let sent = main.last_request();
    assert!(
        sent.iter()
            .any(|message| message.content.as_deref() == Some("FOLDED")),
        "the next request carries the summary"
    );
    assert_eq!(
        sent.len(),
        9,
        "system + 4 kept messages + the summary + the first turn's own exchange + the new user line"
    );
}

#[tokio::test]
async fn run_turn_survives_a_failing_compaction_provider_and_does_not_retry() {
    let (_dir, path, store) = seeded_store(6).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(FailingProvider::new());

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "unconfigured".to_string(),
    });
    let (observer, folded) = fold_finished();
    agent.set_observer(observer);

    let outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        outcome.reply, "done",
        "the turn proceeds on the full history"
    );

    // The failure happens off the turn's path, where it cannot reach the reply.
    folded.notified().await;
    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "a failed compaction is not retried"
    );

    let sent = main.last_request();
    assert_eq!(
        sent.len(),
        14,
        "system + the full 12-message transcript + the user line"
    );
    assert_eq!(sent[1].role, Role::User);
    assert_eq!(sent[1].content.as_deref(), Some("question 0"));

    // Every original message is still there, none folded, no summary written.
    assert_eq!(
        count_rows(&path, "1 = 1"),
        14,
        "12 replayed + the user line + the reply"
    );
    assert_eq!(count_rows(&path, "compacted_at IS NOT NULL"), 0);
    assert_eq!(count_rows(&path, "is_summary = 1"), 0);
}

/// `/compact` is the user asking for a fold. When the summary call fails, that is the
/// answer — it must not be reported as "there was nothing to compact", which sends the
/// user looking for a transcript that is not there.
#[tokio::test]
async fn the_explicit_compact_reports_a_failed_summary_as_a_failure() {
    let (_dir, path, store) = seeded_store(6).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(FailingProvider::new());

    let agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "unconfigured".to_string(),
    });

    let error = agent.compact_current_session().await.unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains("no credential for this provider"),
        "the real reason is reported, got: {text}"
    );
    assert!(
        !text.contains("nothing to compact"),
        "a failed summary is not an empty transcript, got: {text}"
    );
    assert_eq!(
        count_rows(&path, "compacted_at IS NOT NULL"),
        0,
        "nothing was folded, so nothing is marked folded"
    );
}

/// `/compact` without a compaction provider says so, instead of claiming the transcript
/// has nothing to fold.
#[tokio::test]
async fn the_explicit_compact_says_when_no_compaction_provider_is_set_up() {
    let (_dir, _path, store) = seeded_store(6).await;
    let main = Arc::new(RecordingProvider::new("done"));

    let agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    });

    let error = agent.compact_current_session().await.unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains("compaction is unavailable"),
        "the missing setup is named, got: {text}"
    );
}

#[tokio::test]
async fn summarize_session_produces_compact_summary() {
    let (_dir, _path, store) = seeded_store(3).await;
    let replayed = store.replay("s1", usize::MAX).await.unwrap();
    let provider = CannedProvider::new("The user asked 3 questions and got 3 answers.");

    let summary = summarize_session(&provider, "test-model", &replayed.history)
        .await
        .unwrap();

    assert_eq!(summary, "The user asked 3 questions and got 3 answers.");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn agent_summarize_current_session_works_and_rejects_empty() {
    let (_dir, _path, store) = seeded_store(2).await;
    let provider = Arc::new(CannedProvider::new("Session summary content"));

    let agent = Agent::new(
        provider.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    });

    let summary = agent.summarize_current_session("test-model").await.unwrap();
    assert_eq!(summary, "Session summary content");

    // Fresh empty session
    let empty_agent = Agent::new(
        provider.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "empty_sess".to_string(),
        resume_max_messages: 200,
    });

    assert!(empty_agent
        .summarize_current_session("test-model")
        .await
        .is_err());
}

#[tokio::test]
async fn fork_session_copies_all_messages_and_metadata() {
    let (_tmp, _path, store) = seeded_store(5).await;

    // Set model on s1
    store
        .set_model("s1", "nvidia", "deepseek-v4.1")
        .await
        .unwrap();
    store.set_name("s1", "feature-work").await.unwrap();

    let forked_id = store.fork_session("s1", Some("My Fork")).await.unwrap();

    // Verify record
    let forked_rec = store.get(&forked_id).await.unwrap();
    assert_eq!(forked_rec.title, "My Fork");
    assert_eq!(forked_rec.name, "feature-work");
    assert_eq!(
        forked_rec.model,
        Some(("nvidia".to_string(), "deepseek-v4.1".to_string()))
    );

    // Verify messages
    let orig_replay = store.replay("s1", 100).await.unwrap();
    let forked_replay = store.replay(&forked_id, 100).await.unwrap();

    assert_eq!(orig_replay.total, forked_replay.total);
    assert_eq!(orig_replay.history.len(), forked_replay.history.len());
    for (orig, forked) in orig_replay.history.iter().zip(forked_replay.history.iter()) {
        assert_eq!(orig.message.role, forked.message.role);
        assert_eq!(orig.message.content, forked.message.content);
        assert_eq!(orig.seq, forked.seq);
    }
}

// ---------------------------------------------------------------- the gauge across a fold

/// A transcript whose newest reply carries the usage a real provider reported, as every reply in a
/// live session does: it is the number the gauge shows, and the number a fold has to retire.
async fn seeded_store_with_reported_usage(
    turns: usize,
    reported_prompt_tokens: u32,
) -> (TempDir, std::path::PathBuf, Arc<SqliteSessionStore>) {
    let (dir, path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    for turn in 0..turns {
        store
            .append(
                "s1",
                &Message::user(format!("question {} {}", turn, "x".repeat(400))),
                false,
            )
            .await
            .unwrap();
        let mut reply = Message::assistant(format!("answer {}", turn));
        if turn + 1 == turns {
            // Reported for the request that produced this reply, which covers the transcript as it
            // stood before the fold.
            reply.usage = Some(TokenUsage {
                prompt_tokens: reported_prompt_tokens,
                completion_tokens: 20,
                total_tokens: reported_prompt_tokens + 20,
            });
        }
        store.append("s1", &reply, false).await.unwrap();
    }
    (dir, path, Arc::new(store))
}

/// A fold has to announce the shrunken context itself: `/compact` runs between turns, so nothing
/// recomputes the figure until the next request reports usage, and until then the readout keeps
/// describing the history that was just folded away.
#[tokio::test]
async fn a_fold_announces_the_shrunken_context_without_waiting_for_a_model_call() {
    let (_dir, _path, store) = seeded_store_with_reported_usage(6, 90_000).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED"));

    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = events.clone();

    let agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    })
    .with_observer(Arc::new(move |event| {
        let mut log = recorder.lock().unwrap();
        match event {
            AgentEvent::CompactionStarted { .. } => log.push("started".to_string()),
            AgentEvent::CompactionFinished { folded, error } => log.push(format!(
                "finished folded={} error={}",
                folded.is_some(),
                error.is_some()
            )),
            AgentEvent::ContextTokensUpdated { tokens_used } => {
                log.push(format!("ctx {tokens_used}"))
            }
            _ => {}
        }
    }));

    agent.compact_current_session().await.unwrap();

    let log = events.lock().unwrap().clone();
    assert!(
        log.iter()
            .any(|line| line == "finished folded=true error=false"),
        "the fold itself is reported: {log:?}"
    );
    let finished_at = log
        .iter()
        .position(|line| line.starts_with("finished"))
        .expect("the fold is announced");
    let announced = log[finished_at + 1..]
        .iter()
        .find_map(|line| line.strip_prefix("ctx "))
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or_else(|| panic!("the fold must refresh the gauge, log: {log:?}"));

    // The bound is an order of magnitude, not a number: the bounded transcript is a summary plus two
    // kept turns, and only the pre-fold report was 90k.
    assert!(
        announced < 30_000,
        "the announced figure describes the bounded transcript, got {announced}"
    );
}

/// `/compact` refreshes the gauge through `estimate_session_tokens` *after* the fold, and the kept
/// tail still carries the usage the provider reported for the pre-fold prompt. Preferring that
/// report blindly hands the gauge the size of the history the fold just removed, which is the
/// readout the user is left staring at until the next request.
#[tokio::test]
async fn estimate_after_a_fold_ignores_the_usage_of_the_pre_fold_transcript() {
    let (_dir, _path, store) = seeded_store_with_reported_usage(6, 90_000).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED"));

    let agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    });

    assert_eq!(
        agent.estimate_session_tokens().await.unwrap(),
        90_020,
        "with no fold newer than it, the last reported usage is the size of the session"
    );

    agent.compact_current_session().await.unwrap();

    let after = agent.estimate_session_tokens().await.unwrap();
    assert!(
        after < 30_000,
        "the fold retires the pre-fold figure, got {after}"
    );

    // And it is the live transcript's own estimate, not a number borrowed from the folded rows.
    let replayed = store.replay("s1", 200).await.unwrap();
    let live: Vec<Message> = replayed
        .history
        .iter()
        .map(|stored| stored.message.clone())
        .collect();
    assert_eq!(after, heuristic_tokens(&live));
}

/// A fold boundary is never the end of the transcript.
///
/// The kept count is a *floor*: when the session holds fewer turns than that, the boundary moves
/// to a token budget and the newest messages stay. Asking to keep more turns than exist is an
/// ordinary request — it is what a freshly configured threshold does on a young session — and it
/// must not fold the conversation away.
#[tokio::test]
async fn a_keep_count_larger_than_the_transcript_still_keeps_the_newest_messages() {
    let mut big = history(3);
    big[0].message.content = Some("x".repeat(500_000));

    let boundary = fold_boundary(&big, Some(20));
    assert!(
        boundary < big.len(),
        "something is foldable in an oversized transcript"
    );
    assert!(
        boundary > 0,
        "but the newest messages are never the ones folded"
    );

    let provider = RecordingProvider::new("FOLDED");
    let outcome = compact(&provider, "canned-model", &big, Some(20))
        .await
        .unwrap()
        .expect("an oversized transcript is foldable whatever the keep count");

    assert!(!outcome.kept.is_empty(), "the kept tail is never empty");
    assert_eq!(
        outcome.summary, "FOLDED",
        "the folded turns are replaced by a summary, not by nothing"
    );
}

#[tokio::test]
async fn compact_accepts_optional_keep_recent_turns_none() {
    let mut big = history(2);
    big[0].message.content = Some("x".repeat(500_000));
    let provider = RecordingProvider::new("FOLDED WITH NONE");

    let outcome = compact(&provider, "canned-model", &big, None)
        .await
        .unwrap()
        .expect("none keep_recent_turns still folds budget-exceeding messages");

    assert_eq!(outcome.summary, "FOLDED WITH NONE");
}

/// Auto-compact is a lower bar on the one fold, not a second mechanism — and it does not hold the
/// turn that trips it.
///
/// The fold is a bound on the *next* request. The turn that crosses the threshold is answered on
/// the transcript it already has; the fold lands in the store while the answer is being read. What
/// it must leave behind is a summary the next request carries: marking rows folded and writing
/// nothing in their place is what reads as the agent losing the conversation.
#[tokio::test]
async fn auto_compact_folds_in_the_background_and_the_next_request_carries_the_summary() {
    let (_dir, path, store) = seeded_store(6).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED BY AUTO-COMPACT"));

    let mut cfg = agent_config();
    cfg.auto_compact_enabled = true;
    cfg.auto_compact_threshold = 10; // fires on the first turn
    cfg.auto_compact_turns = 2;

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    });
    let (observer, folded) = fold_finished();
    agent.set_observer(observer);

    let outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome.reply, "done");
    assert_eq!(
        main.last_request().len(),
        14,
        "the turn that tripped the threshold did not wait for the fold"
    );

    // The fold reports itself when it is done, off the turn's path.
    folded.notified().await;
    assert_eq!(summariser.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        count_rows(&path, "is_summary = 1"),
        1,
        "the folded turns left a summary behind"
    );
    assert_eq!(count_rows(&path, "compacted_at IS NOT NULL"), 8);

    // And the next turn is the one that sends the bounded transcript.
    let second = agent
        .run_turn("second", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(second.reply, "done");
    let sent = main.last_request();
    assert!(
        sent.iter()
            .any(|message| message.content.as_deref() == Some("FOLDED BY AUTO-COMPACT")),
        "the next request carries the summary"
    );
    assert!(
        sent.len() < 14,
        "and it is smaller than the transcript it replaced: {} messages",
        sent.len()
    );
    assert_eq!(sent[sent.len() - 1].content.as_deref(), Some("second"));
}

/// Answers only after a delay, so a fold spawned at the start of a turn has certainly finished by
/// the time that turn's own checks run again.
struct SlowProvider {
    delay: std::time::Duration,
}

impl SlowProvider {
    fn new(delay_ms: u64) -> Self {
        Self {
            delay: std::time::Duration::from_millis(delay_ms),
        }
    }
}

#[async_trait]
impl ModelProvider for SlowProvider {
    fn provider_id(&self) -> &str {
        "slow"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["slow-model".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        tokio::time::sleep(self.delay).await;
        Ok(Message::assistant("done"))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(Vec::new())
    }
}

/// A turn that crosses the bar on its own reply folds by itself, with no second user message.
///
/// The check at the top of a turn only runs when a turn starts, so a transcript that grew past the
/// threshold *during* a turn stayed unfolded until the user came back — which reads as an
/// auto-compaction that only works when you prompt it. The turn's own reply is weight enough to trip
/// the bar, so the turn that wrote it is the one that starts the fold.
#[tokio::test]
async fn auto_compact_folds_when_the_turn_itself_crosses_the_bar() {
    let (_dir, path, store) = seeded_store(3).await;
    let main = Arc::new(RecordingProvider::new(&"x".repeat(2_000_000)));
    let summariser = Arc::new(CannedProvider::new("FOLDED WITHOUT BEING ASKED"));

    let mut cfg = agent_config();
    // The long-standing bar stays out of the way, and the auto-compact bar sits between what the
    // session weighs before this turn and what it weighs after it.
    cfg.compact_at_tokens = 500_000;
    cfg.auto_compact_enabled = true;
    cfg.auto_compact_threshold = 100_000;
    cfg.auto_compact_turns = 1;

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    });
    let (observer, folded) = fold_finished();
    agent.set_observer(observer);

    // One turn, and only one: nothing else is sent to the agent.
    let outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome.reply.chars().count(), 2_000_000);
    assert_eq!(
        main.call_count(),
        1,
        "the turn ran on the transcript it had; the fold is not part of it"
    );

    tokio::time::timeout(std::time::Duration::from_secs(10), folded.notified())
        .await
        .expect("the turn must start the fold itself, without another user message");

    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "one summarising call"
    );
    assert_eq!(
        count_rows(&path, "is_summary = 1"),
        1,
        "the fold wrote its summary"
    );
    assert!(
        count_rows(&path, "compacted_at IS NOT NULL") > 0,
        "and it folded rows rather than only marking the session"
    );
}

/// One fold per turn — even though the turn's own request still measures the *unfolded* transcript.
///
/// A fold that lands while a turn runs rewrites the stored history, and the turn's remaining
/// model calls go on reporting their own (stale, larger) size. The re-armed check measures the
/// STORED transcript, so it folds again only when the store itself is still over the bar — never
/// because of a stale in-memory report, and never folding the summary it just wrote.
#[tokio::test]
async fn a_turn_folds_once_even_when_its_reports_still_describe_the_unfolded_transcript() {
    let (_dir, path, store) = seeded_store_with_reported_usage(6, 20_000).await;
    // Slow enough that the fold from the top of the turn has finished before the turn ends, which is
    // the state the check has to get right: nothing in flight, and a transcript already rewritten.
    let main = Arc::new(SlowProvider::new(300));
    let summariser = Arc::new(CannedProvider::new("FOLDED"));

    let mut cfg = agent_config();
    // The bar sits between the seeded transcript's REPORTED size (20k, crossed before the turn's
    // own work starts) and the folded store (a summary plus the 2 kept turns plus the system
    // prompt ≈ 3.3k). The re-armed check therefore finds the store under the bar and answers the
    // crossing exactly once.
    cfg.compact_at_tokens = 5_000;
    cfg.auto_compact_enabled = false;
    cfg.keep_recent_turns = 2;

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    });
    let (observer, folded) = fold_finished();
    agent.set_observer(observer);

    let outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome.reply, "done");

    folded.notified().await;
    // Room for a second fold to land, were one going to.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "the crossing was folded once"
    );
    assert_eq!(
        count_rows(&path, "is_summary = 1"),
        1,
        "one summary, not a summary of the summary"
    );
}

/// The configured turn count is how many recent turns to *keep*, and it may exceed what the
/// session holds — that is the ordinary state of a young session on a low threshold. This is the
/// shape that used to take the whole transcript with it: no cutoff row was found, so every message
/// was marked folded, and the next replay had nothing to hand the model.
#[tokio::test]
async fn auto_compact_never_empties_a_session_that_holds_fewer_turns_than_it_keeps() {
    let (_dir, path, store) = temp_store();
    store.ensure_session("s_auto").await.unwrap();
    // Three turns, the first of them big enough that the transcript is over any budget.
    store
        .append(
            "s_auto",
            &Message::user(format!("opening question {}", "x".repeat(500_000))),
            false,
        )
        .await
        .unwrap();
    store
        .append("s_auto", &Message::assistant("first answer"), false)
        .await
        .unwrap();
    store
        .append("s_auto", &Message::user("second question"), false)
        .await
        .unwrap();
    store
        .append("s_auto", &Message::assistant("second answer"), false)
        .await
        .unwrap();

    let store = Arc::new(store);
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED"));

    let mut cfg = agent_config();
    cfg.auto_compact_enabled = true;
    cfg.auto_compact_threshold = 10;
    cfg.auto_compact_turns = 20; // more turns than this session has

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(MockEvolver),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s_auto".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    });
    let (observer, folded) = fold_finished();
    agent.set_observer(observer);

    agent
        .run_turn("new prompt", CancellationToken::new())
        .await
        .unwrap();
    folded.notified().await;

    let live = store.replay("s_auto", 100).await.unwrap();
    assert!(
        live.total > 0,
        "the session still holds the conversation: {} live messages",
        live.total
    );
    assert!(
        live.history.iter().any(|stored| stored.is_summary),
        "what was folded is represented by a summary"
    );
    assert_eq!(
        count_rows(&path, "is_summary = 1"),
        1,
        "exactly one fold happened, and it wrote its summary"
    );

    // The newest turns survived verbatim, and the model was sent them.
    let sent = main.last_request();
    assert!(
        sent.iter()
            .any(|message| message.content.as_deref() == Some("second question")),
        "the most recent turns are still in the request"
    );
}
