use futures_util::stream::{BoxStream, StreamExt};
use reqwest::header::{CONTENT_TYPE, USER_AGENT};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tracing::{info, warn};

use crate::auth::CredentialManager;
use harness_core::types::{
    CompletionOptions, Message, Role, StreamEvent, TokenUsage, ToolDefinition,
};
use harness_core::HarnessError;

pub const MANAGEMENT_ENDPOINT: &str = "https://management.us-east-1.kiro.dev/";
pub const RUNTIME_ENDPOINT: &str = "https://runtime.us-east-1.kiro.dev/";
pub const REFRESH_ENDPOINT: &str = "https://prod.us-east-1.auth.desktop.kiro.dev/refreshToken";
pub const KIRO_USER_AGENT: &str = "kiro-cli/2.16.2";

pub const KIRO_MODELS: &[&str] = &[
    "auto",
    "claude-sonnet-4.5",
    "claude-haiku-4.5",
    "deepseek-3.2",
    "minimax-m2.5",
    "minimax-m2.1",
    "glm-5",
    "qwen3-coder-next",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KiroCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub profile_arn: String,
}

pub fn extract_kiro_credentials_from_db() -> Option<KiroCredentials> {
    let home = harness_core::config::home_dir();
    let db_path = home.join(".local/share/kiro-cli/data.sqlite3");
    if !db_path.exists() {
        return None;
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()?;
    let mut stmt = conn
        .prepare("SELECT value FROM auth_kv WHERE key='kirocli:social:token' LIMIT 1")
        .ok()?;
    let mut rows = stmt.query([]).ok()?;
    if let Ok(Some(row)) = rows.next() {
        let val: String = row.get(0).ok()?;
        let json_val: serde_json::Value = serde_json::from_str(&val).ok()?;
        let access_token = json_val
            .get("access_token")
            .and_then(|t| t.as_str())?
            .to_string();
        let refresh_token = json_val
            .get("refresh_token")
            .and_then(|t| t.as_str())
            .map(str::to_string);
        let profile_arn = json_val
            .get("profile_arn")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();

        return Some(KiroCredentials {
            access_token,
            refresh_token,
            profile_arn,
        });
    }
    None
}

pub fn update_kiro_token_in_db(token: &KiroCredentials) -> Result<(), HarnessError> {
    let home = harness_core::config::home_dir();
    let db_path = home.join(".local/share/kiro-cli/data.sqlite3");
    if !db_path.exists() {
        return Ok(());
    }

    let conn = rusqlite::Connection::open(&db_path).map_err(|e| HarnessError::ProviderError {
        provider: "kiro".to_string(),
        message: format!("Failed to open kiro database for token update: {}", e),
    })?;

    let val = json!({
        "access_token": token.access_token,
        "refresh_token": token.refresh_token,
        "profile_arn": token.profile_arn,
        "provider": "google",
    });

    conn.execute(
        "UPDATE auth_kv SET value = ?1 WHERE key = 'kirocli:social:token'",
        [val.to_string()],
    )
    .map_err(|e| HarnessError::ProviderError {
        provider: "kiro".to_string(),
        message: format!("Failed to update kiro social token: {}", e),
    })?;

    Ok(())
}

pub async fn refresh_kiro_token(
    http_client: &reqwest::Client,
    refresh_token: &str,
) -> Result<KiroCredentials, HarnessError> {
    info!(
        "Refreshing Kiro social access token via {}",
        REFRESH_ENDPOINT
    );
    let payload = json!({ "refreshToken": refresh_token });

    let resp = http_client
        .post(REFRESH_ENDPOINT)
        .header(CONTENT_TYPE, "application/json")
        .header(USER_AGENT, KIRO_USER_AGENT)
        .json(&payload)
        .send()
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "kiro".to_string(),
            message: format!("Token refresh network error: {}", e),
        })?;

    if !resp.status().is_success() {
        let err_body = resp.text().await.unwrap_or_default();
        return Err(HarnessError::ProviderError {
            provider: "kiro".to_string(),
            message: format!("Token refresh failed: {}", err_body),
        });
    }

    let data: serde_json::Value = resp.json().await.map_err(|e| HarnessError::ProviderError {
        provider: "kiro".to_string(),
        message: format!("Token refresh response parse error: {}", e),
    })?;

    let access_token = data
        .get("accessToken")
        .and_then(|t| t.as_str())
        .ok_or_else(|| HarnessError::ProviderError {
            provider: "kiro".to_string(),
            message: "Missing accessToken in refresh response".to_string(),
        })?
        .to_string();

    let profile_arn = data
        .get("profileArn")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();

    let new_creds = KiroCredentials {
        access_token,
        refresh_token: Some(refresh_token.to_string()),
        profile_arn,
    };

    if let Err(error) = update_kiro_token_in_db(&new_creds) {
        warn!("could not persist the refreshed Kiro token: {error}");
    }

    Ok(new_creds)
}

pub fn resolve_kiro_credentials(secret_str: &str) -> KiroCredentials {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(secret_str) {
        let access_token = val
            .get("access_token")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        let refresh_token = val
            .get("refresh_token")
            .and_then(|s| s.as_str())
            .map(str::to_string);
        let profile_arn = val
            .get("profile_arn")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();

        if !access_token.is_empty() && !profile_arn.is_empty() {
            return KiroCredentials {
                access_token,
                refresh_token,
                profile_arn,
            };
        }
    }

    if let Some(mut creds) = extract_kiro_credentials_from_db() {
        if !secret_str.is_empty() && !secret_str.starts_with('{') {
            creds.access_token = secret_str.to_string();
        }
        return creds;
    }

    KiroCredentials {
        access_token: secret_str.to_string(),
        refresh_token: None,
        profile_arn: String::new(),
    }
}

pub async fn sync_kiro_credential_if_needed(
    cred_manager: &CredentialManager,
) -> Result<(), HarnessError> {
    let creds = match extract_kiro_credentials_from_db() {
        Some(c) => c,
        None => return Ok(()),
    };

    let serialized = json!({
        "access_token": creds.access_token,
        "refresh_token": creds.refresh_token,
        "profile_arn": creds.profile_arn,
    })
    .to_string();

    for prov in ["kiro", "kiro-cli"] {
        match cred_manager.acquire_credential(prov).await {
            Ok(existing) => {
                let existing_str = existing.expose_secret();
                if existing_str != &serialized {
                    if let Err(error) = cred_manager
                        .update_credential_key(prov, existing_str, &serialized)
                        .await
                    {
                        warn!("could not update the stored Kiro credential: {error}");
                    }
                }
            }
            Err(_) => {
                if let Err(error) = cred_manager
                    .add_credential(prov, "kiro-social", &serialized, true)
                    .await
                {
                    warn!("could not store the Kiro credential: {error}");
                }
            }
        }
    }
    Ok(())
}

pub async fn fetch_kiro_models(
    http_client: &reqwest::Client,
    raw_secret: &str,
) -> Result<Vec<String>, HarnessError> {
    let mut creds = resolve_kiro_credentials(raw_secret);

    let send = |c: &KiroCredentials| {
        let client = http_client.clone();
        let token = c.access_token.clone();
        let body = json!({
            "origin": "KIRO_CLI",
            "profileArn": c.profile_arn,
        });
        async move {
            client
                .post(MANAGEMENT_ENDPOINT)
                .header("Authorization", format!("Bearer {}", token))
                .header(CONTENT_TYPE, "application/x-amz-json-1.0")
                .header(
                    "x-amz-target",
                    "AmazonCodeWhispererService.ListAvailableModels",
                )
                .header(USER_AGENT, KIRO_USER_AGENT)
                .json(&body)
                .send()
                .await
        }
    };

    let mut resp = send(&creds)
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "kiro".to_string(),
            message: format!("HTTP error listing models: {}", e),
        })?;

    if (resp.status().as_u16() == 401
        || resp.status().as_u16() == 400
        || resp.status().as_u16() == 403)
        && creds.refresh_token.is_some()
    {
        if let Some(ref rt) = creds.refresh_token.clone() {
            if let Ok(new_creds) = refresh_kiro_token(http_client, rt).await {
                creds = new_creds;
                if let Ok(retry_resp) = send(&creds).await {
                    if retry_resp.status().is_success() {
                        resp = retry_resp;
                    }
                }
            }
        }
    }

    if resp.status().is_success() {
        if let Ok(data) = resp.json::<serde_json::Value>().await {
            if let Some(models) = data.get("models").and_then(|m| m.as_array()) {
                let mut names = Vec::new();
                for m in models {
                    if let Some(id) = m.get("modelId").and_then(|id| id.as_str()) {
                        names.push(id.to_string());
                    }
                }
                if !names.is_empty() {
                    return Ok(names);
                }
            }
        }
    }

    warn!("Failed to query dynamic Kiro models over HTTP, falling back to static model list");
    Ok(KIRO_MODELS.iter().map(|s| s.to_string()).collect())
}

pub fn build_kiro_payload(
    messages: &[Message],
    tools: &[ToolDefinition],
    _options: &CompletionOptions,
    model: &str,
    profile_arn: &str,
) -> serde_json::Value {
    let resolved_model = if model.is_empty() { "auto" } else { model };

    let mut system_instructions = Vec::new();
    let mut history_entries = Vec::new();
    let mut current_user_message = String::new();

    for (i, msg) in messages.iter().enumerate() {
        let is_last = i == messages.len() - 1;
        match msg.role {
            Role::System => {
                if let Some(ref c) = msg.content {
                    system_instructions.push(c.clone());
                }
            }
            Role::User => {
                if is_last {
                    current_user_message = msg.content.clone().unwrap_or_default();
                } else {
                    history_entries.push(json!({
                        "userInputMessage": {
                            "content": msg.content.clone().unwrap_or_default(),
                            "origin": "KIRO_CLI",
                            "modelId": resolved_model
                        }
                    }));
                }
            }
            Role::Assistant => {
                history_entries.push(json!({
                    "assistantResponseMessage": {
                        "content": msg.content.clone().unwrap_or_default()
                    }
                }));
            }
            Role::Tool => {
                let content = msg.content.clone().unwrap_or_default();
                history_entries.push(json!({
                    "userInputMessage": {
                        "content": format!("[Tool Result]: {}", content),
                        "origin": "KIRO_CLI",
                        "modelId": resolved_model
                    }
                }));
            }
        }
    }

    if current_user_message.is_empty() && !messages.is_empty() {
        if let Some(last_msg) = messages.last() {
            current_user_message = last_msg.content.clone().unwrap_or_default();
        }
    }

    let final_content = if !system_instructions.is_empty() {
        format!(
            "System Instructions:\n{}\n\n{}",
            system_instructions.join("\n\n"),
            current_user_message
        )
    } else {
        current_user_message
    };

    let mut user_input_msg = json!({
        "content": final_content,
        "origin": "KIRO_CLI",
        "modelId": resolved_model
    });

    if !tools.is_empty() {
        let tool_specs: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": {
                        "json": t.parameters
                    }
                })
            })
            .collect();
        user_input_msg["userInputMessageContext"] = json!({
            "tools": tool_specs
        });
    }

    json!({
        "conversationState": {
            "conversationId": uuid::Uuid::new_v4().to_string(),
            "chatTriggerType": "MANUAL",
            "currentMessage": {
                "userInputMessage": user_input_msg
            },
            "history": history_entries
        },
        "profileArn": profile_arn
    })
}

/// POST one `GenerateAssistantResponse` request.
async fn send_kiro_stream_request(
    http_client: &reqwest::Client,
    creds: &KiroCredentials,
    payload: &serde_json::Value,
) -> Result<reqwest::Response, reqwest::Error> {
    http_client
        .post(RUNTIME_ENDPOINT)
        .header("Authorization", format!("Bearer {}", creds.access_token))
        .header(CONTENT_TYPE, "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            "AmazonCodeWhispererStreamingService.GenerateAssistantResponse",
        )
        .header(USER_AGENT, KIRO_USER_AGENT)
        .json(payload)
        .send()
        .await
}

/// Refresh the credentials after a rejected request, persisting the new pair when a manager is
/// available. Returns whether a refresh actually happened.
async fn try_refresh_kiro_credentials(
    http_client: &reqwest::Client,
    creds: &mut KiroCredentials,
    cred_manager: Option<&Arc<CredentialManager>>,
    raw_secret: &str,
    resp: &reqwest::Response,
) -> bool {
    if !matches!(resp.status().as_u16(), 401 | 400 | 403) {
        return false;
    }
    let Some(rt) = creds.refresh_token.clone() else {
        return false;
    };
    let Ok(new_creds) = refresh_kiro_token(http_client, &rt).await else {
        return false;
    };
    *creds = new_creds;
    if let Some(cm) = cred_manager {
        let serialized = json!({
            "access_token": creds.access_token,
            "refresh_token": creds.refresh_token,
            "profile_arn": creds.profile_arn,
        })
        .to_string();
        for prov in ["kiro", "kiro-cli"] {
            if let Err(error) = cm
                .update_credential_key(prov, raw_secret, &serialized)
                .await
            {
                warn!("could not update the refreshed Kiro credential: {error}");
            }
        }
    }
    true
}

/// Pull one complete event frame out of the buffer, or `None` when more bytes are needed.
///
/// A frame too short to hold its own header is skipped; a payload that does not parse is still
/// consumed and reported as `Some(None)`.
fn next_kiro_frame(buffer: &mut Vec<u8>) -> Option<Option<serde_json::Value>> {
    loop {
        if buffer.len() < 16 {
            return None;
        }
        let total_len = u32::from_be_bytes(buffer[0..4].try_into().ok()?) as usize;
        let headers_len = u32::from_be_bytes(buffer[4..8].try_into().ok()?) as usize;

        if total_len < headers_len + 16 {
            buffer.drain(..total_len.min(buffer.len()));
            continue;
        }

        if buffer.len() < total_len {
            return None;
        }

        let payload_len = total_len - headers_len - 16;
        let payload_start = 12 + headers_len;
        let payload_end = payload_start + payload_len;

        let payload = if payload_end <= buffer.len() {
            serde_json::from_slice::<serde_json::Value>(&buffer[payload_start..payload_end]).ok()
        } else {
            None
        };

        buffer.drain(..total_len);
        return Some(payload);
    }
}

/// Emit every stream event one decoded Kiro frame carries.
async fn emit_kiro_frame(
    data: &serde_json::Value,
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    last_usage: &mut Option<TokenUsage>,
) {
    // A metadata frame carries the account's own counts. Recording them
    // keeps this provider's rows in the ledger comparable with every
    // other route's; without it the turn is written from a character
    // estimate and the cost column is a guess. Absent means the route
    // said nothing, and the caller keeps its estimate.
    if let Some(usage) = metadata_token_usage(data) {
        *last_usage = Some(usage);
    }

    if let Some(content) = data.get("content").and_then(|c| c.as_str()) {
        if !content.is_empty() {
            let _ = tx
                .send(Ok(StreamEvent::ContentDelta(content.to_string())))
                .await;
        }
    }

    if let Some(reasoning) = data.get("reasoningContent").and_then(|r| {
        r.get("text")
            .and_then(|t| t.as_str())
            .or_else(|| r.as_str())
    }) {
        if !reasoning.is_empty() {
            let _ = tx
                .send(Ok(StreamEvent::ReasoningDelta(reasoning.to_string())))
                .await;
        }
    }

    if let Some(tool_uses) = data.get("toolUses").and_then(|t| t.as_array()) {
        for (idx, tu) in tool_uses.iter().enumerate() {
            let id = tu
                .get("toolUseId")
                .and_then(|s| s.as_str())
                .map(str::to_string);
            let name = tu.get("name").and_then(|s| s.as_str()).map(str::to_string);
            let args = tu.get("input").map(|v| v.to_string());
            let _ = tx
                .send(Ok(StreamEvent::ToolCallDelta {
                    index: idx,
                    id,
                    name,
                    arguments_delta: args,
                    signature: None,
                }))
                .await;
        }
    }

    if let Some(stop_reason) = data.get("stopReason").and_then(|s| s.as_str()) {
        let _ = tx
            .send(Ok(StreamEvent::Completed {
                finish_reason: Some(stop_reason.to_string()),
                usage: last_usage.take(),
            }))
            .await;
    }
}

pub async fn stream_kiro_completion(
    http_client: &reqwest::Client,
    raw_secret: &str,
    cred_manager: Option<&Arc<CredentialManager>>,
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
    let mut creds = resolve_kiro_credentials(raw_secret);
    let payload = build_kiro_payload(messages, tools, options, model, &creds.profile_arn);

    let mut resp = send_kiro_stream_request(http_client, &creds, &payload)
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "kiro".to_string(),
            message: format!("HTTP error: {}", e),
        })?;

    if try_refresh_kiro_credentials(http_client, &mut creds, cred_manager, raw_secret, &resp).await
    {
        let retry_payload = build_kiro_payload(messages, tools, options, model, &creds.profile_arn);
        if let Ok(retry_resp) = send_kiro_stream_request(http_client, &creds, &retry_payload).await
        {
            if retry_resp.status().is_success() {
                resp = retry_resp;
            }
        }
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        return Err(HarnessError::ProviderError {
            provider: "kiro".to_string(),
            message: format!("Kiro API error (HTTP {}): {}", status, err_body),
        });
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, HarnessError>>(100);

    tokio::spawn(async move {
        let mut byte_stream = resp.bytes_stream();
        let mut buffer = Vec::new();
        // The account's own counts, carried by a metadata frame and attached to whichever
        // completion frame follows it.
        let mut last_usage: Option<TokenUsage> = None;

        while let Some(chunk_res) = byte_stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);
                    while let Some(data) = next_kiro_frame(&mut buffer) {
                        if let Some(data) = data {
                            emit_kiro_frame(&data, &tx, &mut last_usage).await;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(HarnessError::ProviderError {
                            provider: "kiro".to_string(),
                            message: format!("EventStream error: {}", e),
                        }))
                        .await;
                    break;
                }
            }
        }
    });

    let rx_stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
    Ok(rx_stream.boxed())
}

/// The counts a Kiro metadata frame reports, in any of the spellings this route is known to use.
///
/// `tokenUsage` nests the numbers; the camelCase names are what the wire carries and the
/// snake_case ones are accepted because an aggregator in front of the route re-serializes them.
/// Returns `None` when nothing countable is present — "said nothing" is not the same as zero, and
/// a fabricated zero row would deflate every average built on it.
fn metadata_token_usage(data: &serde_json::Value) -> Option<TokenUsage> {
    let usage = data.get("tokenUsage").or_else(|| data.get("token_usage"))?;

    let count = |camel: &str, snake: &str| -> Option<u32> {
        usage
            .get(camel)
            .or_else(|| usage.get(snake))
            .and_then(|value| value.as_u64())
            .map(|value| value as u32)
    };

    let cached_tokens = count("cacheReadInputTokens", "cache_read_input_tokens").unwrap_or(0);
    let uncached_input = count("uncachedInputTokens", "uncached_input_tokens");
    let output = count("outputTokens", "output_tokens");
    let total = count("totalTokens", "total_tokens");

    // `uncachedInputTokens` excludes the cache read, so the prompt the model saw is their sum —
    // the same relationship every other route's `prompt_tokens` describes. `totalTokens` is not
    // used to cap it: a total that disagrees with the parts is the route's inconsistency, and
    // silently trimming the input side to match would hide it behind a plausible number.
    let prompt_tokens = match (uncached_input, total, output) {
        (Some(uncached), _, _) => uncached.saturating_add(cached_tokens),
        // Only a total was named: the input side is what is left once the reply is taken out.
        (None, Some(total), Some(output)) => total.saturating_sub(output),
        (None, Some(total), None) => {
            if cached_tokens == 0 {
                return None;
            }
            total
        }
        (None, None, _) => {
            if cached_tokens == 0 {
                return None;
            }
            cached_tokens
        }
    };

    let completion_tokens =
        output.unwrap_or_else(|| total.unwrap_or(0).saturating_sub(prompt_tokens));

    Some(TokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens.saturating_add(completion_tokens),
        cached_tokens,
    })
}

pub async fn execute_kiro_completion(
    http_client: &reqwest::Client,
    raw_secret: &str,
    cred_manager: Option<&Arc<CredentialManager>>,
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
) -> Result<Message, HarnessError> {
    let mut stream = stream_kiro_completion(
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
            // A later completion frame does not always repeat the counts. Overwriting a real
            // reading with a silent `None` would erase it and hand the ledger an estimate
            // instead, so a frame that says nothing leaves what was already read alone.
            StreamEvent::Completed { usage, .. } if usage.is_some() => final_usage = usage,
            _ => {}
        }
    }

    let prompt_tokens = (messages
        .iter()
        .map(|m| m.content.as_deref().unwrap_or("").len())
        .sum::<usize>()
        / 4)
    .max(1) as u32;
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
        usage: final_usage.or(Some(TokenUsage::estimated(
            prompt_tokens,
            completion_tokens,
        ))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_metadata_frame_yields_the_counts_it_reported() {
        let usage = metadata_token_usage(&json!({
            "tokenUsage": {
                "uncachedInputTokens": 1_200,
                "cacheReadInputTokens": 800,
                "outputTokens": 300,
                "totalTokens": 2_300,
            }
        }))
        .expect("a frame naming the counts is a reading");

        // `uncachedInputTokens` excludes the cache read, so the prompt is their sum — the same
        // relationship `prompt_tokens` describes on every other route.
        assert_eq!(usage.prompt_tokens, 2_000);
        assert_eq!(usage.completion_tokens, 300);
        assert_eq!(usage.cached_tokens, 800);
    }

    /// A route that contradicts itself must not have the contradiction hidden: the parts are what
    /// the ledger records, and a total that disagrees with them is the route's problem to fix.
    #[test]
    fn a_total_that_disagrees_with_the_parts_does_not_trim_them() {
        let usage = metadata_token_usage(&json!({
            "tokenUsage": { "uncachedInputTokens": 1_200, "cacheReadInputTokens": 800, "totalTokens": 1_500 }
        }))
        .unwrap();

        assert_eq!(usage.prompt_tokens, 2_000);
    }

    #[test]
    fn the_snake_case_spelling_of_the_same_frame_parses_to_the_same_reading() {
        let camel = metadata_token_usage(&json!({
            "tokenUsage": { "uncachedInputTokens": 10, "cacheReadInputTokens": 5, "outputTokens": 2 }
        }))
        .unwrap();
        let snake = metadata_token_usage(&json!({
            "token_usage": { "uncached_input_tokens": 10, "cache_read_input_tokens": 5, "output_tokens": 2 }
        }))
        .unwrap();

        assert_eq!(camel, snake);
    }

    #[test]
    fn a_frame_that_names_only_a_total_still_gives_the_input_side() {
        let usage = metadata_token_usage(&json!({
            "tokenUsage": { "totalTokens": 1_000, "outputTokens": 400 }
        }))
        .unwrap();

        assert_eq!(usage.prompt_tokens, 600);
        assert_eq!(usage.completion_tokens, 400);
    }

    /// Silence is not zero. A frame carrying no counts must yield nothing so the caller keeps its
    /// own estimate, rather than writing a zero row that deflates every average built on it.
    #[test]
    fn a_frame_without_counts_is_not_a_reading() {
        assert!(metadata_token_usage(&json!({ "stopReason": "end_turn" })).is_none());
        assert!(metadata_token_usage(&json!({ "tokenUsage": {} })).is_none());
        assert!(metadata_token_usage(&json!({})).is_none());
        // A cache read on its own is still worth recording: it is a real number the route named.
        assert_eq!(
            metadata_token_usage(&json!({ "tokenUsage": { "cacheReadInputTokens": 64 } }))
                .unwrap()
                .cached_tokens,
            64
        );
    }
}
