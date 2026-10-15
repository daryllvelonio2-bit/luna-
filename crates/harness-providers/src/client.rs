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
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", api_key))
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

        let content = msg_val.get("content").and_then(|c| c.as_str()).map(|s| s.to_string());

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

        Ok(Message {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_result: None,
        })
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        let (headers, query_param, _secret) = self.build_auth_headers().await?;
        let mut url = format!("{}/chat/completions", self.base_url);
        if let Some(q) = query_param {
            url = format!("{}?{}", url, q);
        }

        let model = options.model.clone().unwrap_or_else(|| self.default_model.clone());

        let mut body = json!({
            "model": model,
            "messages": self.serialize_messages(messages),
            "stream": true,
            "stream_options": { "include_usage": true }
        });

        if !tools.is_empty() {
            body["tools"] = self.serialize_tools(tools);
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

        let event_source = resp.bytes_stream().eventsource();
        let provider_id = self.provider_id.clone();

        let mapped = event_source.map(move |event_result| match event_result {
            Ok(event) => {
                if event.data == "[DONE]" {
                    return Ok(StreamEvent::Completed {
                        finish_reason: Some("stop".to_string()),
                        usage: None,
                    });
                }

                let parsed: serde_json::Value = serde_json::from_str(&event.data).map_err(|e| {
                    HarnessError::SerializationError(format!("Failed to parse SSE JSON: {}", e))
                })?;

                if let Some(choices) = parsed.get("choices").and_then(|c| c.as_array()) {
                    if let Some(choice) = choices.first() {
                        if let Some(delta) = choice.get("delta") {
                            if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
                                return Ok(StreamEvent::ContentDelta(text.to_string()));
                            }
                        }
                    }
                }

                if let Some(usage_val) = parsed.get("usage") {
                    let prompt = usage_val.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
                    let completion = usage_val.get("completion_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
                    let total = usage_val.get("total_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as u32;

                    return Ok(StreamEvent::Completed {
                        finish_reason: None,
                        usage: Some(TokenUsage {
                            prompt_tokens: prompt,
                            completion_tokens: completion,
                            total_tokens: total,
                        }),
                    });
                }

                Ok(StreamEvent::ContentDelta("".to_string()))
            }
            Err(e) => Err(HarnessError::ProviderError {
                provider: provider_id.clone(),
                message: format!("SSE stream error: {}", e),
            }),
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
