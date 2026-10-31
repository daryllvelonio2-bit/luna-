//! The system prompt's MCP block: the marker is substituted in place of the template, the
//! configured tools show up by their namespaced name, and a config with no servers says so
//! instead of leaving a `{{MCP_TOOLS}}` stub for the model to puzzle over.

use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use harness_core::*;

struct MockProvider;

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
        Ok(Message::assistant("Done!"))
    }
    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
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
    async fn save_note(&self, _t: &str, _c: &str, _g: &[String]) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn search_notes(&self, _q: &str, _l: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn list_notes(&self, _l: usize, _o: usize) -> Result<Vec<Note>, HarnessError> {
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
        _g: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        unreachable!("not used by this test")
    }
    async fn delete_note(&self, _id: &str) -> Result<bool, HarnessError> {
        unreachable!("not used by this test")
    }
}

/// A tool with a fixed name, standing in for an MCP adapter registered under `mcp__server__tool`.
struct NamedTool(&'static str);

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        "an MCP tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
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

fn agent_with(registry: DefaultToolRegistry) -> Agent<MockProvider, MockMemory, MockNotes, DefaultToolRegistry> {
    Agent::new(
        Arc::new(MockProvider),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(registry),
        AgentConfig::default(),
    )
}

/// With a server registered: the marker is gone, the namespaced name is present.
#[tokio::test]
async fn registered_mcp_tool_appears_in_the_prompt() {
    let registry = DefaultToolRegistry::new();
    registry
        .register(Arc::new(NamedTool("mcp__filesystem__read_file")))
        .await
        .expect("register");

    let prompt = agent_with(registry)
        .build_system_prompt("hello")
        .await
        .expect("prompt builds");

    assert!(
        !prompt.contains("{{MCP_TOOLS}}"),
        "the marker must be substituted, not shipped to the model"
    );
    assert!(prompt.contains("mcp__filesystem__read_file"), "{prompt}");
}

/// With no server registered: the fallback line is in the prompt, still no leftover marker.
#[tokio::test]
async fn absent_mcp_tools_yield_the_fallback_and_no_stub() {
    let prompt = agent_with(DefaultToolRegistry::new())
        .build_system_prompt("hello")
        .await
        .expect("prompt builds");

    assert!(!prompt.contains("{{MCP_TOOLS}}"), "{prompt}");
    assert!(prompt.contains("no MCP tools registered"), "{prompt}");
}
