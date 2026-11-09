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

use harness_core::session::{heuristic_tokens, sendable_messages};
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
        if n >= 2 {
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
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
        Ok(false)
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

struct ToolLoopUsageProvider {
    calls: AtomicUsize,
    rounds: usize,
    stale_usage: usize,
}

#[async_trait]
impl ModelProvider for ToolLoopUsageProvider {
    fn provider_id(&self) -> &str {
        "tool_loop"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["tool-loop-model".to_string()])
    }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n > 0 {
            let input_tokens = estimate_tokens(messages).tokens;
            assert!(
                input_tokens < 50_000,
                "round {n} prompt must be rebound to compacted messages (<50k), got {input_tokens}"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut message = Message::assistant("ok");
        let prompt_tokens = if n == 0 {
            self.stale_usage as u32
        } else {
            estimate_tokens(messages).tokens as u32
        };
        message.usage = Some(TokenUsage::estimated(prompt_tokens, 10));
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

#[tokio::test]
async fn auto_compact_does_not_repeat_and_updates_context_gauge_mid_turn() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = Arc::new(SqliteSessionStore::new(Some(path.clone())).expect("open store"));
    store.ensure_session("s1").await.unwrap();

    // Seed 4 large turns in history so estimated tokens exceed 100k
    for i in 0..4 {
        store
            .append(
                "s1",
                &Message::user(format!("prior user query {i} {}", "a".repeat(120_000))),
                false,
            )
            .await
            .unwrap();
        let mut asst = Message::assistant("short answer");
        asst.usage = Some(TokenUsage::estimated(120_000, 10));
        store.append("s1", &asst, false).await.unwrap();
    }

    let main = Arc::new(ToolLoopUsageProvider {
        calls: AtomicUsize::new(0),
        rounds: 3,
        stale_usage: 150_000,
    });

    let cfg = AgentConfig {
        compact_at_tokens: 1_000_000,
        keep_recent_turns: 1,
        auto_compact_enabled: true,
        auto_compact_threshold: 50_000,
        auto_compact_turns: 1,
        ..AgentConfig::default()
    };

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
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

    let started_count = Arc::new(AtomicUsize::new(0));
    let started_watcher = started_count.clone();
    let token_updates = Arc::new(std::sync::Mutex::new(Vec::new()));
    let tokens_watcher = token_updates.clone();

    agent.set_observer(Arc::new(move |event| match event {
        AgentEvent::CompactionStarted { .. } => {
            started_watcher.fetch_add(1, Ordering::SeqCst);
        }
        AgentEvent::ContextTokensUpdated { tokens_used, reset } => {
            tokens_watcher.lock().unwrap().push((tokens_used, reset));
        }
        _ => {}
    }));

    let outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(outcome.reply, "ok");
    // Main provider executed 4 calls (3 tool calls + final reply)
    assert_eq!(main.calls.load(Ordering::SeqCst), 4);

    // Compaction should only start ONCE, not repeatedly on every tool round
    assert_eq!(
        started_count.load(Ordering::SeqCst),
        1,
        "compaction must not re-trigger on subsequent tool calls within the same turn"
    );

    let summaries = count_summaries(&path);
    assert_eq!(summaries, 1, "exactly one compaction fold performed");

    let updates = token_updates.lock().unwrap().clone();
    // The final context token updates must reflect the compacted transcript, not the stale 150k usage
    let last_update = updates.last().unwrap().0;
    assert!(
        last_update < 50_000,
        "context gauge at end of turn must reflect compacted tokens (<50k), got {last_update}"
    );
    // The turn adopted the fold between rounds (the provider above verifies it saw the folded
    // transcript). That measurement is not a stale report the turn will contradict, so it must be
    // allowed to lower the gauge immediately rather than waiting for the turn boundary.
    assert!(
        updates
            .iter()
            .any(|(tokens, reset)| *reset && *tokens < 50_000),
        "an adopted fold must reset the context gauge mid-turn: {updates:?}"
    );
}

/// A tool loop that paces itself, and remembers the size of every prompt it answered.
///
/// The pacing matters: a fold is a background call and needs a moment to land, and the point here is
/// what the turn sends while folds come and go.
struct PacedToolLoopProvider {
    rounds: usize,
    reply_chars: usize,
    calls: AtomicUsize,
    prompts: std::sync::Mutex<Vec<usize>>,
}

impl PacedToolLoopProvider {
    fn new(rounds: usize, reply_chars: usize) -> Self {
        Self {
            rounds,
            reply_chars,
            calls: AtomicUsize::new(0),
            prompts: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn prompts(&self) -> Vec<usize> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait]
impl ModelProvider for PacedToolLoopProvider {
    fn provider_id(&self) -> &str {
        "paced"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["paced-model".to_string()])
    }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts
            .lock()
            .unwrap()
            .push(estimate_tokens(messages).tokens as usize);
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        let mut message = Message::assistant("x".repeat(self.reply_chars));
        message.usage = Some(TokenUsage::estimated(
            estimate_tokens(messages).tokens,
            self.reply_chars as u32 / 4,
        ));
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

/// One long turn, many folds: the turn must send the folded transcript after each one.
///
/// A fold that lands mid-turn retires rows of *this* turn, and if the turn keeps the copy it had,
/// the bar sees the size of the transcript the fold replaced on every later round — so it folds
/// again, and again, while the stored transcript collapses to a fraction of the configured target.
/// A real session folded twenty times inside one turn this way and its next prompt went from 180k
/// tokens to 11k: not the 50% that was configured. What the turn sends is what the store holds.
#[tokio::test]
async fn a_fold_landing_mid_turn_bounds_what_the_turn_sends_next() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = Arc::new(SqliteSessionStore::new(Some(path.clone())).expect("open store"));
    store.ensure_session("s1").await.unwrap();

    // 60 rounds of ~3k tokens: the transcript crosses a 20k bar again and again after every fold.
    let main = Arc::new(PacedToolLoopProvider::new(60, 12_000));

    let cfg = AgentConfig {
        compact_at_tokens: 1_000_000, // the legacy bar stays out of the way
        keep_recent_turns: 1,
        auto_compact_enabled: true,
        auto_compact_threshold: 20_000,
        auto_compact_percent: Some(50),
        ..AgentConfig::default()
    };

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
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

    agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    // Let a fold started by the last round land before reading the store.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // Three thousand tokens a round against a 20k bar buys about three rounds per fold, so the
    // count itself is a shape of the test, not the thing under test. What must hold is that a fold
    // buys the rounds it should: one per round is a fold that bought nothing.
    let folds = fold_count.load(Ordering::SeqCst);
    assert!(
        folds < 30,
        "a fold must bound the transcript the turn sends, not fire once per round: {folds} folds in 60 rounds"
    );

    let prompts = main.prompts();
    let biggest = prompts.iter().copied().max().unwrap_or(0);
    assert!(
        biggest < 60_000,
        "the turn must not keep sending the transcript a fold replaced: biggest prompt {biggest} tokens under a 20k bar"
    );

    let live = store.replay("s1", 10_000).await.unwrap();
    let live_tokens = heuristic_tokens(&sendable_messages(&live.history)) as usize;
    assert!(
        (5_000..=40_000).contains(&live_tokens),
        "a 50% fold of a 20k bar leaves the stored transcript near the bar: {live_tokens} tokens live"
    );
}

/// A mid-turn round that pushes the request past the model window is folded before the next call,
/// not after it. The measured prompt sizes distinguish the two: a background fold leaves the next
/// request carrying every round, while a preflight fold hands it the bounded transcript.
struct BigRoundProvider {
    calls: AtomicUsize,
    prompt_chars: std::sync::Mutex<Vec<usize>>,
    big: String,
    big_rounds: usize,
}

#[async_trait]
impl ModelProvider for BigRoundProvider {
    fn provider_id(&self) -> &str {
        "big_round"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["big-round-model".to_string()])
    }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let chars: usize = messages
            .iter()
            .map(|message| {
                message.content.as_deref().map_or(0, str::len)
                    + message.reasoning.as_deref().map_or(0, str::len)
                    + message
                        .tool_result
                        .as_ref()
                        .map_or(0, |result| result.content.len())
                    + message
                        .tool_calls
                        .iter()
                        .map(|call| call.arguments.to_string().len())
                        .sum::<usize>()
            })
            .sum();
        self.prompt_chars.lock().unwrap().push(chars);

        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.big_rounds {
            let mut message = Message::assistant(self.big.clone());
            message.tool_calls = vec![ToolCall {
                id: format!("call_{n}"),
                name: "no_such_tool".to_string(),
                arguments: serde_json::json!({}),
                signature: None,
            }];
            return Ok(message);
        }
        Ok(Message::assistant("done"))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(Vec::new())
    }
}

/// Slow enough that a background fold cannot land between the spawn and the next model call.
struct SlowSummariser {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for SlowSummariser {
    fn provider_id(&self) -> &str {
        "slow-canned"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["slow-canned-model".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(Message::assistant("FOLDED"))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn an_over_window_turn_folds_before_the_next_model_call() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");
    let store = Arc::new(SqliteSessionStore::new(Some(path.clone())).expect("open store"));
    store.ensure_session("s1").await.unwrap();

    // 120k chars is about 30k heuristic tokens per round. Two rounds fit under the 70k bar and
    // the 90k urgent ceiling; three cross both, and the urgent path must win.
    let main = Arc::new(BigRoundProvider {
        calls: AtomicUsize::new(0),
        prompt_chars: std::sync::Mutex::new(Vec::new()),
        big: "x".repeat(120_000),
        big_rounds: 3,
    });
    let summariser = Arc::new(SlowSummariser {
        calls: AtomicUsize::new(0),
    });

    let cfg = AgentConfig {
        context_window: Some(100_000),
        compact_at_tokens: 1_000_000,
        keep_recent_turns: 1,
        auto_compact_enabled: true,
        auto_compact_threshold: 1_000_000,
        auto_compact_percent: Some(70),
        auto_compact_turns: 1,
        ..AgentConfig::default()
    };

    let agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 500,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "slow-canned-model".to_string(),
    });

    agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();

    let prompts = main.prompt_chars.lock().unwrap().clone();
    assert!(
        prompts.len() >= 4,
        "the turn runs until the model stops asking for tools: {prompts:?}"
    );
    assert!(
        prompts[2] > 200_000,
        "two big rounds are in the third request: {prompts:?}"
    );
    assert!(
        prompts[3] < 160_000,
        "the fourth request is folded before it is sent, not after: {prompts:?}"
    );
    assert!(
        summariser.calls.load(Ordering::SeqCst) >= 1,
        "the preflight fold ran a summary call"
    );
    assert!(count_summaries(&path) >= 1, "the fold wrote a summary row");
}
