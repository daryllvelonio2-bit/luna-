//! Runtime feature toggles: a disabled family is withheld from the request *and* rejected if the
//! model asks for it anyway. The second half matters because a resumed conversation or a provider
//! replay can carry a call the current request never advertised.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use harness_core::*;

/// Asks for `save_memory` once, then answers, recording the tool names each request carried.
struct AskingProvider {
    answered: AtomicUsize,
    seen_tools: Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl ModelProvider for AskingProvider {
    fn provider_id(&self) -> &str {
        "mock"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["mock-1".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        self.seen_tools
            .lock()
            .unwrap()
            .push(tools.iter().map(|tool| tool.name.clone()).collect());
        if self.answered.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(Message::assistant_with_tools(vec![ToolCall {
                id: "call_0".to_string(),
                name: "save_memory".to_string(),
                arguments: json!({"subject": "user", "predicate": "profile", "object": "test"}),
                signature: None,
            }]));
        }
        Ok(Message::assistant("done"))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

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

/// Records whether it ever ran, so a rejected call is provably not an executed one.
struct RecordingMemoryTool {
    executed: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for RecordingMemoryTool {
    fn name(&self) -> &str {
        "save_memory"
    }
    fn description(&self) -> &str {
        "records a memory"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    async fn execute(
        &self,
        call_id: &str,
        _arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        self.executed.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::success(call_id, "saved"))
    }
}

#[tokio::test]
async fn a_disabled_memory_tool_is_withheld_and_rejected() {
    let executed = Arc::new(AtomicUsize::new(0));
    let registry = Arc::new(DefaultToolRegistry::new());
    registry
        .register(Arc::new(RecordingMemoryTool {
            executed: executed.clone(),
        }))
        .await
        .expect("register");
    let provider = Arc::new(AskingProvider {
        answered: AtomicUsize::new(0),
        seen_tools: Mutex::new(Vec::new()),
    });
    let agent = Agent::new(
        provider.clone(),
        Arc::new(NoMemory),
        Arc::new(NoNotes),
        registry,
        AgentConfig {
            memory_enabled: false,
            ..Default::default()
        },
    )
    .with_approval(Arc::new(AllowAll), ApprovalSettings::default());

    let outcome = agent
        .run_turn("remember this", CancellationToken::new())
        .await
        .expect("turn completes");

    let seen = provider.seen_tools.lock().unwrap();
    assert!(
        seen.iter()
            .all(|names| !names.iter().any(|name| name == "save_memory")),
        "the disabled tool must never be advertised: {seen:?}"
    );
    drop(seen);

    assert_eq!(
        executed.load(Ordering::SeqCst),
        0,
        "a rejected call must not reach the tool body"
    );
    assert_eq!(outcome.tool_calls.len(), 1);
    assert!(outcome.tool_calls[0].is_error);
    assert!(
        outcome.tool_calls[0]
            .output
            .contains("disabled for this session"),
        "{}",
        outcome.tool_calls[0].output
    );
}

#[tokio::test]
async fn an_enabled_memory_tool_is_advertised_and_runs() {
    let executed = Arc::new(AtomicUsize::new(0));
    let registry = Arc::new(DefaultToolRegistry::new());
    registry
        .register(Arc::new(RecordingMemoryTool {
            executed: executed.clone(),
        }))
        .await
        .expect("register");
    let provider = Arc::new(AskingProvider {
        answered: AtomicUsize::new(0),
        seen_tools: Mutex::new(Vec::new()),
    });
    let agent = Agent::new(
        provider.clone(),
        Arc::new(NoMemory),
        Arc::new(NoNotes),
        registry,
        AgentConfig::default(),
    )
    .with_approval(Arc::new(AllowAll), ApprovalSettings::default());

    let outcome = agent
        .run_turn("remember this", CancellationToken::new())
        .await
        .expect("turn completes");

    let seen = provider.seen_tools.lock().unwrap();
    assert!(
        seen.iter()
            .any(|names| names.iter().any(|name| name == "save_memory")),
        "the enabled tool must be advertised: {seen:?}"
    );
    drop(seen);

    assert_eq!(executed.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.tool_calls.len(), 1);
    assert!(!outcome.tool_calls[0].is_error);
}

fn prompt_agent(tui_output: bool) -> Agent<AskingProvider, NoMemory, NoNotes, DefaultToolRegistry> {
    Agent::new(
        Arc::new(AskingProvider {
            answered: AtomicUsize::new(1),
            seen_tools: Mutex::new(Vec::new()),
        }),
        Arc::new(NoMemory),
        Arc::new(NoNotes),
        Arc::new(DefaultToolRegistry::new()),
        AgentConfig {
            tui_output,
            ..Default::default()
        },
    )
}

/// The rich-output invitation is for the full-screen renderer only. Headless, piped and inline CLI
/// prompts must stay free of table/ANSI instructions they cannot render.
#[tokio::test]
async fn the_tui_render_block_is_only_present_for_the_tui() {
    let plain = prompt_agent(false)
        .build_system_prompt("hello")
        .await
        .expect("prompt builds");
    assert!(!plain.contains("full-screen terminal"), "{plain}");
    assert!(!plain.contains("ANSI SGR"), "{plain}");
    assert!(!plain.contains("{{RICH_OUTPUT_PROTOCOL}}"), "{plain}");

    let tui = prompt_agent(true)
        .build_system_prompt("hello")
        .await
        .expect("prompt builds");
    assert!(tui.contains("full-screen terminal"), "{tui}");
    assert!(tui.contains("Markdown tables"), "{tui}");
    assert!(tui.contains("ANSI SGR"), "{tui}");
    assert!(!tui.contains("{{RICH_OUTPUT_PROTOCOL}}"), "{tui}");
}
