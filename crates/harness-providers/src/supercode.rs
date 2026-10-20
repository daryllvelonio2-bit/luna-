use std::path::PathBuf;
use std::sync::Arc;
use futures_util::stream::{BoxStream, StreamExt};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use secrecy::ExposeSecret;
use serde_json::json;

use harness_core::types::{
    CompletionOptions, Message, Role, StreamEvent, TokenUsage, ToolDefinition,
};
use harness_core::HarnessError;
use crate::auth::CredentialManager;

pub const SUPERCODE_ENDPOINT: &str = "https://supercode-8w7e.onrender.com/api/ai/chat";
pub const SUPERCODE_USER_AGENT: &str = "supercode-cli/0.1.99";

pub const SUPERCODE_MODELS: &[&str] = &[
    "kimi-k3",
    "deepseek-v4-flash",
    "kimi-k2-7-code",
    "kimi-k2-6",
    "minimax-m3",
    "glm-5.2",
    "glm-5.1",
    "mimo-v2.5",
    "hy3",
    "gemini-2.5-flash",
    "meta/llama-3.3-70b-instruct",
    "orcarouter/auto",
    "stealth/ox-alpha",
];

pub fn extract_supercode_token_from_file() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let token_path = PathBuf::from(home).join(".better-auth/token.json");
    if !token_path.exists() {
        return None;
    }

    let data = std::fs::read_to_string(token_path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&data).ok()?;
    val.get("access_token")
        .and_then(|t| t.as_str())
        .map(str::to_string)
}

pub async fn sync_supercode_credential_if_needed(
    cred_manager: &CredentialManager,
) -> Result<(), HarnessError> {
    let token = match extract_supercode_token_from_file() {
        Some(t) => t,
        None => return Ok(()),
    };

    match cred_manager.acquire_credential("supercode").await {
        Ok(existing) => {
            let existing_str = existing.expose_secret();
            if existing_str != &token {
                let _ = cred_manager
                    .update_credential_key("supercode", existing_str, &token)
                    .await;
            }
        }
        Err(_) => {
            let _ = cred_manager
                .add_credential("supercode", "supercode-social", &token, true)
                .await;
        }
    }
    Ok(())
}

pub async fn fetch_supercode_models() -> Result<Vec<String>, HarnessError> {
    Ok(SUPERCODE_MODELS.iter().map(|s| s.to_string()).collect())
}

pub fn format_messages_for_supercode(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();

    for msg in messages {
        let role = match msg.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };

        let mut content = msg.content.clone().unwrap_or_default();
        if let Some(ref r) = msg.reasoning {
            content = format!("<thinking>\n{}\n</thinking>\n{}", r, content);
        }

        let mut obj = json!({
            "role": role,
            "content": content,
        });

        if let Some(ref r) = msg.reasoning {
            obj["reasoning_content"] = json!(r);
        }

        if msg.role == Role::Tool {
            let tool_call_id = msg
                .tool_result
                .as_ref()
                .map(|tr| tr.call_id.clone())
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| "call_0".to_string());
            obj["tool_call_id"] = json!(tool_call_id);
            if let Some(ref tr) = msg.tool_result {
                obj["content"] = json!(tr.content);
            }
        }

        if !msg.tool_calls.is_empty() {
            let tcs: Vec<serde_json::Value> = msg
                .tool_calls
                .iter()
                .map(|tc| {
                    let args_str = match &tc.arguments {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    json!({
                        "id": if tc.id.is_empty() { "call_0".to_string() } else { tc.id.clone() },
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": args_str,
                        }
                    })
                })
                .collect();
            obj["tool_calls"] = json!(tcs);
        }

        out.push(obj);
    }

    out
}

pub fn serialize_tools_for_supercode(tools: &[ToolDefinition]) -> Option<serde_json::Value> {
    if tools.is_empty() {
        return None;
    }
    let mut map = serde_json::Map::new();
    for t in tools {
        map.insert(
            t.name.clone(),
            json!({
                "description": t.description,
                "parameters": t.parameters,
            }),
        );
    }
    Some(serde_json::Value::Object(map))
}

pub async fn stream_supercode_completion(
    http_client: &reqwest::Client,
    raw_secret: &str,
    _cred_manager: Option<&Arc<CredentialManager>>,
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    _options: &CompletionOptions,
) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
    let resolved_token = if raw_secret.is_empty() {
        extract_supercode_token_from_file().ok_or_else(|| HarnessError::AuthenticationFailed {
            provider: "supercode".to_string(),
            message: "No credential found in ~/.better-auth/token.json or credential manager".to_string(),
        })?
    } else {
        raw_secret.to_string()
    };

    let resolved_model = if model.is_empty() { "kimi-k3" } else { model };
    let formatted_messages = format_messages_for_supercode(messages);

    let mut body = json!({
        "messages": formatted_messages,
        "provider": "supercode",
        "model": resolved_model,
    });

    if let Some(tools_obj) = serialize_tools_for_supercode(tools) {
        body["tools"] = tools_obj;
    }

    let resp = http_client
        .post(SUPERCODE_ENDPOINT)
        .header(AUTHORIZATION, format!("Bearer {}", resolved_token))
        .header(CONTENT_TYPE, "application/json")
        .header(USER_AGENT, SUPERCODE_USER_AGENT)
        .json(&body)
        .send()
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "supercode".to_string(),
            message: format!("HTTP request error: {}", e),
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        return Err(HarnessError::ProviderError {
            provider: "supercode".to_string(),
            message: format!("Supercode API error (HTTP {}): {}", status, err_body),
        });
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, HarnessError>>(100);

    tokio::spawn(async move {
        let mut byte_stream = resp.bytes_stream();
        let mut line_buffer = String::new();
        let mut has_received_content = false;
        let mut last_error: Option<String> = None;

        while let Some(chunk_res) = byte_stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    line_buffer.push_str(&text);

                    while let Some(pos) = line_buffer.find('\n') {
                        let line = line_buffer[..pos].trim().to_string();
                        line_buffer.drain(..=pos);

                        if line.is_empty() {
                            continue;
                        }

                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                            let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            match event_type {
                                "status" => {
                                    // Status notifications (accepted, plan_gate, budget, streaming, etc.)
                                }
                                "text" => {
                                    if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                                        if !content.is_empty() {
                                            has_received_content = true;
                                            let _ = tx.send(Ok(StreamEvent::ContentDelta(content.to_string()))).await;
                                        }
                                    }
                                }
                                "reasoning" => {
                                    if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                                        if !content.is_empty() {
                                            let _ = tx.send(Ok(StreamEvent::ReasoningDelta(content.to_string()))).await;
                                        }
                                    }
                                }
                                "tool-call" => {
                                    let name = event.get("toolName").and_then(|n| n.as_str()).map(str::to_string);
                                    let args = event.get("args").map(|a| a.to_string());
                                    let id = event.get("toolCallId").and_then(|i| i.as_str()).map(str::to_string);
                                    let _ = tx.send(Ok(StreamEvent::ToolCallDelta {
                                        index: 0,
                                        id,
                                        name,
                                        arguments_delta: args,
                                        signature: None,
                                    })).await;
                                }
                                "error" => {
                                    let msg = event.get("message").and_then(|m| m.as_str()).unwrap_or("Supercode upstream error");
                                    last_error = Some(msg.to_string());
                                }
                                "finish" => {
                                    let reason = event.get("reason").and_then(|r| r.as_str()).map(str::to_string);
                                    let usage = event.get("usage").and_then(|u| {
                                        let prompt_tokens = u.get("inputTokens").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
                                        let completion_tokens = u.get("outputTokens").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
                                        let total_tokens = u.get("totalTokens").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
                                        Some(TokenUsage {
                                            prompt_tokens,
                                            completion_tokens,
                                            total_tokens,
                                        })
                                    });
                                    let _ = tx.send(Ok(StreamEvent::Completed {
                                        finish_reason: reason.or(Some("stop".to_string())),
                                        usage,
                                    })).await;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(HarnessError::ProviderError {
                        provider: "supercode".to_string(),
                        message: format!("Stream read error: {}", e),
                    })).await;
                    break;
                }
            }
        }

        if !has_received_content {
            if let Some(err_msg) = last_error {
                let _ = tx.send(Err(HarnessError::ProviderError {
                    provider: "supercode".to_string(),
                    message: err_msg,
                })).await;
            }
        }
    });

    let rx_stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
    Ok(rx_stream.boxed())
}

pub async fn execute_supercode_completion(
    http_client: &reqwest::Client,
    raw_secret: &str,
    cred_manager: Option<&Arc<CredentialManager>>,
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
) -> Result<Message, HarnessError> {
    let mut stream = stream_supercode_completion(
        http_client,
        raw_secret,
        cred_manager,
        model,
        messages,
        tools,
        options,
    )
    .await?;

    let mut full_text = String::new();
    let mut thought = String::new();
    let mut final_usage = None;

    while let Some(ev_res) = stream.next().await {
        match ev_res? {
            StreamEvent::ContentDelta(d) => full_text.push_str(&d),
            StreamEvent::ReasoningDelta(r) => thought.push_str(&r),
            StreamEvent::Completed { usage, .. } => final_usage = usage,
            _ => {}
        }
    }

    let prompt_tokens = (messages.iter().map(|m| m.content.as_deref().unwrap_or("").len()).sum::<usize>() / 4).max(1) as u32;
    let completion_tokens = (full_text.len() / 4).max(1) as u32;

    Ok(Message {
        role: Role::Assistant,
        content: if full_text.is_empty() {
            None
        } else {
            Some(full_text)
        },
        reasoning: if thought.is_empty() {
            None
        } else {
            Some(thought)
        },
        tool_calls: Vec::new(),
        tool_result: None,
        usage: final_usage.or(Some(TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        })),
    })
}
