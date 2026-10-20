use std::path::PathBuf;
use std::sync::Arc;
use futures_util::stream::{BoxStream, StreamExt};
use reqwest::header::{CONTENT_TYPE, USER_AGENT};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use harness_core::types::{
    CompletionOptions, Message, Role, StreamEvent, TokenUsage, ToolDefinition,
};
use harness_core::HarnessError;
use crate::auth::CredentialManager;

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
    let home = std::env::var("HOME").ok()?;
    let db_path = PathBuf::from(home).join(".local/share/kiro-cli/data.sqlite3");
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
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return Ok(()),
    };
    let db_path = PathBuf::from(&home).join(".local/share/kiro-cli/data.sqlite3");
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
    info!("Refreshing Kiro social access token via {}", REFRESH_ENDPOINT);
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

    let _ = update_kiro_token_in_db(&new_creds);

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

pub async fn sync_kiro_credential_if_needed(cred_manager: &CredentialManager) -> Result<(), HarnessError> {
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
                    let _ = cred_manager.update_credential_key(prov, existing_str, &serialized).await;
                }
            }
            Err(_) => {
                let _ = cred_manager
                    .add_credential(prov, "kiro-social", &serialized, true)
                    .await;
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
                .header("x-amz-target", "AmazonCodeWhispererService.ListAvailableModels")
                .header(USER_AGENT, KIRO_USER_AGENT)
                .json(&body)
                .send()
                .await
        }
    };

    let mut resp = send(&creds).await.map_err(|e| HarnessError::ProviderError {
        provider: "kiro".to_string(),
        message: format!("HTTP error listing models: {}", e),
    })?;

    if (resp.status().as_u16() == 401 || resp.status().as_u16() == 400 || resp.status().as_u16() == 403)
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

    let send = |c: &KiroCredentials, p: &serde_json::Value| {
        let client = http_client.clone();
        let token = c.access_token.clone();
        let body = p.clone();
        async move {
            client
                .post(RUNTIME_ENDPOINT)
                .header("Authorization", format!("Bearer {}", token))
                .header(CONTENT_TYPE, "application/x-amz-json-1.0")
                .header(
                    "x-amz-target",
                    "AmazonCodeWhispererStreamingService.GenerateAssistantResponse",
                )
                .header(USER_AGENT, KIRO_USER_AGENT)
                .json(&body)
                .send()
                .await
        }
    };

    let mut resp = send(&creds, &payload).await.map_err(|e| HarnessError::ProviderError {
        provider: "kiro".to_string(),
        message: format!("HTTP error: {}", e),
    })?;

    if (resp.status().as_u16() == 401 || resp.status().as_u16() == 400 || resp.status().as_u16() == 403)
        && creds.refresh_token.is_some()
    {
        if let Some(ref rt) = creds.refresh_token.clone() {
            if let Ok(new_creds) = refresh_kiro_token(http_client, rt).await {
                creds = new_creds;
                if let Some(cm) = cred_manager {
                    let serialized = json!({
                        "access_token": creds.access_token,
                        "refresh_token": creds.refresh_token,
                        "profile_arn": creds.profile_arn,
                    })
                    .to_string();
                    for prov in ["kiro", "kiro-cli"] {
                        let _ = cm.update_credential_key(prov, raw_secret, &serialized).await;
                    }
                }
                let retry_payload =
                    build_kiro_payload(messages, tools, options, model, &creds.profile_arn);
                if let Ok(retry_resp) = send(&creds, &retry_payload).await {
                    if retry_resp.status().is_success() {
                        resp = retry_resp;
                    }
                }
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

        while let Some(chunk_res) = byte_stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);
                    while buffer.len() >= 16 {
                        let total_len = match buffer[0..4].try_into() {
                            Ok(b) => u32::from_be_bytes(b) as usize,
                            Err(_) => break,
                        };
                        let headers_len = match buffer[4..8].try_into() {
                            Ok(b) => u32::from_be_bytes(b) as usize,
                            Err(_) => break,
                        };

                        if total_len < headers_len + 16 {
                            buffer.drain(..total_len.min(buffer.len()));
                            continue;
                        }

                        if buffer.len() < total_len {
                            break;
                        }

                        let payload_len = total_len - headers_len - 16;
                        let payload_start = 12 + headers_len;
                        let payload_end = payload_start + payload_len;

                        if payload_end <= buffer.len() {
                            let payload_bytes = &buffer[payload_start..payload_end];
                            if let Ok(data) =
                                serde_json::from_slice::<serde_json::Value>(payload_bytes)
                            {
                                if let Some(content) = data.get("content").and_then(|c| c.as_str()) {
                                    if !content.is_empty() {
                                        let _ = tx
                                            .send(Ok(StreamEvent::ContentDelta(content.to_string())))
                                            .await;
                                    }
                                }

                                if let Some(reasoning) = data
                                    .get("reasoningContent")
                                    .and_then(|r| r.get("text").and_then(|t| t.as_str()).or_else(|| r.as_str()))
                                {
                                    if !reasoning.is_empty() {
                                        let _ = tx
                                            .send(Ok(StreamEvent::ReasoningDelta(
                                                reasoning.to_string(),
                                            )))
                                            .await;
                                    }
                                }

                                if let Some(tool_uses) =
                                    data.get("toolUses").and_then(|t| t.as_array())
                                {
                                    for (idx, tu) in tool_uses.iter().enumerate() {
                                        let id = tu
                                            .get("toolUseId")
                                            .and_then(|s| s.as_str())
                                            .map(str::to_string);
                                        let name =
                                            tu.get("name").and_then(|s| s.as_str()).map(str::to_string);
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

                                if let Some(stop_reason) =
                                    data.get("stopReason").and_then(|s| s.as_str())
                                {
                                    let _ = tx
                                        .send(Ok(StreamEvent::Completed {
                                            finish_reason: Some(stop_reason.to_string()),
                                            usage: None,
                                        }))
                                        .await;
                                }
                            }
                        }

                        buffer.drain(..total_len);
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
