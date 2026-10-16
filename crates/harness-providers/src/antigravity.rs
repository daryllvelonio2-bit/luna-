use std::path::PathBuf;
use std::process::Command;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::json;

use harness_core::{
    CompletionOptions, HarnessError, Message, ReasoningEffort, Role, StreamEvent, TokenUsage,
    ToolCall, ToolDefinition,
};

pub const DEFAULT_PROJECT: &str = "aicode-consumers";
pub const CODE_ASSIST_ENDPOINT: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse";
pub const MODELS_ENDPOINT: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels";
pub const GOOGLE_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub const AGY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub const AGY_CLIENT_SECRET: &str = "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";
pub const DEFAULT_USER_AGENT: &str = "antigravity/1.2.7";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntigravityCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub email: Option<String>,
    pub expiry: Option<String>,
}

/// Detect and copy logged-in Antigravity credentials from SecretService or Shiina.
pub fn detect_antigravity_credentials() -> Result<AntigravityCredentials, HarnessError> {
    // 1. Try Linux secret-tool (service: gemini, username: antigravity)
    if let Ok(output) = Command::new("secret-tool")
        .args(["lookup", "service", "gemini", "username", "antigravity"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !stdout.is_empty() {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    let tok = val.get("token").unwrap_or(&val);
                    if let Some(access) = tok.get("access_token").and_then(|a| a.as_str()) {
                        let refresh = tok.get("refresh_token").and_then(|r| r.as_str()).map(|s| s.to_string());
                        let expiry = tok.get("expiry").and_then(|e| e.as_str()).map(|s| s.to_string());
                        let email = extract_email_from_jwt(&val);
                        return Ok(AntigravityCredentials {
                            access_token: access.to_string(),
                            refresh_token: refresh,
                            email,
                            expiry,
                        });
                    }
                }
            }
        }
    }

    // 2. Try ~/.shiina/auth.json or profile auth.json
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let mut candidate_paths = vec![
        PathBuf::from(&home).join(".shiina").join("auth.json"),
    ];

    let profiles_dir = PathBuf::from(&home).join(".shiina").join("profiles");
    if profiles_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(profiles_dir) {
            for entry in entries.flatten() {
                candidate_paths.push(entry.path().join("auth.json"));
            }
        }
    }

    for path in candidate_paths {
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(agy) = data
                        .get("providers")
                        .and_then(|p| p.get("antigravity"))
                    {
                        if let Some(access) = agy.get("access_token").and_then(|a| a.as_str()) {
                            let refresh = agy.get("refresh_token").and_then(|r| r.as_str()).map(|s| s.to_string());
                            let email = agy.get("email").and_then(|e| e.as_str()).map(|s| s.to_string());
                            return Ok(AntigravityCredentials {
                                access_token: access.to_string(),
                                refresh_token: refresh,
                                email,
                                expiry: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // 3. Try environment variables
    for var in &["ANTIGRAVITY_ACCESS_TOKEN", "AGY_ACCESS_TOKEN", "GEMINI_ACCESS_TOKEN"] {
        if let Ok(token) = std::env::var(var) {
            if !token.trim().is_empty() {
                return Ok(AntigravityCredentials {
                    access_token: token.trim().to_string(),
                    refresh_token: None,
                    email: None,
                    expiry: None,
                });
            }
        }
    }

    Err(HarnessError::AuthenticationFailed {
        provider: "antigravity".to_string(),
        message: "No Antigravity credentials found in SecretService, ~/.shiina/auth.json, or environment.".to_string(),
    })
}

fn extract_email_from_jwt(val: &serde_json::Value) -> Option<String> {
    if let Some(email) = val.get("email").and_then(|e| e.as_str()) {
        return Some(email.to_string());
    }
    if let Some(id_tok) = val.get("id_token").and_then(|t| t.as_str()) {
        let parts: Vec<&str> = id_tok.split('.').collect();
        if parts.len() >= 2 {
            let padded = format!("{}{}", parts[1], "=".repeat((4 - parts[1].len() % 4) % 4));
            if let Ok(decoded) = base64_url_decode(&padded) {
                if let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&decoded) {
                    if let Some(email) = claims.get("email").and_then(|e| e.as_str()) {
                        return Some(email.to_string());
                    }
                }
            }
        }
    }
    None
}

fn base64_url_decode(s: &str) -> Result<Vec<u8>, String> {
    let clean: String = s.chars().map(|c| match c {
        '-' => '+',
        '_' => '/',
        other => other,
    }).collect();
    let mut bytes = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for b in clean.bytes() {
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buf >> bits) as u8);
        }
    }
    Ok(bytes)
}

/// Refresh an Antigravity Google OAuth access token using a refresh token.
pub async fn refresh_antigravity_token(
    http_client: &reqwest::Client,
    refresh_token: &str,
) -> Result<String, HarnessError> {
    let params = [
        ("client_id", AGY_CLIENT_ID),
        ("client_secret", AGY_CLIENT_SECRET),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];

    let resp = http_client
        .post(GOOGLE_OAUTH_TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| HarnessError::AuthenticationFailed {
            provider: "antigravity".to_string(),
            message: format!("OAuth refresh network error: {}", e),
        })?;

    if !resp.status().is_success() {
        let err_body = resp.text().await.unwrap_or_default();
        return Err(HarnessError::AuthenticationFailed {
            provider: "antigravity".to_string(),
            message: format!("Failed to refresh Antigravity OAuth token: {}", err_body),
        });
    }

    let data: serde_json::Value = resp.json().await.map_err(|e| HarnessError::AuthenticationFailed {
        provider: "antigravity".to_string(),
        message: format!("Invalid JSON response from Google OAuth: {}", e),
    })?;

    let access = data
        .get("access_token")
        .and_then(|a| a.as_str())
        .ok_or_else(|| HarnessError::AuthenticationFailed {
            provider: "antigravity".to_string(),
            message: "Missing access_token in OAuth refresh response".to_string(),
        })?;

    Ok(access.to_string())
}

/// Extracts the raw bearer token string if stored as a JSON credential record.
pub fn extract_bearer_token(secret: &str) -> (String, Option<String>) {
    let trimmed = secret.trim();
    if trimmed.starts_with('{') {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let access = val.get("access_token").and_then(|a| a.as_str()).unwrap_or(trimmed);
            let refresh = val.get("refresh_token").and_then(|r| r.as_str()).map(|s| s.to_string());
            return (access.to_string(), refresh);
        }
    }
    (trimmed.to_string(), None)
}

pub const ANTIGRAVITY_MODELS: &[&str] = &[
    "gemini-3.8-flash",
    "claude-sonnet-4-6",
    "claude-opus-4-6",
    "gpt-oss-120b",
];

/// Resolves user-facing clean model names and reasoning effort settings to backend Code Assist model names.
pub fn resolve_antigravity_model(
    raw_model: &str,
    reasoning_effort: Option<&ReasoningEffort>,
) -> String {
    let cleaned = raw_model.trim();
    let cleaned = cleaned.strip_prefix("antigravity/").unwrap_or(cleaned);
    let cleaned = cleaned.strip_prefix("agy/").unwrap_or(cleaned);
    let lower = cleaned.to_lowercase();

    if lower.contains("gemini") || lower.contains("flash") {
        match reasoning_effort {
            Some(ReasoningEffort::High | ReasoningEffort::Xhigh | ReasoningEffort::Max) => {
                "gemini-3.8-flash-high".to_string()
            }
            Some(ReasoningEffort::Medium) => "gemini-3.8-flash-medium".to_string(),
            Some(ReasoningEffort::Low | ReasoningEffort::Minimal) => {
                "gemini-3.8-flash-low".to_string()
            }
            None => "gemini-3.8-flash-tiered".to_string(),
        }
    } else if lower.contains("opus") {
        "claude-opus-4-6-thinking".to_string()
    } else if lower.contains("sonnet") {
        "claude-sonnet-4-6".to_string()
    } else if lower.contains("gpt-oss") || lower.contains("oss") {
        "gpt-oss-120b-medium".to_string()
    } else {
        cleaned.to_string()
    }
}

/// Fetches supported Antigravity models, returning clean user-facing model names without reasoning effort suffixes.
pub async fn fetch_antigravity_models(
    _http_client: &reqwest::Client,
    _bearer_token: &str,
) -> Result<Vec<String>, HarnessError> {
    Ok(ANTIGRAVITY_MODELS.iter().map(|&m| m.to_string()).collect())
}

/// Translate one Code Assist SSE payload into stream events. Both the streaming path and the
/// buffered `execute_antigravity_completion` go through here, so a thought chunk is classified
/// identically either way.
fn parse_antigravity_chunk(event: &serde_json::Value) -> Vec<StreamEvent> {
    let resp_data = event.get("response").unwrap_or(event);
    let mut events = Vec::new();

    if let Some(meta) = resp_data.get("usageMetadata") {
        let prompt_tokens = meta.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let completion_tokens = meta.get("candidatesTokenCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        events.push(StreamEvent::Completed {
            finish_reason: None,
            usage: Some(TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
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
            let is_thought = part.get("thought").and_then(|t| t.as_bool()).unwrap_or(false);
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
                    id: call.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()),
                    name: call.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()),
                    arguments_delta: Some(
                        call.get("args").cloned().unwrap_or_else(|| json!({})).to_string(),
                    ),
                });
            }
        }
    }

    events
}

/// Build the Code Assist payload for one turn: messages translated to `contents`, tools to
/// function declarations, and the reasoning effort to a thinking budget.
fn build_antigravity_payload(
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
    model: &str,
) -> serde_json::Value {
    let mut gemini_contents = Vec::new();
    let mut system_instruction = None;

    for msg in messages {
        match msg.role {
            Role::System => {
                if let Some(ref text) = msg.content {
                    system_instruction = Some(json!({
                        "parts": [{ "text": text }]
                    }));
                }
            }
            Role::User => {
                let text = msg.content.as_deref().unwrap_or("");
                gemini_contents.push(json!({
                    "role": "user",
                    "parts": [{ "text": text }]
                }));
            }
            Role::Assistant => {
                let mut parts = Vec::new();
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
                    parts.push(json!({
                        "functionCall": {
                            "id": call_id,
                            "name": c.name,
                            "args": c.arguments
                        }
                    }));
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
                let result_content = msg.tool_result.as_ref().map(|r| r.content.as_str()).unwrap_or("");
                let tool_name = messages
                    .iter()
                    .filter_map(|m| {
                        m.tool_calls
                            .iter()
                            .find(|tc| tc.id == call_id)
                            .map(|tc| tc.name.as_str())
                    })
                    .next()
                    .unwrap_or("terminal");
                gemini_contents.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "id": call_id,
                            "name": tool_name,
                            "response": { "result": result_content }
                        }
                    }]
                }));
            }
        }
    }

    let mut req_obj = json!({ "contents": gemini_contents });

    if let Some(sys) = system_instruction {
        req_obj["systemInstruction"] = sys;
    }

    if !tools.is_empty() {
        let func_decls: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters
                })
            })
            .collect();
        req_obj["tools"] = json!([{ "functionDeclarations": func_decls }]);
    }

    let mut gen_config = json!({});
    if let Some(temp) = options.temperature {
        gen_config["temperature"] = json!(temp);
    }
    if let Some(max_tok) = options.max_tokens {
        gen_config["maxOutputTokens"] = json!(max_tok);
    }
    if let Some(top_p) = options.top_p {
        gen_config["topP"] = json!(top_p);
    }
    if let Some(effort) = options.reasoning_effort {
        match effort {
            ReasoningEffort::Minimal => gen_config["thinkingConfig"] = json!({ "thinkingBudget": 1024 }),
            ReasoningEffort::Low => gen_config["thinkingConfig"] = json!({ "thinkingBudget": 2048 }),
            ReasoningEffort::Medium => gen_config["thinkingConfig"] = json!({ "thinkingBudget": 8192 }),
            ReasoningEffort::High => gen_config["thinkingConfig"] = json!({ "thinkingBudget": 16384 }),
            ReasoningEffort::Xhigh | ReasoningEffort::Max => {
                gen_config["thinkingConfig"] = json!({ "thinkingBudget": 32768 })
            }
        }
    }
    if gen_config.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
        req_obj["generationConfig"] = gen_config;
    }

    json!({
        "project": DEFAULT_PROJECT,
        "model": resolve_antigravity_model(model, options.reasoning_effort.as_ref()),
        "request": req_obj
    })
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

    let send = |token: String| {
        let client = http_client.clone();
        let payload = payload.clone();
        async move {
            client
                .post(CODE_ASSIST_ENDPOINT)
                .header(AUTHORIZATION, format!("Bearer {}", token))
                .header(USER_AGENT, DEFAULT_USER_AGENT)
                .header(CONTENT_TYPE, "application/json")
                .json(&payload)
                .send()
                .await
        }
    };

    let mut resp = send(bearer_token.clone()).await.map_err(|e| HarnessError::ProviderError {
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
                        .update_credential_key("antigravity", raw_secret, &updated_secret.to_string())
                        .await;
                }
                resp = send(bearer_token).await.map_err(|e| HarnessError::ProviderError {
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
) -> Result<futures_util::stream::BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
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
                                let msg = if let Some(m) = err_val.get("message").and_then(|m| m.as_str()) {
                                    m.to_string()
                                } else {
                                    err_val.to_string()
                                };
                                let code = err_val.get("code").and_then(|c| c.as_u64()).unwrap_or(0);
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
                                parse_antigravity_chunk(&parsed).into_iter().map(Ok).collect()
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
    let mut stream =
        stream_antigravity_completion(http_client, raw_secret, cred_manager, model, messages, tools, options)
            .await?;

    let mut full_text = String::new();
    let mut thought_buf = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut usage = None;

    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::ContentDelta(text) => full_text.push_str(&text),
            StreamEvent::ReasoningDelta(text) => thought_buf.push_str(&text),
            StreamEvent::ToolCallDelta { id, name, arguments_delta, .. } => {
                if let Some(name) = name {
                    let arguments = arguments_delta
                        .as_deref()
                        .and_then(|raw| serde_json::from_str(raw).ok())
                        .unwrap_or_else(|| json!({}));
                    tool_calls.push(ToolCall {
                        id: id.unwrap_or_else(|| "call_agy".to_string()),
                        name,
                        arguments,
                    });
                }
            }
            StreamEvent::Completed { usage: event_usage, .. } => {
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
                    let rest = format!("{}{}", &full_text[..start], &full_text[end + close_tag.len()..]).trim().to_string();
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

    if full_text.trim().is_empty() && tool_calls.is_empty() {
        if let Some(r) = reasoning {
            return Ok(Message {
                role: Role::Assistant,
                content: Some("(Completed reasoning with no text response)".to_string()),
                reasoning: Some(r),
                tool_calls: Vec::new(),
                tool_result: None,
                usage,
            });
        }
        return Err(HarnessError::ProviderError {
            provider: "antigravity".to_string(),
            message: "Model returned an empty response. The model may be overloaded or unavailable.".to_string(),
        });
    }

    let mut res_msg = Message::assistant(full_text);
    res_msg.reasoning = reasoning;
    res_msg.tool_calls = tool_calls;
    res_msg.usage = usage;
    Ok(res_msg)
}

