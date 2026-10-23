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
pub const DEFAULT_USER_AGENT: &str = "antigravity/1.2.7";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntigravityCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub email: Option<String>,
    pub expiry: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityQuotaBucket {
    pub bucket_id: Option<String>,
    pub display_name: Option<String>,
    pub window: Option<String>,
    pub reset_time: Option<String>,
    pub remaining_fraction: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityQuotaGroup {
    pub display_name: Option<String>,
    #[serde(default)]
    pub buckets: Vec<AntigravityQuotaBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityModelQuotaInfo {
    pub remaining_fraction: Option<f64>,
    pub reset_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AntigravityQuotaSummary {
    #[serde(default)]
    pub groups: Vec<AntigravityQuotaGroup>,
    #[serde(default)]
    pub models_quota: std::collections::HashMap<String, AntigravityModelQuotaInfo>,
}

impl AntigravityQuotaSummary {
    pub fn format_quota_display(&self, model: &str) -> Option<String> {
        let lower = model.to_lowercase();
        let is_gemini =
            lower.contains("gemini") || lower.contains("flash") || lower.contains("pro");
        let is_claude_gpt = lower.contains("claude")
            || lower.contains("opus")
            || lower.contains("sonnet")
            || lower.contains("gpt");

        let resolved = resolve_antigravity_model(model, None);

        // Check model-specific quotaInfo from fetchAvailableModels first if Gemini
        if is_gemini {
            if let Some(q_info) = self.models_quota.get(&resolved).or_else(|| {
                self.models_quota
                    .iter()
                    .find(|(k, _)| {
                        k.to_lowercase().contains("flash") || k.to_lowercase().contains("gemini")
                    })
                    .map(|(_, v)| v)
            }) {
                let weekly_part = q_info.remaining_fraction.map(|frac| {
                    let pct_str = format!("{:.0}%", (frac.max(0.0).min(1.0) * 100.0).round());
                    let time_str = q_info
                        .reset_time
                        .as_ref()
                        .map(|t| format_reset_time_remaining(t))
                        .unwrap_or_default();
                    if time_str.is_empty() {
                        format!("w {}", pct_str)
                    } else {
                        format!("w {} {}", pct_str, time_str)
                    }
                });

                // Check if any group has 5h bucket for Gemini
                let hourly_part = self
                    .groups
                    .iter()
                    .find(|g| {
                        g.display_name
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains("gemini")
                    })
                    .and_then(|g| {
                        g.buckets.iter().find(|b| {
                            let w = b.window.as_deref().unwrap_or("").to_lowercase();
                            let b_id = b.bucket_id.as_deref().unwrap_or("").to_lowercase();
                            w == "5h" || b_id.contains("5h")
                        })
                    })
                    .map(|b| {
                        let pct_str = if let Some(frac) = b.remaining_fraction {
                            format!("{:.0}%", (frac.max(0.0).min(1.0) * 100.0).round())
                        } else {
                            "100%".to_string()
                        };
                        let time_str = b
                            .reset_time
                            .as_ref()
                            .map(|t| format_reset_time_remaining(t))
                            .unwrap_or_default();
                        if time_str.is_empty() {
                            format!("5h {}", pct_str)
                        } else {
                            format!("5h {} {}", pct_str, time_str)
                        }
                    })
                    .or_else(|| Some("5h 100%".to_string()));

                return match (hourly_part, weekly_part) {
                    (Some(h), Some(w)) => Some(format!("{} · {}", h, w)),
                    (Some(h), None) => Some(h),
                    (None, Some(w)) => Some(w),
                    (None, None) => None,
                };
            }
        }

        // Pick target group based on active model:
        // Gemini models match groups mentioning "gemini"; Claude/GPT models match groups mentioning "claude" or "gpt".
        let target_group = self
            .groups
            .iter()
            .find(|g| {
                let name = g.display_name.as_deref().unwrap_or("").to_lowercase();
                if is_gemini && name.contains("gemini") {
                    true
                } else if is_claude_gpt && (name.contains("claude") || name.contains("gpt")) {
                    true
                } else {
                    false
                }
            })
            .or_else(|| self.groups.first());

        let group = target_group?;

        let mut hourly_part = None;
        let mut weekly_part = None;

        for b in &group.buckets {
            let w = b.window.as_deref().unwrap_or("").to_lowercase();
            let b_id = b.bucket_id.as_deref().unwrap_or("").to_lowercase();

            let is_5h = w == "5h" || b_id.contains("5h");
            let is_weekly = w == "weekly" || w == "w" || b_id.contains("weekly");

            if !is_5h && !is_weekly {
                continue;
            }

            let pct_str = if let Some(frac) = b.remaining_fraction {
                format!("{:.0}%", (frac.max(0.0).min(1.0) * 100.0).round())
            } else {
                "100%".to_string()
            };

            let time_str = if let Some(ref r_time) = b.reset_time {
                format_reset_time_remaining(r_time)
            } else {
                String::new()
            };

            if is_5h && hourly_part.is_none() {
                let part = if time_str.is_empty() {
                    format!("5h {}", pct_str)
                } else {
                    format!("5h {} {}", pct_str, time_str)
                };
                hourly_part = Some(part);
            } else if is_weekly && weekly_part.is_none() {
                let part = if time_str.is_empty() {
                    format!("w {}", pct_str)
                } else {
                    format!("w {} {}", pct_str, time_str)
                };
                weekly_part = Some(part);
            }
        }

        match (hourly_part, weekly_part) {
            (Some(h), Some(w)) => Some(format!("{} · {}", h, w)),
            (Some(h), None) => Some(h),
            (None, Some(w)) => Some(w),
            (None, None) => None,
        }
    }
}

pub fn format_reset_time_remaining(reset_time_str: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(reset_time_str) {
        let now = chrono::Utc::now();
        let diff = dt.with_timezone(&chrono::Utc) - now;
        let total_secs = diff.num_seconds();
        if total_secs <= 0 {
            "(0m)".to_string()
        } else {
            let days = total_secs / 86400;
            let hours = (total_secs % 86400) / 3600;
            let minutes = (total_secs % 3600) / 60;
            if days > 0 {
                format!("({}d {}h)", days, hours)
            } else if hours > 0 {
                format!("({}h {}m)", hours, minutes)
            } else {
                format!("({}m)", minutes.max(1))
            }
        }
    } else {
        String::new()
    }
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
                        let refresh = tok
                            .get("refresh_token")
                            .and_then(|r| r.as_str())
                            .map(|s| s.to_string());
                        let expiry = tok
                            .get("expiry")
                            .and_then(|e| e.as_str())
                            .map(|s| s.to_string());
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
    let mut candidate_paths = vec![PathBuf::from(&home).join(".shiina").join("auth.json")];

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
                    if let Some(agy) = data.get("providers").and_then(|p| p.get("antigravity")) {
                        if let Some(access) = agy.get("access_token").and_then(|a| a.as_str()) {
                            let refresh = agy
                                .get("refresh_token")
                                .and_then(|r| r.as_str())
                                .map(|s| s.to_string());
                            let email = agy
                                .get("email")
                                .and_then(|e| e.as_str())
                                .map(|s| s.to_string());
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
    for var in &[
        "ANTIGRAVITY_ACCESS_TOKEN",
        "AGY_ACCESS_TOKEN",
        "GEMINI_ACCESS_TOKEN",
    ] {
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
    let clean: String = s
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            other => other,
        })
        .collect();
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

    let data: serde_json::Value =
        resp.json()
            .await
            .map_err(|e| HarnessError::AuthenticationFailed {
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
            let access = val
                .get("access_token")
                .and_then(|a| a.as_str())
                .unwrap_or(trimmed);
            let refresh = val
                .get("refresh_token")
                .and_then(|r| r.as_str())
                .map(|s| s.to_string());
            return (access.to_string(), refresh);
        }
    }
    (trimmed.to_string(), None)
}

pub const ANTIGRAVITY_MODELS: &[&str] = &[
    "gemini-3.8-flash",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    "gemini-3-flash-preview",
    "claude-sonnet-4-6",
    "claude-opus-4-6",
    "gpt-oss-120b",
    "gpt-oss-20b",
];

/// Resolves user-facing clean model names and reasoning effort settings to backend Code Assist model names.
pub fn resolve_antigravity_model(
    raw_model: &str,
    _reasoning_effort: Option<&ReasoningEffort>,
) -> String {
    let cleaned = raw_model.trim();
    let cleaned = cleaned.strip_prefix("antigravity/").unwrap_or(cleaned);
    let cleaned = cleaned.strip_prefix("agy/").unwrap_or(cleaned);
    let lower = cleaned.to_lowercase();

    if lower.contains("opus") {
        "claude-opus-4-6-thinking".to_string()
    } else if lower.contains("sonnet") {
        "claude-sonnet-4-6".to_string()
    } else if lower.contains("gpt-oss") || lower.contains("oss") {
        "gpt-oss-120b-medium".to_string()
    } else if lower.contains("pro") {
        "gemini-2.5-pro".to_string()
    } else if lower.contains("gemini") || lower.contains("flash") {
        "gemini-3.8-flash-tiered".to_string()
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
        let prompt_tokens = meta
            .get("promptTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let completion_tokens = meta
            .get("candidatesTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
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
                let tool_name = messages
                    .iter()
                    .filter_map(|m| {
                        m.tool_calls
                            .iter()
                            .find(|tc| tc.id == call_id)
                            .map(|tc| tc.name.as_str())
                    })
                    .next();

                let Some(tool_name) = tool_name else {
                    // Do not emit an orphaned functionResponse without a matching functionCall
                    continue;
                };

                let resp_part = json!({
                    "functionResponse": {
                        "id": call_id,
                        "name": tool_name,
                        "response": { "result": result_content }
                    }
                });

                // Merge into previous user turn if it already contains functionResponse
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

    let mut req_obj = json!({ "contents": merged_contents });

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
            ReasoningEffort::Minimal => {
                gen_config["thinkingConfig"] = json!({ "thinkingBudget": 1024 })
            }
            ReasoningEffort::Low => {
                gen_config["thinkingConfig"] = json!({ "thinkingBudget": 2048 })
            }
            ReasoningEffort::Medium => {
                gen_config["thinkingConfig"] = json!({ "thinkingBudget": 8192 })
            }
            ReasoningEffort::High => {
                gen_config["thinkingConfig"] = json!({ "thinkingBudget": 16384 })
            }
            ReasoningEffort::Xhigh | ReasoningEffort::Max => {
                gen_config["thinkingConfig"] = json!({ "thinkingBudget": 32768 })
            }
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
        "model": resolve_antigravity_model(model, options.reasoning_effort.as_ref()),
        "request": req_obj
    })
}

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

/// Retrieve Antigravity user quota summary (hourly and weekly buckets).
pub async fn retrieve_antigravity_quota(
    http_client: &reqwest::Client,
    raw_secret: &str,
    cred_manager: Option<&std::sync::Arc<crate::auth::CredentialManager>>,
) -> Result<AntigravityQuotaSummary, HarnessError> {
    let (mut bearer_token, refresh_tok_opt) = extract_bearer_token(raw_secret);

    let send = |endpoint: &'static str, token: String| {
        let client = http_client.clone();
        async move {
            client
                .post(endpoint)
                .header(AUTHORIZATION, format!("Bearer {}", token))
                .header(USER_AGENT, DEFAULT_USER_AGENT)
                .header(CONTENT_TYPE, "application/json")
                .json(&json!({}))
                .send()
                .await
        }
    };

    let endpoints = [USER_QUOTA_ENDPOINT, FALLBACK_USER_QUOTA_ENDPOINT];
    let mut resp_opt = None;

    for ep in endpoints {
        match send(ep, bearer_token.clone()).await {
            Ok(r) => {
                if r.status().as_u16() == 401 {
                    if let Some(ref refresh_tok) = refresh_tok_opt {
                        if let Ok(new_token) =
                            refresh_antigravity_token(http_client, refresh_tok).await
                        {
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
                            if let Ok(r2) = send(ep, bearer_token.clone()).await {
                                if r2.status().is_success() {
                                    resp_opt = Some(r2);
                                    break;
                                }
                            }
                        }
                    }
                } else if r.status().is_success() {
                    resp_opt = Some(r);
                    break;
                }
            }
            Err(_) => continue,
        }
    }

    let mut summary = if let Some(resp) = resp_opt {
        resp.json::<AntigravityQuotaSummary>()
            .await
            .unwrap_or_default()
    } else {
        AntigravityQuotaSummary::default()
    };

    // Also query fetchAvailableModels to obtain exact per-model quotaInfo
    let model_endpoints = [MODELS_ENDPOINT, FALLBACK_MODELS_ENDPOINT];
    for ep in model_endpoints {
        if let Ok(resp) = send(ep, bearer_token.clone()).await {
            if resp.status().is_success() {
                if let Ok(val) = resp.json::<serde_json::Value>().await {
                    if let Some(models_map) = val.get("models").and_then(|m| m.as_object()) {
                        for (model_name, m_val) in models_map {
                            if let Some(q_val) = m_val.get("quotaInfo") {
                                let frac = q_val.get("remainingFraction").and_then(|f| f.as_f64());
                                let r_time = q_val
                                    .get("resetTime")
                                    .and_then(|t| t.as_str())
                                    .map(|s| s.to_string());
                                summary.models_quota.insert(
                                    model_name.clone(),
                                    AntigravityModelQuotaInfo {
                                        remaining_fraction: frac,
                                        reset_time: r_time,
                                    },
                                );
                            }
                        }
                    }
                    break;
                }
            }
        }
    }

    Ok(summary)
}

/// Strictly resolve the authentic user email belonging to the given Antigravity credential.
/// Checks the JWT id_token first, then queries Google tokeninfo endpoint, ensuring the
/// active display matches the real account and does not show an old or mismatched label.
pub async fn resolve_antigravity_email_strict(
    http_client: &reqwest::Client,
    raw_secret: &str,
) -> Option<String> {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(raw_secret) {
        // 1. Check if email is embedded directly in jwt id_token
        if let Some(email) = extract_email_from_jwt(&val) {
            return Some(email);
        }
        // 2. Query Google tokeninfo with access_token
        let tok = val.get("token").unwrap_or(&val);
        if let Some(access) = tok.get("access_token").and_then(|a| a.as_str()) {
            let url = format!(
                "https://oauth2.googleapis.com/tokeninfo?access_token={}",
                access
            );
            if let Ok(resp) = http_client
                .get(&url)
                .timeout(std::time::Duration::from_secs(3))
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(info) = resp.json::<serde_json::Value>().await {
                        if let Some(email) = info.get("email").and_then(|e| e.as_str()) {
                            return Some(email.to_string());
                        }
                    }
                }
            }
        }
        // 3. Email property on JSON object
        if let Some(email) = val.get("email").and_then(|e| e.as_str()) {
            return Some(email.to_string());
        }
    }
    None
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
                                parse_antigravity_chunk(&parsed)
                                    .into_iter()
                                    .map(Ok)
                                    .collect()
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
        return Err(HarnessError::ProviderError {
            provider: "antigravity".to_string(),
            message:
                "Model returned an empty response. The model may be overloaded or unavailable."
                    .to_string(),
        });
    }

    let mut res_msg = Message::assistant(full_text);
    res_msg.reasoning = reasoning;
    res_msg.tool_calls = tool_calls;
    res_msg.usage = usage;
    Ok(res_msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_antigravity_model_mappings() {
        assert_eq!(
            resolve_antigravity_model("gemini-3.8-flash", Some(&ReasoningEffort::High)),
            "gemini-3.8-flash-tiered"
        );
        assert_eq!(
            resolve_antigravity_model("gemini-3.8-flash", None),
            "gemini-3.8-flash-tiered"
        );
        assert_eq!(
            resolve_antigravity_model("antigravity/gemini-3.8-flash", Some(&ReasoningEffort::Low)),
            "gemini-3.8-flash-tiered"
        );
        assert_eq!(
            resolve_antigravity_model("gpt-oss-120b", None),
            "gpt-oss-120b-medium"
        );
        assert_eq!(
            resolve_antigravity_model("claude-sonnet-4-6", None),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            resolve_antigravity_model("claude-opus-4-6", None),
            "claude-opus-4-6-thinking"
        );
    }

    #[test]
    fn test_build_antigravity_payload_merges_tool_responses_and_sets_thought_signature() {
        let messages = vec![
            Message::user("run two tools"),
            Message {
                role: Role::Assistant,
                content: None,
                reasoning: None,
                tool_calls: vec![
                    ToolCall {
                        id: "c1".to_string(),
                        name: "read_file".to_string(),
                        arguments: json!({ "path": "test.txt" }),
                        signature: Some("sig-for-read".to_string()),
                    },
                    ToolCall {
                        id: "c2".to_string(),
                        name: "terminal".to_string(),
                        arguments: json!({ "command": "cargo test" }),
                        signature: Some("sig-for-terminal".to_string()),
                    },
                ],
                tool_result: None,
                usage: None,
            },
            Message::tool(harness_core::ToolResult::success("c1", "content")),
            Message::tool(harness_core::ToolResult::success("c2", "test ok")),
        ];

        let payload = build_antigravity_payload(
            &messages,
            &[],
            &CompletionOptions::default(),
            "gemini-3.8-flash",
        );

        let contents = payload["request"]["contents"]
            .as_array()
            .expect("contents array");
        // Must be exactly 3 turns: user, model, user (with both function responses merged)
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["role"], "user");

        // Model turn must carry the model's own signature on each functionCall — the value the
        // provider issued, replayed verbatim.
        let model_parts = contents[1]["parts"].as_array().expect("model parts");
        assert_eq!(model_parts.len(), 2);
        assert_eq!(model_parts[0]["thoughtSignature"], json!("sig-for-read"));
        assert_eq!(
            model_parts[1]["thoughtSignature"],
            json!("sig-for-terminal")
        );

        // User turn must carry both functionResponses in the single turn
        let user_parts = contents[2]["parts"].as_array().expect("user parts");
        assert_eq!(user_parts.len(), 2);
        assert_eq!(user_parts[0]["functionResponse"]["name"], "read_file");
        assert_eq!(user_parts[1]["functionResponse"]["name"], "terminal");
    }

    /// A call recorded without a signature (another provider wrote the rows, or the session
    /// predates the field) is replayed with Google's documented dummy on a model that validates
    /// signatures: omitting the field is what 400s the request, and the dummy makes the validator
    /// skip the check instead.
    #[test]
    fn a_signatureless_call_carries_the_documented_dummy_on_a_strict_model() {
        let messages = signatureless_call_conversation();

        let payload = build_antigravity_payload(
            &messages,
            &[],
            &CompletionOptions::default(),
            "gemini-3.8-flash",
        );
        let contents = payload["request"]["contents"].as_array().expect("contents");
        let model_parts = contents[1]["parts"].as_array().expect("model parts");
        assert_eq!(
            model_parts[0]["thoughtSignature"],
            json!("skip_thought_signature_validator"),
            "a strict model rejects a function call replayed without a signature"
        );
    }

    /// A model that does not validate signatures is sent no `thoughtSignature` at all: the dummy
    /// is a documented last resort for strict validation, and Google notes it degrades model
    /// performance, so it must not be volunteered to a model that never asks for one.
    #[test]
    fn a_signatureless_call_sends_no_field_to_a_non_validating_model() {
        let messages = signatureless_call_conversation();

        let payload = build_antigravity_payload(
            &messages,
            &[],
            &CompletionOptions::default(),
            "gemini-2.5-flash",
        );
        let contents = payload["request"]["contents"].as_array().expect("contents");
        let model_parts = contents[1]["parts"].as_array().expect("model parts");
        assert!(
            model_parts[0].get("thoughtSignature").is_none(),
            "no signature concept for this model: {:?}",
            model_parts[0]
        );
    }

    /// The other side of the same rule: a non-Gemini model on Code Assist (Claude, GPT) has no
    /// signature concept either, even though the endpoint is Google's.
    #[test]
    fn a_signatureless_call_sends_no_field_to_a_non_gemini_model() {
        let messages = signatureless_call_conversation();

        let payload = build_antigravity_payload(
            &messages,
            &[],
            &CompletionOptions::default(),
            "claude-sonnet-4-6",
        );
        let contents = payload["request"]["contents"].as_array().expect("contents");
        let model_parts = contents[1]["parts"].as_array().expect("model parts");
        assert!(model_parts[0].get("thoughtSignature").is_none());
    }

    /// One user turn, one model turn that called a tool without a signature, and the result.
    fn signatureless_call_conversation() -> Vec<Message> {
        vec![
            Message::user("run one tool"),
            Message {
                role: Role::Assistant,
                content: None,
                reasoning: None,
                tool_calls: vec![ToolCall {
                    id: "c1".to_string(),
                    name: "terminal".to_string(),
                    arguments: json!({ "command": "ls" }),
                    signature: None,
                }],
                tool_result: None,
                usage: None,
            },
            Message::tool(harness_core::ToolResult::success("c1", "ok")),
        ]
    }

    /// A reasoning-bearing assistant turn replays its thinking as a thought part before the
    /// functionCall, so the replayed history says what the model actually produced.
    #[test]
    fn a_reasoning_turn_replays_a_thought_part_before_its_call() {
        let messages = vec![
            Message::user("run one tool"),
            Message {
                role: Role::Assistant,
                content: None,
                reasoning: Some("I should list the files first.".to_string()),
                tool_calls: vec![ToolCall {
                    id: "c1".to_string(),
                    name: "list_files".to_string(),
                    arguments: json!({ "path": "." }),
                    signature: Some("sig-1".to_string()),
                }],
                tool_result: None,
                usage: None,
            },
            Message::tool(harness_core::ToolResult::success("c1", "a\nb")),
        ];

        let payload = build_antigravity_payload(
            &messages,
            &[],
            &CompletionOptions::default(),
            "gemini-3.8-flash",
        );
        let contents = payload["request"]["contents"].as_array().expect("contents");
        let model_parts = contents[1]["parts"].as_array().expect("model parts");
        assert_eq!(model_parts.len(), 2);
        assert_eq!(model_parts[0]["thought"], json!(true));
        assert_eq!(
            model_parts[0]["text"],
            json!("I should list the files first.")
        );
        assert_eq!(model_parts[1]["thoughtSignature"], json!("sig-1"));
    }

    #[test]
    fn format_quota_display_separates_gemini_and_claude_groups() {
        let mut models_quota = std::collections::HashMap::new();
        models_quota.insert(
            "gemini-3.8-flash-tiered".to_string(),
            AntigravityModelQuotaInfo {
                remaining_fraction: Some(0.0628),
                reset_time: None,
            },
        );

        let summary = AntigravityQuotaSummary {
            groups: vec![AntigravityQuotaGroup {
                display_name: Some("Claude and GPT models".to_string()),
                buckets: vec![
                    AntigravityQuotaBucket {
                        bucket_id: Some("3p-5h".to_string()),
                        display_name: Some("Five Hour Limit Remaining".to_string()),
                        window: Some("5h".to_string()),
                        reset_time: None,
                        remaining_fraction: Some(1.0),
                    },
                    AntigravityQuotaBucket {
                        bucket_id: Some("3p-weekly".to_string()),
                        display_name: Some("Weekly Limit Remaining".to_string()),
                        window: Some("weekly".to_string()),
                        reset_time: None,
                        remaining_fraction: Some(0.46),
                    },
                ],
            }],
            models_quota,
        };

        // When active model is gemini, it must select the 6% quotaInfo from models_quota
        let gemini_fmt = summary
            .format_quota_display("gemini-3.8-flash")
            .expect("gemini quota");
        assert_eq!(gemini_fmt, "5h 100% · w 6%");

        // When active model is claude, it must select the Claude and GPT models group (46%)
        let claude_fmt = summary
            .format_quota_display("claude-sonnet-4-6")
            .expect("claude quota");
        assert_eq!(claude_fmt, "5h 100% · w 46%");
    }
}
