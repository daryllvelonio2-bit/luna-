//! OpenAI Codex's own route: the ChatGPT backend the Codex CLI talks to, without the CLI.
//!
//! The user's Codex login buys its models directly, so this route posts to the same
//! `backend-api/codex/responses` endpoint the CLI uses, with the same headers, and reads the same
//! SSE stream back. Nothing is spawned: there is no `codex exec` in this path and no local server
//! to keep alive — the credential is the CLI's, the wire belongs to the API.
//!
//! This file is the facade. It owns the request path (headers, status codes the turn loop
//! distinguishes, frame-to-event mapping) and re-exports `credentials` (who is signed in) and
//! `payload` (the two translations between our types and theirs).

mod credentials;
mod payload;

pub use credentials::{
    codex_auth_paths, detect_codex_credentials, detect_codex_credentials_in, parse_codex_secret,
    CodexCredentials,
};
use payload::{build_codex_payload, parse_codex_frame};

use std::path::PathBuf;
use std::sync::OnceLock;

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};

use harness_core::{CompletionOptions, HarnessError, Message, StreamEvent, ToolDefinition};

pub const CODEX_RESPONSES_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
pub const CODEX_MODELS_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/models";

/// The product half of the `User-Agent` and the `originator` header: the client these requests
/// are attributed to. The backend answers a plainly-OpenAI-shaped request the same way, but the
/// Codex client identity is what the account's own entitlement is attached to.
const CODEX_ORIGINATOR: &str = "codex_cli_rs";

/// The version to claim when no local Codex installation can be read. Only the `client_version`
/// the models endpoint demands and the `User-Agent` carry it, so a stale value is cosmetic.
const FALLBACK_CLIENT_VERSION: &str = "0.167.2";

/// Advertised cool-off for a rate-limited turn.
const RATE_LIMIT_COOLDOWN_SECS: u64 = 30;

/// The Codex version this machine has, as the CLI itself recorded it.
///
/// Read from disk rather than by running `codex --version`: this route's whole point is not to
/// spawn the CLI, and the version cannot change mid-session anyway.
pub fn codex_client_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        installed_client_version().unwrap_or_else(|| FALLBACK_CLIENT_VERSION.into())
    })
}

fn installed_client_version() -> Option<String> {
    let dir = std::env::var("CODEX_HOME")
        .ok()
        .filter(|dir| !dir.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| harness_core::config::home_dir().join(".codex"));
    let raw = std::fs::read_to_string(dir.join("version.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let version = value.get("latest_version")?.as_str()?.trim();
    is_version_like(version).then(|| version.to_string())
}

/// A version is digits and dots. Refusing anything else keeps a stray file's contents out of an
/// HTTP header.
fn is_version_like(value: &str) -> bool {
    !value.is_empty()
        && value.starts_with(|c: char| c.is_ascii_digit())
        && value.chars().all(|c| c.is_ascii_digit() || c == '.')
}

fn user_agent() -> String {
    format!("{CODEX_ORIGINATOR}/{}", codex_client_version())
}

/// The headers every request on this route carries.
fn codex_headers(credentials: &CodexCredentials) -> Result<HeaderMap, HarnessError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", credentials.access_token))
            .map_err(|e| HarnessError::ConfigurationError(e.to_string()))?,
    );
    if let Some(account_id) = credentials
        .account_id
        .as_deref()
        .filter(|account| !account.trim().is_empty())
    {
        headers.insert(
            "chatgpt-account-id",
            HeaderValue::from_str(account_id)
                .map_err(|e| HarnessError::ConfigurationError(e.to_string()))?,
        );
    }
    headers.insert(
        "OpenAI-Beta",
        HeaderValue::from_static("responses=experimental"),
    );
    headers.insert("originator", HeaderValue::from_static(CODEX_ORIGINATOR));
    headers.insert(
        "session_id",
        HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
            .map_err(|e| HarnessError::ConfigurationError(e.to_string()))?,
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&user_agent())
            .map_err(|e| HarnessError::ConfigurationError(e.to_string()))?,
    );
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(headers)
}

/// The models the account can reach, asked of the backend the CLI asks.
///
/// The endpoint requires a `client_version` query parameter, and answers with the same catalogue
/// the CLI's model picker shows — including the entries its picker hides, which are reachable
/// through the API and which a power user may well be looking for.
pub async fn fetch_codex_models(
    http_client: &reqwest::Client,
    credentials: &CodexCredentials,
) -> Result<Vec<String>, HarnessError> {
    let url = format!(
        "{CODEX_MODELS_ENDPOINT}?client_version={}",
        codex_client_version()
    );
    let resp = http_client
        .get(url)
        .headers(codex_headers(credentials)?)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "codex".to_string(),
            message: format!("model list request failed: {e}"),
        })?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(models_error(status, &body));
    }

    let parsed: serde_json::Value = resp.json().await.map_err(|e| HarnessError::ProviderError {
        provider: "codex".to_string(),
        message: format!("model list was not JSON: {e}"),
    })?;

    let models: Vec<String> = parsed
        .get("models")
        .and_then(|models| models.as_array())
        .map(|models| {
            models
                .iter()
                .filter(|model| {
                    // A model the backend says it cannot serve over the API is not offered: the
                    // list is what the picker shows, and offering it would only produce a 404.
                    model
                        .get("supported_in_api")
                        .and_then(|supported| supported.as_bool())
                        .unwrap_or(true)
                })
                .filter_map(|model| model.get("slug").and_then(|slug| slug.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    Ok(models)
}

fn models_error(status: u16, body: &str) -> HarnessError {
    if status == 401 || status == 403 {
        return expired_credential_error();
    }
    HarnessError::ProviderError {
        provider: "codex".to_string(),
        message: format!("model list failed (HTTP {status}): {body}"),
    }
}

/// What a 401 on this route means, and what to do about it.
///
/// The access token is a ChatGPT OAuth token with a fixed lifetime, and this route does not renew
/// it: a refresh here would rotate the token and leave the Codex CLI holding a refresh token it no
/// longer owns. Re-importing costs one keystroke and re-reads a file the CLI keeps fresh.
fn expired_credential_error() -> HarnessError {
    HarnessError::AuthenticationFailed {
        provider: "codex".to_string(),
        message: "the Codex token was rejected (401). It has most likely expired. Refresh it by \
                  opening the Codex CLI once (or `codex login`), then import it again with \
                  /connect → Import Codex."
            .to_string(),
    }
}

async fn post_codex_turn(
    http_client: &reqwest::Client,
    credentials: &CodexCredentials,
    body: &serde_json::Value,
) -> Result<reqwest::Response, HarnessError> {
    let resp = http_client
        .post(CODEX_RESPONSES_ENDPOINT)
        .headers(codex_headers(credentials)?)
        .json(body)
        .send()
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "codex".to_string(),
            message: format!("HTTP error: {e}"),
        })?;

    match resp.status().as_u16() {
        401 | 403 => Err(expired_credential_error()),
        429 => Err(HarnessError::RateLimited {
            provider: "codex".to_string(),
            retry_after_secs: Some(RATE_LIMIT_COOLDOWN_SECS),
        }),
        status if !resp.status().is_success() => {
            let body = resp.text().await.unwrap_or_default();
            Err(HarnessError::ProviderError {
                provider: "codex".to_string(),
                message: turn_error_message(status, &body),
            })
        }
        _ => Ok(resp),
    }
}

/// A usage-limit refusal is not a malformed request, and the raw JSON says so only in prose.
fn turn_error_message(status: u16, body: &str) -> String {
    let lowered = body.to_lowercase();
    if lowered.contains("usage limit")
        || lowered.contains("rate limit")
        || lowered.contains("too many requests")
    {
        return format!("Codex usage limit reached (HTTP {status}): {body}");
    }
    format!("Codex API error (HTTP {status}): {body}")
}

/// Stream one Codex turn: SSE frames are translated as they arrive, so text reaches the caller
/// while the model is still writing instead of after the whole body lands.
pub async fn stream_codex_completion(
    http_client: &reqwest::Client,
    raw_secret: &str,
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
) -> Result<futures_util::stream::BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError>
{
    let credentials = parse_codex_secret(raw_secret);
    if credentials.access_token.trim().is_empty() {
        return Err(expired_credential_error());
    }

    let payload = build_codex_payload(messages, tools, options, model);
    let resp = post_codex_turn(http_client, &credentials, &payload).await?;

    let stream = resp.bytes_stream().eventsource().flat_map(|event_result| {
        let events: Vec<Result<StreamEvent, HarnessError>> = match event_result {
            Ok(event) => {
                if event.data.trim().is_empty() || event.data == "[DONE]" {
                    Vec::new()
                } else {
                    match serde_json::from_str::<serde_json::Value>(&event.data) {
                        Ok(parsed) => frame_events(&parsed),
                        Err(e) => vec![Err(HarnessError::SerializationError(format!(
                            "Failed to parse SSE JSON: {e}"
                        )))],
                    }
                }
            }
            Err(e) => vec![Err(HarnessError::ProviderError {
                provider: "codex".to_string(),
                message: format!("SSE stream error: {e}"),
            })],
        };
        futures_util::stream::iter(events)
    });

    Ok(Box::pin(stream))
}

/// One parsed frame as stream events.
///
/// Three shapes end a turn with an error rather than with content: a transport-level `error`
/// frame, a `response.failed` frame, and an error inside any other frame. Each carries the
/// provider's own sentence, which is the only part of a failed turn worth showing.
fn frame_events(parsed: &serde_json::Value) -> Vec<Result<StreamEvent, HarnessError>> {
    let frame_type = parsed
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or_default();

    if frame_type == "error" {
        return vec![Err(HarnessError::ProviderError {
            provider: "codex".to_string(),
            message: error_message(parsed).unwrap_or_else(|| parsed.to_string()),
        })];
    }

    if frame_type == "response.failed" {
        let message = parsed
            .get("response")
            .and_then(error_message)
            .or_else(|| error_message(parsed))
            .unwrap_or_else(|| "the model returned a failed response".to_string());
        return vec![Err(HarnessError::ProviderError {
            provider: "codex".to_string(),
            message,
        })];
    }

    if let Some(error) = parsed.get("error").filter(|error| !error.is_null()) {
        let message = error_message(parsed).unwrap_or_else(|| error.to_string());
        return vec![Err(HarnessError::ProviderError {
            provider: "codex".to_string(),
            message,
        })];
    }

    parse_codex_frame(parsed).into_iter().map(Ok).collect()
}

fn error_message(value: &serde_json::Value) -> Option<String> {
    let error = value.get("error")?;
    if let Some(message) = error.get("message").and_then(|m| m.as_str()) {
        return Some(message.to_string());
    }
    if let Some(message) = error.as_str() {
        return Some(message.to_string());
    }
    Some(error.to_string())
}

/// Buffer a whole turn out of the stream above.
///
/// Draining the streaming path keeps one parser for both: the live path and this one classify
/// text, reasoning and calls identically.
pub async fn execute_codex_completion(
    http_client: &reqwest::Client,
    raw_secret: &str,
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
) -> Result<Message, HarnessError> {
    let started = std::time::Instant::now();

    let mut stream =
        stream_codex_completion(http_client, raw_secret, model, messages, tools, options).await?;

    let mut full_text = String::new();
    let mut thought_buf = String::new();
    let mut tool_calls: Vec<harness_core::ToolCall> = Vec::new();
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
                let arguments = match harness_core::json::parse_tool_arguments(raw) {
                    Ok(value) => value,
                    Err(error) => {
                        return Err(HarnessError::ProviderError {
                            provider: "codex".to_string(),
                            message: format!(
                                "Model returned an incomplete tool call for '{name}': its \
                                 arguments were not valid JSON ({error}). The call was not run."
                            ),
                        });
                    }
                };
                tool_calls.push(harness_core::ToolCall {
                    id: id.unwrap_or_else(|| "call_codex".to_string()),
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

    let reasoning = (!thought_buf.is_empty()).then_some(thought_buf);

    if full_text.trim().is_empty() && tool_calls.is_empty() {
        // The model spent the whole output budget before writing anything. Re-sending the same
        // request with the same cap is the same nothing, so this must not read as transient.
        if matches!(
            finish_reason.as_deref(),
            Some("max_tokens") | Some("length")
        ) {
            return Err(HarnessError::output_limit_exhausted(
                "codex",
                options.max_tokens,
            ));
        }
        if let Some(reasoning) = reasoning {
            return Ok(Message {
                role: harness_core::Role::Assistant,
                content: Some(harness_core::types::NO_TEXT_RESPONSE.to_string()),
                reasoning: Some(reasoning),
                tool_calls: Vec::new(),
                tool_result: None,
                usage,
            });
        }
        return Err(HarnessError::ProviderError {
            provider: "codex".to_string(),
            message: format!(
                "Codex returned no answer: the stream ended after {}s without text, tool calls or \
                 reasoning, for a request of {} messages and {} tools.",
                started.elapsed().as_secs(),
                messages.len(),
                tools.len()
            ),
        });
    }

    let mut message = Message::assistant(full_text);
    message.reasoning = reasoning;
    message.tool_calls = tool_calls;
    message.usage = usage;
    Ok(message)
}

#[cfg(test)]
mod tests;
