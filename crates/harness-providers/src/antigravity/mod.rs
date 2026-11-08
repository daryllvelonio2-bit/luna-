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
use std::sync::OnceLock;

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

/// The OAuth client secret used to refresh an Antigravity login.
///
/// The desktop client embeds one, which is the secret every login was issued against; the
/// environment overrides exist for a caller that registered its own client. Falling back to the
/// embedded value is what keeps a stored `refresh_token` usable: without it the hourly access-token
/// expiry turned into a hard authentication failure and the only way back was to copy `agy`'s
/// credentials again.
pub fn agy_client_secret() -> String {
    if let Ok(sec) = std::env::var("ANTIGRAVITY_CLIENT_SECRET") {
        if !sec.trim().is_empty() {
            return sec;
        }
    }
    if let Ok(sec) = std::env::var("AGY_CLIENT_SECRET") {
        if !sec.trim().is_empty() {
            return sec;
        }
    }
    AGY_CLIENT_SECRET.to_string()
}
/// The product half of the `User-Agent`.
///
/// Code Assist gates on the client identity, not just a version number: `antigravity/1.x` is the
/// retired desktop app and is answered with "This version of Antigravity is no longer supported",
/// even at 1.5.3, while `antigravity-cli/1.x` — what the `agy` CLI sends — is served normally.
/// (Probed against the live endpoint: `antigravity/1.2.7` and `antigravity/1.5.3` are refused,
/// `antigravity-cli/1.5.3` and `antigravity/2.0.0` are answered.)
const USER_AGENT_PRODUCT: &str = "antigravity-cli";

/// The version to claim when no local `agy` can be asked.
pub const FALLBACK_CLIENT_VERSION: &str = "1.5.3";

/// The `User-Agent` this route sends, e.g. `antigravity-cli/1.5.3`.
///
/// The version is read from the installed `agy` so the header follows the client's own updates
/// instead of freezing at a number the server later retires (the old `antigravity/1.2.7` is
/// exactly that). Probing happens once per process: `Command` is not cheap, and the version cannot
/// change mid-session.
pub fn user_agent() -> &'static str {
    static USER_AGENT: OnceLock<String> = OnceLock::new();
    USER_AGENT.get_or_init(|| {
        let version =
            installed_client_version().unwrap_or_else(|| FALLBACK_CLIENT_VERSION.to_string());
        format!("{USER_AGENT_PRODUCT}/{version}")
    })
}

/// The version `agy --version` prints, or `None` when the binary is absent or unusable.
fn installed_client_version() -> Option<String> {
    let output = Command::new("agy").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8(output.stdout).ok()?;
    let version = version.trim();
    // A version is digits and dots. Refusing anything else keeps a wrapper script's chatter (or a
    // newline-laden error) out of an HTTP header.
    is_version_like(version).then(|| version.to_string())
}

fn is_version_like(value: &str) -> bool {
    !value.is_empty()
        && value.starts_with(|c: char| c.is_ascii_digit())
        && value.chars().all(|c| c.is_ascii_digit() || c == '.')
}

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
    AntigravityQuotaSummary, QuotaLine, QuotaWindow, ANTIGRAVITY_MODELS,
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
                .header(USER_AGENT, user_agent())
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
    let (mut bearer_token, _) = extract_bearer_token(raw_secret);

    let mut resp = post_antigravity_with_transport_retry(http_client, &bearer_token, payload)
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "antigravity".to_string(),
            message: format!("HTTP error: {}", e),
        })?;

    // The access token lives about an hour. A 401 is the ordinary expiry, not a dead credential:
    // spend it on one refresh (which rewrites the stored record in place) and retry the request.
    let mut refresh_error = None;
    if resp.status().as_u16() == 401 {
        match credentials::refresh_and_store_antigravity_credential(
            http_client,
            raw_secret,
            cred_manager,
        )
        .await
        {
            Ok(new_token) => {
                bearer_token = new_token;
                resp = post_antigravity_with_transport_retry(http_client, &bearer_token, payload)
                    .await
                    .map_err(|e| HarnessError::ProviderError {
                        provider: "antigravity".to_string(),
                        message: format!("HTTP error on retry: {}", e),
                    })?;
            }
            Err(error) => refresh_error = Some(error),
        }
    }

    match resp.status().as_u16() {
        401 => Err(HarnessError::AuthenticationFailed {
            provider: "antigravity".to_string(),
            message: match refresh_error {
                Some(error) => format!(
                    "Antigravity OAuth token expired or unauthorized (401) and the automatic \
                     refresh failed: {error}"
                ),
                None => "Antigravity OAuth token expired or unauthorized (401)".to_string(),
            },
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
    // Measured across the POST too: the wait before the first frame is where a long prompt is
    // spent, and timing only the drain reported "ended after 0s" for a request that took a minute.
    let started = std::time::Instant::now();

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

    let mut full_text = String::new();
    let mut thought_buf = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut usage = None;
    let mut finish_reason = None;

    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::ContentDelta(text) => full_text.push_str(&text),
            StreamEvent::ReasoningDelta(text) => thought_buf.push_str(&text),
            StreamEvent::ToolCallDelta {
                id,
                name: Some(name),
                arguments_delta,
                signature,
                ..
            } => {
                let raw = arguments_delta.as_deref().unwrap_or("");
                // Empty means "no arguments"; unparseable non-empty means the provider cut the
                // call off mid-JSON. Never dispatch the latter as `{}`.
                let arguments = if raw.trim().is_empty() {
                    json!({})
                } else {
                    match serde_json::from_str(raw) {
                        Ok(value) => value,
                        Err(error) => {
                            return Err(HarnessError::ProviderError {
                                provider: "antigravity".to_string(),
                                message: format!(
                                    "Model returned an incomplete tool call for '{name}': its \
                                     arguments were not valid JSON ({error}). The call was not run."
                                ),
                            });
                        }
                    }
                };
                tool_calls.push(ToolCall {
                    id: id.unwrap_or_else(|| "call_agy".to_string()),
                    name,
                    arguments,
                    signature,
                });
            }
            StreamEvent::Completed {
                usage: event_usage,
                finish_reason: event_finish,
            } => {
                if event_usage.is_some() {
                    usage = event_usage;
                }
                if event_finish.is_some() {
                    finish_reason = event_finish;
                }
            }
            _ => {}
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
        // The model spent the whole output budget before writing anything. Retrying the same
        // request with the same cap is the same nothing, so this must not look transient.
        if matches!(
            finish_reason.as_deref(),
            Some("max_tokens") | Some("length")
        ) {
            return Err(HarnessError::output_limit_exhausted(
                "antigravity",
                options.max_tokens,
            ));
        }
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
