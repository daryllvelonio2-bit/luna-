use futures_util::stream::BoxStream;
use harness_core::types::{
    CompletionOptions, Message, Role, StreamEvent, TokenUsage, ToolDefinition,
};
use harness_core::HarnessError;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::binary::{find_opencode_binary, opencode_command};
use super::models::normalize_opencode_model;
use super::prompt::format_opencode_prompt;
use super::tools::normalize_tool_arguments;
use super::ui::get_ui_bridge;
use super::usage::{add_usage, parse_tokens};

/// Executes a one-shot completion via `opencode run`.
/// Temporary files holding the decoded pasted images for `opencode run --file`.
///
/// The CLI takes paths, not data URLs, so the bytes have to exist somewhere the child can read
/// them. Deleting them on drop keeps a chat session from leaving screenshots behind in the
/// temp directory.
pub(super) struct AttachmentFiles(Vec<PathBuf>);

impl AttachmentFiles {
    pub(super) fn write(files: &[serde_json::Value]) -> Result<Self, HarnessError> {
        use base64::Engine as _;

        let mut paths = Vec::with_capacity(files.len());
        for (index, file) in files.iter().enumerate() {
            let uri = file
                .get("uri")
                .and_then(|uri| uri.as_str())
                .unwrap_or_default();
            let Some((mime, payload)) = uri
                .strip_prefix("data:")
                .and_then(|rest| rest.split_once(";base64,"))
            else {
                continue;
            };
            let extension = mime.rsplit('/').next().unwrap_or("png");
            let path = std::env::temp_dir().join(format!(
                "luna-opencode-{}-{}-{}.{}",
                std::process::id(),
                index,
                uuid::Uuid::new_v4(),
                extension
            ));
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .map_err(|e| HarnessError::ProviderError {
                    provider: "opencode".to_string(),
                    message: format!("Failed to decode pasted image for opencode: {}", e),
                })?;
            std::fs::write(&path, bytes).map_err(|e| HarnessError::ProviderError {
                provider: "opencode".to_string(),
                message: format!("Failed to write pasted image to {}: {}", path.display(), e),
            })?;
            paths.push(path);
        }
        Ok(Self(paths))
    }

    pub(super) fn paths(&self) -> impl Iterator<Item = &Path> {
        self.0.iter().map(PathBuf::as_path)
    }
}

impl Drop for AttachmentFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A spawned `opencode run` with its output readers and the attachment files it reads.
struct OpencodeRun {
    child: tokio::process::Child,
    stdout: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stderr: Option<tokio::process::ChildStderr>,
    /// Kept alive for as long as the child that reads the decoded images.
    attachments: AttachmentFiles,
}

/// Spawn `opencode run` with the prompt on stdin and the pasted images as files.
async fn start_opencode_run(
    model: &str,
    messages: &[Message],
) -> Result<OpencodeRun, HarnessError> {
    let binary = find_opencode_binary()?;
    let target_model = normalize_opencode_model(model);
    let prompt = format_opencode_prompt(messages);
    let attachments = AttachmentFiles::write(&prompt.files)?;

    let mut cmd = opencode_command(&binary);
    cmd.args([
        "run",
        "--format",
        "json",
        "--thinking",
        "--auto",
        "-m",
        &target_model,
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    for path in attachments.paths() {
        cmd.arg("--file").arg(path);
    }

    let mut child = cmd.spawn().map_err(|e| HarnessError::ProviderError {
        provider: "opencode".to_string(),
        message: format!(
            "Failed to spawn opencode process ({}): {}",
            binary.display(),
            e
        ),
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.text.as_bytes()).await;
        let _ = stdin.flush().await;
        drop(stdin);
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: "Failed to open stdout of opencode process".to_string(),
        })?;

    Ok(OpencodeRun {
        stderr: child.stderr.take(),
        child,
        stdout: BufReader::new(stdout).lines(),
        attachments,
    })
}

/// Read whatever the child wrote to stderr, joined line by line.
async fn collect_stderr(stderr: Option<tokio::process::ChildStderr>) -> String {
    let Some(stderr) = stderr else {
        return String::new();
    };
    let mut err_reader = BufReader::new(stderr).lines();
    let mut err_lines = Vec::new();
    while let Ok(Some(line)) = err_reader.next_line().await {
        err_lines.push(line);
    }
    err_lines.join("\n")
}

/// Fold one JSON line of `execute` output into the accumulating reply.
fn apply_cli_event(
    val: &serde_json::Value,
    full_text: &mut String,
    full_reasoning: &mut String,
    token_usage: &mut Option<TokenUsage>,
    last_error: &mut Option<String>,
) {
    let ev_type = val.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let part = val.get("part").unwrap_or(&serde_json::Value::Null);

    match ev_type {
        "text" => {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                full_text.push_str(text);
            }
        }
        "reasoning" => {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                full_reasoning.push_str(text);
            }
        }
        "tool_use" => {}
        "step_finish" => {
            if let Some(usage) = parse_tokens(part) {
                add_usage(token_usage, &usage);
            }
        }
        "error" => {
            let msg = val
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .or_else(|| val.get("message").and_then(|m| m.as_str()))
                .unwrap_or("Unknown OpenCode error");
            *last_error = Some(msg.to_string());
        }
        _ => {}
    }
}

pub async fn execute_opencode_completion_cli(
    model: &str,
    messages: &[Message],
    _tools: &[ToolDefinition],
    _options: &CompletionOptions,
) -> Result<Message, HarnessError> {
    let mut run = start_opencode_run(model, messages).await?;

    let mut full_text = String::new();
    let mut full_reasoning = String::new();
    let mut token_usage = None;
    let mut last_error = None;

    while let Ok(Some(line)) = run.stdout.next_line().await {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }

        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            apply_cli_event(
                &val,
                &mut full_text,
                &mut full_reasoning,
                &mut token_usage,
                &mut last_error,
            );
        }
    }

    let status = run
        .child
        .wait()
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: format!("Failed to wait on opencode process: {}", e),
        })?;

    if !status.success() && full_text.trim().is_empty() {
        let stderr_msg = collect_stderr(run.stderr.take()).await;

        let err_detail = last_error.unwrap_or_else(|| {
            if !stderr_msg.is_empty() {
                stderr_msg
            } else {
                format!("Process exited with status code {:?}", status.code())
            }
        });

        return Err(HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: format!("opencode run failed: {}", err_detail),
        });
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

/// Whether the streaming reader should keep reading or stop after an event.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CliFlow {
    Continue,
    Stop,
}

struct CliStreamState {
    saw_output: bool,
    last_error: Option<String>,
    last_usage: Option<TokenUsage>,
    total_usage: Option<TokenUsage>,
    last_finish_reason: Option<String>,
}

impl CliStreamState {
    fn new() -> Self {
        Self {
            saw_output: false,
            last_error: None,
            last_usage: None,
            total_usage: None,
            last_finish_reason: Some("stop".to_string()),
        }
    }
}

async fn emit_cli_text_delta(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    part: &serde_json::Value,
    state: &mut CliStreamState,
) -> CliFlow {
    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
        if !text.is_empty() {
            state.saw_output = true;
            if tx
                .send(Ok(StreamEvent::ContentDelta(text.to_string())))
                .await
                .is_err()
            {
                return CliFlow::Stop;
            }
        }
    }
    CliFlow::Continue
}

async fn emit_cli_reasoning_delta(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    part: &serde_json::Value,
    state: &mut CliStreamState,
) -> CliFlow {
    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
        if !text.is_empty() {
            state.saw_output = true;
            if tx
                .send(Ok(StreamEvent::ReasoningDelta(text.to_string())))
                .await
                .is_err()
            {
                return CliFlow::Stop;
            }
        }
    }
    CliFlow::Continue
}

async fn emit_cli_tool_use(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    part: &serde_json::Value,
    state: &mut CliStreamState,
) {
    let tool_name = part
        .get("tool")
        .and_then(|t| t.as_str())
        .unwrap_or("tool")
        .to_string();
    let tool_state = part.get("state").unwrap_or(&serde_json::Value::Null);
    let mut input = tool_state
        .get("input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    normalize_tool_arguments(&mut input);
    let status = tool_state.get("status").and_then(|s| s.as_str());
    let output = tool_state
        .get("output")
        .and_then(|o| o.as_str())
        .unwrap_or("")
        .to_string();
    let is_error = status == Some("failed") || status == Some("error");

    state.saw_output = true;
    let _ = tx
        .send(Ok(StreamEvent::ToolExecuting {
            name: tool_name.clone(),
            arguments: input,
        }))
        .await;
    if status == Some("completed") || is_error {
        let _ = tx
            .send(Ok(StreamEvent::ToolExecuted {
                name: tool_name,
                output,
                is_error,
            }))
            .await;
    }
}

/// Emit one usage report and update the context gauge with it.
async fn emit_cli_usage(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    usage: &TokenUsage,
    state: &mut CliStreamState,
) {
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

async fn handle_cli_step_finish(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    part: &serde_json::Value,
    val: &serde_json::Value,
    state: &mut CliStreamState,
) {
    if let Some(reason) = part.get("reason").and_then(|r| r.as_str()) {
        state.last_finish_reason = Some(reason.to_string());
    }
    if let Some(usage) = parse_tokens(part).or_else(|| parse_tokens(val)) {
        add_usage(&mut state.total_usage, &usage);
        emit_cli_usage(tx, &usage, state).await;
    }
}

async fn handle_cli_usage_event(
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    part: &serde_json::Value,
    val: &serde_json::Value,
    state: &mut CliStreamState,
) {
    if let Some(usage) = parse_tokens(part).or_else(|| parse_tokens(val)) {
        emit_cli_usage(tx, &usage, state).await;
    }
}

async fn handle_cli_stream_event(
    val: &serde_json::Value,
    tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, HarnessError>>,
    state: &mut CliStreamState,
) -> CliFlow {
    let ev_type = val.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let part = val.get("part").unwrap_or(&serde_json::Value::Null);

    match ev_type {
        "text" => emit_cli_text_delta(tx, part, state).await,
        "reasoning" => emit_cli_reasoning_delta(tx, part, state).await,
        "tool_use" => {
            emit_cli_tool_use(tx, part, state).await;
            CliFlow::Continue
        }
        "step_finish" => {
            handle_cli_step_finish(tx, part, val, state).await;
            CliFlow::Continue
        }
        "usage" => {
            handle_cli_usage_event(tx, part, val, state).await;
            CliFlow::Continue
        }
        "error" => {
            let msg = val
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .or_else(|| val.get("message").and_then(|m| m.as_str()))
                .unwrap_or("Unknown OpenCode error");
            state.last_error = Some(msg.to_string());
            CliFlow::Continue
        }
        _ => CliFlow::Continue,
    }
}

/// Executes a streaming completion via `opencode run`.
pub async fn stream_opencode_completion_cli(
    model: &str,
    messages: &[Message],
    _tools: &[ToolDefinition],
    _options: &CompletionOptions,
) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
    let run = start_opencode_run(model, messages).await?;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, HarnessError>>(100);

    tokio::spawn(async move {
        // Keep the decoded attachment files alive for as long as the child that reads them.
        let OpencodeRun {
            mut child,
            mut stdout,
            stderr,
            attachments: _attachments,
        } = run;
        let mut state = CliStreamState::new();

        while let Ok(Some(line)) = stdout.next_line().await {
            let line = line.trim();
            if line.is_empty() || !line.starts_with('{') {
                continue;
            }

            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                if handle_cli_stream_event(&val, &tx, &mut state).await == CliFlow::Stop {
                    break;
                }
            }
        }

        let status = child.wait().await;
        let success = status.as_ref().map(|s| s.success()).unwrap_or(false);

        if (!success || state.last_error.is_some()) && !state.saw_output {
            let stderr_msg = collect_stderr(stderr).await;

            let err_detail = state.last_error.unwrap_or_else(|| {
                if !stderr_msg.is_empty() {
                    stderr_msg
                } else {
                    format!("Process exited with status {:?}", status.map(|s| s.code()))
                }
            });

            let _ = tx
                .send(Err(HarnessError::ProviderError {
                    provider: "opencode".to_string(),
                    message: format!("opencode run failed: {}", err_detail),
                }))
                .await;
            return;
        }

        // Send completed event at the end
        let _ = tx
            .send(Ok(StreamEvent::Completed {
                finish_reason: state.last_finish_reason,
                usage: state.total_usage.or(state.last_usage),
            }))
            .await;
    });

    let rx_stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
    Ok(Box::pin(rx_stream))
}
