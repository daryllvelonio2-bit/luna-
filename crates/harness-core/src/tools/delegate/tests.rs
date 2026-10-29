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
