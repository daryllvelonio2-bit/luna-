//! The fold watermark must re-arm within a turn.
//!
//! A turn is not a fixed number of model calls: an agentic turn can run for hundreds of tool
//! rounds, and one fold at its start does not bound the transcript for the rest of it. The
//! watermark exists to stop a fold from re-folding its own summary — not to pin the transcript
//! against every later crossing the same turn makes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Connection;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use harness_core::*;

/// Answers large on the call indexes in `big_at`, small otherwise.
///
/// Heavy replies push the transcript over the fold bar; light replies let it settle back under.
/// This is the shape of a long agentic turn: growth, a fold, then more growth.
struct ShrinkReplyProvider {
    big_at: Vec<usize>,
    rounds: usize,
    calls: AtomicUsize,
    big: String,
}

impl ShrinkReplyProvider {
    fn new(big_at: Vec<usize>, rounds: usize, big_len: usize) -> Self {
        Self {
            big_at,
            rounds,
            calls: AtomicUsize::new(0),
            big: "x".repeat(big_len),
        }
    }
}

#[async_trait]
impl ModelProvider for ShrinkReplyProvider {
    fn provider_id(&self) -> &str {
        "shrink"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["shrink-model".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        // Give the background fold time to land before the later rounds: the gate can only
        // re-arm once it has seen the fold's summary in the store.
        if n >= 3 {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        // Round the tool loop on the early calls: a plain-text reply ends the turn, and the
        // mid-turn fold gate only runs between tool rounds.
        let mut message = if self.big_at.contains(&n) {
            Message::assistant(self.big.clone())
        } else {
            Message::assistant("small")
        };
        if n < self.rounds {
            message.tool_calls = vec![ToolCall {
                id: format!("call_{n}"),
                name: "list_files".to_string(),
                arguments: serde_json::json!({ "path": "." }),
                signature: None,
            }];
        }
        Ok(message)
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(Vec::new())
    }
}

struct CannedSummariser {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for CannedSummariser {
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
        Ok(Message::assistant("FOLDED"))
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
        Ok(false)
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

/// Counts the folds that reported themselves finished. A counter, not a Notify: two folds can
/// finish while nobody is awaiting, and a Notify stores only one permit for that case.
fn fold_counter() -> (AgentObserver, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let watcher = count.clone();
    let observer: AgentObserver = Arc::new(move |event| {
        if matches!(
            event,
            AgentEvent::CompactionFinished {
                folded: Some(_),
                error: None
            }
        ) {
            watcher.fetch_add(1, Ordering::SeqCst);
        }
    });
    (observer, count)
}

fn count_summaries(path: &std::path::Path) -> i64 {
    let conn = Connection::open(path).expect("open sessions db");
    conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE is_summary = 1",
        [],
        |row| row.get(0),
    )
    .expect("count summaries")
}

/// One turn that crosses the fold bar, folds, keeps growing past the bar again, and folds a
/// second time — with no second user message anywhere.
///
/// The old watermark logic skipped every later check in the turn once one fold had landed, so a
/// single unbounded agentic turn pinned an unbounded transcript: a real session grew from 110k to
/// 318k tokens under a 100k bar with the fold armed and idle.
#[tokio::test]
async fn a_long_turn_folds_again_when_it_crosses_the_bar_a_second_time() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = Arc::new(SqliteSessionStore::new(Some(path.clone())).expect("open store"));
    store.ensure_session("s1").await.unwrap();

    // Turn shape: two very large model replies (crossing the bar), then small ones.
    // big, small, big: the second big reply re-crosses the bar AFTER fold #1 landed.
    let main = Arc::new(ShrinkReplyProvider::new(vec![0, 2, 4], 6, 600_000));

    let cfg = AgentConfig {
        compact_at_tokens: 1_000_000, // the legacy bar stays out of the way
        keep_recent_turns: 1,
        auto_compact_enabled: true,
        auto_compact_threshold: 100_000,
        auto_compact_turns: 1,
        ..AgentConfig::default()
    };

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
        resume_max_messages: 500,
    })
    .with_compaction(CompactionSetup {
        provider: Arc::new(CannedSummariser {
            calls: AtomicUsize::new(0),
        }),
        model: "canned-model".to_string(),
    });

    let (observer, fold_count) = fold_counter();
    agent.set_observer(observer);

    let _outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();

    // Two folds are expected from this one turn.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while fold_count.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the turn folds twice: the first crossing, and its own re-crossing");

    let summaries = count_summaries(&path);
    assert!(
        summaries >= 2,
        "a long turn that re-crosses the bar folds again; got {summaries} summary rows"
    );
}
