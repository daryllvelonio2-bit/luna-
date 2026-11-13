use super::prompt::format_opencode_prompt;
use harness_core::types::{Message, ToolResult};

/// A failed tool result must remain explicit in the exact text sent on the next request.
#[test]
fn opencode_prompt_marks_tool_failures_as_errors() {
    let message = Message::tool(ToolResult::error(
        "call-1",
        "Missing required argument 'command'",
    ));

    let prompt = format_opencode_prompt(&[message]);

    assert!(prompt.text.contains("[Tool Error]"));
    assert!(prompt.text.contains("Missing required argument 'command'"));
    assert!(prompt.text.contains("call-1"));
}

#[test]
fn opencode_prompt_marks_successful_tool_results_as_output() {
    let message = Message::tool(ToolResult::success("call-2", "done"));

    let prompt = format_opencode_prompt(&[message]);

    assert!(prompt.text.contains("[Tool Output]"));
    assert!(prompt.text.contains("done"));
    assert!(!prompt.text.contains("[Tool Error]"));
}
