#![allow(dead_code, unused_imports)]
//! Session persistence (`sessions.db`), replay bounds, and bounded context (compaction).
//!
//! The store tests run against a real sqlite file in a temp dir and then count rows with `rusqlite`
//! directly, because the acceptance criteria are stated as `sqlite3` row counts — a `:memory:`
//! database has no rows to count from outside. Compaction runs against fake providers: the
//! summarising call is a plain `ModelProvider::complete`, so a canned provider pins the boundary
//! and a failing one proves the non-fatal path.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::Connection;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use harness_core::*;

// ---------------------------------------------------------------- fakes

/// Answers every completion with one canned summary; counts the calls so a retry is visible.
pub struct CannedProvider {
    pub summary: String,
    pub calls: AtomicUsize,
}

impl CannedProvider {
    pub     fn new(summary: &str) -> Self {
        Self {
            summary: summary.to_string(),
            calls: AtomicUsize::new(0),
        }
    }
}

/// Always fails, the way a provider with no credentials fails.
pub struct FailingProvider {
    pub calls: AtomicUsize,
}

impl FailingProvider {
    pub     fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

/// Returns a fixed reply and remembers every request it was sent.
pub struct RecordingProvider {
    pub reply: String,
    pub received: Mutex<Vec<Vec<Message>>>,
}

impl RecordingProvider {
    pub     fn new(reply: &str) -> Self {
        Self {
            reply: reply.to_string(),
            received: Mutex::new(Vec::new()),
        }
    }

    pub     fn last_request(&self) -> Vec<Message> {
        self.received
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }

    pub     fn call_count(&self) -> usize {
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
pub struct ReasoningProvider {
    pub calls: AtomicUsize,
    pub budgets: Mutex<Vec<u32>>,
}

impl ReasoningProvider {
    pub     fn new() -> Self {
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
pub struct SilentProvider {
    pub calls: AtomicUsize,
}

impl SilentProvider {
    pub     fn new() -> Self {
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

pub struct MockMemory;

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

pub struct MockNotes;

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
            ..Default::default()
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

// ---------------------------------------------------------------- helpers

pub fn temp_store() -> (TempDir, std::path::PathBuf, SqliteSessionStore) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("sessions.db");
    let store = SqliteSessionStore::new(Some(path.clone())).expect("open the sessions db");
    (dir, path, store)
}

/// Rows in `messages` straight out of the file, the way `sqlite3` would count them.
pub fn count_rows(path: &std::path::Path, filter: &str) -> i64 {
    let conn = Connection::open(path).expect("open the sessions db read-only for counting");
    conn.query_row(
        &format!("SELECT COUNT(*) FROM messages WHERE {}", filter),
        [],
        |row| row.get(0),
    )
    .expect("count rows")
}

/// A history of `turns` user/assistant pairs, one turn per index.
pub fn history(turns: usize) -> Vec<StoredMessage> {
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
pub fn stored(role: Role, content: String) -> StoredMessage {
    stored_with(role, content, vec![], None)
}

/// One stored message, with tool calls and a tool result.
pub fn stored_with(
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


pub fn agent_config() -> AgentConfig {
    AgentConfig {
        compact_at_tokens: 1,
        keep_recent_turns: 2,
        auto_compact_enabled: false,
        ..AgentConfig::default()
    }
}

pub async fn seeded_store(turns: usize) -> (TempDir, std::path::PathBuf, Arc<SqliteSessionStore>) {
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
pub fn fold_finished() -> (AgentObserver, Arc<tokio::sync::Notify>) {
    let notify = Arc::new(tokio::sync::Notify::new());
    let signal = notify.clone();
    let observer: AgentObserver = Arc::new(move |event| {
        if matches!(event, AgentEvent::CompactionFinished { .. }) {
            signal.notify_one();
        }
    });
    (observer, notify)
}

/// A transcript whose newest reply carries the usage a real provider reported, as every reply in a
/// live session does: it is the number the gauge shows, and the number a fold has to retire.
pub async fn seeded_store_with_reported_usage(
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
            reply.usage = Some(TokenUsage::estimated(reported_prompt_tokens, 20));
        }
        store.append("s1", &reply, false).await.unwrap();
    }
    (dir, path, Arc::new(store))
}

