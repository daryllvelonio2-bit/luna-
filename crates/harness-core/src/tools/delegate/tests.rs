use super::*;
use async_trait::async_trait;
use futures_util::stream::{BoxStream, StreamExt};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn the_inactivity_watchdog_stops_a_silent_child() {
    let tracker = SubagentActivityTracker::new("sub_silent".to_string(), "wait".to_string(), None);
    let cancel = CancellationToken::new();

    let result = await_subagent_with_inactivity_timeout(
        std::future::pending::<()>(),
        &tracker,
        Duration::from_millis(40),
        &cancel,
    )
    .await;

    assert!(result.is_err(), "a silent child must not run forever");
    assert!(
        result.unwrap_err().contains("no progress"),
        "the failure names the inactivity timeout"
    );
    assert!(cancel.is_cancelled(), "the stalled child is cancelled");
}

#[tokio::test]
async fn progress_resets_the_inactivity_watchdog() {
    let tracker = SubagentActivityTracker::new("sub_active".to_string(), "work".to_string(), None);
    let progress_tracker = tracker.clone();
    let cancel = CancellationToken::new();
    let progress = async move {
        for _ in 0..8 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            progress_tracker.record(SubagentActivity::ModelStreaming);
        }
        Ok::<(), String>(())
    };

    let result = await_subagent_with_inactivity_timeout(
        progress,
        &tracker,
        Duration::from_millis(100),
        &cancel,
    )
    .await;

    assert!(
        result.is_ok(),
        "a child that keeps reporting progress is not stopped by total duration"
    );
}

struct EchoProvider;

#[async_trait]
impl ModelProvider for EchoProvider {
    fn provider_id(&self) -> &str {
        "mock"
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
        Ok(Message::assistant("done"))
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        let events: Vec<Result<StreamEvent, HarnessError>> = vec![
            Ok(StreamEvent::ContentDelta("done".to_string())),
            Ok(StreamEvent::Completed {
                finish_reason: Some("stop".to_string()),
                usage: None,
            }),
        ];
        Ok(futures_util::stream::iter(events).boxed())
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(Vec::new())
    }
}

struct EmptyMemory;

#[async_trait]
impl MemoryEngine for EmptyMemory {
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

    async fn update(&self, _id: &str, _record: &MemoryRecord) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn forget(&self, _id: &str) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn add_note_pointer(&self, _note_id: &str, _pointer: &str) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn remove_note_pointer(&self, _note_id: &str) -> Result<(), HarnessError> {
        Ok(())
    }

    fn format_as_system_prompt(&self, _memories: &[MemoryRecord]) -> String {
        String::new()
    }
}

struct EmptyNotes;

#[async_trait]
impl NotesEngine for EmptyNotes {
    async fn save_note(
        &self,
        _title: &str,
        _content: &str,
        _tags: &[String],
    ) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }

    async fn search_notes(&self, _query: &str, _limit: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(Vec::new())
    }

    async fn list_notes(&self, _limit: usize, _offset: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(Vec::new())
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

#[tokio::test]
async fn a_background_child_reports_start_progress_and_finish() {
    let tmp = tempfile::tempdir().expect("temporary directory");
    let mut config = Config::default();
    config.paths.workspace_root = tmp.path().to_path_buf();
    config.paths.todos_dir = tmp.path().join("todos");
    config.paths.jobs_dir = tmp.path().join("jobs");

    let delegate = DelegateTaskTool::new(
        Arc::new(EchoProvider),
        Arc::new(EmptyMemory),
        Arc::new(EmptyNotes),
        None,
        config,
    );
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_by_callback = seen.clone();
    delegate.set_event_notifier(Arc::new(move |event| {
        let label = match event {
            SubagentTaskEvent::Started { .. } => "started",
            SubagentTaskEvent::Progress { .. } => "progress",
            SubagentTaskEvent::Finished { .. } => "finished",
        };
        seen_by_callback.lock().unwrap().push(label.to_string());
    }));

    let cancel = CancellationToken::new();
    let result = delegate
        .execute(
            "call_background",
            &json!({ "tasks": [{ "goal": "Inspect the delegate path" }] }),
            &cancel,
        )
        .await
        .expect("background spawn succeeds");

    assert!(!result.is_error, "{}", result.content);
    for _ in 0..200 {
        if seen.lock().unwrap().len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let seen = seen.lock().unwrap().clone();
    assert!(seen.contains(&"started".to_string()), "{seen:?}");
    assert!(seen.contains(&"progress".to_string()), "{seen:?}");
    assert!(seen.contains(&"finished".to_string()), "{seen:?}");
}

#[tokio::test]
async fn a_worker_forks_the_conversation_and_reports_as_a_subagent() {
    let tmp = tempfile::tempdir().expect("temporary directory");
    let mut config = Config::default();
    config.paths.workspace_root = tmp.path().to_path_buf();
    config.paths.todos_dir = tmp.path().join("todos");
    config.paths.jobs_dir = tmp.path().join("jobs");

    let store = Arc::new(
        SqliteSessionStore::new(Some(tmp.path().join("sessions.db"))).expect("open session store"),
    );
    let source_id = store.ensure_session("s_source").await.unwrap().id;
    store
        .append(
            &source_id,
            &Message::user("the bug is in src/parser.rs"),
            false,
        )
        .await
        .unwrap();

    let delegate = DelegateTaskTool::new(
        Arc::new(EchoProvider),
        Arc::new(EmptyMemory),
        Arc::new(EmptyNotes),
        Some(store.clone()),
        config,
    );
    let seen = Arc::new(Mutex::new(Vec::<SubagentTaskEvent>::new()));
    let seen_by_callback = seen.clone();
    delegate.set_event_notifier(Arc::new(move |event| {
        seen_by_callback.lock().unwrap().push(event);
    }));

    let profile = WorkerProfile {
        provider: "nvidia".to_string(),
        model: "tiny-model".to_string(),
        reasoning_effort: Some(ReasoningEffort::Low),
    };
    let child_id = delegate
        .spawn_worker(
            "carbon",
            "fix the failing parser test",
            None,
            &source_id,
            Arc::new(EchoProvider),
            &profile,
        )
        .await
        .expect("worker spawns");

    // The fork is seeded from the source conversation and carries the worker identity + model.
    let record = store.get(&child_id).await.unwrap();
    assert_eq!(record.name, "worker:carbon");
    assert_eq!(
        record.model,
        Some(("nvidia".to_string(), "tiny-model".to_string()))
    );
    let replay = store.replay(&child_id, 100).await.unwrap();
    let history = replay
        .history
        .iter()
        .filter_map(|stored| stored.message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        history.contains("the bug is in src/parser.rs"),
        "worker replays the caller's conversation: {history}"
    );

    let mut finished = false;
    for _ in 0..300 {
        let done = seen
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, SubagentTaskEvent::Finished { .. }));
        if done {
            finished = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(finished, "worker must report a finish event");

    let record = delegate
        .list_tasks()
        .await
        .into_iter()
        .find(|task| task.id == child_id)
        .expect("worker is recorded as a subagent task");
    assert!(
        record
            .context
            .as_deref()
            .is_some_and(|context| context.starts_with("worker profile:")),
        "the record is marked as a worker for the viewer"
    );
}

#[test]
fn test_delegate_tool_mutating_calls() {
    let dummy = DelegateTaskTool::new(
        Arc::new(EchoProvider),
        Arc::new(EmptyMemory),
        Arc::new(EmptyNotes),
        None,
        Config::default(),
    );

    assert!(dummy.is_mutating());
    assert!(dummy.is_mutating_call(&json!({"action": "spawn", "tasks": [{"goal": "test"}]})));
    assert!(dummy.is_mutating_call(&json!({"tasks": [{"goal": "test"}]})));
    assert!(!dummy.is_mutating_call(&json!({"action": "list"})));
    assert!(!dummy.is_mutating_call(&json!({"action": "poll", "task_id": "sub_1"})));
    assert!(!dummy.is_mutating_call(&json!({"action": "wait", "timeout_secs": 10})));
}

#[tokio::test]
async fn test_subagent_child_approver_fails_closed() {
    use crate::approval::{ApprovalRequest, Approver};
    let approver = NonInteractive::new("delegated subagent");
    let req = ApprovalRequest::default();
    let err = approver.approve(&req).await.unwrap_err();
    assert!(matches!(err, HarnessError::ApprovalRejected(_)));
}

#[tokio::test]
async fn test_watchdog_does_not_timeout_while_tool_is_running() {
    let tracker =
        SubagentActivityTracker::new("sub_tool_busy".to_string(), "tool".to_string(), None);
    let cancel = CancellationToken::new();

    // Tool starts: active_tools becomes 1
    tracker.record(SubagentActivity::ToolStarted {
        name: "long_running_tool".to_string(),
    });

    let work = async {
        // Sleep longer than the idle timeout
        tokio::time::sleep(Duration::from_millis(80)).await;
        "success"
    };

    let result =
        await_subagent_with_inactivity_timeout(work, &tracker, Duration::from_millis(30), &cancel)
            .await;

    assert!(result.is_ok(), "active tool must prevent idle timeout");
    assert_eq!(result.unwrap(), "success");
    assert!(!cancel.is_cancelled());
}
