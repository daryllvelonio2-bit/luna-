//! Parallel tool dispatch: which calls may share a turn, and what the turn still sees in order.
//!
//! The behaviour under test is the boundary, not the speed-up: consecutive read-only calls run
//! together, and *everything* the gate may touch — a mutating call, a hook denial, a rule that
//! wants a human — stays serial and keeps its place in the transcript. The overlap is measured
//! with an in-flight counter rather than a stopwatch, so these tests mean the same thing on an
//! idle machine and on a box running four builds.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use harness_core::*;

/// Every read keeps this long, so a batch that overlaps is visible as overlap and a batch that
/// does not is visible as a duration.
const CALL_MS: u64 = 60;

// ---- a provider that asks for a burst of tool calls, then answers --------------------------

struct BurstProvider {
    calls: Vec<(String, serde_json::Value)>,
    answered: AtomicUsize,
}

#[async_trait]
impl ModelProvider for BurstProvider {
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
        if self.answered.fetch_add(1, Ordering::SeqCst) == 0 {
            let calls = self
                .calls
                .iter()
                .enumerate()
                .map(|(index, (name, arguments))| ToolCall {
                    id: format!("call_{index}"),
                    name: name.clone(),
                    arguments: arguments.clone(),
                    signature: None,
                })
                .collect();
            return Ok(Message::assistant_with_tools(calls));
        }
        Ok(Message::assistant("done"))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

// ---- a tool that reports how many siblings it ran beside -----------------------------------

/// What the tools saw: how many were ever in flight at once, and in what order they finished.
#[derive(Default)]
struct Probe {
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    finished: Mutex<Vec<String>>,
}

impl Probe {
    fn peak(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
}

/// `mutating` is the whole difference between a call that may share a turn and one that may not.
struct SlowTool {
    name: &'static str,
    mutating: bool,
    probe: Arc<Probe>,
}

#[async_trait]
impl Tool for SlowTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "sleeps, then reports how many siblings were running"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {"label": {"type": "string"}}})
    }

    fn is_mutating(&self) -> bool {
        self.mutating
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let now = self.probe.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.probe.max_in_flight.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(CALL_MS)).await;
        let label = arguments
            .get("label")
            .and_then(|l| l.as_str())
            .unwrap_or(self.name)
            .to_string();
        self.probe.finished.lock().await.push(label.clone());
        self.probe.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolResult::success(
            call_id,
            format!("{{\"label\":\"{label}\"}}"),
        ))
    }
}

// ---- stubs for the agent's other engines ---------------------------------------------------

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

// ---- one turn over a burst of calls --------------------------------------------------------

/// Run one turn in which the model asks for `calls` in order, and report what the tools saw.
async fn run_burst(calls: Vec<(&str, serde_json::Value)>) -> (TurnOutcome, Arc<Probe>, usize) {
    let probe = Arc::new(Probe::default());
    let registry = Arc::new(DefaultToolRegistry::new());
    registry
        .register(Arc::new(SlowTool {
            name: "slow_read",
            mutating: false,
            probe: probe.clone(),
        }))
        .await
        .expect("register read tool");
    registry
        .register(Arc::new(SlowTool {
            name: "slow_write",
            mutating: true,
            probe: probe.clone(),
        }))
        .await
        .expect("register write tool");

    let provider = Arc::new(BurstProvider {
        calls: calls
            .iter()
            .map(|(name, arguments)| (name.to_string(), arguments.clone()))
            .collect(),
        answered: AtomicUsize::new(0),
    });
    let approver = Arc::new(AllowAll);
    let agent = Agent::new(
        provider,
        Arc::new(NoMemory),
        Arc::new(NoNotes),
        registry,
        AgentConfig::default(),
    )
    .with_approval(approver, ApprovalSettings::default());

    let outcome = agent
        .run_turn("go", CancellationToken::new())
        .await
        .expect("turn completes");
    let peak = probe.peak();
    (outcome, probe, peak)
}

fn read(label: &str) -> (&'static str, serde_json::Value) {
    ("slow_read", json!({ "label": label }))
}

fn write(label: &str) -> (&'static str, serde_json::Value) {
    ("slow_write", json!({ "label": label }))
}

// ---- the contracts -------------------------------------------------------------------------

#[tokio::test]
async fn consecutive_read_only_calls_share_the_turn() {
    let (outcome, probe, peak) = run_burst(vec![read("a"), read("b"), read("c")]).await;

    assert!(
        peak >= 2,
        "three independent reads should overlap, peak in flight was {peak}"
    );
    assert_eq!(outcome.tool_calls.len(), 3);
    assert!(outcome.tool_calls.iter().all(|c| !c.is_error));
    // The probe's in-flight count is back to zero: nothing was left running past the turn.
    assert_eq!(probe.in_flight.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_batch_never_exceeds_its_width() {
    let calls = (0..6).map(|i| read(&format!("r{i}"))).collect();
    let (_outcome, _probe, peak) = run_burst(calls).await;

    assert!(
        peak <= 4,
        "more than four read-only calls were in flight at once (peak {peak})"
    );
}

#[tokio::test]
async fn a_mutating_call_is_never_run_beside_a_read() {
    let (outcome, _probe, peak) = run_burst(vec![read("a"), write("w"), read("b")]).await;

    assert_eq!(
        peak, 1,
        "a mutating call must not run beside a read (peak in flight was {peak})"
    );
    // Order survives: the mutating call keeps its place between the two reads.
    let labels: Vec<String> = outcome
        .tool_calls
        .iter()
        .map(|c| {
            c.arguments
                .get("label")
                .and_then(|l| l.as_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(labels, vec!["a", "w", "b"]);
}

/// A run that starts read-only and ends read-only, with a mutating call in the middle, is split
/// into two batches around it rather than serialized whole.
#[tokio::test]
async fn reads_on_both_sides_of_a_write_each_get_their_own_batch() {
    let (outcome, _probe, peak) =
        run_burst(vec![read("a"), read("b"), write("w"), read("c"), read("d")]).await;

    assert!(
        peak >= 2,
        "the reads before the write should overlap (peak {peak})"
    );
    let labels: Vec<String> = outcome
        .tool_calls
        .iter()
        .map(|c| {
            c.arguments
                .get("label")
                .and_then(|l| l.as_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(labels, vec!["a", "b", "w", "c", "d"]);
}

/// The transcript the model reads next keeps the order the model asked in, whatever order the
/// tool bodies finished in.
#[tokio::test]
async fn results_are_recorded_in_call_order() {
    let (outcome, probe, _peak) =
        run_burst(vec![read("first"), read("second"), read("third")]).await;

    let recorded: Vec<String> = outcome
        .tool_calls
        .iter()
        .map(|c| c.output.clone())
        .collect();
    assert_eq!(
        recorded,
        vec![
            "{\"label\":\"first\"}".to_string(),
            "{\"label\":\"second\"}".to_string(),
            "{\"label\":\"third\"}".to_string(),
        ]
    );
    // And every one of them actually ran.
    let mut finished = probe.finished.lock().await.clone();
    finished.sort();
    assert_eq!(finished, vec!["first", "second", "third"]);
}
