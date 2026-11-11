use eventsource_stream::Eventsource;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use harness_core::approval::{ApprovalDecision, ApprovalRequest};
use harness_core::types::{CompletionOptions, Message, StreamEvent, TokenUsage, ToolDefinition};
use harness_core::{ClarifyRequest, HarnessError};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::Ordering;

use super::form::{
    form_value_for_answer, form_value_from_raw, parse_opencode_form_to_clarify, FormFieldMapping,
    SKIPPED_ANSWER,
};
use super::models::parse_opencode_model_ref;
use super::prompt::format_opencode_prompt;
use super::service::{base64_encode, OpencodeServiceInfo};
use super::tools::normalize_tool_arguments;
use super::ui::{get_ui_bridge, OpencodeUiBridge};
use super::usage::{session_total_usage, step_context_usage};

/// Permissions handed to the OpenCode session Luna drives.
///
/// Luna brings its own tools, so OpenCode's Code Mode must stay off. With `execute` available the
/// model writes JavaScript to reach the filesystem instead of calling the session's tools and
/// skills, which is what made every turn look the same. The rule goes last because the last
/// matching rule wins; denying `execute` also removes the MCP tools reachable only through it.
pub(super) fn session_permissions(is_yolo: bool) -> serde_json::Value {
    let mut rules = if is_yolo {
        vec![json!({ "action": "*", "resource": "*", "effect": "allow" })]
    } else {
        vec![
            json!({ "action": "*", "resource": "*", "effect": "allow" }),
            json!({ "action": "edit", "resource": "*", "effect": "ask" }),
            json!({ "action": "write", "resource": "*", "effect": "ask" }),
            json!({ "action": "patch", "resource": "*", "effect": "ask" }),
            json!({ "action": "delete", "resource": "*", "effect": "ask" }),
            json!({ "action": "shell", "resource": "*", "effect": "ask" }),
            json!({ "action": "terminal", "resource": "*", "effect": "ask" }),
            json!({ "action": "bash", "resource": "*", "effect": "ask" }),
        ]
    };
    rules.push(json!({ "action": "execute", "resource": "*", "effect": "deny" }));
    serde_json::Value::Array(rules)
}

/// Everything the event loop needs to talk back to the session it is reading.
struct NativeStreamContext {
    service_url: String,
    session_id: String,
    client: reqwest::Client,
    headers: HeaderMap,
}

/// The running state of one native stream: usage, finish reason and tool-call assembly.
struct NativeStreamState {
    last_usage: Option<TokenUsage>,
    total_usage: Option<TokenUsage>,
    last_finish_reason: Option<String>,
    ended_successfully: bool,
    saw_deltas: bool,
    tool_names: HashMap<String, String>,
    tool_inputs: HashMap<String, serde_json::Value>,
}

impl NativeStreamState {
    fn new() -> Self {
        Self {
            last_usage: None,
            total_usage: None,
            last_finish_reason: Some("stop".to_string()),
            ended_successfully: false,
            saw_deltas: false,
            tool_names: HashMap::new(),
            tool_inputs: HashMap::new(),
        }
    }
}

/// Whether the event loop should keep reading or stop after an event.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

fn native_auth_headers(service: &OpencodeServiceInfo) -> HeaderMap {
    let mut auth_headers = HeaderMap::new();
    if let Some(ref pwd) = service.password {
        let auth_str = format!(
            "Basic {}",
            base64_encode(format!("opencode:{}", pwd).as_bytes())
        );
        if let Ok(val) = HeaderValue::from_str(&auth_str) {
            auth_headers.insert(AUTHORIZATION, val);
        }
    }
    auth_headers
}

/// Step 1: establish the SSE stream first so no events are missed.
async fn connect_event_stream(
    http_client: &reqwest::Client,
    service: &OpencodeServiceInfo,
    auth_headers: &HeaderMap,
) -> Result<reqwest::Response, HarnessError> {
    let sse_url = format!("{}/api/event", service.url.trim_end_matches('/'));
    let sse_resp = http_client
        .get(&sse_url)
        .headers(auth_headers.clone())
        .send()
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: format!("Failed to connect to OpenCode event stream: {}", e),
        })?;

    if !sse_resp.status().is_success() {
        let status = sse_resp.status();
        let body = sse_resp.text().await.unwrap_or_default();
        return Err(HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: format!("OpenCode event stream returned HTTP {}: {}", status, body),
        });
    }

    Ok(sse_resp)
}

/// Step 2: create the per-completion session and return its id.
async fn create_native_session(
    http_client: &reqwest::Client,
    service: &OpencodeServiceInfo,
    auth_headers: &HeaderMap,
    cwd: &str,
    provider_id: &str,
    model_id: &str,
) -> Result<String, HarnessError> {
    let mut ses_headers = auth_headers.clone();
    ses_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Ok(dir_val) = HeaderValue::from_str(cwd) {
        ses_headers.insert("x-opencode-directory", dir_val);
    }

    let is_yolo = get_ui_bridge()
        .map(|b| b.allow_all.load(Ordering::SeqCst))
        .unwrap_or(false);

    let permissions_payload = session_permissions(is_yolo);

    let create_session_url = format!("{}/api/session", service.url.trim_end_matches('/'));
    let create_payload = json!({
        "model": {
            "providerID": provider_id,
            "id": model_id
        },
        "permissions": permissions_payload
    });

    let ses_res = http_client
        .post(&create_session_url)
        .headers(ses_headers.clone())
        .json(&create_payload)
        .send()
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: format!("Failed to create OpenCode session: {}", e),
        })?;

    if !ses_res.status().is_success() {
        let status = ses_res.status();
        let body = ses_res.text().await.unwrap_or_default();
        return Err(HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: format!(
                "OpenCode session creation failed (HTTP {}): {}",
                status, body
            ),
        });
    }

    let ses_json: serde_json::Value = ses_res.json().await.map_err(|e| {
        HarnessError::SerializationError(format!("Failed to parse session JSON: {}", e))
    })?;

    Ok(ses_json
        .get("data")
        .and_then(|d| d.get("id"))
        .and_then(|i| i.as_str())
        .ok_or_else(|| HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: "Missing session ID in OpenCode response".to_string(),
        })?
        .to_string())
}

/// Step 3: post the prompt, deleting the session if the service rejects it.
#[allow(clippy::too_many_arguments)]
async fn send_native_prompt(
    http_client: &reqwest::Client,
    service: &OpencodeServiceInfo,
    auth_headers: &HeaderMap,
    cwd: &str,
    session_id: &str,
    messages: &[Message],
    provider_id: &str,
    model_id: &str,
) -> Result<(), HarnessError> {
    let mut ses_headers = auth_headers.clone();
    ses_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Ok(dir_val) = HeaderValue::from_str(cwd) {
        ses_headers.insert("x-opencode-directory", dir_val);
    }

    let prompt = format_opencode_prompt(messages);
    let prompt_url = format!(
        "{}/api/session/{}/prompt",
        service.url.trim_end_matches('/'),
        session_id
    );
    let mut prompt_payload = json!({
        "text": prompt.text,
        "model": {
            "providerID": provider_id,
            "id": model_id
        }
    });
    if !prompt.files.is_empty() {
        prompt_payload["files"] = json!(prompt.files);
    }

    let prompt_res = http_client
        .post(&prompt_url)
        .headers(ses_headers)
        .json(&prompt_payload)
        .send()
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: format!("Failed to post prompt to OpenCode: {}", e),
        })?;

    if !prompt_res.status().is_success() {
        let status = prompt_res.status();
        let body = prompt_res.text().await.unwrap_or_default();
        let clean_url = format!(
            "{}/api/session/{}",
            service.url.trim_end_matches('/'),
            session_id
        );
        let clean_client = http_client.clone();
        let clean_headers = auth_headers.clone();
        tokio::spawn(async move {
            let _ = clean_client
                .delete(&clean_url)
                .headers(clean_headers)
                .send()
                .await;
        });
        return Err(HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: format!("OpenCode prompt failed (HTTP {}): {}", status, body),
        });
    }

    Ok(())
}

pub async fn native_opencode_stream(
    http_client: &reqwest::Client,
    service: &OpencodeServiceInfo,
    model: &str,
    messages: &[Message],
    _tools: &[ToolDefinition],
    _options: &CompletionOptions,
) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
    let auth_headers = native_auth_headers(service);

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let (provider_id, model_id) = parse_opencode_model_ref(model);

    let sse_resp = connect_event_stream(http_client, service, &auth_headers).await?;
    let session_id = create_native_session(
        http_client,
        service,
        &auth_headers,
        &cwd,
        &provider_id,
        &model_id,
    )
    .await?;
    send_native_prompt(
        http_client,
        service,
        &auth_headers,
        &cwd,
        &session_id,
        messages,
        &provider_id,
        &model_id,
    )
    .await?;

    // 4. Transform SSE Events
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, HarnessError>>(100);
    let mut clean_headers = auth_headers.clone();
    clean_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let ctx = NativeStreamContext {
        service_url: service.url.trim_end_matches('/').to_string(),
        session_id,
        client: http_client.clone(),
        headers: clean_headers,
    };

    tokio::spawn(async move {
        let clean_url = format!("{}/api/session/{}", ctx.service_url, ctx.session_id);
        run_native_event_pump(sse_resp, tx, &ctx).await;
        let _ = ctx
            .client
            .delete(&clean_url)
            .headers(ctx.headers.clone())
            .send()
            .await;
    });

    let rx_stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
    Ok(Box::pin(rx_stream))
}

/// Read the SSE stream to its end, dispatching every event that belongs to this session.
async fn run_native_event_pump(
    sse_resp: reqwest::Response,
    tx: tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    ctx: &NativeStreamContext,
) {
    let mut event_source = sse_resp.bytes_stream().eventsource();
    // `session.usage.updated` reports the running totals for the whole OpenCode session. This
    // connection creates a fresh session per completion, so the newest figure is this call's
    // total and belongs in the ledger; it is *not* the size of the context, which is what the
    // last step reported and what the gauge shows.
    let mut state = NativeStreamState::new();

    while let Some(item) = event_source.next().await {
        match item {
            Ok(event) => {
                let data_str = event.data.trim();
                if data_str.is_empty() || data_str == "[DONE]" {
                    continue;
                }
                let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data_str) else {
                    continue;
                };
                let ev_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let data = parsed.get("data").unwrap_or(&serde_json::Value::Null);
                if event_session_id(&parsed, data) != Some(&ctx.session_id) {
                    continue;
                }

                if handle_native_event(ev_type, data, &tx, &mut state, ctx).await == Flow::Stop {
                    break;
                }
            }

            Err(e) => {
                let _ = tx
                    .send(Err(HarnessError::ProviderError {
                        provider: "opencode".to_string(),
                        message: format!("OpenCode SSE error: {}", e),
                    }))
                    .await;
                break;
            }
        }
    }

    if !state.ended_successfully && state.saw_deltas {
        let _ = tx
            .send(Ok(StreamEvent::Completed {
                finish_reason: state.last_finish_reason,
                usage: state.total_usage.or(state.last_usage),
            }))
            .await;
    }
}

/// The session an event belongs to, in any of the spellings this route is known to use.
fn event_session_id<'a>(
    parsed: &'a serde_json::Value,
    data: &'a serde_json::Value,
) -> Option<&'a str> {
    data.get("sessionID")
        .or_else(|| data.get("sessionId"))
        .or_else(|| data.get("session_id"))
        .or_else(|| data.get("session").and_then(|s| s.get("id")))
        .or_else(|| {
            data.get("form")
                .and_then(|f| f.get("sessionID").or_else(|| f.get("sessionId")))
        })
        .or_else(|| parsed.get("sessionID").or_else(|| parsed.get("sessionId")))
        .and_then(|s| s.as_str())
}

/// The tool-call id an event refers to, in any of the spellings this route is known to use.
fn event_tool_id(data: &serde_json::Value) -> Option<&str> {
    data.get("id")
        .or_else(|| data.get("callID"))
        .or_else(|| data.get("callId"))
        .and_then(|v| v.as_str())
}

fn event_tool_name(data: &serde_json::Value, id: &str, state: &NativeStreamState) -> String {
    data.get("name")
        .or_else(|| data.get("tool"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())
        .or_else(|| state.tool_names.get(id).cloned())
        .unwrap_or_else(|| "tool".to_string())
}

async fn handle_native_event(
    ev_type: &str,
    data: &serde_json::Value,
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    state: &mut NativeStreamState,
    ctx: &NativeStreamContext,
) -> Flow {
    match ev_type {
        "session.reasoning.delta" | "session.next.reasoning.delta" => {
            emit_reasoning_delta(tx, data, state).await
        }
        "session.text.delta" | "session.next.text.delta" => {
            emit_content_delta(tx, data, state).await
        }
        "session.tool.input.started" | "session.next.tool.input.started" => {
            record_tool_input_started(data, state);
            Flow::Continue
        }
        "session.tool.input.ended" | "session.next.tool.input.ended" => {
            record_tool_input_ended(data, state);
            Flow::Continue
        }
        "session.tool.called" | "session.next.tool.called" => {
            emit_tool_called(tx, data, state).await
        }
        "session.tool.success" | "session.next.tool.success" => {
            emit_tool_success(tx, data, state).await
        }
        "session.tool.failed" | "session.next.tool.failed" => {
            emit_tool_failed(tx, data, state).await
        }
        "session.step.ended" | "session.next.step.ended" => {
            handle_step_ended(tx, ev_type, data, state).await;
            Flow::Continue
        }
        "session.usage.updated" | "session.next.usage.updated" => {
            // Session-cumulative, not one request's prompt. Feeding it to the context gauge is
            // what pushed the readout past the window while an inner loop ran: every step's
            // prompt was added to the last.
            if let Some(usage) = session_total_usage(ev_type, data) {
                state.total_usage = Some(usage);
            }
            Flow::Continue
        }
        "session.execution.succeeded"
        | "session.next.execution.succeeded"
        | "session.idle"
        | "session.next.idle" => finish_native_stream(tx, state).await,
        "session.execution.failed" | "session.next.execution.failed" => {
            emit_execution_failed(tx, data).await
        }
        "session.status" | "session.next.status" => handle_session_status(tx, data, state).await,
        "form.created" | "session.form.created" | "session.next.form.created" => {
            handle_form_created(data, ctx);
            Flow::Continue
        }
        "permission.asked" | "session.permission.asked" | "session.next.permission.asked" => {
            handle_permission_asked(data, ctx);
            Flow::Continue
        }
        _ => Flow::Continue,
    }
}

async fn emit_reasoning_delta(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    data: &serde_json::Value,
    state: &mut NativeStreamState,
) -> Flow {
    if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
        if !delta.is_empty() {
            state.saw_deltas = true;
            if tx
                .send(Ok(StreamEvent::ReasoningDelta(delta.to_string())))
                .await
                .is_err()
            {
                return Flow::Stop;
            }
        }
    }
    Flow::Continue
}

async fn emit_content_delta(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    data: &serde_json::Value,
    state: &mut NativeStreamState,
) -> Flow {
    if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
        if !delta.is_empty() {
            state.saw_deltas = true;
            if tx
                .send(Ok(StreamEvent::ContentDelta(delta.to_string())))
                .await
                .is_err()
            {
                return Flow::Stop;
            }
        }
    }
    Flow::Continue
}

fn record_tool_input_started(data: &serde_json::Value, state: &mut NativeStreamState) {
    if let Some(id) = event_tool_id(data) {
        if let Some(name) = data
            .get("name")
            .or_else(|| data.get("tool"))
            .and_then(|v| v.as_str())
        {
            state.tool_names.insert(id.to_string(), name.to_string());
        }
    }
}

fn record_tool_input_ended(data: &serde_json::Value, state: &mut NativeStreamState) {
    let Some(id) = event_tool_id(data) else {
        return;
    };
    if let Some(text) = data
        .get("text")
        .or_else(|| data.get("arguments"))
        .and_then(|v| v.as_str())
    {
        if let Ok(mut parsed_input) = serde_json::from_str::<serde_json::Value>(text) {
            normalize_tool_arguments(&mut parsed_input);
            state.tool_inputs.insert(id.to_string(), parsed_input);
        }
    } else if let Some(mut input_obj) = data.get("input").or_else(|| data.get("arguments")).cloned()
    {
        normalize_tool_arguments(&mut input_obj);
        state.tool_inputs.insert(id.to_string(), input_obj);
    }
}

async fn emit_tool_called(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    data: &serde_json::Value,
    state: &mut NativeStreamState,
) -> Flow {
    let id = event_tool_id(data).unwrap_or("");
    let name = event_tool_name(data, id, state);
    let mut input = data
        .get("input")
        .or_else(|| data.get("arguments"))
        .cloned()
        .or_else(|| state.tool_inputs.get(id).cloned())
        .unwrap_or(serde_json::Value::Null);
    normalize_tool_arguments(&mut input);
    state.tool_inputs.insert(id.to_string(), input.clone());
    state.saw_deltas = true;
    if tx
        .send(Ok(StreamEvent::ToolExecuting {
            name,
            arguments: input,
        }))
        .await
        .is_err()
    {
        return Flow::Stop;
    }
    Flow::Continue
}

async fn emit_tool_success(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    data: &serde_json::Value,
    state: &mut NativeStreamState,
) -> Flow {
    let id = event_tool_id(data).unwrap_or("");
    let name = event_tool_name(data, id, state);
    let mut output = String::new();
    if let Some(content) = data.get("content").and_then(|c| c.as_array()) {
        for item in content {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                output.push_str(text);
            }
        }
    }
    if output.is_empty() {
        if let Some(out) = data
            .get("output")
            .or_else(|| data.get("result"))
            .and_then(|o| o.as_str())
        {
            output.push_str(out);
        }
    }
    state.saw_deltas = true;
    if tx
        .send(Ok(StreamEvent::ToolExecuted {
            name,
            output,
            is_error: false,
        }))
        .await
        .is_err()
    {
        return Flow::Stop;
    }
    Flow::Continue
}

async fn emit_tool_failed(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    data: &serde_json::Value,
    state: &mut NativeStreamState,
) -> Flow {
    let id = event_tool_id(data).unwrap_or("");
    let name = event_tool_name(data, id, state);
    let error_msg = data
        .get("error")
        .map(|e| {
            if let Some(s) = e.as_str() {
                s.to_string()
            } else {
                e.to_string()
            }
        })
        .unwrap_or_else(|| "Tool failed".to_string());
    state.saw_deltas = true;
    if tx
        .send(Ok(StreamEvent::ToolExecuted {
            name,
            output: error_msg,
            is_error: true,
        }))
        .await
        .is_err()
    {
        return Flow::Stop;
    }
    Flow::Continue
}

async fn handle_step_ended(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    ev_type: &str,
    data: &serde_json::Value,
    state: &mut NativeStreamState,
) {
    if let Some(finish) = data
        .get("finish")
        .or_else(|| data.get("finishReason"))
        .or_else(|| data.get("reason"))
        .and_then(|f| f.as_str())
    {
        state.last_finish_reason = Some(finish.to_string());
    }
    if let Some(usage) = step_context_usage(ev_type, data) {
        state.last_usage = Some(usage.clone());
        let _ = tx.send(Ok(StreamEvent::Usage(usage.clone()))).await;
        if let Some(bridge) = get_ui_bridge() {
            if let Some(ref update_fn) = bridge.update_context_used_fn {
                let total_context = usage.prompt_tokens.saturating_add(usage.completion_tokens);
                if total_context > 0 {
                    update_fn(total_context);
                }
            }
        }
    }
}

/// Emit the completion event and stop reading: the turn succeeded.
async fn finish_native_stream(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    state: &mut NativeStreamState,
) -> Flow {
    state.ended_successfully = true;
    let _ = tx
        .send(Ok(StreamEvent::Completed {
            finish_reason: state.last_finish_reason.clone(),
            usage: state
                .total_usage
                .clone()
                .or_else(|| state.last_usage.clone()),
        }))
        .await;
    Flow::Stop
}

async fn emit_execution_failed(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    data: &serde_json::Value,
) -> Flow {
    let err_msg = data
        .get("error")
        .map(|e| e.to_string())
        .unwrap_or_else(|| "Execution failed".to_string());
    let _ = tx
        .send(Err(HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: format!("OpenCode execution failed: {}", err_msg),
        }))
        .await;
    Flow::Stop
}

async fn handle_session_status(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    data: &serde_json::Value,
    state: &mut NativeStreamState,
) -> Flow {
    let status_type = data
        .get("status")
        .and_then(|s| s.get("type").or(Some(s)))
        .and_then(|t| t.as_str())
        .or_else(|| data.get("type").and_then(|t| t.as_str()));
    if status_type == Some("idle") && state.saw_deltas {
        return finish_native_stream(tx, state).await;
    }
    Flow::Continue
}

fn handle_form_created(data: &serde_json::Value, ctx: &NativeStreamContext) {
    let form = data.get("form").unwrap_or(data);
    let form_id = form
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if form_id.is_empty() {
        return;
    }
    let (clarify_req, field_mappings) = parse_opencode_form_to_clarify(form);
    let bridge = get_ui_bridge();
    let client = ctx.client.clone();
    let reply_url = format!(
        "{}/api/session/{}/form/{}/reply",
        ctx.service_url, ctx.session_id, form_id
    );
    let cancel_url = format!(
        "{}/api/session/{}/form/{}",
        ctx.service_url, ctx.session_id, form_id
    );
    let post_headers = ctx.headers.clone();

    tokio::spawn(async move {
        submit_form_reply(
            clarify_req,
            field_mappings,
            bridge,
            client,
            reply_url,
            cancel_url,
            post_headers,
        )
        .await;
    });
}

async fn submit_form_reply(
    clarify_req: ClarifyRequest,
    field_mappings: Vec<FormFieldMapping>,
    bridge: Option<OpencodeUiBridge>,
    client: reqwest::Client,
    reply_url: String,
    cancel_url: String,
    post_headers: HeaderMap,
) {
    let maybe_resp = if let Some(bridge) = bridge {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        if bridge.clarify_tx.send((clarify_req, resp_tx)).is_ok() {
            resp_rx.await.ok()
        } else {
            None
        }
    } else {
        None
    };

    let skipped = maybe_resp.as_ref().is_some_and(|resp| {
        resp.answers
            .iter()
            .any(|answer| answer.answer == SKIPPED_ANSWER)
    });

    let answer_obj = match maybe_resp {
        Some(resp) if !skipped => {
            let mut answer_obj = serde_json::Map::new();
            for (idx, mapping) in field_mappings.iter().enumerate() {
                let value = match resp.answers.get(idx) {
                    Some(answer) => form_value_for_answer(mapping, answer),
                    // No answer for this field: keep the form's first option rather than an
                    // empty, invalid value.
                    None => mapping
                        .option_values
                        .first()
                        .map(|first| form_value_from_raw(mapping, first)),
                };
                if let Some(value) = value {
                    answer_obj.insert(mapping.key.clone(), value);
                }
            }
            Some(answer_obj)
        }
        _ => None,
    };

    if let Some(answer_obj) = answer_obj.filter(|a| !a.is_empty()) {
        match client
            .post(&reply_url)
            .headers(post_headers)
            .json(&json!({ "answer": answer_obj }))
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => {}
            Ok(res) => {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                tracing::warn!("OpenCode form reply failed (HTTP {status}): {body}");
            }
            Err(error) => tracing::warn!("OpenCode form reply failed: {error}"),
        }
    } else {
        // Esc means "ask me nothing": cancel the form so the service resumes instead of waiting
        // on it forever.
        match client
            .delete(&cancel_url)
            .headers(post_headers)
            .query(&[("message", "skipped by the user")])
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => {}
            Ok(res) => {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                tracing::warn!("OpenCode form cancel failed (HTTP {status}): {body}");
            }
            Err(error) => tracing::warn!("OpenCode form cancel failed: {error}"),
        }
    }
}

fn handle_permission_asked(data: &serde_json::Value, ctx: &NativeStreamContext) {
    let request_id = data
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if request_id.is_empty() {
        return;
    }
    let action = data
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("tool")
        .to_string();
    let resources: Vec<String> = data
        .get("resources")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let metadata = data
        .get("metadata")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let diff = metadata
        .get("diff")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string());
    let target = metadata
        .get("filepath")
        .and_then(|f| f.as_str())
        .map(|s| s.to_string())
        .or_else(|| resources.first().cloned());

    let bridge = get_ui_bridge();
    let client = ctx.client.clone();
    let reply_url = format!(
        "{}/api/session/{}/permission/{}/reply",
        ctx.service_url, ctx.session_id, request_id
    );
    let post_headers = ctx.headers.clone();

    tokio::spawn(async move {
        submit_permission_decision(
            bridge,
            client,
            reply_url,
            post_headers,
            action,
            request_id,
            target,
            diff,
            resources,
        )
        .await;
    });
}

#[allow(clippy::too_many_arguments)]
async fn submit_permission_decision(
    bridge: Option<OpencodeUiBridge>,
    client: reqwest::Client,
    reply_url: String,
    post_headers: HeaderMap,
    action: String,
    request_id: String,
    target: Option<String>,
    diff: Option<String>,
    resources: Vec<String>,
) {
    let is_yolo = bridge
        .as_ref()
        .map(|b| b.allow_all.load(Ordering::SeqCst))
        .unwrap_or(false);

    let decision = if is_yolo {
        "once"
    } else if let Some(bridge) = bridge {
        let arguments = if action == "shell" || action == "terminal" || action == "bash" {
            json!({ "command": target.clone().unwrap_or_default() })
        } else {
            json!({
                "path": target.clone().unwrap_or_default(),
                "diff": diff.clone().unwrap_or_default(),
                "action": action,
                "resources": resources,
            })
        };

        let approval_req = ApprovalRequest {
            tool: action,
            call_id: request_id,
            target,
            diff,
            arguments,
        };

        let (dec_tx, dec_rx) = tokio::sync::oneshot::channel();
        if bridge.approval_tx.send((approval_req, dec_tx)).is_ok() {
            match dec_rx.await {
                Ok(ApprovalDecision::Run) | Ok(ApprovalDecision::RunWithArguments(_)) => {
                    if bridge.allow_all.load(Ordering::SeqCst) {
                        "always"
                    } else {
                        "once"
                    }
                }
                _ => "reject",
            }
        } else {
            "reject"
        }
    } else {
        "reject"
    };

    let _ = client
        .post(&reply_url)
        .headers(post_headers)
        .json(&json!({ "decision": decision }))
        .send()
        .await;
}
