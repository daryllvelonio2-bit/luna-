//! The human-in-the-loop approval gate, driven through `Agent::run_task`.
//!
//! Same shape as `agent_guardrail_test.rs`: a scripted provider, a recording tool and an approver
//! that stands in for the human. What is asserted here is the part a chat transcript cannot show
//! reliably — `execute` ran exactly once on allow, zero times on deny and zero on timeout.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use harness_core::*;

const TOOL: &str = "recording_tool";

/// Asks for one `recording_tool` call, then concludes the task.
struct OneCallProvider {
    turns: AtomicUsize,
    seen: Arc<Mutex<Vec<Message>>>,
}

impl OneCallProvider {
    fn new() -> Self {
        Self {
            turns: AtomicUsize::new(0),
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Every tool result the agent fed back to the model, joined.
    fn tool_result_text(&self) -> String {
        self.seen
            .lock()
            .expect("no other thread panicked while holding the lock")
            .iter()
            .filter_map(|message| {
                message
                    .tool_result
                    .as_ref()
                    .map(|result| result.content.clone())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait]
impl ModelProvider for OneCallProvider {
    fn provider_id(&self) -> &str {
        "mock"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec![])
    }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        self.seen
            .lock()
            .expect("no other thread panicked while holding the lock")
            .extend_from_slice(messages);

        if self.turns.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Message::assistant_with_tools(vec![ToolCall {
                id: "call_0".to_string(),
                name: TOOL.to_string(),
                arguments: json!({ "path": "/tmp/gate-target" }),
                signature: None,
            }]))
        } else {
            Ok(Message::assistant("Done"))
        }
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

/// Records that it ran, what arguments it saw, and whether it calls itself mutating.
struct RecordingTool {
    runs: Arc<AtomicUsize>,
    last_arguments: Arc<Mutex<Option<serde_json::Value>>>,
    mutating: bool,
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        TOOL
    }

    fn description(&self) -> &str {
        "records that it ran"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({})
    }

    fn is_mutating(&self) -> bool {
        self.mutating
    }

    async fn preview(&self, arguments: &serde_json::Value) -> Option<ToolPreview> {
        Some(ToolPreview {
            target: arguments
                .get("path")
                .and_then(|path| path.as_str())
                .map(str::to_string),
            diff: None,
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        *self
            .last_arguments
            .lock()
            .expect("no other thread panicked while holding the lock") = Some(arguments.clone());
        Ok(ToolResult::success(call_id, "ran"))
    }
}

/// Counts prompts and answers with one scripted decision.
struct ScriptedApprover {
    prompts: Arc<AtomicUsize>,
    decision: ApprovalDecision,
}

#[async_trait]
impl Approver for ScriptedApprover {
    async fn approve(&self, _request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        self.prompts.fetch_add(1, Ordering::SeqCst);
        Ok(self.decision.clone())
    }
}

/// Never answers at all — the gate's timeout is the only thing that can end it.
struct NeverAnswers;

#[async_trait]
impl Approver for NeverAnswers {
    async fn approve(&self, _request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        std::future::pending::<()>().await;
        Ok(ApprovalDecision::Deny)
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
    async fn save_note(
        &self,
        _title: &str,
        _content: &str,
        _tags: &[String],
    ) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn search_notes(&self, _query: &str, _limit: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn list_notes(&self, _limit: usize, _offset: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn read_note(&self, _id: &str) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn update_note(
        &self,
        _id: &str,
        _title: Option<&str>,
        _content: Option<&str>,
        _tags: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn delete_note(&self, _id: &str) -> Result<bool, HarnessError> {
        unreachable!("not used by this test")
    }
}

/// What one run of the loop did.
struct Observed {
    reply: Result<String, HarnessError>,
    tool_runs: usize,
    saw_arguments: Option<serde_json::Value>,
    tool_results: String,
}

/// One turn of the agent loop against the recording tool.
async fn run_case(
    config: AgentConfig,
    settings: ApprovalSettings,
    approver: Arc<dyn Approver>,
    tool_is_mutating: bool,
) -> Observed {
    let provider = Arc::new(OneCallProvider::new());
    let results_from = provider.clone();
    let runs = Arc::new(AtomicUsize::new(0));
    let last_arguments = Arc::new(Mutex::new(None));

    let registry = Arc::new(DefaultToolRegistry::new());
    registry
        .register(Arc::new(RecordingTool {
            runs: runs.clone(),
            last_arguments: last_arguments.clone(),
            mutating: tool_is_mutating,
        }))
        .await
        .expect("registering the recording tool cannot fail");

    let agent = Agent::new(
        provider,
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        registry,
        config,
    )
    .with_approval(approver, settings);

    let reply = agent.run_task("do it", CancellationToken::new()).await;
    let saw_arguments = last_arguments
        .lock()
        .expect("no other thread panicked while holding the lock")
        .clone();

    Observed {
        reply,
        tool_runs: runs.load(Ordering::SeqCst),
        saw_arguments,
        tool_results: results_from.tool_result_text(),
    }
}

#[tokio::test]
async fn a_denied_call_never_runs_and_says_so() {
    let prompts = Arc::new(AtomicUsize::new(0));
    let observed = run_case(
        AgentConfig::default(),
        ApprovalSettings::default(),
        Arc::new(ScriptedApprover {
            prompts: prompts.clone(),
            decision: ApprovalDecision::Deny,
        }),
        true,
    )
    .await;

    assert_eq!(
        observed.tool_runs, 0,
        "a denied call must not reach the tool"
    );
    assert_eq!(prompts.load(Ordering::SeqCst), 1, "exactly one prompt");
    assert!(
        observed.tool_results.contains("approval denied"),
        "the model must be told: {}",
        observed.tool_results
    );
    // The turn survives a denial: it is a tool result, not a harness abort.
    assert_eq!(observed.reply.expect("the turn completed"), "Done");
}

#[tokio::test]
async fn an_allowed_call_runs_exactly_once() {
    let prompts = Arc::new(AtomicUsize::new(0));
    let observed = run_case(
        AgentConfig::default(),
        ApprovalSettings::default(),
        Arc::new(ScriptedApprover {
            prompts: prompts.clone(),
            decision: ApprovalDecision::Run,
        }),
        true,
    )
    .await;

    assert_eq!(observed.tool_runs, 1);
    assert_eq!(prompts.load(Ordering::SeqCst), 1);
    assert!(
        observed.tool_results.contains("ran"),
        "{}",
        observed.tool_results
    );
}

#[tokio::test]
async fn an_edit_and_run_answer_replaces_the_arguments() {
    let prompts = Arc::new(AtomicUsize::new(0));
    let observed = run_case(
        AgentConfig::default(),
        ApprovalSettings::default(),
        Arc::new(ScriptedApprover {
            prompts: prompts.clone(),
            decision: ApprovalDecision::RunWithArguments(json!({ "path": "/tmp/edited" })),
        }),
        true,
    )
    .await;

    assert_eq!(observed.tool_runs, 1);
    assert_eq!(
        observed.saw_arguments,
        Some(json!({ "path": "/tmp/edited" })),
        "the human's arguments must be what the tool sees"
    );
}

#[tokio::test]
async fn no_answer_within_the_timeout_is_a_denial() {
    let observed = run_case(
        AgentConfig::default(),
        ApprovalSettings {
            timeout: Duration::from_millis(50),
            mutating_overrides: vec![],
        },
        Arc::new(NeverAnswers),
        true,
    )
    .await;

    assert_eq!(observed.tool_runs, 0);
    assert!(
        observed.tool_results.contains("no answer within"),
        "{}",
        observed.tool_results
    );
}

#[tokio::test]
async fn the_flag_switched_off_short_circuits_the_gate() {
    let prompts = Arc::new(AtomicUsize::new(0));
    let observed = run_case(
        AgentConfig {
            max_consecutive_failures: 5,
            require_hitl_for_mutation: false,
            ..AgentConfig::default()
        },
        ApprovalSettings::default(),
        // Even an approver that would deny is never asked.
        Arc::new(ScriptedApprover {
            prompts: prompts.clone(),
            decision: ApprovalDecision::Deny,
        }),
        true,
    )
    .await;

    assert_eq!(prompts.load(Ordering::SeqCst), 0, "the flag is read");
    assert_eq!(observed.tool_runs, 1, "the write lands without a prompt");
}

#[tokio::test]
async fn read_only_tools_never_prompt() {
    let prompts = Arc::new(AtomicUsize::new(0));
    let observed = run_case(
        AgentConfig::default(),
        ApprovalSettings::default(),
        Arc::new(ScriptedApprover {
            prompts: prompts.clone(),
            decision: ApprovalDecision::Deny,
        }),
        false,
    )
    .await;

    assert_eq!(prompts.load(Ordering::SeqCst), 0, "a read must not ask");
    assert_eq!(observed.tool_runs, 1);
}

#[tokio::test]
async fn mutating_overrides_gate_a_tool_that_claims_to_be_read_only() {
    let prompts = Arc::new(AtomicUsize::new(0));
    let observed = run_case(
        AgentConfig::default(),
        ApprovalSettings {
            timeout: Duration::from_secs(120),
            mutating_overrides: vec![TOOL.to_string()],
        },
        Arc::new(ScriptedApprover {
            prompts: prompts.clone(),
            decision: ApprovalDecision::Deny,
        }),
        false,
    )
    .await;

    assert_eq!(prompts.load(Ordering::SeqCst), 1);
    assert_eq!(observed.tool_runs, 0);
}
