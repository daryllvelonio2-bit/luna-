use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, warn};

use crate::auth::CredentialManager;
use crate::catalog::{resolve_descriptor, AuthHeaderFormat};
use crate::discovery::ModelDiscovery;
use crate::thought_signature;
use harness_core::{
    config::ProviderOverride, CompletionOptions, HarnessError, Message, ModelProvider,
    ReasoningEffort, Role, StreamEvent, TokenUsage, ToolCall, ToolDefinition,
};

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
                        id: call
                            .get("id")
                            .and_then(|i| i.as_str())
                            .map(|s| s.to_string()),
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
                        // Gemini's OpenAI-compatible route carries the call's thought signature in
                        // an `extra_content` extension; every other provider simply has none.
                        signature: thought_signature::signature_from_tool_call(call),
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
                prompt_tokens: usage
                    .get("prompt_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0) as u32,
                completion_tokens: usage
                    .get("completion_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0) as u32,
                total_tokens: usage
                    .get("total_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0) as u32,
            }),
        }));
    }

    events
}

/// A `reqwest` client for one provider, with the per-provider timeout the config resolved
/// (`providers.<id>.timeout_secs`, default `catalog::DEFAULT_TIMEOUT_SECS`).
pub fn build_http_client(timeout_secs: u64) -> Result<reqwest::Client, HarnessError> {
    reqwest::Client::builder()
        .user_agent("luna-harness/0.1.0")
        .timeout(std::time::Duration::from_secs(timeout_secs.max(600)))
        .connect_timeout(std::time::Duration::from_secs(30))
        .tcp_keepalive(Some(std::time::Duration::from_secs(15)))
        .pool_idle_timeout(Some(std::time::Duration::from_secs(15)))
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
    async fn build_auth_headers(
        &self,
    ) -> Result<(HeaderMap, Option<String>, SecretString), HarnessError> {
        let secret = self
            .cred_manager
            .acquire_credential(&self.provider_id)
            .await?;
        let api_key = secret.expose_secret().to_string();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let mut query_param = None;

        match self.auth_format.clone() {
            AuthHeaderFormat::Bearer => {
                let token = if self.provider_id == "antigravity" || self.provider_id == "agy" {
                    crate::antigravity::extract_bearer_token(&api_key).0
                } else if self.provider_id.starts_with("opencode") {
                    if api_key.trim().is_empty() {
                        "public".to_string()
                    } else {
                        api_key
                    }
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

        if self.provider_id.starts_with("opencode") || self.base_url.contains("opencode.ai") {
            let session_id = crate::opencode::get_or_create_opencode_session_id();
            headers.insert(
                "User-Agent",
                HeaderValue::from_static("opencode/latest/2.0.29/cli"),
            );
            headers.insert("x-opencode-client", HeaderValue::from_static("cli"));
            headers.insert("x-opencode-project", HeaderValue::from_static("global"));
            if let Ok(v) = HeaderValue::from_str(&session_id) {
                headers.insert("x-opencode-session", v.clone());
                headers.insert("x-opencode-session-id", v.clone());
                headers.insert("x-session-affinity", v.clone());
                headers.insert("x-session-id", v);
            }
        }

        Ok((headers, query_param, secret))
    }

    /// Report the credential that just failed so the manager retires it and the next call fails
    /// over to another key. Returns whether another credential is available for failover.
    async fn retire_failed_credential(&self, secret: &SecretString) -> bool {
        let retired = self
            .cred_manager
            .mark_credential_failed(&self.provider_id, secret.expose_secret())
            .await;
        if retired {
            warn!(
                provider = %self.provider_id,
                "Provider rejected the selected credential; failing over to another credential in pool"
            );
        } else {
            debug!(
                provider = %self.provider_id,
                "Provider rejected the selected credential, but no other key is available to fail over to"
            );
        }
        retired
    }

    /// Serialize the transcript for an OpenAI-compatible `/chat/completions` body.
    ///
    /// `model` decides whether a replayed function call must carry Google's thought signature: the
    /// value the model issued is written back verbatim, and a call we hold none for carries the
    /// documented dummy on the models that would otherwise reject the request outright. Every
    /// non-Google provider is unaffected — it neither sends a signature nor is sent one.
    fn serialize_messages(&self, messages: &[Message], model: &str) -> serde_json::Value {
        let enforce_signatures = thought_signature::model_enforces_thought_signature(model);
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
                msg_obj["content"] = serialize_content_with_images(role_str, text);
            } else if m.role == Role::Assistant {
                msg_obj["content"] = serde_json::Value::Null;
            }

            if let Some(ref r) = m.reasoning {
                msg_obj["reasoning_content"] = json!(r);
            }

            if !m.tool_calls.is_empty() {
                let tc_json: Vec<_> = m
                    .tool_calls
                    .iter()
                    .map(|tc| {
                        let mut call = json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments.to_string(),
                            }
                        });
                        if let Some(signature) = thought_signature::signature_for_replay(
                            tc.signature.as_deref(),
                            enforce_signatures,
                        ) {
                            call["extra_content"] =
                                json!({ "google": { "thought_signature": signature } });
                        }
                        call
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
        if self.provider_id == "kiro" || self.provider_id == "kiro-cli" {
            let _ = crate::kiro::sync_kiro_credential_if_needed(&self.cred_manager).await;
        }

        if self.provider_id == "supercode" {
            let _ = crate::supercode::sync_supercode_credential_if_needed(&self.cred_manager).await;
        }
        let (headers, query_param, secret) = self.build_auth_headers().await?;
        let mut url = format!("{}/chat/completions", self.base_url);
        if let Some(q) = query_param {
            url = format!("{}?{}", url, q);
        }

        let model = options
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());

        if self.provider_id == "supercode" {
            let res = crate::supercode::execute_supercode_completion(
                &self.http_client,
                secret.expose_secret(),
                Some(&self.cred_manager),
                &model,
                messages,
                tools,
                options,
            )
            .await;
            if let Err(ref err) = res {
                if matches!(
                    err,
                    HarnessError::AuthenticationFailed { .. } | HarnessError::RateLimited { .. }
                ) {
                    self.retire_failed_credential(&secret).await;
                }
            }
            return res;
        }

        if self.provider_id == "kiro" || self.provider_id == "kiro-cli" {
            let res = crate::kiro::execute_kiro_completion(
                &self.http_client,
                secret.expose_secret(),
                Some(&self.cred_manager),
                &model,
                messages,
                tools,
                options,
            )
            .await;
            if let Err(ref err) = res {
                if matches!(
                    err,
                    HarnessError::AuthenticationFailed { .. } | HarnessError::RateLimited { .. }
                ) {
                    self.retire_failed_credential(&secret).await;
                }
            }
            return res;
        }

        if self.provider_id == "antigravity" || self.provider_id == "agy" {
            let res = crate::antigravity::execute_antigravity_completion(
                &self.http_client,
                secret.expose_secret(),
                Some(&self.cred_manager),
                &model,
                messages,
                tools,
                options,
            )
            .await;
            if let Err(ref err) = res {
                if matches!(
                    err,
                    HarnessError::AuthenticationFailed { .. } | HarnessError::RateLimited { .. }
                ) {
                    self.retire_failed_credential(&secret).await;
                }
            }
            return res;
        }

        if self.provider_id.starts_with("opencode") || self.base_url.contains("opencode.ai") {
            let mut stream = self.stream(messages, tools, options).await?;
            let mut content = String::new();
            let mut reasoning = String::new();
            let mut usage = None;
            // Call fragments arrive one field at a time, so each call is accumulated into the shape
            // it is finally returned in: text fields appended, `arguments` held as the raw JSON
            // fragment string until the stream ends, and the signature kept verbatim.
            let mut tool_builders: std::collections::BTreeMap<usize, ToolCall> =
                std::collections::BTreeMap::new();

            while let Some(ev_res) = stream.next().await {
                match ev_res? {
                    StreamEvent::ContentDelta(d) => content.push_str(&d),
                    StreamEvent::ReasoningDelta(r) => reasoning.push_str(&r),
                    StreamEvent::ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments_delta,
                        signature,
                    } => {
                        let entry = tool_builders.entry(index).or_insert_with(|| ToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: serde_json::Value::String(String::new()),
                            signature: None,
                        });
                        if let Some(i) = id {
                            entry.id.push_str(&i);
                        }
                        if let Some(n) = name {
                            entry.name.push_str(&n);
                        }
                        if let Some(signature) = signature {
                            entry.signature = Some(signature);
                        }
                        if let Some(fragment) = arguments_delta {
                            let mut raw = match entry.arguments.take() {
                                serde_json::Value::String(raw) => raw,
                                _ => String::new(),
                            };
                            raw.push_str(&fragment);
                            entry.arguments = serde_json::Value::String(raw);
                        }
                    }
                    StreamEvent::Completed { usage: u, .. } => {
                        usage = u;
                    }
                }
            }

            let tool_calls: Vec<ToolCall> = tool_builders
                .into_values()
                .filter(|call| !call.name.is_empty())
                .map(|mut call| {
                    call.arguments = match call.arguments {
                        serde_json::Value::String(raw) => {
                            serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
                        }
                        other => other,
                    };
                    call
                })
                .collect();

            return Ok(Message {
                role: Role::Assistant,
                content: if content.is_empty() {
                    None
                } else {
                    Some(content)
                },
                reasoning: if reasoning.is_empty() {
                    None
                } else {
                    Some(reasoning)
                },
                tool_calls,
                tool_result: None,
                usage,
            });
        }

        let mut body = json!({
            "model": model,
            "messages": self.serialize_messages(messages, model.as_str()),
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
            if let Some(s) = normalize_reasoning_effort(&self.provider_id, &model, effort) {
                body["reasoning_effort"] = json!(s);
            }
        }

        let mut resp = match self
            .http_client
            .post(&url)
            .headers(headers.clone())
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Err(HarnessError::ProviderError {
                    provider: self.provider_id.clone(),
                    message: format!("HTTP error: {}", e),
                })
            }
        };

        // If 400 Bad Request with Invalid request occurs and reasoning_effort was included,
        // retry once without reasoning_effort for models that do not support it.
        if resp.status().as_u16() == 400 && body.get("reasoning_effort").is_some() {
            if let Some(map) = body.as_object_mut() {
                map.remove("reasoning_effort");
            }
            if let Ok(retry_resp) = self
                .http_client
                .post(&url)
                .headers(headers.clone())
                .json(&body)
                .send()
                .await
            {
                resp = retry_resp;
            }
        }

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

        let resp_json: serde_json::Value =
            resp.json().await.map_err(|e| HarnessError::ProviderError {
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

        let msg_val = choice
            .get("message")
            .ok_or_else(|| HarnessError::ProviderError {
                provider: self.provider_id.clone(),
                message: "Choice has no message".to_string(),
            })?;

        let mut reasoning = msg_val
            .get("reasoning_content")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        let mut content = msg_val
            .get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());

        if reasoning.is_none() {
            if let Some(c) = &content {
                if let Some(start) = c.find("<think>") {
                    if let Some(end) = c.find("</think>") {
                        let think_text = c[start + 7..end].trim().to_string();
                        let rest = format!("{}{}", &c[..start], &c[end + 8..])
                            .trim()
                            .to_string();
                        if !think_text.is_empty() {
                            reasoning = Some(think_text);
                        }
                        content = if rest.is_empty() { None } else { Some(rest) };
                    }
                }
            }
        }

        // DeepSeek's DSML scaffolding can also arrive whole (non-streaming, or a gateway that does
        // not split it), and it is protocol noise rather than the model speaking — so it must never
        // become the stored message text. `raw_text_present` remembers that the model did emit
        // *something*, so an all-scaffolding reply is reported as a no-text turn, not an empty one.
        let raw_text_present = content
            .as_deref()
            .map(|text| !text.trim().is_empty())
            .unwrap_or(false);
        if let Some(text) = content.as_deref() {
            let scrubbed = harness_core::text::strip_dsml_artifacts(text);
            content = if scrubbed.trim().is_empty() {
                None
            } else {
                Some(scrubbed)
            };
        }

        let mut tool_calls = Vec::new();
        if let Some(tc_array) = msg_val.get("tool_calls").and_then(|tc| tc.as_array()) {
            for tc in tc_array {
                let id = tc
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                let func = tc.get("function").unwrap_or(&serde_json::Value::Null);
                let name = func
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let args_raw = func
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .unwrap_or("{}");
                let args: serde_json::Value = serde_json::from_str(args_raw).unwrap_or(json!({}));
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments: args,
                    // Gemini's OpenAI-compatible route hands the call's thought signature back in
                    // an `extra_content` extension; without it a replayed call is rejected.
                    signature: thought_signature::signature_from_tool_call(tc),
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
            // A reply whose text was nothing but protocol scaffolding still *happened* — the model
            // spoke, it just spoke in markup — so it is reported as a no-text turn instead of being
            // treated as an empty (failed) response.
            if reasoning.is_some() || raw_text_present {
                return Ok(Message {
                    role: Role::Assistant,
                    content: Some(harness_core::types::NO_TEXT_RESPONSE.to_string()),
                    reasoning,
                    tool_calls: Vec::new(),
                    tool_result: None,
                    usage,
                });
            }
            return Err(HarnessError::ProviderError {
                provider: self.provider_id.clone(),
                message:
                    "Model returned an empty response. The model may be overloaded or unavailable."
                        .to_string(),
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
        if self.provider_id == "kiro" || self.provider_id == "kiro-cli" {
            let _ = crate::kiro::sync_kiro_credential_if_needed(&self.cred_manager).await;
        }

        if self.provider_id == "supercode" {
            let _ = crate::supercode::sync_supercode_credential_if_needed(&self.cred_manager).await;
        }

        let model = options
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());

        if self.provider_id == "supercode" {
            let (_, _, secret) = self.build_auth_headers().await?;
            let res = crate::supercode::stream_supercode_completion(
                &self.http_client,
                secret.expose_secret(),
                Some(&self.cred_manager),
                &model,
                messages,
                tools,
                options,
            )
            .await;
            if let Err(ref err) = res {
                if matches!(
                    err,
                    HarnessError::AuthenticationFailed { .. } | HarnessError::RateLimited { .. }
                ) {
                    self.retire_failed_credential(&secret).await;
                }
            }
            return res;
        }

        if self.provider_id == "kiro" || self.provider_id == "kiro-cli" {
            let (_, _, secret) = self.build_auth_headers().await?;
            let res = crate::kiro::stream_kiro_completion(
                &self.http_client,
                secret.expose_secret(),
                Some(&self.cred_manager),
                &model,
                messages,
                tools,
                options,
            )
            .await;
            if let Err(ref err) = res {
                if matches!(
                    err,
                    HarnessError::AuthenticationFailed { .. } | HarnessError::RateLimited { .. }
                ) {
                    self.retire_failed_credential(&secret).await;
                }
            }
            return res;
        }

        // Antigravity speaks the Code Assist protocol, not the OpenAI one, so it has its own
        // SSE reader rather than the `/chat/completions` body below.
        if self.provider_id == "antigravity" || self.provider_id == "agy" {
            let (_, _, secret) = self.build_auth_headers().await?;
            let res = crate::antigravity::stream_antigravity_completion(
                &self.http_client,
                secret.expose_secret(),
                Some(&self.cred_manager),
                &model,
                messages,
                tools,
                options,
            )
            .await;
            if let Err(ref err) = res {
                if matches!(
                    err,
                    HarnessError::AuthenticationFailed { .. } | HarnessError::RateLimited { .. }
                ) {
                    self.retire_failed_credential(&secret).await;
                }
            }
            return res;
        }

        let (headers, query_param, secret) = self.build_auth_headers().await?;
        let mut url = format!("{}/chat/completions", self.base_url);
        if let Some(q) = query_param {
            url = format!("{}?{}", url, q);
        }

        let mut body = json!({
            "model": model,
            "messages": self.serialize_messages(messages, model.as_str()),
            "stream": true,
            "stream_options": { "include_usage": true }
        });

        if !tools.is_empty() {
            body["tools"] = self.serialize_tools(tools);
        }
        if let Some(effort) = options.reasoning_effort {
            if let Some(s) = normalize_reasoning_effort(&self.provider_id, &model, effort) {
                body["reasoning_effort"] = json!(s);
            }
        }

        if self.provider_id.starts_with("opencode") || self.base_url.contains("opencode.ai") {
            crate::opencode::ensure_compat_tools(&mut body);
        }

        let mut resp = self
            .http_client
            .post(&url)
            .headers(headers.clone())
            .json(&body)
            .send()
            .await
            .map_err(|e| HarnessError::ProviderError {
                provider: self.provider_id.clone(),
                message: format!("HTTP error: {}", e),
            })?;

        // If 400 Bad Request with Invalid request occurs and reasoning_effort was included,
        // retry once without reasoning_effort for models that do not support it.
        if resp.status().as_u16() == 400 && body.get("reasoning_effort").is_some() {
            if let Some(map) = body.as_object_mut() {
                map.remove("reasoning_effort");
            }
            if let Ok(retry_resp) = self
                .http_client
                .post(&url)
                .headers(headers)
                .json(&body)
                .send()
                .await
            {
                resp = retry_resp;
            }
        }

        let status = resp.status();
        if status.as_u16() == 429 {
            self.retire_failed_credential(&secret).await;
            return Err(HarnessError::RateLimited {
                provider: self.provider_id.clone(),
                retry_after_secs: Some(10),
            });
        }
        if !status.is_success() {
            if matches!(status.as_u16(), 401 | 403) {
                self.retire_failed_credential(&secret).await;
            }
            let body_text = resp.text().await.unwrap_or_default();
            let is_overloaded =
                status.as_u16() == 503 || body_text.to_lowercase().contains("overloaded");
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

        let json_val: serde_json::Value =
            resp.json().await.map_err(|e| HarnessError::ProviderError {
                provider: self.provider_id.clone(),
                message: format!("Failed to parse embeddings JSON: {}", e),
            })?;

        let mut embeddings = Vec::new();
        if let Some(data) = json_val.get("data").and_then(|d| d.as_array()) {
            for item in data {
                if let Some(emb_arr) = item.get("embedding").and_then(|e| e.as_array()) {
                    let vec: Vec<f32> = emb_arr
                        .iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect();
                    embeddings.push(vec);
                }
            }
        }

        Ok(embeddings)
    }
}

/// Serialize one message's text into a provider `content` value. A message that carries pasted
/// screenshots (data URLs written by the CLI's Ctrl+V path) becomes the multimodal array form —
/// `[{"type":"text",...},{"type":"image_url",...}]` — so vision-capable providers receive the
/// image. Plain text stays a bare string, which every provider accepts and which keeps the
/// common path byte-identical to what it was before images existed.
fn serialize_content_with_images(role: &str, text: &str) -> serde_json::Value {
    // Only user turns carry pasted images; a data URL inside a system/assistant/tool string is
    // prose about an image, not an attachment, and must not be lifted into a content part.
    if role != "user" || !text.contains("data:image/") {
        return json!(text);
    }

    let mut parts: Vec<serde_json::Value> = Vec::new();
    let mut prose = String::new();

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("data:image/") && trimmed.contains(";base64,") {
            let flushed = prose.trim_end().to_string();
            if !flushed.is_empty() {
                parts.push(json!({ "type": "text", "text": flushed }));
                prose.clear();
            }
            parts.push(json!({
                "type": "image_url",
                "image_url": { "url": trimmed }
            }));
        } else {
            prose.push_str(line);
        }
    }

    let tail = prose.trim_end().to_string();
    if !tail.is_empty() {
        parts.push(json!({ "type": "text", "text": tail }));
    }

    if !parts.iter().any(|p| p["type"] == "image_url") {
        return json!(text);
    }

    json!(parts)
}

/// Normalize reasoning effort strings for providers with strict accepted-variant sets.
/// NVIDIA NIM's DeepSeek deployment supports "low" and "high" (rejecting "minimal" and "medium" with HTTP 400).
fn normalize_reasoning_effort(
    provider_id: &str,
    model: &str,
    effort: ReasoningEffort,
) -> Option<&'static str> {
    let lower_model = model.to_lowercase();
    if provider_id == "nvidia" && lower_model.contains("deepseek") {
        match effort {
            ReasoningEffort::Minimal | ReasoningEffort::Low => Some("low"),
            ReasoningEffort::Medium
            | ReasoningEffort::High
            | ReasoningEffort::Xhigh
            | ReasoningEffort::Max => Some("high"),
        }
    } else {
        Some(effort.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_for(provider_id: &str, base_url: &str) -> GenericProviderClient {
        let dir = std::env::temp_dir().join(format!(
            "luna-thought-signature-{}-{}",
            provider_id,
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        GenericProviderClient {
            provider_id: provider_id.to_string(),
            base_url: base_url.to_string(),
            auth_format: AuthHeaderFormat::Bearer,
            cred_manager: CredentialManager::new(Some(dir.join("credentials.json")))
                .expect("creds"),
            discovery: Arc::new(ModelDiscovery::new(
                Some(dir.join("models_cache.json")),
                24,
                Default::default(),
            )),
            http_client: build_http_client(600).expect("http client"),
            default_model: "test-model".to_string(),
        }
    }

    fn assistant_call(signature: Option<&str>) -> Message {
        Message {
            role: Role::Assistant,
            content: None,
            reasoning: None,
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "list_files".to_string(),
                arguments: json!({ "path": "." }),
                signature: signature.map(|s| s.to_string()),
            }],
            tool_result: None,
            usage: None,
        }
    }

    fn first_tool_call(messages: &serde_json::Value) -> serde_json::Value {
        messages
            .as_array()
            .expect("message list")
            .iter()
            .find_map(|m| m.get("tool_calls").and_then(|t| t.as_array()))
            .and_then(|calls| calls.first().cloned())
            .expect("a serialized tool call")
    }

    /// The whole point of the fix: the signature Gemini issued on the OpenAI-compatible route is
    /// replayed beside the same call, in the extension that route documents.
    #[test]
    fn a_gemini_signature_is_replayed_on_the_openai_compatible_route() {
        let client = client_for(
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai",
        );
        let messages =
            client.serialize_messages(&[assistant_call(Some("sig-abc"))], "gemini-3.5-flash-lite");

        assert_eq!(
            first_tool_call(&messages)["extra_content"]["google"]["thought_signature"],
            json!("sig-abc")
        );
    }

    /// A strict model whose call we hold no signature for would 400 the request; it carries the
    /// documented dummy so the validator skips the check instead.
    #[test]
    fn a_strict_gemini_model_without_a_signature_gets_the_documented_dummy() {
        let client = client_for(
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai",
        );
        let messages = client.serialize_messages(&[assistant_call(None)], "gemini-3.5-flash-lite");

        assert_eq!(
            first_tool_call(&messages)["extra_content"]["google"]["thought_signature"],
            json!("skip_thought_signature_validator")
        );
    }

    /// The fix must not touch a provider that has no signature concept: adding the extension to
    /// every tool call would be junk on the wire for all of them.
    #[test]
    fn a_non_google_provider_is_sent_no_signature_extension() {
        let client = client_for("deepseek", "https://api.deepseek.com/v1");
        let messages = client.serialize_messages(&[assistant_call(None)], "deepseek-v4.1-flash");

        assert!(first_tool_call(&messages).get("extra_content").is_none());
    }

    /// An earlier Gemini family tolerates an absent signature, so it is not sent the dummy.
    #[test]
    fn a_non_validating_gemini_version_is_not_sent_the_dummy() {
        let client = client_for(
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai",
        );
        let messages = client.serialize_messages(&[assistant_call(None)], "gemini-2.5-flash");

        assert!(first_tool_call(&messages).get("extra_content").is_none());
    }

    #[test]
    fn test_nvidia_deepseek_reasoning_effort_normalization() {
        // DeepSeek on NVIDIA NIM rejects "minimal" and "medium" with HTTP 400.
        // It must normalize to "low" and "high".
        assert_eq!(
            normalize_reasoning_effort(
                "nvidia",
                "deepseek-ai/deepseek-v4.1-flash",
                ReasoningEffort::Minimal
            ),
            Some("low")
        );
        assert_eq!(
            normalize_reasoning_effort(
                "nvidia",
                "deepseek-ai/deepseek-v4.1-flash",
                ReasoningEffort::Low
            ),
            Some("low")
        );
        assert_eq!(
            normalize_reasoning_effort(
                "nvidia",
                "deepseek-ai/deepseek-v4.1-flash",
                ReasoningEffort::Medium
            ),
            Some("high")
        );
        assert_eq!(
            normalize_reasoning_effort(
                "nvidia",
                "deepseek-ai/deepseek-v4.1-flash",
                ReasoningEffort::High
            ),
            Some("high")
        );

        // Other models/providers should keep their original string
        assert_eq!(
            normalize_reasoning_effort("nvidia", "z-ai/glm-5.3-flash", ReasoningEffort::Medium),
            Some("medium")
        );
        assert_eq!(
            normalize_reasoning_effort(
                "openrouter",
                "deepseek-ai/deepseek-v4.1-flash",
                ReasoningEffort::Medium
            ),
            Some("medium")
        );
    }
}
