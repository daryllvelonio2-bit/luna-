//! Hooks inside the agent loop: what a hook can and cannot change about a turn.
//!
//! The runner-level contract lives in `hooks_test.rs`. These tests are about the seam the model
//! actually experiences: which arguments the tool ran with, whether the human gate was consulted,
//! and what text lands in the transcript the model reads next. They are Unix-only because the
//! fixture hooks use `printf` and POSIX shell quoting.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use harness_core::hooks::HookRunner;
use harness_core::*;

// ---- a provider that calls one tool, then answers -----------------------------------------

struct OneToolProvider {
    tool: String,
    arguments: serde_json::Value,
    /// Every message list the provider was handed, so a test can read what the model saw.
    seen: Arc<Mutex<Vec<Vec<Message>>>>,
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for OneToolProvider {
    fn provider_id(&self) -> &str {
        "mock"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["mock-1".to_string()])
    }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        self.seen.lock().await.push(messages.to_vec());
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(Message::assistant_with_tools(vec![ToolCall {
                id: "call_1".to_string(),
                name: self.tool.clone(),
                arguments: self.arguments.clone(),
                signature: None,
            }]));
        }
        Ok(Message::assistant("done"))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

// ---- a tool that records what it was asked to do ------------------------------------------

#[derive(Default)]
struct RecordingTool {
    calls: Mutex<Vec<serde_json::Value>>,
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        "record"
    }
    fn description(&self) -> &str {
        "records its arguments and returns a canned payload"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    fn is_mutating(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        self.calls.lock().await.push(arguments.clone());
        Ok(ToolResult::success(
            call_id,
            "SECRET=hunter2 file=notes.txt",
        ))
    }
}

// ---- an approver that counts how often it was asked ---------------------------------------

#[derive(Default)]
struct CountingApprover {
    asked: AtomicUsize,
}

#[async_trait]
impl Approver for CountingApprover {
    async fn approve(&self, _request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Ok(ApprovalDecision::Run)
    }
}

// ---- stubs for the agent's other engines --------------------------------------------------

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

// ---- helpers ------------------------------------------------------------------------------

fn write_hooks(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("hooks.json");
    std::fs::write(&path, body).expect("write hooks file");
    path
}

fn hooks_settings() -> HooksSettings {
    HooksSettings {
        enabled: true,
        trust_workspace: false,
        timeout_secs: 10,
        disabled: Vec::new(),
    }
}

/// Run one turn with `hook_body` bound to the user hooks file.
async fn run_turn_with_hooks(
    hook_body: &str,
    tool_arguments: serde_json::Value,
) -> (
    TurnOutcome,
    Vec<serde_json::Value>,
    Vec<Vec<Message>>,
    usize,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let user_hooks = write_hooks(dir.path(), hook_body);
    let hooks = Arc::new(
        HookRunner::load_with_user_file(dir.path(), &hooks_settings(), Some(&user_hooks))
            .expect("hooks load"),
    );

    let seen = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool::default());
    let approver = Arc::new(CountingApprover::default());

    let registry = Arc::new(DefaultToolRegistry::new());
    registry.register(tool.clone()).await.expect("register");

    let agent = Agent::new(
        Arc::new(OneToolProvider {
            tool: "record".to_string(),
            arguments: tool_arguments,
            seen: seen.clone(),
            calls: AtomicUsize::new(0),
        }),
        Arc::new(NoMemory),
        Arc::new(NoNotes),
        registry,
        AgentConfig::default(),
    )
    .with_approval(approver.clone(), ApprovalSettings::default())
    .with_hooks(hooks);

    let outcome = agent
        .run_turn("go", CancellationToken::new())
        .await
        .expect("turn completes");

    let calls = tool.calls.lock().await.clone();
    let messages = seen.lock().await.clone();
    let asked = approver.asked.load(Ordering::SeqCst);
    (outcome, calls, messages, asked)
}

// ---- the contracts -----------------------------------------------------------------------

#[tokio::test]
async fn a_pre_tool_use_rewrite_is_what_the_tool_and_the_gate_see() {
    let hooks = r#"{
      "hooks": {
        "pre_tool_use": [
          { "command": "printf '{\"updatedInput\":{\"path\":\"/tmp/safe.txt\"}}'" }
        ]
      }
    }"#;
    let (_outcome, calls, _messages, asked) =
        run_turn_with_hooks(hooks, json!({"path": "/etc/passwd"})).await;

    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        json!({"path": "/tmp/safe.txt"}),
        "the tool runs with the rewritten arguments, not the model's"
    );
    assert_eq!(asked, 1, "the gate still runs after a rewrite");
}

#[tokio::test]
async fn a_pre_tool_use_denial_means_the_gate_is_never_asked() {
    let hooks = r#"{
      "hooks": {
        "pre_tool_use": [
          { "command": "printf '{\"decision\":\"deny\",\"reason\":\"no touching that\"}'; exit 0" }
        ]
      }
    }"#;
    let (outcome, calls, _messages, asked) =
        run_turn_with_hooks(hooks, json!({"path": "/etc/passwd"})).await;

    assert!(calls.is_empty(), "the tool never ran");
    assert_eq!(asked, 0, "the gate is not consulted on a hook denial");
    assert_eq!(outcome.tool_calls.len(), 1);
    assert!(outcome.tool_calls[0].denied);
    assert!(
        outcome.tool_calls[0].output.contains("no touching that"),
        "the reason reaches the model: {}",
        outcome.tool_calls[0].output
    );
}

#[tokio::test]
async fn a_post_tool_use_rewrite_reaches_the_model_even_though_the_tool_ran() {
    // A redaction hook: the tool's output contains a secret, and what the model reads must not.
    let hooks = r#"{
      "hooks": {
        "post_tool_use": [
          { "command": "printf '{\"content\":\"SECRET=[redacted]\"}'" }
        ]
      }
    }"#;
    let (outcome, calls, messages, _asked) = run_turn_with_hooks(hooks, json!({})).await;

    assert_eq!(calls.len(), 1, "the tool still ran");
    assert_eq!(
        outcome.tool_calls[0].output, "SECRET=[redacted]",
        "the transcript gets the hook's text"
    );

    // ...and the next model call sees the redacted text, not the raw output.
    let second_call = messages.last().expect("the provider ran twice");
    let tool_message = second_call
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("the tool result is in the next request");
    let content = tool_message.content.clone().unwrap_or_default();
    assert!(content.contains("[redacted]"), "got: {content}");
    assert!(
        !content.contains("hunter2"),
        "the secret must not survive the hook: {content}"
    );
}

#[tokio::test]
async fn a_post_hook_payload_without_content_keeps_the_tool_output() {
    let hooks = r#"{
      "hooks": { "post_tool_use": [ { "command": "printf '{\"unexpected\":true}'" } ] }
    }"#;
    let (outcome, _calls, _messages, _asked) = run_turn_with_hooks(hooks, json!({})).await;
    assert_eq!(
        outcome.tool_calls[0].output, "SECRET=hunter2 file=notes.txt",
        "an unparseable replacement must not blank the result"
    );
}

#[tokio::test]
async fn a_gating_event_can_refuse_the_turn_and_an_observation_event_cannot() {
    let hooks = r#"{
      "hooks": {
        "user_prompt_submit": [ { "command": "printf '{\"decision\":\"deny\",\"reason\":\"prompt refused\"}'" } ],
        "notification":      [ { "command": "printf '{\"decision\":\"deny\",\"reason\":\"ignored\"}'" } ]
      }
    }"#;
    let dir = tempfile::tempdir().unwrap();
    let user_hooks = write_hooks(dir.path(), hooks);
    let runner = HookRunner::load_with_user_file(dir.path(), &hooks_settings(), Some(&user_hooks))
        .expect("hooks load");

    let registry = Arc::new(DefaultToolRegistry::new());
    let agent = Agent::new(
        Arc::new(OneToolProvider {
            tool: "record".to_string(),
            arguments: json!({}),
            seen: Arc::new(Mutex::new(Vec::new())),
            calls: AtomicUsize::new(0),
        }),
        Arc::new(NoMemory),
        Arc::new(NoNotes),
        registry,
        AgentConfig::default(),
    )
    .with_hooks(Arc::new(runner));

    let refused = agent
        .emit_hook_event(harness_core::HookEvent::UserPromptSubmit, Some("hi"))
        .await;
    assert!(
        matches!(refused, Err(HarnessError::ApprovalRejected(_))),
        "a denied prompt is an error, not a shrug: {refused:?}"
    );

    let observed = agent
        .emit_hook_event(harness_core::HookEvent::Notification, Some("hi"))
        .await;
    assert!(
        observed.is_ok(),
        "an observation event never blocks the turn"
    );
}
