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

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        unimplemented!()
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
            system_prompt_prefix: None,
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
