//! Lifecycle hook contracts: what runs, what is refused, and what a hook is not allowed to do.
//!
//! These tests exercise the runner through real `sh` processes (the same path production takes),
//! so a passing test means the wire format on stdin and the exit-code/JSON contract on stdout are
//! the ones documented in `hooks.rs`.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use harness_core::hooks::{HookEvent, HookVerdict};
use harness_core::*;

fn write_hooks(dir: &Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("hooks.json");
    std::fs::write(&path, body).expect("write hooks file");
    path
}

fn settings(enabled: bool, trust_workspace: bool) -> HooksSettings {
    HooksSettings {
        enabled,
        trust_workspace,
        timeout_secs: 10,
        disabled: Vec::new(),
    }
}

fn project_hooks(workspace: &Path, body: &str) {
    let dir = workspace.join(".luna");
    std::fs::create_dir_all(&dir).expect("create .luna");
    std::fs::write(dir.join("hooks.json"), body).expect("write project hooks");
}

#[tokio::test]
async fn disabled_hooks_run_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    project_hooks(
        dir.path(),
        &format!(
            r#"{{"hooks":{{"session_start":[{{"command":"touch {}"}}]}}}}"#,
            marker.display()
        ),
    );

    // `enabled: false` — even with trust_workspace set, nothing runs.
    let runner = HookRunner::load(dir.path(), &settings(false, true)).unwrap();
    assert!(runner.is_empty(), "disabled hooks must not load");
    runner
        .run(HookEvent::SessionStart, &HookContext::default())
        .await;
    assert!(!marker.exists(), "a disabled hook must not execute");
}

#[tokio::test]
async fn project_hooks_need_trust() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    project_hooks(
        dir.path(),
        &format!(
            r#"{{"hooks":{{"session_start":[{{"command":"touch {}"}}]}}}}"#,
            marker.display()
        ),
    );

    let untrusted = HookRunner::load(dir.path(), &settings(true, false)).unwrap();
    assert!(
        untrusted.is_empty(),
        "a repository-supplied hook file must be ignored until it is trusted"
    );
    untrusted
        .run(HookEvent::SessionStart, &HookContext::default())
        .await;
    assert!(!marker.exists());

    let trusted = HookRunner::load(dir.path(), &settings(true, true)).unwrap();
    assert_eq!(trusted.len(), 1);
    trusted
        .run(HookEvent::SessionStart, &HookContext::default())
        .await;
    assert!(marker.exists(), "a trusted project hook must execute");
}

#[tokio::test]
async fn user_hooks_load_without_workspace_trust() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let marker = workspace.path().join("ran");
    let user_file = write_hooks(
        home.path(),
        &format!(
            r#"{{"hooks":{{"session_start":[{{"command":"touch {}"}}]}}}}"#,
            marker.display()
        ),
    );

    let runner =
        HookRunner::load_with_user_file(workspace.path(), &settings(true, false), Some(&user_file))
            .unwrap();
    assert_eq!(runner.len(), 1);
    runner
        .run(HookEvent::SessionStart, &HookContext::default())
        .await;
    assert!(
        marker.exists(),
        "the user's own hooks run without project trust"
    );
}

#[tokio::test]
async fn exit_code_two_denies_with_stderr_as_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let user_file = write_hooks(
        dir.path(),
        r#"{"hooks":{"pre_tool_use":[{"command":"echo 'rm -rf is forbidden' >&2; exit 2"}]}}"#,
    );
    let runner =
        HookRunner::load_with_user_file(dir.path(), &settings(true, false), Some(&user_file))
            .unwrap();

    let outcome = runner
        .run(
            HookEvent::PreToolUse,
            &HookContext {
                tool: Some("terminal"),
                arguments: Some(&serde_json::json!({"command": "rm -rf /"})),
                ..HookContext::default()
            },
        )
        .await;

    assert!(outcome.is_denied());
    assert_eq!(outcome.denial(), Some("rm -rf is forbidden"));
}

#[tokio::test]
async fn json_decision_deny_is_honoured() {
    let dir = tempfile::tempdir().unwrap();
    let user_file = write_hooks(
        dir.path(),
        r#"{"hooks":{"pre_tool_use":[{"command":"echo '{\"decision\":\"deny\",\"reason\":\"not on main\"}'"}]}}"#,
    );
    let runner =
        HookRunner::load_with_user_file(dir.path(), &settings(true, true), Some(&user_file))
            .unwrap();

    let outcome = runner
        .run(
            HookEvent::PreToolUse,
            &HookContext {
                tool: Some("terminal"),
                ..HookContext::default()
            },
        )
        .await;

    assert_eq!(
        outcome.verdict,
        HookVerdict::Deny("not on main".to_string())
    );
}

#[tokio::test]
async fn updated_input_rewrites_the_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let user_file = write_hooks(
        dir.path(),
        r#"{"hooks":{"pre_tool_use":[{"command":"echo '{\"updatedInput\":{\"command\":\"ls\"}}'"}]}}"#,
    );
    let runner =
        HookRunner::load_with_user_file(dir.path(), &settings(true, true), Some(&user_file))
            .unwrap();

    let outcome = runner
        .run(
            HookEvent::PreToolUse,
            &HookContext {
                tool: Some("terminal"),
                arguments: Some(&serde_json::json!({"command": "rm -rf build"})),
                ..HookContext::default()
            },
        )
        .await;

    assert_eq!(
        outcome.verdict,
        HookVerdict::ProceedWith(serde_json::json!({"command": "ls"}))
    );
}

#[tokio::test]
async fn matcher_scopes_a_hook_to_one_tool() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let user_file = write_hooks(
        dir.path(),
        &format!(
            r#"{{"hooks":{{"pre_tool_use":[{{"matcher":"write_*","command":"touch {}"}}]}}}}"#,
            marker.display()
        ),
    );
    let runner =
        HookRunner::load_with_user_file(dir.path(), &settings(true, true), Some(&user_file))
            .unwrap();

    runner
        .run(
            HookEvent::PreToolUse,
            &HookContext {
                tool: Some("read_file"),
                ..HookContext::default()
            },
        )
        .await;
    assert!(
        !marker.exists(),
        "a non-matching tool must not trigger the hook"
    );

    runner
        .run(
            HookEvent::PreToolUse,
            &HookContext {
                tool: Some("write_file"),
                ..HookContext::default()
            },
        )
        .await;
    assert!(marker.exists(), "a glob matcher must match its tool");
}

#[tokio::test]
async fn the_payload_carries_the_event_and_the_tool_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let captured = dir.path().join("payload.json");
    let user_file = write_hooks(
        dir.path(),
        &format!(
            r#"{{"hooks":{{"pre_tool_use":[{{"command":"cat > {}"}}]}}}}"#,
            captured.display()
        ),
    );
    let runner =
        HookRunner::load_with_user_file(dir.path(), &settings(true, true), Some(&user_file))
            .unwrap();

    runner
        .run(
            HookEvent::PreToolUse,
            &HookContext {
                session_id: Some("sess-1"),
                tool: Some("terminal"),
                arguments: Some(&serde_json::json!({"command": "echo hi"})),
                ..HookContext::default()
            },
        )
        .await;

    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&captured).unwrap()).unwrap();
    assert_eq!(payload["event"], "pre_tool_use");
    assert_eq!(payload["tool"], "terminal");
    assert_eq!(payload["session_id"], "sess-1");
    assert_eq!(payload["arguments"]["command"], "echo hi");
    assert_eq!(payload["cwd"], dir.path().to_string_lossy().to_string());
}

#[tokio::test]
async fn a_timed_out_gating_hook_denies() {
    let dir = tempfile::tempdir().unwrap();
    let user_file = write_hooks(
        dir.path(),
        r#"{"hooks":{"pre_tool_use":[{"command":"sleep 5","timeout_secs":1}]}}"#,
    );
    let runner =
        HookRunner::load_with_user_file(dir.path(), &settings(true, true), Some(&user_file))
            .unwrap();

    let outcome = runner
        .run(
            HookEvent::PreToolUse,
            &HookContext {
                tool: Some("terminal"),
                ..HookContext::default()
            },
        )
        .await;

    // Fail closed: a guardrail that cannot answer in time has not said yes.
    assert!(
        outcome.is_denied(),
        "gating events must fail closed on timeout"
    );
}

#[tokio::test]
async fn an_unknown_event_name_is_a_configuration_error() {
    let dir = tempfile::tempdir().unwrap();
    let user_file = write_hooks(dir.path(), r#"{"hooks":{"pre_tool_usee":[]}}"#);
    let error =
        HookRunner::load_with_user_file(dir.path(), &settings(true, true), Some(&user_file))
            .err()
            .expect("a typo in an event name must not silently disable a guardrail");
    assert!(
        error.to_string().contains("pre_tool_usee"),
        "the error must name the offending event: {error}"
    );
}

#[tokio::test]
async fn disabled_events_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let user_file = write_hooks(
        dir.path(),
        &format!(
            r#"{{"hooks":{{"session_start":[{{"command":"touch {}"}}]}}}}"#,
            marker.display()
        ),
    );
    let mut config = settings(true, true);
    config.disabled = vec!["session_start".to_string()];
    let runner = HookRunner::load_with_user_file(dir.path(), &config, Some(&user_file)).unwrap();
    runner
        .run(HookEvent::SessionStart, &HookContext::default())
        .await;
    assert!(!marker.exists());
}

#[tokio::test]
async fn a_non_blocking_hook_failure_does_not_break_the_event() {
    let dir = tempfile::tempdir().unwrap();
    let user_file = write_hooks(
        dir.path(),
        r#"{"hooks":{"post_tool_use":[{"command":"echo boom >&2; exit 9"}]}}"#,
    );
    let runner =
        HookRunner::load_with_user_file(dir.path(), &settings(true, true), Some(&user_file))
            .unwrap();

    let outcome = runner
        .run(
            HookEvent::PostToolUse,
            &HookContext {
                tool: Some("terminal"),
                ..HookContext::default()
            },
        )
        .await;
    assert_eq!(
        outcome.verdict,
        HookVerdict::Proceed,
        "observability hooks must not fail a turn"
    );
}

// ---- agent-level wiring -------------------------------------------------------------------

struct OneCallProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for OneCallProvider {
    fn provider_id(&self) -> &str {
        "mock"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["mock".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            Ok(Message::assistant_with_tools(vec![ToolCall {
                id: "call_1".to_string(),
                name: "recorder".to_string(),
                arguments: serde_json::json!({"value": "original"}),
                signature: None,
            }]))
        } else {
            Ok(Message::assistant("done"))
        }
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

struct StubMemory;
#[async_trait]
impl MemoryEngine for StubMemory {
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

struct StubNotes;
#[async_trait]
impl NotesEngine for StubNotes {
    async fn save_note(
        &self,
        _title: &str,
        _content: &str,
        _tags: &[String],
    ) -> Result<Note, HarnessError> {
        unimplemented!()
    }
    async fn search_notes(&self, _query: &str, _limit: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn list_notes(&self, _limit: usize, _offset: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn read_note(&self, _id: &str) -> Result<Note, HarnessError> {
        unimplemented!()
    }
    async fn update_note(
        &self,
        _id: &str,
        _title: Option<&str>,
        _content: Option<&str>,
        _tags: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        unimplemented!()
    }
    async fn delete_note(&self, _id: &str) -> Result<bool, HarnessError> {
        unimplemented!()
    }
}

struct StubEvolver;
#[async_trait]
impl EvolverEngine for StubEvolver {
    async fn inspect_source(&self, _path: &str) -> Result<String, HarnessError> {
        Ok(String::new())
    }
    async fn propose_patch(&self, _desc: &str, _diff: &str) -> Result<String, HarnessError> {
        Ok("patch_1".to_string())
    }
    async fn test_patch_sandbox(&self, _patch_id: &str) -> Result<bool, HarnessError> {
        Ok(true)
    }
    async fn apply_patch(&self, _patch_id: &str) -> Result<(), HarnessError> {
        Ok(())
    }
}

struct RecorderTool {
    seen: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

#[async_trait]
impl Tool for RecorderTool {
    fn name(&self) -> &str {
        "recorder"
    }

    fn description(&self) -> &str {
        "Records the arguments it was called with"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        self.seen.lock().unwrap().push(arguments.clone());
        Ok(ToolResult::success(call_id, "recorded"))
    }
}

async fn run_agent_with_hooks(
    workspace: &Path,
    hooks_body: &str,
    trust_workspace: bool,
) -> (TurnOutcome, Vec<serde_json::Value>) {
    project_hooks(workspace, hooks_body);
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));

    let registry = Arc::new(DefaultToolRegistry::new());
    registry
        .register(Arc::new(RecorderTool { seen: seen.clone() }))
        .await
        .unwrap();

    let provider = Arc::new(OneCallProvider {
        calls: AtomicUsize::new(0),
    });
    let memory = Arc::new(StubMemory);
    let notes = Arc::new(StubNotes);
    let evolver = Arc::new(StubEvolver);

    let runner = HookRunner::load(workspace, &settings(true, trust_workspace)).unwrap();
    let agent = Agent::new(
        provider,
        memory,
        notes,
        evolver,
        registry,
        AgentConfig::default(),
    )
    .with_approval(Arc::new(AllowAll), ApprovalSettings::default())
    .with_hooks(Arc::new(runner));

    let outcome = agent
        .run_turn("go", CancellationToken::new())
        .await
        .expect("turn");
    let seen = seen.lock().unwrap().clone();
    (outcome, seen)
}

#[tokio::test]
async fn a_pre_tool_use_hook_refuses_the_call_before_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (outcome, seen) = run_agent_with_hooks(
        dir.path(),
        r#"{"hooks":{"pre_tool_use":[{"matcher":"recorder","command":"exit 2"}]}}"#,
        true,
    )
    .await;

    assert!(seen.is_empty(), "a denied call must never reach the tool");
    assert_eq!(outcome.tool_calls.len(), 1);
    assert!(outcome.tool_calls[0].is_error);
    assert!(
        outcome.tool_calls[0].output.contains("approval denied"),
        "the refusal must be reported to the model: {}",
        outcome.tool_calls[0].output
    );
}

#[tokio::test]
async fn a_pre_tool_use_hook_can_rewrite_what_the_tool_receives() {
    let dir = tempfile::tempdir().unwrap();
    let (_outcome, seen) = run_agent_with_hooks(
        dir.path(),
        r#"{"hooks":{"pre_tool_use":[{"matcher":"recorder","command":"echo '{\"updatedInput\":{\"value\":\"rewritten\"}}'"}]}}"#,
        true,
    )
    .await;

    assert_eq!(seen.len(), 1, "the tool must run exactly once");
    assert_eq!(seen[0]["value"], "rewritten");
}
