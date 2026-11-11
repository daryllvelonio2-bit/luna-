use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use harness_core::types::{CompletionOptions, Message, Role, StreamEvent, ToolDefinition};
use harness_core::HarnessError;

use super::native::native_opencode_stream;
use super::service::{get_http_client, get_or_start_opencode_service, OpencodeServiceInfo};

pub async fn native_opencode_complete(
    http_client: &reqwest::Client,
    service: &OpencodeServiceInfo,
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
) -> Result<Message, HarnessError> {
    let mut stream =
        native_opencode_stream(http_client, service, model, messages, tools, options).await?;

    let mut full_text = String::new();
    let mut full_reasoning = String::new();
    let mut token_usage = None;

    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::ContentDelta(delta) => full_text.push_str(&delta),
            StreamEvent::ReasoningDelta(delta) => full_reasoning.push_str(&delta),
            StreamEvent::Completed {
                usage: Some(usage), ..
            } => token_usage = Some(usage),
            _ => {}
        }
    }

    Ok(Message {
        role: Role::Assistant,
        content: if full_text.trim().is_empty() {
            None
        } else {
            Some(full_text)
        },
        reasoning: if full_reasoning.trim().is_empty() {
            None
        } else {
            Some(full_reasoning)
        },
        tool_calls: vec![],
        tool_result: None,
        usage: token_usage,
    })
}

/// Executes a one-shot completion natively via OpenCode daemon HTTP API.
pub async fn execute_opencode_completion(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
) -> Result<Message, HarnessError> {
    let client = get_http_client();
    let service = get_or_start_opencode_service(client).await.ok_or_else(|| {
        HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: "OpenCode daemon is not running. Please start the service with 'opencode service start' or ensure ~/.local/state/opencode/service.json exists.".to_string(),
        }
    })?;

    tracing::debug!(service_url = %service.url, model = %model, "executing completion via native OpenCode daemon HTTP API");
    native_opencode_complete(client, &service, model, messages, tools, options).await
}

/// Executes a streaming completion natively via OpenCode daemon SSE API.
pub async fn stream_opencode_completion(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
    let client = get_http_client();
    let service = get_or_start_opencode_service(client).await.ok_or_else(|| {
        HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: "OpenCode daemon is not running. Please start the service with 'opencode service start' or ensure ~/.local/state/opencode/service.json exists.".to_string(),
        }
    })?;

    tracing::debug!(service_url = %service.url, model = %model, "executing stream via native OpenCode daemon SSE API");
    native_opencode_stream(client, &service, model, messages, tools, options).await
}
