//! Where the context window went, part by part.
//!
//! [`estimate_tokens`](crate::session::estimate_tokens) answers "how full is the window", which is
//! the number a fold compares against. It cannot answer "with what": a transcript that is 80% tool
//! output and one that is 80% the system prompt fill the window the same way but call for opposite
//! fixes. This module splits the same message set by what each part is *for*.
//!
//! Every figure is the same character heuristic used for the total, so the parts add up to what the
//! gauge shows instead of disagreeing with it: `used()` is the sum of the parts by construction.
//! A part is only ever counted once — a tool call's arguments are not also its content.

use crate::types::{Message, Role};

use super::compaction::{text_content_chars, CHARS_PER_TOKEN};

/// The window, split by what fills it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextParts {
    /// The system prompt: the harness contract, profile text and configured prefix.
    pub system: u32,
    /// What the person typed.
    pub user: u32,
    /// What the model answered, excluding its reasoning.
    pub assistant: u32,
    /// Thinking / reasoning blocks, weighted denser than prose.
    pub reasoning: u32,
    /// Tool names, arguments and signatures the model asked for.
    pub tool_calls: u32,
    /// Tool output the harness fed back.
    pub tool_results: u32,
}

impl ContextParts {
    /// Everything the next request would send, which is the number the window is measured against.
    pub fn used(&self) -> u32 {
        [
            self.system,
            self.user,
            self.assistant,
            self.reasoning,
            self.tool_calls,
            self.tool_results,
        ]
        .into_iter()
        .fold(0u32, u32::saturating_add)
    }

    /// What is left of a window of `window` tokens. Zero when the estimate overshoots the window,
    /// which a provider-reported figure inside the messages can make happen.
    pub fn free(&self, window: u32) -> u32 {
        window.saturating_sub(self.used())
    }

    /// `part` as a percentage of `window`, clamped so a bar can be drawn straight from it.
    pub fn percent(&self, part: u32, window: u32) -> f64 {
        if window == 0 {
            return 0.0;
        }
        (part as f64 * 100.0 / window as f64).clamp(0.0, 100.0)
    }
}

/// Measure a transcript against the static part of the system prompt.
///
/// `system_prompt_chars` is the prompt *text* the harness composes without retrieval — the profile
/// text and configured prefix included, retrieved memories not. A report must not run a memory
/// search to draw itself, so the part it cannot know is left to the caller to supply or omit.
pub fn measure(messages: &[Message], system_prompt_chars: usize) -> ContextParts {
    let mut parts = ContextParts {
        system: tokens(system_prompt_chars),
        ..ContextParts::default()
    };

    for message in messages {
        let content = tokens(message.content.as_deref().map_or(0, text_content_chars));
        match message.role {
            Role::System => parts.system = parts.system.saturating_add(content),
            Role::User => parts.user = parts.user.saturating_add(content),
            Role::Assistant => parts.assistant = parts.assistant.saturating_add(content),
            Role::Tool => parts.tool_results = parts.tool_results.saturating_add(content),
        }

        if let Some(reasoning) = &message.reasoning {
            // Reasoning is denser than prose (~2 chars/token); the same weighting the total uses.
            parts.reasoning = parts.reasoning.saturating_add(tokens(reasoning.len() * 2));
        }

        for call in &message.tool_calls {
            let mut chars = call
                .name
                .len()
                .saturating_add(call.arguments.to_string().len());
            if let Some(signature) = &call.signature {
                chars = chars.saturating_add(signature.len() * 2);
            }
            parts.tool_calls = parts.tool_calls.saturating_add(tokens(chars));
        }

        if let Some(result) = &message.tool_result {
            parts.tool_results = parts
                .tool_results
                .saturating_add(tokens(text_content_chars(&result.content)));
        }
    }

    parts
}

/// Characters to tokens, rounding up, and never reporting zero for text that exists.
fn tokens(chars: usize) -> u32 {
    if chars == 0 {
        return 0;
    }
    chars.div_ceil(CHARS_PER_TOKEN).max(1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolCall;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: "c1".to_string(),
            name: name.to_string(),
            arguments: serde_json::json!({ "path": "src/main.rs" }),
            signature: None,
        }
    }

    /// The parts are the window: summing them cannot disagree with the gauge the fold reads.
    #[test]
    fn the_parts_add_up_to_the_whole() {
        let messages = vec![
            Message::system("system words here"),
            Message::user("what is in this file"),
            Message::assistant("it is rust"),
        ];
        let parts = measure(&messages, 400);
        let sum = parts.user + parts.assistant + parts.system + parts.reasoning;
        assert_eq!(parts.used(), sum);
        assert!(parts.system >= 100, "the prompt counts: {parts:?}");
    }

    /// A tool call is charged to the call, its output to the results — never to each other.
    #[test]
    fn calls_and_results_are_counted_separately() {
        let mut assistant = Message::assistant("");
        assistant.tool_calls = vec![call("read_file")];
        let mut tool = Message {
            role: Role::Tool,
            ..Message::assistant("")
        };
        tool.tool_result = Some(crate::types::ToolResult::success("c1", "x".repeat(4_000)));

        let parts = measure(&[assistant, tool], 0);
        assert!(parts.tool_calls > 0, "the call is charged: {parts:?}");
        assert!(
            parts.tool_results >= 1_000,
            "the output is charged: {parts:?}"
        );
        assert_eq!(parts.assistant, 0, "neither is the reply: {parts:?}");
    }

    /// Reasoning is denser than prose, so it is not measured at four characters per token.
    #[test]
    fn thinking_is_weighted_denser_than_prose() {
        let mut with_reasoning = Message::assistant("");
        with_reasoning.reasoning = Some("y".repeat(400));
        let parts = measure(&[with_reasoning], 0);
        assert!(
            parts.reasoning >= 200,
            "400 reasoning chars are ~200 tokens: {parts:?}"
        );
    }

    /// An empty transcript in an empty window is not a division by zero, and free space cannot
    /// go negative when a provider's own figure overshoots the window.
    #[test]
    fn shares_are_bounded() {
        let parts = ContextParts {
            system: 10,
            ..ContextParts::default()
        };
        assert_eq!(parts.percent(10, 0), 0.0);
        assert_eq!(parts.free(5), 0);
        assert_eq!(parts.percent(10, 100), 10.0);
    }
}
