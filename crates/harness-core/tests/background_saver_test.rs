//! The agent loop's side of the background memory saver.
//!
//! These tests drive real turns (`run_turn`, not the saver in isolation) and assert what the loop
//! promises: the saver is handed the transcript *after* the reply exists, on the interval the config
//! asks for, off the turn's critical path, and a saver that explodes never fails the turn.

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use harness_core::saver::{BackgroundSaver, SaveReport};
use harness_core::*;

/// A provider that answers the first call with a tool-less reply, so one turn is one model call.
struct MockProvider;

#[async_trait]
impl ModelProvider for MockProvider {
    fn provider_id(&self) -> &str {
        "mock"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["mock-model".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        Ok(Message::assistant("Done!"))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
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
        Ok(vec![])
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
    async fn save_note(&self, _t: &str, _c: &str, _g: &[String]) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn search_notes(&self, _q: &str, _l: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn list_notes(&self, _l: usize, _o: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn read_note(&self, _id: &str) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn update_note(
        &self,
        _id: &str,
        _t: Option<&str>,
        _c: Option<&str>,
        _g: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn delete_note(&self, _id: &str) -> Result<bool, HarnessError> {
        unreachable!("not used by this test")
    }
}

/// Records every pass it is asked to run; optionally panics, to prove the turn survives it.
struct RecordingSaver {
    passes: Arc<Mutex<Vec<Vec<Message>>>>,
    calls: AtomicUsize,
    every_turns: usize,
    tail: usize,
    explode: bool,
}

impl RecordingSaver {
    fn new(passes: Arc<Mutex<Vec<Vec<Message>>>>, every_turns: usize, tail: usize) -> Arc<Self> {
        Arc::new(Self {
            passes,
            calls: AtomicUsize::new(0),
            every_turns,
            tail,
            explode: false,
        })
    }
}

#[async_trait]
impl BackgroundSaver for RecordingSaver {
    async fn distil(&self, tail: &[Message]) -> Result<SaveReport, HarnessError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.explode {
            return Err(HarnessError::ProviderError {
                provider: "recording".to_string(),
                message: "saver is down".to_string(),
            });
        }
        self.passes.lock().unwrap().push(tail.to_vec());
        Ok(SaveReport {
            memories: 1,
            notes: 0,
            skipped: 0,
        })
    }

    fn tail_limit(&self) -> usize {
        self.tail
    }

    fn every_turns(&self) -> usize {
        self.every_turns
    }

    fn model(&self) -> &str {
        "recording"
    }
}

fn agent(
    saver: Arc<dyn BackgroundSaver>,
) -> Agent<MockProvider, MockMemory, MockNotes, DefaultToolRegistry> {
    Agent::new(
        Arc::new(MockProvider),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        AgentConfig {
            require_hitl_for_mutation: false,
            ..AgentConfig::default()
        },
    )
    .with_background_saver(saver)
}

/// The detached pass runs on the runtime, so a turn that returns is not proof it finished.
async fn settle() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn a_completed_turn_hands_the_transcript_to_the_saver() {
    let passes = Arc::new(Mutex::new(Vec::new()));
    let saver = RecordingSaver::new(passes.clone(), 1, 24);
    let agent = agent(saver.clone());

    let outcome = agent
        .run_turn("remember that I build with cargo", CancellationToken::new())
        .await
        .expect("the turn completes");
    assert_eq!(outcome.reply, "Done!");

    settle().await;
    let recorded = passes.lock().unwrap();
    assert_eq!(recorded.len(), 1, "the saver never ran");
    let tail = &recorded[0];
    // The user's message and the reply, in order, without the system prompt.
    assert!(
        tail.iter()
            .any(|message| message.content.as_deref() == Some("remember that I build with cargo")),
        "{tail:?}"
    );
    assert!(
        tail.iter()
            .any(|message| message.content.as_deref() == Some("Done!")),
        "{tail:?}"
    );
    assert!(tail.iter().all(|message| message.role != Role::System));
}

#[tokio::test]
async fn the_interval_is_counted_in_turns_not_in_wall_time() {
    let passes = Arc::new(Mutex::new(Vec::new()));
    let saver = RecordingSaver::new(passes.clone(), 3, 24);
    let agent = agent(saver.clone());

    for _ in 0..2 {
        agent
            .run_turn("turn", CancellationToken::new())
            .await
            .expect("turn completes");
    }
    settle().await;
    assert_eq!(
        saver.calls.load(Ordering::SeqCst),
        0,
        "ran before its interval"
    );

    agent
        .run_turn("third turn", CancellationToken::new())
        .await
        .expect("turn completes");
    settle().await;
    assert_eq!(saver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(passes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_saver_that_fails_does_not_fail_the_turn() {
    let passes = Arc::new(Mutex::new(Vec::new()));
    let mut failing = RecordingSaver::new(passes.clone(), 1, 24);
    Arc::get_mut(&mut failing).expect("sole owner").explode = true;
    let agent = agent(failing.clone());

    let outcome = agent
        .run_turn("anything", CancellationToken::new())
        .await
        .expect("a broken saver must not break the turn");

    assert_eq!(outcome.reply, "Done!");
    settle().await;
    assert_eq!(
        failing.calls.load(Ordering::SeqCst),
        1,
        "the saver was never asked"
    );
    assert!(passes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn without_a_saver_nothing_is_spawned_and_the_turn_is_unchanged() {
    let agent = Agent::new(
        Arc::new(MockProvider),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        AgentConfig::default(),
    );
    assert!(agent.background_saver().is_none());

    let outcome = agent
        .run_turn("nothing to distil", CancellationToken::new())
        .await
        .expect("turn completes");
    assert_eq!(outcome.reply, "Done!");
}

#[tokio::test]
async fn a_zero_interval_disables_the_saver_entirely() {
    let passes = Arc::new(Mutex::new(Vec::new()));
    let saver = RecordingSaver::new(passes.clone(), 0, 24);
    let agent = agent(saver.clone());

    for _ in 0..3 {
        agent
            .run_turn("turn", CancellationToken::new())
            .await
            .expect("turn completes");
    }
    settle().await;
    assert_eq!(saver.calls.load(Ordering::SeqCst), 0);
    assert!(passes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn memory_mode_switches_system_prompt_instructions_dynamically() {
    let config = AgentConfig {
        memory_mode: harness_core::MemoryMode::Session,
        ..Default::default()
    };

    let agent = Agent::new(
        Arc::new(MockProvider),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        config,
    );

    // 1. In session mode: prompt instructs to call save_memory / save_note
    let session_prompt = agent.build_system_prompt("hello").await.unwrap();
    assert!(session_prompt.contains("Memory & Note Management (Session Mode)"));
    assert!(session_prompt.contains("save_memory"));
    assert!(!session_prompt.contains("Memory Protocol (Background Saver Mode)"));

    // 2. In Background saver mode: zero prompt bloat — no memory saving instructions at all!
    let mut bg_agent = agent;
    bg_agent.set_memory_mode(harness_core::MemoryMode::Background);
    let bg_prompt = bg_agent.build_system_prompt("hello").await.unwrap();
    assert!(!bg_prompt.contains("Memory & Note Management (Session Mode)"));
    assert!(!bg_prompt.contains("SESSION WRITER MODE"));
    assert!(!bg_prompt.contains("save_memory"));
    assert!(!bg_prompt.contains("save_note"));
}
