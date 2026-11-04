//! Soft interrupt: a message typed while the model is streaming stops that call, keeps what was
//! already produced, and continues the same turn. These tests pin the three cases apart: streaming
//! (cut now, keep the partial), a tool batch (wait for it, then deliver), and the hard cancel that
//! still ends the turn.

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use harness_core::*;

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
        _t: Option<&str>,
        _c: Option<&str>,
        _tags: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn delete_note(&self, _id: &str) -> Result<bool, HarnessError> {
        unreachable!("not used by this test")
    }
}

/// A stream that produces a thought and a partial answer, then holds the request open as a real
/// provider would. The soft interrupt is what ends it; the provider never does.
fn held_stream(started: Arc<Notify>) -> BoxStream<'static, Result<StreamEvent, HarnessError>> {
    Box::pin(stream::unfold(0u8, move |step| {
        let started = started.clone();
        async move {
            match step {
                0 => Some((
                    Ok(StreamEvent::ReasoningDelta("thinking hard".to_string())),
                    1,
                )),
                1 => {
                    started.notify_one();
                    Some((
                        Ok(StreamEvent::ContentDelta("partial answer".to_string())),
                        2,
                    ))
                }
                _ => {
                    // The request stays open until the test asks for it to stop. A provider that
                    // finishes on its own would not exercise the interrupt at all.
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    Some((
                        Ok(StreamEvent::ContentDelta("late text".to_string())),
                        step + 1,
                    ))
                }
            }
        }
    }))
}

/// Answers the first call with a stream, then answers the follow-up; the follow-up request is
/// kept so the test can prove the message and the partial reply were both sent back.
struct StreamingInterruptProvider {
    calls: AtomicUsize,
    started: Arc<Notify>,
    last_request: Mutex<Vec<Message>>,
}

#[async_trait]
impl ModelProvider for StreamingInterruptProvider {
    fn provider_id(&self) -> &str {
        "streaming-interrupt-mock"
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
        unreachable!("the streamed path must answer, not the buffered one")
    }

    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(held_stream(self.started.clone()));
        }
        *self.last_request.lock().unwrap() = messages.to_vec();
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::ContentDelta("second answer".to_string())),
            Ok(StreamEvent::Completed {
                usage: None,
                finish_reason: Some("stop".to_string()),
            }),
        ])))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn a_message_during_a_reply_keeps_what_was_thought_and_continues() {
    let started = Arc::new(Notify::new());
    let provider = Arc::new(StreamingInterruptProvider {
        calls: AtomicUsize::new(0),
        started: started.clone(),
        last_request: Mutex::new(Vec::new()),
    });
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_obs = events.clone();
    let agent = Agent::new(
        provider.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        AgentConfig::default(),
    )
    .with_observer(Arc::new(move |event| {
        events_obs.lock().unwrap().push(event);
    }));
    let handle = agent.interrupt_handle();
    let agent = Arc::new(agent);

    let run = tokio::spawn({
        let agent = agent.clone();
        async move {
            agent
                .run_turn("initial question", CancellationToken::new())
                .await
        }
    });

    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .expect("the first call never streamed");
    assert_eq!(
        handle.interrupt("actually do this instead"),
        InterruptOutcome::Streaming
    );

    let outcome = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("the turn never finished after the interrupt")
        .expect("the turn task panicked")
        .expect("the turn failed");
    assert_eq!(outcome.reply, "second answer");
    assert!(!handle.turn_active(), "the guard must clear after the turn");

    // The follow-up request carries both the partial reply and the message that cut it off.
    let request = provider.last_request.lock().unwrap().clone();
    assert!(
        request.iter().any(|message| message.role == Role::Assistant
            && message
                .content
                .as_deref()
                .unwrap_or("")
                .contains("partial answer")),
        "the partial reply was thrown away: {request:?}"
    );
    assert!(
        request.iter().any(|message| message.role == Role::User
            && message.content.as_deref() == Some("actually do this instead")),
        "the message never reached the next request: {request:?}"
    );

    let events = events.lock().unwrap();
    let interrupted = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::Interrupted {
                text,
                reasoning,
                partial,
            } => Some((text.clone(), reasoning.clone(), partial.clone())),
            _ => None,
        })
        .expect("the UI was never told about the interrupt");
    assert_eq!(interrupted.0, "actually do this instead");
    assert!(interrupted.1.contains("thinking hard"), "{interrupted:?}");
    assert!(interrupted.2.contains("partial answer"), "{interrupted:?}");
    assert!(
        interrupted.2.contains("interrupted by your next message"),
        "the partial reply has to be marked: {interrupted:?}"
    );
}

struct SlowTool {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Tool for SlowTool {
    fn name(&self) -> &str {
        "slow_tool"
    }
    fn description(&self) -> &str {
        "blocks until the test lets it finish"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    fn is_mutating(&self) -> bool {
        false
    }
    async fn execute(
        &self,
        call_id: &str,
        _arguments: &serde_json::Value,
        _cancel: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(ToolResult::success(call_id, "tool output"))
    }
}

/// Buffered-only provider: asks for `slow_tool`, then answers the follow-up. `complete` is used
/// because the default `stream` is not implemented, which is the same fallback a non-streaming
/// provider takes in production.
struct ToolWaitProvider {
    calls: AtomicUsize,
    last_request: Mutex<Vec<Message>>,
}

#[async_trait]
impl ModelProvider for ToolWaitProvider {
    fn provider_id(&self) -> &str {
        "tool-wait-mock"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["mock".to_string()])
    }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        *self.last_request.lock().unwrap() = messages.to_vec();
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(Message::assistant_with_tools(vec![ToolCall {
                id: "call_1".to_string(),
                name: "slow_tool".to_string(),
                arguments: serde_json::json!({}),
                signature: None,
            }]));
        }
        Ok(Message::assistant("done after the tool"))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn a_message_during_a_tool_waits_for_the_tool_then_goes_out() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = Arc::new(ToolWaitProvider {
        calls: AtomicUsize::new(0),
        last_request: Mutex::new(Vec::new()),
    });
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_obs = events.clone();
    let registry = Arc::new(DefaultToolRegistry::new());
    registry
        .register(Arc::new(SlowTool {
            started: started.clone(),
            release: release.clone(),
        }))
        .await
        .unwrap();
    let agent = Agent::new(
        provider.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        registry,
        AgentConfig::default(),
    )
    .with_observer(Arc::new(move |event| {
        events_obs.lock().unwrap().push(event);
    }));
    let handle = agent.interrupt_handle();
    let agent = Arc::new(agent);

    let run = tokio::spawn({
        let agent = agent.clone();
        async move {
            agent
                .run_turn("run the slow tool", CancellationToken::new())
                .await
        }
    });

    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .expect("the tool never started");
    // The tool is mid-flight: nothing in the model is streaming, so the line waits.
    assert_eq!(handle.interrupt("after the tool"), InterruptOutcome::Queued);
    release.notify_one();

    let outcome = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("the turn never finished")
        .expect("the turn task panicked")
        .expect("the turn failed");
    assert_eq!(outcome.reply, "done after the tool");

    // The follow-up comes after the tool's answer, not before it: the tool got to finish first.
    let request = provider.last_request.lock().unwrap().clone();
    let tool_index = request
        .iter()
        .position(|message| message.role == Role::Tool)
        .expect("the tool result is missing from the request");
    let follow_up_index = request
        .iter()
        .position(|message| {
            message.role == Role::User && message.content.as_deref() == Some("after the tool")
        })
        .expect("the follow-up is missing from the request");
    assert!(
        follow_up_index > tool_index,
        "the message had to wait for the tool: {request:?}"
    );

    let events = events.lock().unwrap();
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Interrupted { text, .. } if text == "after the tool"
        )),
        "the delayed delivery was never announced: {events:?}"
    );
}

struct HeldProvider {
    started: Arc<Notify>,
}

#[async_trait]
impl ModelProvider for HeldProvider {
    fn provider_id(&self) -> &str {
        "held-mock"
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
        unreachable!("the held stream is the point of this provider")
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        Ok(held_stream(self.started.clone()))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn a_hard_cancel_still_ends_the_turn_and_starts_no_follow_up() {
    let started = Arc::new(Notify::new());
    let provider = Arc::new(HeldProvider {
        started: started.clone(),
    });
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_obs = events.clone();
    let agent = Agent::new(
        provider,
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        AgentConfig::default(),
    )
    .with_observer(Arc::new(move |event| {
        events_obs.lock().unwrap().push(event);
    }));
    let agent = Arc::new(agent);

    let cancel = CancellationToken::new();
    let run = tokio::spawn({
        let agent = agent.clone();
        let cancel = cancel.clone();
        async move { agent.run_turn("initial question", cancel).await }
    });
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .expect("the first call never streamed");
    cancel.cancel();

    let result = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("the hard cancel did not stop the turn")
        .expect("the turn task panicked");
    assert!(
        matches!(result, Err(HarnessError::Cancelled)),
        "a hard cancel must stay a cancelled turn: {result:?}"
    );
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, AgentEvent::Interrupted { .. })),
        "a hard cancel must not be reported as a soft interrupt"
    );
}

#[tokio::test]
async fn a_message_with_no_turn_running_is_refused() {
    let agent = Agent::new(
        Arc::new(HeldProvider {
            started: Arc::new(Notify::new()),
        }),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        AgentConfig::default(),
    );
    let handle = agent.interrupt_handle();
    assert_eq!(
        handle.interrupt("nobody is listening"),
        InterruptOutcome::Idle
    );
    assert!(handle.take_leftover().is_empty());
}
