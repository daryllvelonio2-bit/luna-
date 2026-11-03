//! Google's Code Assist route: the wire shape, the quota, and the credential.
//!
//! This file is the facade. It owns the request path — post a turn, refresh once on a 401, map
//! the status codes the turn loop distinguishes, and stream the frames back — and re-exports the
//! pieces that live in submodules: `credentials` (who is signed in), `quota` (what is left and
//! which models it buys), `payload` (the two translations between our types and theirs).

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;

use harness_core::{
    CompletionOptions, HarnessError, Message, ReasoningEffort, Role, StreamEvent, TokenUsage,
    ToolCall, ToolDefinition,
};

use crate::thought_signature;

pub const DEFAULT_PROJECT: &str = "aicode-consumers";
pub const CODE_ASSIST_ENDPOINT: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse";
pub const FALLBACK_CODE_ASSIST_ENDPOINT: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse";
pub const USER_QUOTA_ENDPOINT: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary";
pub const FALLBACK_USER_QUOTA_ENDPOINT: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary";
pub const MODELS_ENDPOINT: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels";
pub const FALLBACK_MODELS_ENDPOINT: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels";
pub const GOOGLE_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub const AGY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub const AGY_CLIENT_SECRET: &str = "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";

pub fn agy_client_secret() -> Result<String, HarnessError> {
    if let Ok(sec) = std::env::var("ANTIGRAVITY_CLIENT_SECRET") {
        if !sec.trim().is_empty() {
            return Ok(sec);
        }
    }
    if let Ok(sec) = std::env::var("AGY_CLIENT_SECRET") {
        if !sec.trim().is_empty() {
            return Ok(sec);
        }
    }
    Err(HarnessError::AuthenticationFailed {
        provider: "antigravity".to_string(),
        message: "No ANTIGRAVITY_CLIENT_SECRET or AGY_CLIENT_SECRET configured in environment"
            .to_string(),
    })
}
pub const DEFAULT_USER_AGENT: &str = "antigravity/1.2.7";

mod credentials;
mod payload;
mod quota;

pub use credentials::{
    detect_antigravity_credentials, extract_bearer_token, refresh_antigravity_token,
    resolve_antigravity_email_strict, AntigravityCredentials,
};
use payload::{build_antigravity_payload, parse_antigravity_chunk};
pub use quota::{
    fetch_antigravity_models, resolve_antigravity_model, retrieve_antigravity_quota,
    AntigravityModelQuotaInfo, AntigravityQuotaBucket, AntigravityQuotaGroup,
    AntigravityQuotaSummary, QuotaLine, QuotaWindow,
};

async fn post_antigravity_with_transport_retry(
    http_client: &reqwest::Client,
    token: &str,
    payload: &serde_json::Value,
) -> Result<reqwest::Response, reqwest::Error> {
    let endpoints = [CODE_ASSIST_ENDPOINT, FALLBACK_CODE_ASSIST_ENDPOINT];
    let mut last_err = None;

    for (ep_idx, ep) in endpoints.iter().enumerate() {
        for attempt in 0..2 {
            let res = http_client
                .post(*ep)
                .header(AUTHORIZATION, format!("Bearer {}", token))
                .header(USER_AGENT, DEFAULT_USER_AGENT)
                .header(CONTENT_TYPE, "application/json")
                .json(payload)
                .send()
                .await;

            match res {
                Ok(resp) => return Ok(resp),
                Err(err) => {
                    let is_transport_error =
                        err.is_connect() || err.is_request() || err.is_timeout() || err.is_body();
                    last_err = Some(err);
                    if !is_transport_error {
                        return Err(last_err.unwrap());
                    }
                    if attempt == 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    }
                }
            }
        }
        if ep_idx == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
    }

    Err(last_err.expect("at least one endpoint attempt was made"))
}

/// POST one turn to Code Assist, refreshing the OAuth token once on a 401 and mapping the
/// status codes the agent loop distinguishes.
async fn post_antigravity_turn(
    http_client: &reqwest::Client,
    raw_secret: &str,
    cred_manager: Option<&std::sync::Arc<crate::auth::CredentialManager>>,
    payload: &serde_json::Value,
) -> Result<reqwest::Response, HarnessError> {
    let (mut bearer_token, refresh_tok_opt) = extract_bearer_token(raw_secret);

    let mut resp = post_antigravity_with_transport_retry(http_client, &bearer_token, payload)
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "antigravity".to_string(),
            message: format!("HTTP error: {}", e),
        })?;

    if resp.status().as_u16() == 401 {
        if let Some(ref refresh_tok) = refresh_tok_opt {
            if let Ok(new_token) = refresh_antigravity_token(http_client, refresh_tok).await {
                bearer_token = new_token.clone();
                if let Some(cm) = cred_manager {
                    let updated_secret = json!({
                        "access_token": new_token,
                        "refresh_token": refresh_tok,
                    });
                    let _ = cm
                        .update_credential_key(
                            "antigravity",
                            raw_secret,
                            &updated_secret.to_string(),
                        )
                        .await;
                }
                resp = post_antigravity_with_transport_retry(http_client, &bearer_token, payload)
                    .await
                    .map_err(|e| HarnessError::ProviderError {
                        provider: "antigravity".to_string(),
                        message: format!("HTTP error on retry: {}", e),
                    })?;
            }
        }
    }

    match resp.status().as_u16() {
        401 => Err(HarnessError::AuthenticationFailed {
            provider: "antigravity".to_string(),
            message: "Antigravity OAuth token expired or unauthorized (401)".to_string(),
        }),
        429 => Err(HarnessError::RateLimited {
            provider: "antigravity".to_string(),
            retry_after_secs: Some(10),
        }),
        503 => {
            let body = resp.text().await.unwrap_or_default();
            Err(HarnessError::ProviderError {
                provider: "antigravity".to_string(),
                message: format!("Model is overloaded (HTTP 503): {}", body),
            })
        }
        status if !resp.status().is_success() => {
            let body = resp.text().await.unwrap_or_default();
            let is_overloaded = status == 503 || body.to_lowercase().contains("overloaded");
            Err(HarnessError::ProviderError {
                provider: "antigravity".to_string(),
                message: if is_overloaded {
                    format!("Model is overloaded (HTTP {}): {}", status, body)
                } else {
                    format!("Code Assist API error (HTTP {}): {}", status, body)
                },
            })
        }
        _ => Ok(resp),
    }
}

/// Stream one Code Assist turn: SSE chunks are translated as they arrive, so reasoning reaches
/// the caller while the model is still producing it instead of after the whole body lands.
pub async fn stream_antigravity_completion(
    http_client: &reqwest::Client,
    raw_secret: &str,
    cred_manager: Option<&std::sync::Arc<crate::auth::CredentialManager>>,
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
) -> Result<futures_util::stream::BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError>
{
    let payload = build_antigravity_payload(messages, tools, options, model);
    let resp = post_antigravity_turn(http_client, raw_secret, cred_manager, &payload).await?;

    let stream = resp.bytes_stream().eventsource().flat_map(|event_result| {
        let events: Vec<Result<StreamEvent, HarnessError>> = match event_result {
            Ok(event) => {
                if event.data.trim().is_empty() || event.data == "[DONE]" {
                    Vec::new()
                } else {
                    match serde_json::from_str::<serde_json::Value>(&event.data) {
                        Ok(parsed) => {
                            if let Some(err_val) = parsed.get("error") {
                                let msg = if let Some(m) =
                                    err_val.get("message").and_then(|m| m.as_str())
                                {
                                    m.to_string()
                                } else {
                                    err_val.to_string()
                                };
                                let code =
                                    err_val.get("code").and_then(|c| c.as_u64()).unwrap_or(0);
                                if code == 429
                                    || msg.to_lowercase().contains("exhausted")
                                    || msg.to_lowercase().contains("quota")
                                    || msg.to_lowercase().contains("rate limit")
                                {
                                    vec![Err(HarnessError::RateLimited {
                                        provider: "antigravity".to_string(),
                                        retry_after_secs: Some(10),
                                    })]
                                } else if code == 503 || msg.to_lowercase().contains("overloaded") {
                                    vec![Err(HarnessError::ProviderError {
                                        provider: "antigravity".to_string(),
                                        message: format!("Model is overloaded (503): {}", msg),
                                    })]
                                } else {
                                    vec![Err(HarnessError::ProviderError {
                                        provider: "antigravity".to_string(),
                                        message: format!("Code Assist API error: {}", msg),
                                    })]
                                }
                            } else {
                                tracing::debug!(
                                    target: "harness_providers::antigravity",
                                    "code assist frame: {}",
                                    describe_frame(&parsed)
                                );
                                match blocked_reason(&parsed) {
                                    Some(reason) => vec![Err(HarnessError::ProviderError {
                                        provider: "antigravity".to_string(),
                                        message: format!(
                                            "the provider refused this prompt ({reason}). Retrying the same \
                                             request will not change that; the content it objects to has to go."
                                        ),
                                    })],
                                    None => parse_antigravity_chunk(&parsed)
                                        .into_iter()
                                        .map(Ok)
                                        .collect(),
                                }
                            }
                        }
                        Err(e) => vec![Err(HarnessError::SerializationError(format!(
                            "Failed to parse SSE JSON: {}",
                            e
                        )))],
                    }
                }
            }
            Err(e) => vec![Err(HarnessError::ProviderError {
                provider: "antigravity".to_string(),
                message: format!("SSE stream error: {}", e),
            })],
        };
        futures_util::stream::iter(events)
    });

    Ok(Box::pin(stream))
}

/// The error for a stream that carried nothing usable.
///
/// The old text — "the model may be overloaded or unavailable" — was a guess about a condition the
/// caller could not see, and it named nothing to act on. What is known is the shape of the request
/// that got nothing back and how long it waited, so that is what it says.
fn no_answer_error(
    elapsed: std::time::Duration,
    message_count: usize,
    tool_count: usize,
    prompt_tokens: Option<u32>,
) -> HarnessError {
    HarnessError::ProviderError {
        provider: "antigravity".to_string(),
        message: format!(
            "Code Assist returned no answer: the stream ended after {}s without text, tool calls or \
             reasoning, for a request of {message_count} messages and {tool_count} tools{}. The route \
             does this intermittently, most often on long tool-heavy turns — retry, or start a new \
             session / run /compact when one session keeps hitting it.",
            elapsed.as_secs(),
            prompt_tokens
                .map(|tokens| format!(" ({tokens} prompt tokens)"))
                .unwrap_or_default()
        ),
    }
}

/// The provider's own reason for refusing a prompt, when the frame carries one.
///
/// Code Assist reports a refusal as `promptFeedback.blockReason` *instead of* candidates, so without
/// this the frame looks like every other content-free frame and the turn ends with "the model may be
/// overloaded" — which is not what happened and sends the reader looking in the wrong place.
fn blocked_reason(frame: &serde_json::Value) -> Option<&str> {
    let response = frame.get("response").unwrap_or(frame);
    response
        .get("promptFeedback")
        .and_then(|feedback| feedback.get("blockReason"))
        .and_then(|reason| reason.as_str())
}

/// The shape of one frame, for the log: candidate count, finish reason, block reason, which kinds of
/// part it carried, and the prompt size. Never the content — a Code Assist answer can come back as a
/// stream that ends with nothing the turn loop can use, and when it does, this line is the evidence.
fn describe_frame(frame: &serde_json::Value) -> String {
    let response = frame.get("response").unwrap_or(frame);
    let candidates = response
        .get("candidates")
        .and_then(|c| c.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    let finish = response
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.get("finishReason"))
        .and_then(|r| r.as_str())
        .unwrap_or("-");
    let block = response
        .get("promptFeedback")
        .and_then(|feedback| feedback.get("blockReason"))
        .and_then(|r| r.as_str())
        .unwrap_or("-");
    let kinds: Vec<&str> = response
        .get("candidates")
        .and_then(|c| c.as_array())
        .map(|candidates| {
            candidates
                .iter()
                .flat_map(|candidate| {
                    candidate
                        .get("content")
                        .and_then(|c| c.get("parts"))
                        .and_then(|p| p.as_array())
                        .map(|parts| {
                            parts
                                .iter()
                                .map(|part| {
                                    if part.get("functionCall").is_some() {
                                        "functionCall"
                                    } else if part.get("thought").is_some() {
                                        "thought"
                                    } else if part.get("text").is_some() {
                                        "text"
                                    } else {
                                        "other"
                                    }
                                })
                                .collect::<Vec<&str>>()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();
    let prompt_tokens = response
        .get("usageMetadata")
        .and_then(|meta| meta.get("promptTokenCount"))
        .and_then(|t| t.as_u64());
    format!(
        "candidates={candidates} finish={finish} block={block} parts=[{}]{}",
        kinds.join(","),
        prompt_tokens
            .map(|tokens| format!(" promptTokens={tokens}"))
            .unwrap_or_default()
    )
}

pub async fn execute_antigravity_completion(
    http_client: &reqwest::Client,
    raw_secret: &str,
    cred_manager: Option<&std::sync::Arc<crate::auth::CredentialManager>>,
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
) -> Result<Message, HarnessError> {
    // Draining the streaming path keeps one parser for both: the buffered call and the live one
    // classify thought chunks, text and function calls identically.
    let mut stream = stream_antigravity_completion(
        http_client,
        raw_secret,
        cred_manager,
        model,
        messages,
        tools,
        options,
    )
    .await?;

    let started = std::time::Instant::now();
    let mut full_text = String::new();
    let mut thought_buf = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut usage = None;

    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::ContentDelta(text) => full_text.push_str(&text),
            StreamEvent::ReasoningDelta(text) => thought_buf.push_str(&text),
            StreamEvent::ToolCallDelta {
                id,
                name,
                arguments_delta,
                signature,
                ..
            } => {
                if let Some(name) = name {
                    let arguments = arguments_delta
                        .as_deref()
                        .and_then(|raw| serde_json::from_str(raw).ok())
                        .unwrap_or_else(|| json!({}));
                    tool_calls.push(ToolCall {
                        id: id.unwrap_or_else(|| "call_agy".to_string()),
                        name,
                        arguments,
                        signature,
                    });
                }
            }
            StreamEvent::Completed {
                usage: event_usage, ..
            } => {
                if event_usage.is_some() {
                    usage = event_usage;
                }
            }
        }
    }

    // Extract inline  thinking or <thought> tags if thought_buf is empty
    if thought_buf.is_empty() {
        for (open_tag, close_tag) in &[(" thinking", "</think>"), ("<thought>", "</thought>")] {
            if let Some(start) = full_text.find(open_tag) {
                if let Some(end) = full_text.find(close_tag) {
                    let think_text = full_text[start + open_tag.len()..end].trim().to_string();
                    let rest = format!(
                        "{}{}",
                        &full_text[..start],
                        &full_text[end + close_tag.len()..]
                    )
                    .trim()
                    .to_string();
                    if !think_text.is_empty() {
                        thought_buf = think_text;
                    }
                    full_text = rest;
                    break;
                }
            }
        }
    }

    let reasoning = if thought_buf.is_empty() {
        None
    } else {
        Some(thought_buf)
    };

    let elapsed = started.elapsed();
    if full_text.trim().is_empty() && tool_calls.is_empty() {
        if let Some(r) = reasoning {
            return Ok(Message {
                role: Role::Assistant,
                content: Some(harness_core::types::NO_TEXT_RESPONSE.to_string()),
                reasoning: Some(r),
                tool_calls: Vec::new(),
                tool_result: None,
                usage,
            });
        }
        return Err(no_answer_error(
            elapsed,
            messages.len(),
            tools.len(),
            usage.map(|u| u.prompt_tokens),
        ));
    }

    let mut res_msg = Message::assistant(full_text);
    res_msg.reasoning = reasoning;
    res_msg.tool_calls = tool_calls;
    res_msg.usage = usage;
    Ok(res_msg)
}

#[cfg(test)]
mod tests;
