use std::sync::Arc;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use tracing::{debug, warn};

use harness_core::{
    config::ProviderOverride, CompletionOptions, HarnessError, Message, ModelProvider, Role,
    StreamEvent, TokenUsage, ToolCall, ToolDefinition,
};
use crate::auth::CredentialManager;
use crate::catalog::{resolve_descriptor, AuthHeaderFormat};
use crate::discovery::ModelDiscovery;

/// Extract the stream events carried by one OpenAI-compatible SSE chunk. A single chunk can
/// hold reasoning, visible text and a tool-call fragment at the same time, so every field is
/// examined instead of returning on the first hit.
fn delta_events(parsed: &serde_json::Value) -> Vec<Result<StreamEvent, HarnessError>> {
    use StreamEvent as E;

    let mut events = Vec::new();

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
            return vec![Err(HarnessError::RateLimited {
                provider: "model".to_string(),
                retry_after_secs: Some(10),
            })];
        } else if code == 503 || msg.to_lowercase().contains("overloaded") {
            return vec![Err(HarnessError::ProviderError {
                provider: "model".to_string(),
                message: format!("Model is overloaded (503): {}", msg),
            })];
        } else {
            return vec![Err(HarnessError::ProviderError {
                provider: "model".to_string(),
                message: msg,
            })];
        }
    }

    if let Some(choices) = parsed.get("choices").and_then(|c| c.as_array()) {
        for choice in choices {
            let Some(delta) = choice.get("delta") else {
                continue;
            };

            // `reasoning_content` is the NVIDIA/DeepSeek/wider-compat spelling; `reasoning` is
            // what some gateways send instead.
            for key in ["reasoning_content", "reasoning"] {
                if let Some(text) = delta.get(key).and_then(|r| r.as_str()) {
                    if !text.is_empty() {
                        events.push(Ok(E::ReasoningDelta(text.to_string())));
                    }
                }
            }

            if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
                if !text.is_empty() {
                    events.push(Ok(E::ContentDelta(text.to_string())));
                }
            }

            if let Some(calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                for (position, call) in calls.iter().enumerate() {
                    let index = call
                        .get("index")
                        .and_then(|i| i.as_u64())
                        .map(|i| i as usize)
                        .unwrap_or(position);
                    events.push(Ok(E::ToolCallDelta {
                        index,
                        id: call.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()),
                        name: call
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string()),
                        arguments_delta: call
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .map(|s| s.to_string()),
                    }));
                }
            }

            if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                events.push(Ok(E::Completed {
                    finish_reason: Some(reason.to_string()),
                    usage: None,
                }));
            }
        }
    }

    if let Some(usage) = parsed.get("usage").filter(|u| !u.is_null()) {
        events.push(Ok(E::Completed {
            finish_reason: None,
            usage: Some(TokenUsage {
                prompt_tokens: usage.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as u32,
                completion_tokens: usage.get("completion_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as u32,
                total_tokens: usage.get("total_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as u32,
            }),
        }));
    }

    events
}

/// A `reqwest` client for one provider, with the per-provider timeout the config resolved
/// (`providers.<id>.timeout_secs`, default `catalog::DEFAULT_TIMEOUT_SECS`).
pub(crate) fn build_http_client(timeout_secs: u64) -> Result<reqwest::Client, HarnessError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|error| {
            HarnessError::ConfigurationError(format!("failed to build the HTTP client: {}", error))
        })
}

pub struct GenericProviderClient {
    provider_id: String,
    base_url: String,
    auth_format: AuthHeaderFormat,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    http_client: reqwest::Client,
    default_model: String,
}

impl GenericProviderClient {
    /// `overrides` is the resolved `providers.<id>.*` config; the catalog supplies whatever the
    /// user left unset. An unknown provider id is an error rather than a fallback to another
    /// vendor's endpoint — see `catalog::resolve_descriptor`.
    pub fn new(
        provider_id: impl Into<String>,
        default_model: impl Into<String>,
        cred_manager: Arc<CredentialManager>,
        discovery: Arc<ModelDiscovery>,
        overrides: Option<&ProviderOverride>,
    ) -> Result<Self, HarnessError> {
        let p_id = provider_id.into();
        let descriptor = resolve_descriptor(&p_id, overrides)?;

        Ok(Self {
            provider_id: p_id,
            base_url: descriptor.default_base_url,
            auth_format: descriptor.auth_header_format,
            cred_manager,
            discovery,
            http_client: build_http_client(descriptor.timeout_secs)?,
            default_model: default_model.into(),
        })
    }

    /// Builds the auth headers for one request and returns the secret that was used, so a failing
    /// request can tell the [`CredentialManager`] *which* credential to retire.
    async fn build_auth_headers(&self) -> Result<(HeaderMap, Option<String>, SecretString), HarnessError> {
        let secret = self.cred_manager.acquire_credential(&self.provider_id).await?;
        let api_key = secret.expose_secret().to_string();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let mut query_param = None;

        match self.auth_format.clone() {
            AuthHeaderFormat::Bearer => {
                let token = if self.provider_id == "antigravity" || self.provider_id == "agy" {
                    crate::antigravity::extract_bearer_token(&api_key).0
                } else {
                    api_key
                };
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", token))
                        .map_err(|e| HarnessError::ConfigurationError(e.to_string()))?,
                );
            }
            AuthHeaderFormat::AnthropicApiKey => {
                headers.insert(
                    "x-api-key",
                    HeaderValue::from_str(&api_key)
                        .map_err(|e| HarnessError::ConfigurationError(e.to_string()))?,
                );
                headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            }
            AuthHeaderFormat::QueryParam(param) => {
                query_param = Some(format!("{}={}", param, api_key));
            }
            AuthHeaderFormat::None => {}
        }

        Ok((headers, query_param, secret))
    }

    /// Report the credential that just failed so the manager retires it and the next call fails
    /// over to another key. Only ever logged by label/provider — never the key itself.
    async fn retire_failed_credential(&self, secret: &SecretString) {
        let retired = self
            .cred_manager
            .mark_credential_failed(&self.provider_id, secret.expose_secret())
            .await;
        if retired {
            warn!(
                provider = %self.provider_id,
                "Provider rejected the selected credential; it is retired for this process"
            );
        } else {
            debug!(
                provider = %self.provider_id,
                "Provider rejected the selected credential, but no other key is available to fail over to"
            );
        }
    }

    fn serialize_messages(&self, messages: &[Message]) -> serde_json::Value {
        let mut list = Vec::new();
        for m in messages {
            let role_str = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };

            let mut msg_obj = json!({
                "role": role_str,
            });

            if let Some(ref text) = m.content {
                msg_obj["content"] = json!(text);
            }

            if !m.tool_calls.is_empty() {
                let tc_json: Vec<_> = m
                    .tool_calls
                    .iter()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments.to_string(),
                            }
                        })
                    })
                    .collect();
                msg_obj["tool_calls"] = json!(tc_json);
            }

            if let Some(ref res) = m.tool_result {
                msg_obj["tool_call_id"] = json!(res.call_id);
                msg_obj["content"] = json!(res.content);
            }

            list.push(msg_obj);
        }
        json!(list)
    }

    fn serialize_tools(&self, tools: &[ToolDefinition]) -> serde_json::Value {
        let list: Vec<_> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();
        json!(list)
    }
}

#[async_trait]
impl ModelProvider for GenericProviderClient {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        self.discovery
            .get_models(&self.provider_id, &self.cred_manager, false)
            .await
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let (headers, query_param, secret) = self.build_auth_headers().await?;
        let mut url = format!("{}/chat/completions", self.base_url);
        if let Some(q) = query_param {
            url = format!("{}?{}", url, q);
        }

        let model = options.model.clone().unwrap_or_else(|| self.default_model.clone());

        if self.provider_id == "antigravity" || self.provider_id == "agy" {
            return crate::antigravity::execute_antigravity_completion(
                &self.http_client,
                secret.expose_secret(),
                Some(&self.cred_manager),
                &model,
                messages,
                tools,
                options,
            )
            .await;
        }

        let mut body = json!({
            "model": model,
            "messages": self.serialize_messages(messages),
        });

        if !tools.is_empty() {
            body["tools"] = self.serialize_tools(tools);
        }
        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }
        if let Some(max_tok) = options.max_tokens {
            body["max_tokens"] = json!(max_tok);
        }
        if let Some(effort) = options.reasoning_effort {
            body["reasoning_effort"] = json!(effort.as_str());
        }

        let resp = self
            .http_client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| HarnessError::ProviderError {
                provider: self.provider_id.clone(),
                message: format!("HTTP error: {}", e),
            })?;

        if resp.status().as_u16() == 429 {
            self.retire_failed_credential(&secret).await;
            return Err(HarnessError::RateLimited {
                provider: self.provider_id.clone(),
                retry_after_secs: Some(30),
            });
        }

        if !resp.status().is_success() {
            let status = resp.status();
            // 401/403: the key itself is dead (revoked, quota-cancelled account), so retire it and
            // let the next turn fail over instead of replaying it forever.
            if matches!(status.as_u16(), 401 | 403) {
                self.retire_failed_credential(&secret).await;
            }
            let body_text = resp.text().await.unwrap_or_default();
            return Err(HarnessError::ProviderError {
                provider: self.provider_id.clone(),
                message: format!("HTTP {}: {}", status, body_text),
            });
        }

        let resp_json: serde_json::Value = resp.json().await.map_err(|e| HarnessError::ProviderError {
            provider: self.provider_id.clone(),
            message: format!("Failed to parse response JSON: {}", e),
        })?;

        let choice = resp_json
            .get("choices")
            .and_then(|c| c.get(0))
            .ok_or_else(|| HarnessError::ProviderError {
                provider: self.provider_id.clone(),
                message: "No choices returned by LLM".to_string(),
            })?;

        let msg_val = choice.get("message").ok_or_else(|| HarnessError::ProviderError {
            provider: self.provider_id.clone(),
            message: "Choice has no message".to_string(),
        })?;

        let mut reasoning = msg_val
            .get("reasoning_content")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        let mut content = msg_val.get("content").and_then(|c| c.as_str()).map(|s| s.to_string());

        if reasoning.is_none() {
            if let Some(c) = &content {
                if let Some(start) = c.find("<think>") {
                    if let Some(end) = c.find("</think>") {
                        let think_text = c[start + 7..end].trim().to_string();
                        let rest = format!("{}{}", &c[..start], &c[end + 8..]).trim().to_string();
                        if !think_text.is_empty() {
                            reasoning = Some(think_text);
                        }
                        content = if rest.is_empty() { None } else { Some(rest) };
                    }
                }
            }
        }

        let mut tool_calls = Vec::new();
        if let Some(tc_array) = msg_val.get("tool_calls").and_then(|tc| tc.as_array()) {
            for tc in tc_array {
                let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                let func = tc.get("function").unwrap_or(&serde_json::Value::Null);
                let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                let args_raw = func.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                let args: serde_json::Value = serde_json::from_str(args_raw).unwrap_or(json!({}));
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments: args,
                });
            }
        }

        // The response's own `usage` object: what the provider billed for this request. The session
        // store persists it so compaction compares its threshold against a real count instead of a
        // character guess (`harness_core::session::estimate_tokens`).
        let usage = resp_json.get("usage").map(|usage| TokenUsage {
            prompt_tokens: usage
                .get("prompt_tokens")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as u32,
            completion_tokens: usage
                .get("completion_tokens")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as u32,
            total_tokens: usage
                .get("total_tokens")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as u32,
        });

        if content.as_deref().unwrap_or("").trim().is_empty() && tool_calls.is_empty() {
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
                provider: self.provider_id.clone(),
                message: "Model returned an empty response. The model may be overloaded or unavailable.".to_string(),
            });
        }

        Ok(Message {
            role: Role::Assistant,
            content,
            reasoning,
            tool_calls,
            tool_result: None,
            usage,
        })
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        let model = options.model.clone().unwrap_or_else(|| self.default_model.clone());

        // Antigravity speaks the Code Assist protocol, not the OpenAI one, so it has its own
        // SSE reader rather than the `/chat/completions` body below.
        if self.provider_id == "antigravity" || self.provider_id == "agy" {
            let (_, _, secret) = self.build_auth_headers().await?;
            return crate::antigravity::stream_antigravity_completion(
                &self.http_client,
                secret.expose_secret(),
                Some(&self.cred_manager),
                &model,
                messages,
                tools,
                options,
            )
            .await;
        }

        let (headers, query_param, _secret) = self.build_auth_headers().await?;
        let mut url = format!("{}/chat/completions", self.base_url);
        if let Some(q) = query_param {
            url = format!("{}?{}", url, q);
        }

        let mut body = json!({
            "model": model,
            "messages": self.serialize_messages(messages),
            "stream": true,
            "stream_options": { "include_usage": true }
        });

        if !tools.is_empty() {
            body["tools"] = self.serialize_tools(tools);
        }
        if let Some(effort) = options.reasoning_effort {
            body["reasoning_effort"] = json!(effort.as_str());
        }

        let resp = self
            .http_client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| HarnessError::ProviderError {
                provider: self.provider_id.clone(),
                message: format!("HTTP error: {}", e),
            })?;

        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(HarnessError::RateLimited {
                provider: self.provider_id.clone(),
                retry_after_secs: Some(10),
            });
        }
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            let is_overloaded = status.as_u16() == 503 || body_text.to_lowercase().contains("overloaded");
            return Err(HarnessError::ProviderError {
                provider: self.provider_id.clone(),
                message: if is_overloaded {
                    format!("Model is overloaded (HTTP {}): {}", status, body_text)
                } else {
                    format!("HTTP {}: {}", status, body_text)
                },
            });
        }

        let event_source = resp.bytes_stream().eventsource();
        let provider_id = self.provider_id.clone();

        // Each SSE chunk can carry reasoning, content and tool-call fragments at once, so the
        // chunk is expanded into every event it holds rather than only the first match.
        let mapped = event_source.flat_map(move |event_result| {
            let events: Vec<Result<StreamEvent, HarnessError>> = match event_result {
                Ok(event) => {
                    if event.data == "[DONE]" {
                        vec![Ok(StreamEvent::Completed {
                            finish_reason: Some("stop".to_string()),
                            usage: None,
                        })]
                    } else {
                        match serde_json::from_str::<serde_json::Value>(&event.data) {
                            Ok(parsed) => delta_events(&parsed),
                            Err(e) => vec![Err(HarnessError::SerializationError(format!(
                                "Failed to parse SSE JSON: {}",
                                e
                            )))],
                        }
                    }
                }
                Err(e) => vec![Err(HarnessError::ProviderError {
                    provider: provider_id.clone(),
                    message: format!("SSE stream error: {}", e),
                })],
            };
            futures_util::stream::iter(events)
        });

        Ok(Box::pin(mapped))
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        let (headers, query_param, _secret) = self.build_auth_headers().await?;
        let mut url = format!("{}/embeddings", self.base_url);
        if let Some(q) = query_param {
            url = format!("{}?{}", url, q);
        }

        let body = json!({
            "model": "text-embedding-3-small",
            "input": texts,
        });

        let resp = self
            .http_client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| HarnessError::ProviderError {
                provider: self.provider_id.clone(),
                message: format!("HTTP error: {}", e),
            })?;

        let json_val: serde_json::Value = resp.json().await.map_err(|e| HarnessError::ProviderError {
            provider: self.provider_id.clone(),
            message: format!("Failed to parse embeddings JSON: {}", e),
        })?;

        let mut embeddings = Vec::new();
        if let Some(data) = json_val.get("data").and_then(|d| d.as_array()) {
            for item in data {
                if let Some(emb_arr) = item.get("embedding").and_then(|e| e.as_array()) {
                    let vec: Vec<f32> = emb_arr.iter().filter_map(|v| v.as_f64().map(|f| f as f32)).collect();
                    embeddings.push(vec);
                }
            }
        }

        Ok(embeddings)
    }
}
