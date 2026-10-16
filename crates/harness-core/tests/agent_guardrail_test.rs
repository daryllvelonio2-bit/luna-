use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
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
    async fn retrieve(&self, _query: &str, _limit: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
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
    async fn save_note(&self, _title: &str, _content: &str, _tags: &[String]) -> Result<Note, HarnessError> {
        unimplemented!()
    }
    async fn search_notes(&self, _query: &str, _limit: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn read_note(&self, _id: &str) -> Result<Note, HarnessError> {
        unimplemented!()
    }
    async fn update_note(&self, _id: &str, _t: Option<&str>, _c: Option<&str>, _tags: Option<&[String]>) -> Result<Note, HarnessError> {
        unimplemented!()
    }
    async fn delete_note(&self, _id: &str) -> Result<bool, HarnessError> {
        unimplemented!()
    }
}

struct MockEvolver;
#[async_trait]
impl EvolverEngine for MockEvolver {
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

#[tokio::test]
async fn test_5_consecutive_failures_halts_agent() {
    let provider = Arc::new(MockProvider::new());
    let memory = Arc::new(MockMemory);
    let notes = Arc::new(MockNotes);
    let evolver = Arc::new(MockEvolver);
    let registry = Arc::new(DefaultToolRegistry::new());

    registry.register(Arc::new(FailingTool)).await.unwrap();

    let agent = Agent::new(
        provider,
        memory,
        notes,
        evolver,
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
        Err(HarnessError::ConsecutiveFailuresExceeded { failures, last_tool }) => {
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
    let evolver = Arc::new(MockEvolver);
    let registry = Arc::new(DefaultToolRegistry::new());

    let agent = Agent::new(
        provider.clone(),
        memory,
        notes,
        evolver,
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
    let evolver = Arc::new(MockEvolver);
    let registry = Arc::new(DefaultToolRegistry::new());

    let agent = Agent::new(
        provider,
        memory,
        notes,
        evolver,
        registry,
        AgentConfig::default(),
    );

    let cancel = CancellationToken::new();
    let result = agent.run_task("Hello", cancel).await;
    assert!(result.is_err(), "Expected error on empty response, got: {:?}", result);
    let err = result.unwrap_err();
    assert!(err.to_string().contains("empty response") || err.to_string().contains("overloaded"));
}
