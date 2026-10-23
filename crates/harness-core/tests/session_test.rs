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

    let outcome = compact(&provider, "canned-model", &history, 2)
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
    let outcome = compact(&provider, "canned-model", &history, 2)
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

    let result = compact(&provider, "unconfigured", &history, 2).await;
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

    let outcome = compact(&provider, "canned-model", &big, 8)
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

    compact(&provider, "canned-model", &big, 8)
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
    let outcome = compact(&provider, "canned-model", &transcript, 8)
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

/// A transcript that fits the budget has nothing to fold, and must not spend a call.
#[tokio::test]
async fn a_small_transcript_with_few_turns_is_left_alone() {
    let history = history(2);
    let provider = CannedProvider::new("unused");

    assert_eq!(fold_boundary(&history, 8), 0);
    let outcome = compact(&provider, "canned-model", &history, 8)
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

    let outcome = compact(&provider, "reasoning-model", &big, 8)
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

    let result = compact(&provider, "silent-model", &big, 8).await;

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

fn agent_config() -> AgentConfig {
    AgentConfig {
        compact_at_tokens: 1,
        keep_recent_turns: 2,
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

#[tokio::test]
async fn run_turn_compacts_once_and_sends_the_summary_first() {
    let (_dir, path, store) = seeded_store(6).await;
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

    let outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome.reply, "done");
    assert_eq!(
        outcome.compaction,
        Some(CompactionNotice {
            session_id: "s1".to_string(),
            turns: 4,
            messages: 8,
        })
    );
    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "one summarising call"
    );
    assert_eq!(main.call_count(), 1);

    let sent = main.last_request();
    assert_eq!(sent[0].role, Role::System);
    assert_eq!(sent[1].content.as_deref(), Some("FOLDED"));
    assert_eq!(sent[sent.len() - 1].content.as_deref(), Some("hello"));
    assert_eq!(
        sent.len(),
        7,
        "system + summary + 4 kept turns + the new user line"
    );
    assert!(
        sent.len() < 1 + 12 + 1,
        "the request is smaller than the full transcript it replaced"
    );

    // The summary is in the store, so the next process resumes the bounded transcript.
    let replay = store.replay("s1", 200).await.unwrap();
    assert!(replay.history[0].is_summary);
    assert_eq!(replay.history[0].message.content.as_deref(), Some("FOLDED"));
    assert_eq!(count_rows(&path, "compacted_at IS NOT NULL"), 8);
}

#[tokio::test]
async fn run_turn_survives_a_failing_compaction_provider_and_does_not_retry() {
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

    let outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        outcome.reply, "done",
        "the turn proceeds on the full history"
    );
    assert!(outcome.compaction.is_none());
    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "a failed compaction is not retried inside the same turn"
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
