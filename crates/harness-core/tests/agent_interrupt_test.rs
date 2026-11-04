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

/// The OpenCode shape: the provider runs tools inside its own model call and streams
/// `ToolExecuting` / `ToolExecuted`, then answers with prose and no `tool_calls` for the agent to
/// dispatch. Those calls and results still have to reach the transcript, or a later "continue"
/// replays only the final sentence and loses what already ran.
struct ProviderExecutedToolProvider {
    calls: AtomicUsize,
    last_request: Mutex<Vec<Message>>,
}

#[async_trait]
impl ModelProvider for ProviderExecutedToolProvider {
    fn provider_id(&self) -> &str {
        "provider-executed-tools-mock"
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
        *self.last_request.lock().unwrap() = messages.to_vec();
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(Box::pin(stream::iter(vec![
                Ok(StreamEvent::ToolExecuting {
                    name: "bash".to_string(),
                    arguments: serde_json::json!({ "command": "echo hello-from-tool" }),
                }),
                Ok(StreamEvent::ToolExecuted {
                    name: "bash".to_string(),
                    output: "hello-from-tool\n".to_string(),
                    is_error: false,
                }),
                Ok(StreamEvent::ContentDelta("final answer".to_string())),
                Ok(StreamEvent::Completed {
                    usage: None,
                    finish_reason: Some("stop".to_string()),
                }),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::ContentDelta("done after continue".to_string())),
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
async fn provider_executed_tools_are_persisted_and_replayed() {
    let provider = Arc::new(ProviderExecutedToolProvider {
        calls: AtomicUsize::new(0),
        last_request: Mutex::new(Vec::new()),
    });
    let store = Arc::new(SqliteSessionStore::in_memory().expect("in-memory store"));
    let agent = Agent::new(
        provider.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        AgentConfig::default(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s_provider_tools".to_string(),
        resume_max_messages: 200,
    });

    let first = agent
        .run_turn("run the tool", CancellationToken::new())
        .await
        .expect("the first turn runs");
    assert_eq!(first.reply, "final answer");
    assert_eq!(first.tool_calls.len(), 1, "the call is still reported");
    assert_eq!(first.tool_calls[0].name, "bash");

    let replay = store
        .replay("s_provider_tools", 100)
        .await
        .expect("the transcript replays");
    let call_row = replay
        .history
        .iter()
        .position(|stored| !stored.message.tool_calls.is_empty())
        .expect("the assistant tool-call row was written");
    let call = &replay.history[call_row].message.tool_calls[0];
    assert!(
        call.id.starts_with("call_oc_"),
        "the synthetic id has to be a legal call id: {call:?}"
    );
    let result_row = replay
        .history
        .iter()
        .position(|stored| {
            stored
                .message
                .tool_result
                .as_ref()
                .is_some_and(|result| result.call_id == call.id)
        })
        .expect("the matching tool-result row was written");
    assert!(result_row > call_row, "the result follows its call");
    assert_eq!(
        replay.history[result_row]
            .message
            .tool_result
            .as_ref()
            .unwrap()
            .content,
        "hello-from-tool\n"
    );
    let final_row = replay
        .history
        .iter()
        .rposition(|stored| stored.message.content.as_deref() == Some("final answer"))
        .expect("the final prose was written");
    assert!(final_row > result_row, "the answer follows the result");

    // The next turn is the proof: a resumed request carries the call and its result, not just the
    // last sentence.
    let second = agent
        .run_turn("continue", CancellationToken::new())
        .await
        .expect("the second turn runs");
    assert_eq!(second.reply, "done after continue");
    let request = provider.last_request.lock().unwrap().clone();
    assert!(
        request
            .iter()
            .any(|message| message.tool_calls.iter().any(|sent| sent.id == call.id)),
        "the call was dropped from the next request: {request:?}"
    );
    assert!(
        request.iter().any(|message| message
            .tool_result
            .as_ref()
            .is_some_and(|result| result.call_id == call.id)),
        "the result was dropped from the next request: {request:?}"
    );
}

/// OpenCode's native stream reports one `Usage` per inner step and closes with a cumulative
/// `Completed` total. The ledger bills the total; the gauge and the stored row must keep the last
/// step's prompt, or the readout climbs past the model window.
struct InnerLoopUsageProvider;

#[async_trait]
impl ModelProvider for InnerLoopUsageProvider {
    fn provider_id(&self) -> &str {
        "inner-loop-usage-mock"
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
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::Usage(TokenUsage::estimated(100, 10))),
            Ok(StreamEvent::Usage(TokenUsage::estimated(700, 40))),
            Ok(StreamEvent::ContentDelta("done".to_string())),
            Ok(StreamEvent::Completed {
                usage: Some(TokenUsage::estimated(800, 50)),
                finish_reason: Some("stop".to_string()),
            }),
        ])))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn the_gauge_keeps_the_last_step_while_the_ledger_bills_the_whole_call() {
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_obs = events.clone();
    let store = Arc::new(SqliteSessionStore::in_memory().expect("in-memory store"));
    let agent = Agent::new(
        Arc::new(InnerLoopUsageProvider),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        AgentConfig::default(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s_inner_loop".to_string(),
        resume_max_messages: 200,
    })
    .with_observer(Arc::new(move |event| {
        events_obs.lock().unwrap().push(event);
    }));

    let outcome = agent
        .run_turn("go", CancellationToken::new())
        .await
        .expect("the turn runs");

    let billed = outcome.usage.expect("the cumulative total is billed");
    assert_eq!(billed.prompt_tokens, 800);
    assert_eq!(billed.completion_tokens, 50);

    let gauge = events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match event {
            AgentEvent::ContextTokensUpdated { tokens_used } => Some(*tokens_used),
            _ => None,
        });
    assert_eq!(
        gauge,
        Some(740),
        "the gauge must show the last step's prompt (700 + 40), never the cumulative 850"
    );

    let replay = store
        .replay("s_inner_loop", 100)
        .await
        .expect("the transcript replays");
    let stored = replay
        .history
        .iter()
        .rev()
        .find_map(|stored| stored.message.usage.as_ref())
        .expect("the answer carries the per-step usage");
    assert_eq!(stored.prompt_tokens, 700);
    assert_eq!(stored.completion_tokens, 40);
    assert_eq!(
        stored.prompt_tokens + stored.completion_tokens,
        740,
        "a resumed session must estimate the prompt, not the call total"
    );
}

/// A provider that reports its tools, then keeps the call open until the test cancels it. The
/// call and its result already happened; the stop must still write them to the transcript.
struct ProviderToolThenHoldProvider {
    started: Arc<Notify>,
}

fn provider_tool_then_hold(
    started: Arc<Notify>,
) -> BoxStream<'static, Result<StreamEvent, HarnessError>> {
    Box::pin(stream::unfold(0u8, move |step| {
        let started = started.clone();
        async move {
            match step {
                0 => Some((
                    Ok(StreamEvent::ToolExecuting {
                        name: "shell".to_string(),
                        arguments: serde_json::json!({ "command": "echo persisted" }),
                    }),
                    1,
                )),
                1 => Some((
                    Ok(StreamEvent::ToolExecuted {
                        name: "shell".to_string(),
                        output: "persisted\n".to_string(),
                        is_error: false,
                    }),
                    2,
                )),
                2 => {
                    started.notify_one();
                    Some((Ok(StreamEvent::ContentDelta("partial".to_string())), 3))
                }
                _ => {
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

#[async_trait]
impl ModelProvider for ProviderToolThenHoldProvider {
    fn provider_id(&self) -> &str {
        "provider-tool-then-hold-mock"
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
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        Ok(provider_tool_then_hold(self.started.clone()))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn a_stop_keeps_the_provider_tools_that_already_ran() {
    let started = Arc::new(Notify::new());
    let store = Arc::new(SqliteSessionStore::in_memory().expect("in-memory store"));
    let agent = Arc::new(
        Agent::new(
            Arc::new(ProviderToolThenHoldProvider {
                started: started.clone(),
            }),
            Arc::new(MockMemory),
            Arc::new(MockNotes),
            Arc::new(DefaultToolRegistry::new()),
            AgentConfig::default(),
        )
        .with_session(SessionBinding {
            store: store.clone(),
            session_id: "s_stopped_tools".to_string(),
            resume_max_messages: 200,
        }),
    );

    let cancel = CancellationToken::new();
    let run = tokio::spawn({
        let agent = agent.clone();
        let cancel = cancel.clone();
        async move { agent.run_turn("run it", cancel).await }
    });
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .expect("the provider never reported its tool");
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("the stop did not end the turn")
        .expect("the turn task panicked");
    assert!(
        matches!(result, Err(HarnessError::Cancelled)),
        "a stop must stay a cancelled turn: {result:?}"
    );

    let replay = store
        .replay("s_stopped_tools", 100)
        .await
        .expect("the transcript replays");
    let call = replay
        .history
        .iter()
        .find_map(|stored| stored.message.tool_calls.first())
        .expect("the tool call that ran before the stop was not written");
    let result = replay
        .history
        .iter()
        .find_map(|stored| stored.message.tool_result.as_ref())
        .expect("the tool result that ran before the stop was not written");
    assert_eq!(call.name, "shell");
    assert_eq!(call.arguments["command"], "echo persisted");
    assert_eq!(result.call_id, call.id);
    assert_eq!(result.content, "persisted\n");
}
