use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use harness_core::*;

struct MockProvider {
    turns: AtomicUsize,
}

impl MockProvider {
    fn new() -> Self {
        Self {
            turns: AtomicUsize::new(0),
        }
    }
}

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
        let count = self.turns.fetch_add(1, Ordering::SeqCst);
        if count < 5 {
            // Request failing tool call
            Ok(Message::assistant_with_tools(vec![ToolCall {
                id: format!("call_{}", count),
                name: "failing_tool".to_string(),
                arguments: serde_json::json!({}),
                signature: None,
            }]))
        } else {
            Ok(Message::assistant("Done!"))
        }
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        "failing_tool"
    }

    fn description(&self) -> &str {
        "Always returns an error"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    async fn execute(
        &self,
        call_id: &str,
        _arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        Ok(ToolResult::error(call_id, "Simulated tool failure"))
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

#[tokio::test]
async fn test_5_consecutive_failures_halts_agent() {
    let provider = Arc::new(MockProvider::new());
    let memory = Arc::new(MockMemory);
    let notes = Arc::new(MockNotes);
    let registry = Arc::new(DefaultToolRegistry::new());

    registry.register(Arc::new(FailingTool)).await.unwrap();

    let agent = Agent::new(
        provider,
        memory,
        notes,
        registry,
        AgentConfig {
            max_consecutive_failures: 5,
            require_hitl_for_mutation: false,
            ..AgentConfig::default()
        },
    );

    let cancel = CancellationToken::new();
    let result = agent.run_task("Test task", cancel).await;

    match result {
        Err(HarnessError::ConsecutiveFailuresExceeded {
            failures,
            last_tool,
        }) => {
            assert_eq!(failures, 5);
            assert_eq!(last_tool, "failing_tool");
        }
        other => panic!("Expected ConsecutiveFailuresExceeded, got {:?}", other),
    }
}

#[test]
fn test_error_is_overloaded_and_is_retryable_detection() {
    let err_rate = HarnessError::RateLimited {
        provider: "antigravity".to_string(),
        retry_after_secs: Some(10),
    };
    assert!(err_rate.is_overloaded());
    assert!(err_rate.is_retryable());

    let err_503 = HarnessError::ProviderError {
        provider: "antigravity".to_string(),
        message: "Model is overloaded (HTTP 503): Service Unavailable".to_string(),
    };
    assert!(err_503.is_overloaded());
    assert!(err_503.is_retryable());

    let err_quota = HarnessError::ProviderError {
        provider: "openai".to_string(),
        message: "Resource has been exhausted (check quota)".to_string(),
    };
    assert!(err_quota.is_overloaded());
    assert!(err_quota.is_retryable());

    let err_timeout = HarnessError::ProviderError {
        provider: "antigravity".to_string(),
        message: "Connection timeout waiting for server response".to_string(),
    };
    assert!(!err_timeout.is_overloaded());
    assert!(err_timeout.is_retryable());

    let err_auth = HarnessError::AuthenticationFailed {
        provider: "antigravity".to_string(),
        message: "Unauthorized".to_string(),
    };
    assert!(!err_auth.is_overloaded());
    assert!(!err_auth.is_retryable());
}

struct OverloadedMockProvider {
    attempts: AtomicUsize,
}

#[async_trait]
impl ModelProvider for OverloadedMockProvider {
    fn provider_id(&self) -> &str {
        "overloaded-mock"
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
        let n = self.attempts.fetch_add(1, Ordering::SeqCst);
        if n < 2 {
            Err(HarnessError::ProviderError {
                provider: "overloaded-mock".to_string(),
                message: "Model is overloaded (HTTP 503)".to_string(),
            })
        } else {
            Ok(Message::assistant("Recovered after overload!"))
        }
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn test_agent_retries_transient_overloaded_error_and_succeeds() {
    let provider = Arc::new(OverloadedMockProvider {
        attempts: AtomicUsize::new(0),
    });
    let memory = Arc::new(MockMemory);
    let notes = Arc::new(MockNotes);
    let registry = Arc::new(DefaultToolRegistry::new());

    let agent = Agent::new(
        provider.clone(),
        memory,
        notes,
        registry,
        AgentConfig::default(),
    );

    let cancel = CancellationToken::new();
    let result = agent.run_task("Hello", cancel).await;
    assert!(result.is_ok(), "Expected retry success, got: {:?}", result);
    assert_eq!(result.unwrap(), "Recovered after overload!");
    assert_eq!(provider.attempts.load(Ordering::SeqCst), 3);
}

struct EmptyMockProvider;

#[async_trait]
impl ModelProvider for EmptyMockProvider {
    fn provider_id(&self) -> &str {
        "empty-mock"
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
        Ok(Message::assistant(""))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn test_agent_handles_empty_response_without_getting_stuck() {
    let provider = Arc::new(EmptyMockProvider);
    let memory = Arc::new(MockMemory);
    let notes = Arc::new(MockNotes);
    let registry = Arc::new(DefaultToolRegistry::new());

    let agent = Agent::new(provider, memory, notes, registry, AgentConfig::default());

    let cancel = CancellationToken::new();
    let result = agent.run_task("Hello", cancel).await;
    assert!(
        result.is_err(),
        "Expected error on empty response, got: {:?}",
        result
    );
    let err = result.unwrap_err();
    assert!(err.to_string().contains("empty response") || err.to_string().contains("overloaded"));
}

/// A stream that ends on the output cap without one usable part: the shape Code Assist produces
/// when thinking spends the whole `maxOutputTokens` — a 200 response, a `MAX_TOKENS` finish, and
/// nothing the turn loop can use.
struct TruncatedStreamProvider {
    completions: AtomicUsize,
}

#[async_trait]
impl ModelProvider for TruncatedStreamProvider {
    fn provider_id(&self) -> &str {
        "truncated-mock"
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
        self.completions.fetch_add(1, Ordering::SeqCst);
        Ok(Message::assistant("the buffered path must not be asked"))
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<
        futures_util::stream::BoxStream<'static, Result<StreamEvent, HarnessError>>,
        HarnessError,
    > {
        Ok(Box::pin(futures_util::stream::iter(vec![Ok(
            StreamEvent::Completed {
                finish_reason: Some("max_tokens".to_string()),
                usage: Some(TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 4096,
                    total_tokens: 4106,
                    cached_tokens: 0,
                }),
            },
        )])))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

/// A cap the thinking consumed is not the transient empty shape: re-sending the identical request
/// returns the identical nothing, so the turn must fail once, with the condition named.
#[tokio::test]
async fn a_reply_cut_off_at_the_output_cap_is_not_retried_as_an_empty_answer() {
    let provider = Arc::new(TruncatedStreamProvider {
        completions: AtomicUsize::new(0),
    });
    let agent = Agent::new(
        provider.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        AgentConfig::default(),
    );

    let error = agent
        .run_task("Hello", CancellationToken::new())
        .await
        .expect_err("a truncated reply is not an answer");
    assert!(
        error.to_string().contains("output-token limit"),
        "the condition has to be named: {error}"
    );
    assert!(!error.is_retryable(), "{error}");
    assert_eq!(
        provider.completions.load(Ordering::SeqCst),
        0,
        "the buffered path must not re-send the request that just hit the cap"
    );
}

/// A stream that reaches the end of the request — it reports usage — but carries no answer.
struct UsageOnlyStreamProvider {
    streams: AtomicUsize,
    completions: AtomicUsize,
}

#[async_trait]
impl ModelProvider for UsageOnlyStreamProvider {
    fn provider_id(&self) -> &str {
        "usage-only-mock"
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
        self.completions.fetch_add(1, Ordering::SeqCst);
        Ok(Message::assistant("the duplicate request answered"))
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<
        futures_util::stream::BoxStream<'static, Result<StreamEvent, HarnessError>>,
        HarnessError,
    > {
        self.streams.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures_util::stream::iter(vec![Ok(
            StreamEvent::Completed {
                finish_reason: None,
                usage: Some(TokenUsage::estimated(10, 0)),
            },
        )])))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

/// A usage frame says the model ran and answered with nothing. Re-posting the identical request
/// through the buffered path at that moment is a duplicate, not a recovery: the retry loop owns the
/// second attempt, and it is the one that backs off first.
#[tokio::test]
async fn an_empty_stream_that_reached_the_model_is_not_reposted() {
    let provider = Arc::new(UsageOnlyStreamProvider {
        streams: AtomicUsize::new(0),
        completions: AtomicUsize::new(0),
    });
    let agent = Agent::new(
        provider.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        AgentConfig::default(),
    );

    let error = agent
        .run_task("Hello", CancellationToken::new())
        .await
        .expect_err("an empty answer is not a reply");
    assert!(
        error.to_string().contains("empty response"),
        "the shape stays diagnosable: {error}"
    );
    assert_eq!(
        provider.completions.load(Ordering::SeqCst),
        0,
        "the buffered path must not re-post a request the model already answered with nothing"
    );
    assert_eq!(
        provider.streams.load(Ordering::SeqCst),
        4,
        "one attempt per try, all owned by the retry loop"
    );
}

struct SteeringMockProvider {
    steer_observed: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl ModelProvider for SteeringMockProvider {
    fn provider_id(&self) -> &str {
        "steering-mock"
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
        // Check if any message contains the steering marker
        for msg in messages {
            if let Some(ref content) = msg.content {
                if content.contains("OUT-OF-BAND USER MESSAGE")
                    && content.contains("Switch to alternate plan")
                {
                    self.steer_observed.store(true, Ordering::SeqCst);
                    return Ok(Message::assistant("Acknowledged steering instruction"));
                }
            }
        }

        // First round: request tool call
        Ok(Message::assistant_with_tools(vec![ToolCall {
            id: "call_1".to_string(),
            name: "failing_tool".to_string(),
            arguments: serde_json::json!({}),
            signature: None,
        }]))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn test_mid_turn_steering_delivers_user_message() {
    let steer_observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider = Arc::new(SteeringMockProvider {
        steer_observed: steer_observed.clone(),
    });
    let memory = Arc::new(MockMemory);
    let notes = Arc::new(MockNotes);
    let registry = Arc::new(DefaultToolRegistry::new());
    registry.register(Arc::new(FailingTool)).await.unwrap();

    let agent = Agent::new(
        provider,
        memory,
        notes,
        registry,
        AgentConfig {
            max_consecutive_failures: 5,
            ..AgentConfig::default()
        },
    );

    // Queue steering guidance
    assert!(agent.steer("Switch to alternate plan"));

    let cancel = CancellationToken::new();
    let outcome = agent.run_turn("Initial query", cancel).await.unwrap();

    assert!(
        steer_observed.load(Ordering::SeqCst),
        "Model must receive mid-turn steering message"
    );
    assert_eq!(outcome.reply, "Acknowledged steering instruction");
}

struct HugeTool;

#[async_trait]
impl Tool for HugeTool {
    fn name(&self) -> &str {
        "huge_tool"
    }
    fn description(&self) -> &str {
        "returns an enormous output"
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
        _args: &serde_json::Value,
        _cancel: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        Ok(ToolResult::success(call_id, "x".repeat(500_000)))
    }
}

struct ToolResultCappingProvider {
    attempts: AtomicUsize,
    observed_truncated: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl ModelProvider for ToolResultCappingProvider {
    fn provider_id(&self) -> &str {
        "capping-mock"
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
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt > 0 {
            for msg in messages {
                if let Some(ref content) = msg.content {
                    if content.contains("[tool output clipped: middle omitted")
                        && content.len() <= 100_000
                    {
                        self.observed_truncated.store(true, Ordering::SeqCst);
                    }
                }
            }
            return Ok(Message::assistant("Done"));
        }
        Ok(Message::assistant_with_tools(vec![ToolCall {
            id: "call_huge".to_string(),
            name: "huge_tool".to_string(),
            arguments: serde_json::json!({}),
            signature: None,
        }]))
    }
    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn test_huge_tool_result_is_capped_in_dispatch() {
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider = Arc::new(ToolResultCappingProvider {
        attempts: AtomicUsize::new(0),
        observed_truncated: observed.clone(),
    });
    let memory = Arc::new(MockMemory);
    let notes = Arc::new(MockNotes);
    let registry = Arc::new(DefaultToolRegistry::new());
    registry.register(Arc::new(HugeTool)).await.unwrap();

    let agent = Agent::new(provider, memory, notes, registry, AgentConfig::default());

    let cancel = CancellationToken::new();
    let outcome = agent.run_turn("Run huge tool", cancel).await.unwrap();
    assert_eq!(outcome.reply, "Done");
    assert!(
        observed.load(Ordering::SeqCst),
        "Tool output >100,000 chars must be capped before feeding back to the model"
    );
}

/// A tool round followed by a transient model failure. `executed_tools` is the whole turn's
/// ledger, so a retry guard keyed on its emptiness refuses to retry the *second* model call —
/// the exact failure retries exist for. The guard has to look at this call only.
struct ToolThenOverloadProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for ToolThenOverloadProvider {
    fn provider_id(&self) -> &str {
        "tool-then-overload"
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
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        match n {
            0 => Ok(Message::assistant_with_tools(vec![ToolCall {
                id: "call_noop".to_string(),
                name: "noop_tool".to_string(),
                arguments: serde_json::json!({}),
                signature: None,
            }])),
            1 | 2 => Err(HarnessError::ProviderError {
                provider: "tool-then-overload".to_string(),
                message: "Model is overloaded (HTTP 503)".to_string(),
            }),
            _ => Ok(Message::assistant("Recovered after the tool round")),
        }
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

struct NoopTool;

#[async_trait]
impl Tool for NoopTool {
    fn name(&self) -> &str {
        "noop_tool"
    }

    fn description(&self) -> &str {
        "Succeeds without doing anything"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    async fn execute(
        &self,
        call_id: &str,
        _arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        Ok(ToolResult::success(call_id, "ok"))
    }
}

#[tokio::test]
async fn test_agent_retries_transient_error_after_a_tool_round() {
    let provider = Arc::new(ToolThenOverloadProvider {
        calls: AtomicUsize::new(0),
    });
    let registry = Arc::new(DefaultToolRegistry::new());
    registry.register(Arc::new(NoopTool)).await.unwrap();

    let agent = Agent::new(
        provider.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        registry,
        AgentConfig::default(),
    );

    let result = agent
        .run_task("Use the tool", CancellationToken::new())
        .await;
    assert!(
        result.is_ok(),
        "a transient error after a tool round must be retried, got: {result:?}"
    );
    assert_eq!(result.unwrap(), "Recovered after the tool round");
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        4,
        "one tool round, two retried failures, then the answer"
    );
}
