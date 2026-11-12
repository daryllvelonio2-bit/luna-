//! The two translations this route needs: what we send, and what comes back.
//!
//! The Codex backend speaks the OpenAI *Responses* protocol, not chat completions. Outbound, a
//! transcript becomes `instructions` (the system text), a flat list of input items — messages,
//! function calls, function-call outputs — and the tool list in its flat Responses shape. Inbound,
//! each SSE frame becomes the stream events the turn loop already understands.
//!
//! Both shapes were read off the live endpoint rather than assumed: a forced `tool_choice` answers
//! with a `function_call` item carrying `call_id`/`name`/`arguments`, text arrives as
//! `response.output_text.delta`, and a replayed `function_call` needs no `id` beside its
//! `call_id` — which is why a call this harness holds no server-side id for can still be replayed.

use harness_core::{Message, Role, StreamEvent, TokenUsage, ToolDefinition};
use serde_json::json;

use super::CompletionOptions;

/// The stream events one frame carries.
///
/// A frame can produce none (the `response.created` / `in_progress` / `metadata` bookkeeping
/// frames), one, or — as the output-item frame does for a call — exactly one call event.
pub(super) fn parse_codex_frame(event: &serde_json::Value) -> Vec<StreamEvent> {
    match event
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
    {
        "response.output_text.delta" => text_delta(event)
            .map(StreamEvent::ContentDelta)
            .into_iter()
            .collect(),
        // Two spellings for the same thing: the summary channel is what the CLI asks for, the
        // reasoning channel is what a route that returns raw thinking sends.
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            text_delta(event)
                .map(StreamEvent::ReasoningDelta)
                .into_iter()
                .collect()
        }
        "response.output_item.done" => function_call_event(event).into_iter().collect(),
        "response.completed" => vec![StreamEvent::Completed {
            finish_reason: None,
            usage: usage_from(event.get("response")),
        }],
        "response.incomplete" => vec![StreamEvent::Completed {
            finish_reason: event
                .get("response")
                .and_then(|response| response.get("incomplete_details"))
                .and_then(|details| details.get("reason"))
                .and_then(|reason| reason.as_str())
                .map(incomplete_reason),
            usage: usage_from(event.get("response")),
        }],
        _ => Vec::new(),
    }
}

/// The `delta` of a text frame, dropped when it is empty.
///
/// An empty delta is a keep-alive-shaped frame, and forwarding it would append nothing to the
/// answer while waking the UI for a redraw.
fn text_delta(event: &serde_json::Value) -> Option<String> {
    event
        .get("delta")
        .and_then(|delta| delta.as_str())
        .filter(|delta| !delta.is_empty())
        .map(str::to_string)
}

/// A completed `function_call` output item as a call event.
///
/// The arguments arrive whole on this frame, so the buffered path parses them in one step exactly
/// as it does for the other custom routes.
fn function_call_event(event: &serde_json::Value) -> Option<StreamEvent> {
    let item = event.get("item")?;
    if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
        return None;
    }
    let name = item
        .get("name")
        .and_then(|name| name.as_str())
        .filter(|name| !name.is_empty())?;

    Some(StreamEvent::ToolCallDelta {
        index: event
            .get("output_index")
            .and_then(|index| index.as_u64())
            .unwrap_or(0) as usize,
        // `call_id` is what a later `function_call_output` must quote; the item's own `id` is a
        // server-side handle we have no use for.
        id: item
            .get("call_id")
            .and_then(|id| id.as_str())
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        name: Some(name.to_string()),
        arguments_delta: Some(
            item.get("arguments")
                .and_then(|arguments| arguments.as_str())
                .unwrap_or("{}")
                .to_string(),
        ),
        signature: None,
    })
}

/// Why the model stopped before finishing, in the vocabulary the turn loop checks.
///
/// `max_output_tokens` is reported as `max_tokens`, the name every other route uses and the one
/// the caller tests for an answer that spent its whole budget before writing anything.
fn incomplete_reason(reason: &str) -> String {
    match reason {
        "max_output_tokens" => "max_tokens".to_string(),
        other => other.to_string(),
    }
}

fn usage_from(response: Option<&serde_json::Value>) -> Option<TokenUsage> {
    let usage = response?.get("usage")?;
    let count = |key: &str| usage.get(key).and_then(|value| value.as_u64());
    let prompt_tokens = count("input_tokens").unwrap_or(0) as u32;
    let completion_tokens = count("output_tokens").unwrap_or(0) as u32;
    Some(TokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens: count("total_tokens")
            .map(|total| total as u32)
            .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens)),
        // The cached count sits *inside* `input_tokens` on this route, which is exactly the
        // relationship `TokenUsage::cached_tokens` documents.
        cached_tokens: usage
            .get("input_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(|cached| cached.as_u64())
            .unwrap_or(0) as u32,
    })
}

/// The request body for one turn.
///
/// `temperature`, `top_p` and `stop` are deliberately absent: the reasoning models this route
/// serves reject the sampling parameters, and a rejected request is a failed turn rather than a
/// slightly different one.
pub(super) fn build_codex_payload(
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
    model: &str,
) -> serde_json::Value {
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<serde_json::Value> = Vec::new();

    for msg in messages {
        match msg.role {
            Role::System => {
                // Every system message is kept, joined in order: the vector carries the session's
                // system prompt and then the synthetic compaction summary, and letting the last
                // one win would replace the rules with the folded transcript.
                if let Some(text) = msg
                    .content
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                {
                    instructions.push(text.to_string());
                }
            }
            Role::User => {
                input.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": user_content(msg.content.as_deref().unwrap_or("")),
                }));
            }
            Role::Assistant => {
                if let Some(text) = msg.content.as_deref().filter(|text| !text.is_empty()) {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
                for (index, call) in msg.tool_calls.iter().enumerate() {
                    let call_id = if call.id.is_empty() {
                        format!("call_{index}")
                    } else {
                        call.id.clone()
                    };
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call_id,
                        "name": call.name,
                        "arguments": call.arguments.to_string(),
                    }));
                }
            }
            Role::Tool => {
                let call_id = msg
                    .tool_result
                    .as_ref()
                    .map(|result| result.call_id.as_str())
                    .filter(|id| !id.is_empty())
                    .unwrap_or("call_0");
                let output = msg
                    .tool_result
                    .as_ref()
                    .map(|result| result.content.as_str())
                    .unwrap_or("");
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
        }
    }

    // An empty `input` is rejected; an empty user turn is the honest stand-in for a request whose
    // transcript held nothing but instructions.
    if input.is_empty() {
        input.push(json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": "" }],
        }));
    }

    let mut body = json!({
        "model": model,
        "input": input,
        "store": false,
        "stream": true,
    });

    if !instructions.is_empty() {
        body["instructions"] = json!(instructions.join("\n\n"));
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools
            .iter()
            .map(|tool| json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            }))
            .collect::<Vec<_>>());
        body["tool_choice"] = json!("auto");
        body["parallel_tool_calls"] = json!(true);
    }
    if let Some(effort) = options.reasoning_effort.as_ref() {
        body["reasoning"] = json!({ "effort": effort.as_str() });
    }
    if let Some(max_tokens) = options.max_tokens {
        body["max_output_tokens"] = json!(max_tokens);
    }

    body
}

/// One user turn's content parts: prose runs, and any whole-line data URL lifted into an image
/// part so a pasted screenshot reaches the model as pixels instead of megabytes of base64 text.
fn user_content(text: &str) -> Vec<serde_json::Value> {
    let mut parts = Vec::new();
    let mut prose = String::new();

    for line in text.split_inclusive('\n') {
        match split_data_url(line.trim()) {
            Some((mime, data)) if mime.starts_with("image/") => {
                let flushed = prose.trim_end();
                if !flushed.is_empty() {
                    parts.push(json!({ "type": "input_text", "text": flushed }));
                }
                prose.clear();
                parts.push(json!({
                    "type": "input_image",
                    "image_url": format!("data:{mime};base64,{data}"),
                }));
            }
            _ => prose.push_str(line),
        }
    }

    let tail = prose.trim_end();
    if !tail.is_empty() {
        parts.push(json!({ "type": "input_text", "text": tail }));
    }
    if parts.is_empty() {
        parts.push(json!({ "type": "input_text", "text": "" }));
    }
    parts
}

/// The `(mime, base64)` of a whole-string data URL, or `None` when the line is not one.
fn split_data_url(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("data:")?;
    let (mime, after) = rest.split_once(';')?;
    let data = after.strip_prefix("base64,")?;
    (!mime.is_empty() && !data.is_empty()).then_some((mime, data))
}
