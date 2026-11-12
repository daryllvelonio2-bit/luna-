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
    config::ProviderOverride, CompletionOptions, HarnessError, Message, ModelCapabilities,
    ModelProvider, ReasoningEffort, Role, StreamEvent, TokenUsage, ToolCall, ToolDefinition,
};

/// A usage block normalized into the crate's one shape, whichever way the route spelled it.
///
/// OpenAI-shaped usage (OpenAI, Gemini's compatible route, DeepSeek, most aggregators) carries the
/// cache read in `prompt_tokens_details.cached_tokens`, already *inside* `prompt_tokens`. Anthropic
/// puts `cache_read_input_tokens` *beside* the input count, so the prompt the model saw is their
/// sum. Absent from both means zero: a provider that says nothing about caching is not credited
/// with a cache hit, because a hit is a discount and inventing one understates a bill.
fn token_usage_from(usage: &serde_json::Value) -> TokenUsage {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;

    if let Some(cached) = usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(|value| value.as_u64())
    {
        return TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens: (cached as u32).min(prompt_tokens),
        };
    }

    match usage
        .get("cache_read_input_tokens")
        .and_then(|value| value.as_u64())
    {
        Some(cached) => TokenUsage::with_separate_cache(
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached as u32,
        ),
        None => TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens: 0,
        },
    }
}

/// Extract the stream events carried by one OpenAI-compatible SSE chunk. A single chunk can
/// hold reasoning, visible text and a tool-call fragment at the same time, so every field is
/// examined instead of returning on the first hit.
fn delta_events(parsed: &serde_json::Value) -> Vec<Result<StreamEvent, HarnessError>> {
    use StreamEvent as E;

    let mut events = Vec::new();

    if let Some(error_events) = error_delta_events(parsed) {
        return error_events;
    }

    if let Some(choices) = parsed.get("choices").and_then(|c| c.as_array()) {
        for choice in choices {
            append_choice_events(choice, &mut events);
        }
    }

    if let Some(usage) = parsed.get("usage").filter(|u| !u.is_null()) {
        events.push(Ok(E::Completed {
            finish_reason: None,
            usage: Some(token_usage_from(usage)),
        }));
    }

    events
}

/// The error an SSE chunk carries, if it carries one at all.
fn error_delta_events(
    parsed: &serde_json::Value,
) -> Option<Vec<Result<StreamEvent, HarnessError>>> {
    let err_val = parsed.get("error")?;
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
        Some(vec![Err(HarnessError::RateLimited {
            provider: "model".to_string(),
            retry_after_secs: Some(10),
        })])
    } else if code == 503 || msg.to_lowercase().contains("overloaded") {
        Some(vec![Err(HarnessError::ProviderError {
            provider: "model".to_string(),
            message: format!("Model is overloaded (503): {}", msg),
        })])
    } else {
        Some(vec![Err(HarnessError::ProviderError {
            provider: "model".to_string(),
            message: msg,
        })])
    }
}

/// Expand one `choices` entry into the events its delta carries.
fn append_choice_events(
    choice: &serde_json::Value,
    events: &mut Vec<Result<StreamEvent, HarnessError>>,
) {
    use StreamEvent as E;

    let Some(delta) = choice.get("delta") else {
        return;
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
            events.push(Ok(tool_call_delta(call, position)));
        }
    }

    if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
        events.push(Ok(E::Completed {
            finish_reason: Some(reason.to_string()),
            usage: None,
        }));
    }
}

fn tool_call_delta(call: &serde_json::Value, position: usize) -> StreamEvent {
    let index = call
        .get("index")
        .and_then(|i| i.as_u64())
        .map(|i| i as usize)
        .unwrap_or(position);
    StreamEvent::ToolCallDelta {
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
    }
}

/// A `reqwest` client for one provider, with the per-provider timeout the config resolved
/// (`providers.<id>.timeout_secs`, default `catalog::DEFAULT_TIMEOUT_SECS`).
/// Connection setup is not the model's turn: it must fail long before the request timeout.
const HTTP_CONNECT_TIMEOUT_SECS: u64 = 30;
/// Keep pooled connections warm between the rounds of one turn, but do not hold an idle socket
/// open for an entire session.
const HTTP_KEEPALIVE_SECS: u64 = 15;
const HTTP_POOL_IDLE_SECS: u64 = 15;

pub fn build_http_client(timeout_secs: u64) -> Result<reqwest::Client, HarnessError> {
    reqwest::Client::builder()
        .user_agent("luna-harness/0.1.0")
        .timeout(std::time::Duration::from_secs(timeout_secs.max(600)))
        .connect_timeout(std::time::Duration::from_secs(HTTP_CONNECT_TIMEOUT_SECS))
        .tcp_keepalive(Some(std::time::Duration::from_secs(HTTP_KEEPALIVE_SECS)))
        .pool_idle_timeout(Some(std::time::Duration::from_secs(HTTP_POOL_IDLE_SECS)))
        .build()
        .map_err(|error| {
            HarnessError::ConfigurationError(format!("failed to build the HTTP client: {}", error))
        })
}

/// How many times one request may be re-sent with another credential before the provider's
/// error is handed back to the caller.
///
/// The pool is small, so this is a backstop against a pool that keeps rejecting rather than a
/// real budget: the request has to give up eventually.
const MAX_CREDENTIAL_FAILOVER_ATTEMPTS: usize = 4;

/// How long a 429 says to wait, from the response itself.
///
/// The header is the provider's own statement about its limiter, so it beats any guess. A
/// provider that sends none — or one that sends an HTTP date instead of a delay — gets the
/// conservative default: waiting too long costs seconds, waiting too short costs the account.
fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    const DEFAULT_RETRY_AFTER_SECS: u64 = 30;
    let header = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok());

    // The header is either a delay in seconds or an HTTP date. Only the numeric form is
    // actionable here.
    match header.and_then(|value| value.trim().parse::<u64>().ok()) {
        Some(seconds) => Some(seconds.clamp(1, 600)),
        None => Some(DEFAULT_RETRY_AFTER_SECS),
    }
}

fn request_url(base_url: &str, query_param: Option<String>) -> String {
    let mut url = format!("{}/chat/completions", base_url);
    if let Some(q) = query_param {
        url = format!("{}?{}", url, q);
    }
    url
}

/// If a 400 comes back and `reasoning_effort` was included, retry once without it for models that
/// do not support it.
async fn retry_without_reasoning_effort(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderMap,
    body: &mut serde_json::Value,
    resp: &mut reqwest::Response,
) {
    if resp.status().as_u16() == 400 && body.get("reasoning_effort").is_some() {
        if let Some(map) = body.as_object_mut() {
            map.remove("reasoning_effort");
        }
        if let Ok(retry_resp) = client
            .post(url)
            .headers(headers.clone())
            .json(&*body)
            .send()
            .await
        {
            *resp = retry_resp;
        }
    }
}

/// The reply's reasoning and visible text, with `<think>` blocks lifted out of the text when the
/// route does not report reasoning separately.
fn reasoning_and_content(msg_val: &serde_json::Value) -> (Option<String>, Option<String>) {
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

    (reasoning, content)
}

/// The events one SSE `data:` payload holds.
fn sse_data_events(data: &str) -> Vec<Result<StreamEvent, HarnessError>> {
    if data == "[DONE]" {
        return vec![Ok(StreamEvent::Completed {
            finish_reason: Some("stop".to_string()),
            usage: None,
        })];
    }
    match serde_json::from_str::<serde_json::Value>(data) {
        Ok(parsed) => delta_events(&parsed),
        Err(e) => vec![Err(HarnessError::SerializationError(format!(
            "Failed to parse SSE JSON: {}",
            e
        )))],
    }
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
            let request_id = crate::opencode::new_opencode_request_id();
            headers.insert(
                "User-Agent",
                HeaderValue::from_static(
                    "opencode/1.19.5 ai-sdk/provider-utils/4.0.23 runtime/bun/1.4.4",
                ),
            );
            headers.insert("x-opencode-client", HeaderValue::from_static("cli"));
            headers.insert("x-opencode-project", HeaderValue::from_static("global"));
            if let Ok(v) = HeaderValue::from_str(&session_id) {
                headers.insert("x-opencode-session", v.clone());
                headers.insert("x-opencode-session-id", v.clone());
                headers.insert("x-session-affinity", v.clone());
                headers.insert("x-session-id", v);
            }
            if let Ok(v) = HeaderValue::from_str(&request_id) {
                headers.insert("x-opencode-request", v);
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

    /// Retire the credential the provider just rejected and say whether the request may be
    /// re-sent with another one.
    ///
    /// A rate-limited or revoked key is a property of the KEY, not of the request: when the
    /// pool holds another key, the request can be served by that one at once. Handing the error
    /// back instead makes the caller wait out the provider's `Retry-After` — up to ten minutes
    /// — before it discovers the other key. The retirement is what picks the next key, so both
    /// decisions are the same one.
    async fn fail_over_to_another_credential(
        &self,
        secret: &SecretString,
        attempts: usize,
    ) -> bool {
        // The key is retired either way: a rejected key must not be handed out again.
        let retired = self.retire_failed_credential(secret).await;
        if attempts >= MAX_CREDENTIAL_FAILOVER_ATTEMPTS {
            return false;
        }
        retired
    }

    /// Run the provider-specific credential syncs that must happen before a request.
    ///
    /// A failed sync is not fatal: the request itself fails with the real auth error if the sync
    /// was needed, but it is logged so the underlying cause is not invisible.
    async fn sync_provider_credentials(&self, phase: &str) {
        if self.provider_id == "kiro" || self.provider_id == "kiro-cli" {
            if let Err(error) =
                crate::kiro::sync_kiro_credential_if_needed(&self.cred_manager).await
            {
                tracing::warn!("kiro credential sync failed before {phase}: {error}");
            }
        }

        if self.provider_id == "supercode" {
            if let Err(error) =
                crate::supercode::sync_supercode_credential_if_needed(&self.cred_manager).await
            {
                tracing::warn!("supercode credential sync failed before {phase}: {error}");
            }
        }
    }

    /// Retire the credential that just failed, when the failure says the key itself is bad.
    async fn retire_if_auth_failure<T>(
        &self,
        secret: &SecretString,
        result: Result<T, HarnessError>,
    ) -> Result<T, HarnessError> {
        if let Err(ref err) = result {
            if matches!(
                err,
                HarnessError::AuthenticationFailed { .. } | HarnessError::RateLimited { .. }
            ) {
                self.retire_failed_credential(secret).await;
            }
        }
        result
    }

    fn model_or_default(&self, options: &CompletionOptions) -> String {
        options
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone())
    }

    async fn complete_via_supercode(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let (_, _, secret) = self.build_auth_headers().await?;
        let res = crate::supercode::execute_supercode_completion(
            &self.http_client,
            secret.expose_secret(),
            Some(&self.cred_manager),
            model,
            messages,
            tools,
            options,
        )
        .await;
        self.retire_if_auth_failure(&secret, res).await
    }

    async fn complete_via_kiro(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let (_, _, secret) = self.build_auth_headers().await?;
        let res = crate::kiro::execute_kiro_completion(
            &self.http_client,
            secret.expose_secret(),
            Some(&self.cred_manager),
            model,
            messages,
            tools,
            options,
        )
        .await;
        self.retire_if_auth_failure(&secret, res).await
    }

    async fn complete_via_antigravity(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let (_, _, secret) = self.build_auth_headers().await?;
        let res = crate::antigravity::execute_antigravity_completion(
            &self.http_client,
            secret.expose_secret(),
            Some(&self.cred_manager),
            model,
            messages,
            tools,
            options,
        )
        .await;
        self.retire_if_auth_failure(&secret, res).await
    }

    async fn stream_via_supercode(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        let (_, _, secret) = self.build_auth_headers().await?;
        let res = crate::supercode::stream_supercode_completion(
            &self.http_client,
            secret.expose_secret(),
            Some(&self.cred_manager),
            model,
            messages,
            tools,
            options,
        )
        .await;
        self.retire_if_auth_failure(&secret, res).await
    }

    async fn stream_via_kiro(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        let (_, _, secret) = self.build_auth_headers().await?;
        let res = crate::kiro::stream_kiro_completion(
            &self.http_client,
            secret.expose_secret(),
            Some(&self.cred_manager),
            model,
            messages,
            tools,
            options,
        )
        .await;
        self.retire_if_auth_failure(&secret, res).await
    }

    async fn stream_via_antigravity(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        let (_, _, secret) = self.build_auth_headers().await?;
        let res = crate::antigravity::stream_antigravity_completion(
            &self.http_client,
            secret.expose_secret(),
            Some(&self.cred_manager),
            model,
            messages,
            tools,
            options,
        )
        .await;
        self.retire_if_auth_failure(&secret, res).await
    }

    async fn complete_via_codex(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let (_, _, secret) = self.build_auth_headers().await?;
        let res = crate::codex::execute_codex_completion(
            &self.http_client,
            secret.expose_secret(),
            model,
            messages,
            tools,
            options,
        )
        .await;
        self.retire_if_auth_failure(&secret, res).await
    }

    async fn stream_via_codex(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        let (_, _, secret) = self.build_auth_headers().await?;
        let res = crate::codex::stream_codex_completion(
            &self.http_client,
            secret.expose_secret(),
            model,
            messages,
            tools,
            options,
        )
        .await;
        self.retire_if_auth_failure(&secret, res).await
    }

    /// The `/chat/completions` body for a non-streaming request.
    fn build_complete_body(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> serde_json::Value {
        let mut body = json!({
            "model": model,
            "messages": self.serialize_messages(messages, model),
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
            if let Some(s) = normalize_reasoning_effort(&self.provider_id, model, effort) {
                body["reasoning_effort"] = json!(s);
            }
        }
        body
    }

    /// The `/chat/completions` body for a streaming request.
    fn build_stream_body(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> serde_json::Value {
        let mut body = json!({
            "model": model,
            "messages": self.serialize_messages(messages, model),
            "stream": true,
            "stream_options": { "include_usage": true }
        });

        if !tools.is_empty() {
            body["tools"] = self.serialize_tools(tools);
        }
        if let Some(effort) = options.reasoning_effort {
            if let Some(s) = normalize_reasoning_effort(&self.provider_id, model, effort) {
                body["reasoning_effort"] = json!(s);
            }
        }
        body
    }

    /// POST the body, retrying once without `reasoning_effort` on a 400, and failing over to
    /// another credential when the provider rejects or rate-limits the key.
    async fn send_complete_request(
        &self,
        mut body: serde_json::Value,
    ) -> Result<reqwest::Response, HarnessError> {
        // One request may be re-sent with another credential: a rate-limited or revoked key is
        // a property of the KEY, and the pool can hold a key that still works. Handing the error
        // back instead makes the caller wait out the provider's `Retry-After` — up to ten
        // minutes — before it discovers the other key.
        let mut failover_attempts = 0usize;
        let resp = loop {
            let (headers, query_param, secret) = self.build_auth_headers().await?;
            let url = request_url(&self.base_url, query_param);

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

            retry_without_reasoning_effort(&self.http_client, &url, &headers, &mut body, &mut resp)
                .await;

            let status = resp.status();
            if status.as_u16() == 429 {
                let retry_after = retry_after_secs(&resp);
                if self
                    .fail_over_to_another_credential(&secret, failover_attempts)
                    .await
                {
                    failover_attempts += 1;
                    warn!(
                        provider = %self.provider_id,
                        "Provider rate-limited this credential; serving the request with another key"
                    );
                    continue;
                }
                return Err(HarnessError::RateLimited {
                    provider: self.provider_id.clone(),
                    retry_after_secs: retry_after,
                });
            }

            if !status.is_success() {
                // 401/403/503: the key itself is dead (revoked, quota-cancelled account) or overloaded,
                // so retire it and let another key serve the request when the pool holds one.
                if matches!(status.as_u16(), 401 | 403 | 503)
                    && self
                        .fail_over_to_another_credential(&secret, failover_attempts)
                        .await
                {
                    failover_attempts += 1;
                    warn!(
                        provider = %self.provider_id,
                        "Provider rejected or overloaded on this credential; serving the request with another key"
                    );
                    continue;
                }
                let body_text = resp.text().await.unwrap_or_default();
                return Err(HarnessError::ProviderError {
                    provider: self.provider_id.clone(),
                    message: format!("HTTP {}: {}", status, body_text),
                });
            }

            break resp;
        };
        Ok(resp)
    }

    /// Same failover rules as [`Self::send_complete_request`], with the streaming route's
    /// overloaded-response wording.
    async fn send_stream_request(
        &self,
        mut body: serde_json::Value,
    ) -> Result<reqwest::Response, HarnessError> {
        let mut failover_attempts = 0usize;
        let resp = loop {
            let (headers, query_param, secret) = self.build_auth_headers().await?;
            let url = request_url(&self.base_url, query_param);

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

            retry_without_reasoning_effort(&self.http_client, &url, &headers, &mut body, &mut resp)
                .await;

            let status = resp.status();
            if status.as_u16() == 429 {
                let retry_after = retry_after_secs(&resp);
                if self
                    .fail_over_to_another_credential(&secret, failover_attempts)
                    .await
                {
                    failover_attempts += 1;
                    warn!(
                        provider = %self.provider_id,
                        "Provider rate-limited this credential; serving the request with another key"
                    );
                    continue;
                }
                return Err(HarnessError::RateLimited {
                    provider: self.provider_id.clone(),
                    retry_after_secs: retry_after,
                });
            }
            if !status.is_success() {
                // 401/403/503: the key itself is dead (revoked, quota-cancelled account) or overloaded,
                // so retire it and let another key serve the request when the pool holds one.
                if matches!(status.as_u16(), 401 | 403 | 503)
                    && self
                        .fail_over_to_another_credential(&secret, failover_attempts)
                        .await
                {
                    failover_attempts += 1;
                    warn!(
                        provider = %self.provider_id,
                        "Provider rejected or overloaded on this credential; serving the request with another key"
                    );
                    continue;
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

            break resp;
        };
        Ok(resp)
    }

    /// Turn a parsed `/chat/completions` response into the crate's [`Message`].
    fn completion_from_response(
        &self,
        resp_json: &serde_json::Value,
    ) -> Result<Message, HarnessError> {
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

        let (reasoning, mut content) = reasoning_and_content(msg_val);

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

        let tool_calls = self.response_tool_calls(msg_val)?;

        // The response's own `usage` object: what the provider billed for this request. The session
        // store persists it so compaction compares its threshold against a real count instead of a
        // character guess (`harness_core::session::estimate_tokens`).
        let usage = resp_json.get("usage").map(token_usage_from);

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

    fn response_tool_calls(
        &self,
        msg_val: &serde_json::Value,
    ) -> Result<Vec<ToolCall>, HarnessError> {
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
                // An empty fragment is "no arguments"; a whole value followed by trailing junk
                // keeps its first value. Only a value that ends early is a cut-off call.
                let args: serde_json::Value =
                    match harness_core::json::parse_tool_arguments(args_raw) {
                        Ok(value) => value,
                        Err(error) => {
                            return Err(HarnessError::ProviderError {
                                provider: self.provider_id.clone(),
                                message: format!(
                                    "Model returned an incomplete tool call for '{name}': its \
                                     arguments were not valid JSON ({error}). The call was not run."
                                ),
                            });
                        }
                    };
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
        Ok(tool_calls)
    }

    /// Serialize the transcript for an OpenAI-compatible `/chat/completions` body.
    ///
    /// `model` decides whether a replayed function call must carry Google's thought signature: the
    /// value the model issued is written back verbatim, and a call we hold none for carries the
    /// documented dummy on the models that would otherwise reject the request outright. Every
    /// non-Google provider is unaffected — it neither sends a signature nor is sent one.
    fn serialize_messages(&self, messages: &[Message], model: &str) -> serde_json::Value {
        let enforce_signatures = thought_signature::model_enforces_thought_signature(model);
        // Vision is a property of the model, so the serializer is told once per request whether
        // pasted images may become content parts.
        let vision = crate::capability::resolve(&self.provider_id, model).vision;
        let mut dropped_images = false;
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
                if !vision && carries_attached_image(role_str, text) {
                    dropped_images = true;
                }
                msg_obj["content"] = serialize_content_with_images(role_str, text, vision);
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
        if dropped_images {
            // Once per request, not once per replayed message: a session that pasted a screenshot
            // an hour ago still carries it, and the log should say so once.
            warn!(
                provider = %self.provider_id,
                model = %model,
                "dropped pasted image(s): this model has no vision capability, so the pixels cannot be sent (the model is told an image was omitted)"
            );
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
        self.sync_provider_credentials("complete").await;

        let model = self.model_or_default(options);

        if self.provider_id == "supercode" {
            return self
                .complete_via_supercode(&model, messages, tools, options)
                .await;
        }

        if self.provider_id == "kiro" || self.provider_id == "kiro-cli" {
            return self
                .complete_via_kiro(&model, messages, tools, options)
                .await;
        }

        if self.provider_id == "antigravity" || self.provider_id == "agy" {
            return self
                .complete_via_antigravity(&model, messages, tools, options)
                .await;
        }

        // Codex speaks the Responses protocol, not chat completions, so it has its own body and
        // its own SSE reader rather than the `/chat/completions` path below.
        if self.provider_id == "codex" {
            return self
                .complete_via_codex(&model, messages, tools, options)
                .await;
        }

        if self.provider_id == "opencode-cli" || self.provider_id == "opencode-headless" {
            return crate::opencode::execute_opencode_completion_cli(
                &model, messages, tools, options,
            )
            .await;
        }

        if self.provider_id.starts_with("opencode") || self.base_url.contains("opencode.ai") {
            return crate::opencode::execute_opencode_completion(&model, messages, tools, options)
                .await;
        }

        let body = self.build_complete_body(&model, messages, tools, options);
        let resp = self.send_complete_request(body).await?;

        let resp_json: serde_json::Value =
            resp.json().await.map_err(|e| HarnessError::ProviderError {
                provider: self.provider_id.clone(),
                message: format!("Failed to parse response JSON: {}", e),
            })?;

        self.completion_from_response(&resp_json)
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        self.sync_provider_credentials("stream").await;

        let model = self.model_or_default(options);

        if self.provider_id == "supercode" {
            return self
                .stream_via_supercode(&model, messages, tools, options)
                .await;
        }

        if self.provider_id == "kiro" || self.provider_id == "kiro-cli" {
            return self.stream_via_kiro(&model, messages, tools, options).await;
        }

        // Antigravity speaks the Code Assist protocol, not the OpenAI one, so it has its own
        // SSE reader rather than the `/chat/completions` body below.
        if self.provider_id == "antigravity" || self.provider_id == "agy" {
            return self
                .stream_via_antigravity(&model, messages, tools, options)
                .await;
        }

        if self.provider_id == "codex" {
            return self
                .stream_via_codex(&model, messages, tools, options)
                .await;
        }

        if self.provider_id == "opencode-cli" || self.provider_id == "opencode-headless" {
            return crate::opencode::stream_opencode_completion_cli(
                &model, messages, tools, options,
            )
            .await;
        }

        if self.provider_id.starts_with("opencode") || self.base_url.contains("opencode.ai") {
            return crate::opencode::stream_opencode_completion(&model, messages, tools, options)
                .await;
        }

        let body = self.build_stream_body(&model, messages, tools, options);
        let resp = self.send_stream_request(body).await?;

        let event_source = resp.bytes_stream().eventsource();
        let provider_id = self.provider_id.clone();

        // Each SSE chunk can carry reasoning, content and tool-call fragments at once, so the
        // chunk is expanded into every event it holds rather than only the first match.
        let mapped = event_source.flat_map(move |event_result| {
            let events: Vec<Result<StreamEvent, HarnessError>> = match event_result {
                Ok(event) => sse_data_events(&event.data),
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

    /// What this client's resolved model can be asked to do. The model is the one every call will
    /// use unless `CompletionOptions::model` overrides it, so the answer is about `default_model`
    /// — the same model the request body names (`catalog` + `capability` are the two halves of
    /// "what is this model": its window and its kind).
    fn capabilities(&self) -> ModelCapabilities {
        crate::capability::resolve(&self.provider_id, &self.default_model)
    }
}

/// Serialize one message's text into a provider `content` value. A message that carries pasted
/// screenshots (data URLs written by the CLI's Ctrl+V path) becomes the multimodal array form —
/// `[{"type":"text",...},{"type":"image_url",...}]` — so vision-capable providers receive the
/// image. Plain text stays a bare string, which every provider accepts and which keeps the
/// common path byte-identical to what it was before images existed.
///
/// `vision` is the resolved capability of the model this request names. A model without it is
/// sent a one-line note where the image was, instead of a URL it cannot decode: the pixels are
/// dropped either way, but the model is told *why* it is being asked about an image it cannot
/// see, so it answers "I cannot see that image" instead of inventing a description.
fn serialize_content_with_images(role: &str, text: &str, vision: bool) -> serde_json::Value {
    // Only user turns carry pasted images; a data URL inside a system/assistant/tool string is
    // prose about an image, not an attachment, and must not be lifted into a content part.
    if !carries_attached_image(role, text) {
        return json!(text);
    }

    let mut parts: Vec<serde_json::Value> = Vec::new();
    let mut prose = String::new();

    for line in text.split_inclusive('\n') {
        // A data URL is lifted wherever it sits in the line — as its own line (the clipboard
        // paste appends one) or mid-sentence (an inlined file path replaces its own token).
        // Everything around it stays prose, in order, and scanning continues after each URL
        // so several images on one line are all lifted, not just the first.
        let mut rest = line;
        while let Some(start) = find_data_url(rest) {
            let (before, url_and_rest) = rest.split_at(start);
            let url_len = url_and_rest
                .find(char::is_whitespace)
                .unwrap_or(url_and_rest.len());
            let (url, after) = url_and_rest.split_at(url_len);

            prose.push_str(before);
            let flushed = prose.trim().to_string();
            prose.clear();
            if !flushed.is_empty() {
                parts.push(json!({ "type": "text", "text": flushed }));
            }
            if vision {
                parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": url }
                }));
            } else {
                parts.push(json!({ "type": "text", "text": IMAGE_OMITTED_NOTE }));
            }
            rest = after;
        }
        prose.push_str(rest);
    }

    let tail = prose.trim().to_string();
    if !tail.is_empty() {
        parts.push(json!({ "type": "text", "text": tail }));
    }

    if !parts
        .iter()
        .any(|p| p["type"] == "image_url" || p["text"] == IMAGE_OMITTED_NOTE)
    {
        return json!(text);
    }

    json!(parts)
}

/// What a model without vision is told where the image was. Deliberately explicit about the
/// cause: a bare "[image]" invites the model to guess at the pixels.
pub const IMAGE_OMITTED_NOTE: &str =
    "[image omitted: this model does not accept image input, so the image could not be sent]";

/// Whether this message carries a pasted image the serializer would lift into a content part.
/// One predicate, used both to lift and to report the drop, so the two cannot drift apart.
fn carries_attached_image(role: &str, text: &str) -> bool {
    role == "user" && text.contains("data:image/")
}

/// The byte offset of a `data:image/...;base64,...` token's start in `s`, if one occurs there.
fn find_data_url(s: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(offset) = s[from..].find("data:image/") {
        let start = from + offset;
        let rest = &s[start..];
        // A data URL runs to the next whitespace; it must carry the base64 marker to be one.
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if rest[..end].contains(";base64,") {
            return Some(start);
        }
        from = start + "data:image/".len();
    }
    None
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
    fn a_mid_line_data_url_is_lifted_and_the_words_around_it_stay_prose() {
        let content = serialize_content_with_images(
            "user",
            "what is in this image data:image/png;base64,iVBORw0KGgo exactly",
            true,
        );
        let parts = content.as_array().expect("multimodal array");
        let kinds: Vec<&str> = parts.iter().map(|p| p["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["text", "image_url", "text"]);
        assert_eq!(parts[0]["text"], "what is in this image");
        assert_eq!(
            parts[1]["image_url"]["url"],
            "data:image/png;base64,iVBORw0KGgo"
        );
        assert_eq!(parts[2]["text"], "exactly");
    }

    /// The clipboard path appends the URL as its own line; that shape must keep working.
    #[test]
    fn a_line_starting_with_a_data_url_is_lifted() {
        let content = serialize_content_with_images(
            "user",
            "describe it\n\ndata:image/jpeg;base64,QUJD",
            true,
        );
        let parts = content.as_array().expect("multimodal array");
        let kinds: Vec<&str> = parts.iter().map(|p| p["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["text", "image_url"]);
        assert_eq!(parts[1]["image_url"]["url"], "data:image/jpeg;base64,QUJD");
    }

    /// Several data URLs on one line are all lifted — the scan resumes after each URL instead
    /// of stopping at the first.
    #[test]
    fn multiple_data_urls_on_one_line_are_all_lifted() {
        let content = serialize_content_with_images(
            "user",
            "two shots data:image/png;base64,AAAA and data:image/jpeg;base64,BBBB done",
            true,
        );
        let parts = content.as_array().expect("multimodal array");
        let kinds: Vec<&str> = parts.iter().map(|p| p["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            vec!["text", "image_url", "text", "image_url", "text"]
        );
        assert_eq!(parts[0]["text"], "two shots");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAAA");
        assert_eq!(parts[2]["text"], "and");
        assert_eq!(parts[3]["image_url"]["url"], "data:image/jpeg;base64,BBBB");
        assert_eq!(parts[4]["text"], "done");
    }

    #[test]
    fn a_data_url_mention_in_system_role_stays_plain_text() {
        let content =
            serialize_content_with_images("system", "sent data:image/png;base64,QUJD once", true);
        assert!(content.is_string(), "system turns are never multimodal");
    }

    #[test]
    fn prose_without_base64_marker_is_not_lifted() {
        let content = serialize_content_with_images("user", "the spec data:image/png draft", true);
        assert!(content.is_string());
    }

    /// A model without vision is told the image was omitted instead of being handed a base64 URL
    /// it cannot decode — the pixels are dropped either way, and the model must know why.
    #[test]
    fn a_model_without_vision_is_told_the_image_was_omitted() {
        let content = serialize_content_with_images(
            "user",
            "what is this\ndata:image/png;base64,AAAA",
            false,
        );
        let parts = content
            .as_array()
            .expect("the note replaces the image part");
        let kinds: Vec<&str> = parts.iter().map(|p| p["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["text", "text"]);
        assert_eq!(parts[0]["text"], "what is this");
        assert_eq!(parts[1]["text"], IMAGE_OMITTED_NOTE);
        assert!(
            !content.to_string().contains("base64"),
            "no image bytes reach a model that cannot read them"
        );
    }

    /// The same rule one level up: the request body built for a text-only model carries prose and
    /// the omission note, and no `image_url` part at all.
    #[test]
    fn a_text_only_model_request_carries_no_image_part() {
        let client = client_for("deepseek", "https://api.deepseek.com/v1");
        let body = client.serialize_messages(
            &[Message::user("look at this\ndata:image/png;base64,AAAA")],
            "deepseek-chat",
        );
        let text = body.to_string();
        assert!(
            !text.contains("image_url"),
            "no image part for a text model"
        );
        assert!(text.contains(IMAGE_OMITTED_NOTE));
    }

    #[test]
    fn a_vision_model_request_still_carries_the_image_part() {
        let client = client_for("openai", "https://api.openai.com/v1");
        let body = client.serialize_messages(
            &[Message::user("look at this\ndata:image/png;base64,AAAA")],
            "gpt-4o",
        );
        assert!(body.to_string().contains("image_url"));
    }

    /// A cache read is reported in two shapes and both are read: the OpenAI shape nests it under
    /// `prompt_tokens_details`, Anthropic puts `cache_read_input_tokens` beside the other counts.
    #[test]
    fn a_cache_read_is_read_from_either_usage_shape() {
        fn usage_from_chunk(chunk: &serde_json::Value) -> TokenUsage {
            delta_events(chunk)
                .into_iter()
                .find_map(|event| match event {
                    Ok(StreamEvent::Completed {
                        usage: Some(usage), ..
                    }) => Some(usage),
                    _ => None,
                })
                .expect("a usage event")
        }

        let openai = usage_from_chunk(&json!({
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 5,
                "total_tokens": 1005,
                "prompt_tokens_details": { "cached_tokens": 896 }
            }
        }));
        assert_eq!(openai.cached_tokens, 896);
        assert_eq!(openai.prompt_tokens, 1000);

        let anthropic = usage_from_chunk(&json!({
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 5,
                "total_tokens": 1005,
                "cache_read_input_tokens": 640
            }
        }));
        assert_eq!(anthropic.cached_tokens, 640);
        assert_eq!(
            anthropic.prompt_tokens, 1_640,
            "Anthropic reports the cache read beside the input, so the prompt is their sum"
        );
        assert_eq!(anthropic.total_tokens, 1_645);

        // A provider that says nothing about caching is not credited with a hit.
        let silent = usage_from_chunk(&json!({
            "usage": { "prompt_tokens": 7, "completion_tokens": 1, "total_tokens": 8 }
        }));
        assert_eq!(silent.cached_tokens, 0);
    }

    #[test]
    fn the_client_reports_the_capabilities_of_its_resolved_model() {
        let mut client = client_for("openai", "https://api.openai.com/v1");
        client.default_model = "gpt-4o".to_string();
        let caps = client.capabilities();
        assert!(caps.vision && caps.tool_use);

        client.default_model = "meta/llama-guard-4-12b".to_string();
        assert!(!client.capabilities().tool_use);
    }

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
