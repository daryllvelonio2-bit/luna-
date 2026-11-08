//! The two translations this route needs: what we send, and what comes back.
//!
//! Outbound, a transcript becomes `contents` with the tool list as function declarations and the
//! reasoning effort as a thinking budget. Inbound, each SSE frame becomes the stream events the
//! turn loop understands, including the cache counts the ledger records.

use super::quota::resolve_antigravity_model;
use super::*;

/// Translate one Code Assist SSE payload into stream events. Both the streaming path and the
/// buffered `execute_antigravity_completion` go through here, so a thought chunk is classified
/// identically either way.
pub(super) fn parse_antigravity_chunk(event: &serde_json::Value) -> Vec<StreamEvent> {
    let resp_data = event.get("response").unwrap_or(event);
    let mut events = Vec::new();

    if let Some(meta) = resp_data.get("usageMetadata") {
        let count = |key: &str| meta.get(key).and_then(|v| v.as_u64());
        // Thinking is output: Gemini reports it apart from `candidatesTokenCount` but counts it in
        // `totalTokenCount`, and a reply cut off by the output cap can carry only the total and the
        // thought count. Reading just `promptTokenCount`/`candidatesTokenCount` turns those frames
        // into a report of zero tokens, which is the opposite of what a diagnosis needs.
        let thoughts = count("thoughtsTokenCount").unwrap_or(0);
        let completion_tokens = count("candidatesTokenCount").unwrap_or(0) + thoughts;
        let total_tokens = count("totalTokenCount");
        let prompt_tokens = count("promptTokenCount")
            .or_else(|| total_tokens.map(|total| total.saturating_sub(completion_tokens)))
            .unwrap_or(0) as u32;
        // Gemini counts cache hits *inside* `promptTokenCount` and reports them separately here,
        // which is exactly the relationship `TokenUsage::cached_tokens` describes.
        let cached_tokens = count("cachedContentTokenCount").unwrap_or(0) as u32;
        events.push(StreamEvent::Completed {
            finish_reason: None,
            usage: Some(TokenUsage {
                prompt_tokens,
                completion_tokens: completion_tokens as u32,
                total_tokens: total_tokens
                    .map(|total| total as u32)
                    .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens as u32)),
                cached_tokens,
            }),
        });
    }

    let Some(candidates) = resp_data.get("candidates").and_then(|c| c.as_array()) else {
        return events;
    };

    for candidate in candidates {
        if let Some(reason) = candidate.get("finishReason").and_then(|r| r.as_str()) {
            events.push(StreamEvent::Completed {
                finish_reason: Some(reason.to_lowercase()),
                usage: None,
            });
        }
        let Some(parts) = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
        else {
            continue;
        };
        for part in parts {
            // Code Assist marks a reasoning part with a boolean `thought`, but some responses
            // carry the reasoning text under a string `thought` instead.
            let is_thought = part
                .get("thought")
                .and_then(|t| t.as_bool())
                .unwrap_or(false);
            if is_thought {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    events.push(StreamEvent::ReasoningDelta(text.to_string()));
                }
            } else if let Some(thought) = part.get("thought").and_then(|t| t.as_str()) {
                events.push(StreamEvent::ReasoningDelta(thought.to_string()));
            } else if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                events.push(StreamEvent::ContentDelta(text.to_string()));
            }

            if let Some(call) = part.get("functionCall") {
                events.push(StreamEvent::ToolCallDelta {
                    index: 0,
                    id: call
                        .get("id")
                        .and_then(|i| i.as_str())
                        .map(|s| s.to_string()),
                    name: call
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string()),
                    arguments_delta: Some(
                        call.get("args")
                            .cloned()
                            .unwrap_or_else(|| json!({}))
                            .to_string(),
                    ),
                    // The model's own signature for this call: replayed verbatim on the next
                    // request, which is what Code Assist's validator actually checks.
                    signature: part
                        .get("thoughtSignature")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                });
            }
        }
    }

    events
}

/// One piece of a user turn's content: prose, or an image that was embedded as a data URL.
enum UserChunk {
    Text(String),
    /// The image's MIME type and base64 payload, in the shape `inlineData` wants.
    Image(String, String),
}

/// Split user content into its prose and its embedded images.
///
/// The CLI appends pasted screenshots to the prompt as whole-line `data:image/...;base64,...`
/// URLs (the OpenAI-compatible route lifts those into `image_url` parts the same way). Anything
/// that is not a whole-line data URL is prose — a sentence *about* a data URL is text, not pixels.
fn user_text_and_images(text: &str) -> Vec<UserChunk> {
    let mut chunks = Vec::new();
    let mut prose = String::new();

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some((mime, data)) = split_data_url(trimmed) {
            let flushed = prose.trim_end().to_string();
            if !flushed.is_empty() {
                chunks.push(UserChunk::Text(flushed));
                prose.clear();
            }
            chunks.push(UserChunk::Image(mime.to_string(), data.to_string()));
        } else {
            prose.push_str(line);
        }
    }

    let tail = prose.trim_end().to_string();
    if !tail.is_empty() {
        chunks.push(UserChunk::Text(tail));
    }
    chunks
}

/// The `(mime, base64)` of a whole-string data URL, or `None` when `s` is not one.
fn split_data_url(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix("data:")?;
    let (mime, after) = rest.split_once(';')?;
    let data = after.strip_prefix("base64,")?;
    // An empty payload is a malformed embed rather than a zero-byte image.
    (!mime.is_empty() && !data.is_empty()).then_some((mime, data))
}

/// The thinking allowance an effort setting buys, in output tokens.
fn thinking_budget(effort: ReasoningEffort) -> u32 {
    match effort {
        ReasoningEffort::Minimal => 1024,
        ReasoningEffort::Low => 2048,
        ReasoningEffort::Medium => 8192,
        ReasoningEffort::High => 16384,
        ReasoningEffort::Xhigh | ReasoningEffort::Max => 32768,
    }
}

/// Whether this backend model charges its thinking to `maxOutputTokens`.
///
/// Gemini does, and so does any `-thinking` model on Code Assist. The Claude and GPT-OSS entries
/// in the same catalogue do not: for them `maxOutputTokens` is the answer's own allowance.
fn counts_thinking_against_output(model: &str) -> bool {
    model.contains("gemini") || model.contains("thinking")
}

/// Make one tool's JSON Schema acceptable to Code Assist.
///
/// Google's `Schema.enum` is a list of *strings*. JSON Schema also permits an `enum` on a numeric
/// property — `desktop_click_at` constrains `button` to `[0, 1, 2]` — and the numeric form arrives
/// as a `TYPE_STRING` mismatch (`Invalid value at
/// '...function_declarations[4].parameters.properties[0].value.enum[0]' (TYPE_STRING), 0`), which
/// rejects the whole request before the model is called. Drop a non-string `enum` wherever it
/// appears and keep the property: its type and description still say what it takes, and losing a
/// constraint is a weaker prompt rather than a failed turn. A string `enum` — the shape the API
/// accepts — is left untouched, including inside nested `items` and `properties`.
fn gemini_safe_parameters(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let has_non_string_enum = map
                .get("enum")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|values| values.iter().any(|value| !value.is_string()));
            if has_non_string_enum {
                map.remove("enum");
            }
            for child in map.values_mut() {
                gemini_safe_parameters(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                gemini_safe_parameters(item);
            }
        }
        _ => {}
    }
}

/// Build the Code Assist payload for one turn: messages translated to `contents`, tools to
/// function declarations, and the reasoning effort to a thinking budget.
pub(super) fn build_antigravity_payload(
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
    model: &str,
) -> serde_json::Value {
    let mut gemini_contents = Vec::new();
    // Every system message is kept, joined in order. The vector carries two by design: the
    // session's system prompt first, then the synthetic compaction summary. Assigning instead
    // of accumulating makes the summary — the last one — *replace* the system prompt, which
    // drops the persona, the rules and the tool guidance from the request and leaves the folded
    // history masquerading as instructions.
    let mut system_texts: Vec<String> = Vec::new();

    for (msg_idx, msg) in messages.iter().enumerate() {
        match msg.role {
            Role::System => {
                if let Some(ref text) = msg.content {
                    if !text.trim().is_empty() {
                        system_texts.push(text.clone());
                    }
                }
            }
            Role::User => {
                let text = msg.content.as_deref().unwrap_or("");
                let mut parts = Vec::new();
                // A pasted or inlined image rides in the text as a data URL; on this route it
                // must become an `inlineData` part, or the model gets megabytes of base64 as
                // prose and no pixels at all.
                for chunk in user_text_and_images(text) {
                    match chunk {
                        UserChunk::Text(text) => {
                            if !text.trim().is_empty() {
                                parts.push(json!({ "text": text }));
                            }
                        }
                        UserChunk::Image(mime, data) => {
                            parts.push(json!({
                                "inlineData": { "mimeType": mime, "data": data }
                            }));
                        }
                    }
                }
                if parts.is_empty() {
                    parts.push(json!({ "text": "" }));
                }
                gemini_contents.push(json!({
                    "role": "user",
                    "parts": parts
                }));
            }
            Role::Assistant => {
                let mut parts = Vec::new();
                // Replay the turn's own reasoning first, as the thought part it was on the
                // wire: Code Assist validates the current turn's history against what the
                // model actually produced, and dropping the thinking is what makes a replayed
                // functionCall look like it did not follow a thought.
                if let Some(ref reasoning) = msg.reasoning {
                    if !reasoning.is_empty() {
                        parts.push(json!({ "text": reasoning, "thought": true }));
                    }
                }
                if let Some(ref text) = msg.content {
                    if !text.is_empty() {
                        parts.push(json!({ "text": text }));
                    }
                }
                for (idx, c) in msg.tool_calls.iter().enumerate() {
                    let call_id = if c.id.is_empty() {
                        format!("call_{}", idx)
                    } else {
                        c.id.clone()
                    };
                    let mut call_part = json!({
                        "functionCall": {
                            "id": call_id,
                            "name": c.name,
                            "args": c.arguments
                        }
                    });
                    // The signature the model issued with this call is replayed verbatim: Code
                    // Assist validates each functionCall's signature against the model that produced
                    // it. A call we hold none for — another provider wrote the row, or the session
                    // predates the field — carries Google's documented dummy instead, which makes the
                    // validator skip the check. Omitting the field is what 400s the whole request.
                    if let Some(signature) = thought_signature::signature_for_replay(
                        c.signature.as_deref(),
                        thought_signature::model_enforces_thought_signature(model),
                    ) {
                        call_part["thoughtSignature"] = json!(signature);
                    }
                    parts.push(call_part);
                }
                if parts.is_empty() {
                    parts.push(json!({ "text": "" }));
                }
                gemini_contents.push(json!({
                    "role": "model",
                    "parts": parts
                }));
            }
            Role::Tool => {
                let call_id = msg
                    .tool_result
                    .as_ref()
                    .map(|r| r.call_id.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("call_0");
                let result_content = msg
                    .tool_result
                    .as_ref()
                    .map(|r| r.content.as_str())
                    .unwrap_or("");
                // Search backwards from the current message to find the matching tool call:
                // a tool result answers the most recent preceding assistant message that called
                // it. Scanning forward from message 0 is prone to ID collisions across turns (or
                // sequential call_0 defaults), which mismatches the tool name and causes Gemini to
                // reject the request with HTTP 400 ("Request contains an invalid argument").
                let tool_name = messages[..msg_idx]
                    .iter()
                    .rev()
                    .find_map(|m| {
                        m.tool_calls
                            .iter()
                            .rev()
                            .find(|tc| {
                                tc.id == call_id || (tc.id.is_empty() && call_id == "call_0")
                            })
                            .map(|tc| tc.name.as_str())
                    })
                    .or_else(|| {
                        messages.iter().rev().find_map(|m| {
                            m.tool_calls
                                .iter()
                                .rev()
                                .find(|tc| {
                                    tc.id == call_id || (tc.id.is_empty() && call_id == "call_0")
                                })
                                .map(|tc| tc.name.as_str())
                        })
                    });

                let resp_part = if let Some(tool_name) = tool_name {
                    json!({
                        "functionResponse": {
                            "id": call_id,
                            "name": tool_name,
                            "response": { "result": result_content }
                        }
                    })
                } else {
                    // Do not emit an orphaned functionResponse without a matching functionCall
                    // (which Gemini rejects). Emit as user text instead.
                    json!({ "text": format!("[Tool result]: {}", result_content) })
                };

                let is_func_resp = resp_part.get("functionResponse").is_some();

                // Merge into previous user turn if both contain functionResponse
                if is_func_resp {
                    if let Some(last_entry) = gemini_contents.last_mut() {
                        if last_entry.get("role").and_then(|r| r.as_str()) == Some("user") {
                            if let Some(parts_arr) =
                                last_entry.get_mut("parts").and_then(|p| p.as_array_mut())
                            {
                                let has_func_resp = parts_arr
                                    .iter()
                                    .any(|p| p.get("functionResponse").is_some());
                                if has_func_resp {
                                    parts_arr.push(resp_part);
                                    continue;
                                }
                            }
                        }
                    }
                }

                // Gemini requires functionResponse turns to immediately follow a model turn.
                let prev_is_model = gemini_contents
                    .last()
                    .and_then(|e| e.get("role"))
                    .and_then(|r| r.as_str())
                    == Some("model");

                if !prev_is_model && !gemini_contents.is_empty() {
                    gemini_contents.push(json!({
                        "role": "model",
                        "parts": [{ "text": "[The previous response was interrupted before it completed.]" }]
                    }));
                }

                gemini_contents.push(json!({
                    "role": "user",
                    "parts": [resp_part]
                }));
            }
        }
    }

    // Merge adjacent same-role turns to enforce strict Gemini user/model alternation
    let mut merged_contents: Vec<serde_json::Value> = Vec::new();
    for content in gemini_contents {
        let same_role =
            merged_contents.last().and_then(|prev| prev.get("role")) == content.get("role");

        if same_role {
            if let (Some(prev_parts), Some(curr_parts)) = (
                merged_contents
                    .last_mut()
                    .and_then(|p| p.get_mut("parts"))
                    .and_then(|p| p.as_array_mut()),
                content.get("parts").and_then(|p| p.as_array()),
            ) {
                prev_parts.extend(curr_parts.iter().cloned());
            }
        } else {
            merged_contents.push(content);
        }
    }

    // Gemini strictly requires the first content turn to have role: "user".
    // If compaction, session replay, or callers provide messages starting with a model turn
    // (especially one containing functionCall), prepend a synthetic user turn so Gemini never
    // rejects it with HTTP 400 ("Please ensure that function call turn comes immediately after a user turn").
    if merged_contents
        .first()
        .and_then(|c| c.get("role"))
        .and_then(|r| r.as_str())
        == Some("model")
    {
        merged_contents.insert(
            0,
            json!({
                "role": "user",
                "parts": [{ "text": "[Conversation continued from previous turn]" }]
            }),
        );
    }

    if merged_contents.is_empty() {
        merged_contents.push(json!({
            "role": "user",
            "parts": [{ "text": "" }]
        }));
    }

    for turn in &mut merged_contents {
        if let Some(parts) = turn.get_mut("parts").and_then(|p| p.as_array_mut()) {
            if parts.is_empty() {
                parts.push(json!({ "text": "" }));
            }
        }
    }

    let mut req_obj = json!({ "contents": merged_contents });

    if !system_texts.is_empty() {
        req_obj["systemInstruction"] = json!({
            "parts": [{ "text": system_texts.join("\n\n") }]
        });
    }

    if !tools.is_empty() {
        let func_decls: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                let mut parameters = t.parameters.clone();
                gemini_safe_parameters(&mut parameters);
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": parameters
                })
            })
            .collect();
        req_obj["tools"] = json!([{ "functionDeclarations": func_decls }]);
    }

    let mut gen_config = json!({});
    if let Some(temp) = options.temperature {
        gen_config["temperature"] = json!(temp);
    }
    if let Some(top_p) = options.top_p {
        gen_config["topP"] = json!(top_p);
    }

    let resolved_model = resolve_antigravity_model(model, options.reasoning_effort.as_ref());
    let budget = options.reasoning_effort.map(thinking_budget);
    if let Some(budget) = budget {
        gen_config["thinkingConfig"] = json!({ "thinkingBudget": budget });
    }
    // Thinking is charged to `maxOutputTokens`, not beside it. A cap meant for the visible answer
    // therefore has to carry the thinking allowance too, or a hard turn spends the whole budget in
    // its head and comes back as `finishReason=MAX_TOKENS` with no parts at all — an empty answer
    // that a retry of the identical request reproduces exactly. Reproduced against Code Assist:
    // `maxOutputTokens=4096` on a long tool-heavy turn returned one frame with
    // `thoughtsTokenCount=3932`, `MAX_TOKENS`, and no text.
    //
    // With no declared effort Gemini thinks dynamically and publishes no budget to reserve, so the
    // cap is left off entirely: `max_tokens` bounds the answer, and that bound cannot be expressed
    // as a bound on thinking-plus-answer without knowing the former.
    if let Some(max_tok) = options.max_tokens {
        let cap = match (counts_thinking_against_output(&resolved_model), budget) {
            (true, Some(budget)) => Some(max_tok.saturating_add(budget)),
            (true, None) => None,
            (false, _) => Some(max_tok),
        };
        if let Some(cap) = cap {
            gen_config["maxOutputTokens"] = json!(cap);
        }
    }
    if gen_config
        .as_object()
        .map(|o| !o.is_empty())
        .unwrap_or(false)
    {
        req_obj["generationConfig"] = gen_config;
    }

    json!({
        "project": DEFAULT_PROJECT,
        "model": resolved_model,
        "request": req_obj
    })
}
