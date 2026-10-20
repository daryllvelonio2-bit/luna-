use std::path::PathBuf;
use std::process::Stdio;
use futures_util::stream::{self, BoxStream, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use harness_core::types::{
    CompletionOptions, Message, Role, StreamEvent, TokenUsage, ToolDefinition,
};
use harness_core::HarnessError;
use crate::auth::CredentialManager;

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

pub fn find_kiro_bin() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("KIRO_BIN") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let candidates = [
        PathBuf::from(&home).join(".local/bin/kiro-cli"),
        PathBuf::from(&home).join(".local/bin/kiro"),
        PathBuf::from("/usr/local/bin/kiro-cli"),
        PathBuf::from("/usr/bin/kiro-cli"),
    ];
    for c in &candidates {
        if c.exists() {
            return Some(c.clone());
        }
    }
    if let Ok(out) = std::process::Command::new("which").arg("kiro-cli").output() {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    None
}

pub fn extract_kiro_token_from_db() -> Option<String> {
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
        return json_val
            .get("access_token")
            .and_then(|t| t.as_str())
            .map(str::to_string);
    }
    None
}

pub async fn sync_kiro_credential_if_needed(cred_manager: &CredentialManager) -> Result<(), HarnessError> {
    let token = match extract_kiro_token_from_db() {
        Some(t) => t,
        None => return Ok(()),
    };

    for prov in ["kiro", "kiro-cli"] {
        if cred_manager.acquire_credential(prov).await.is_err() {
            let _ = cred_manager
                .add_credential(prov, "kiro-social", &token, true)
                .await;
        }
    }
    Ok(())
}

pub async fn fetch_kiro_models() -> Result<Vec<String>, HarnessError> {
    if let Some(bin) = find_kiro_bin() {
        if let Ok(output) = std::process::Command::new(&bin)
            .args(["chat", "--list-models", "--format", "json"])
            .output()
        {
            if output.status.success() {
                if let Ok(data) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                    if let Some(models) = data.get("models").and_then(|m| m.as_array()) {
                        let mut names = Vec::new();
                        for m in models {
                            if let Some(id) = m.get("model_id").and_then(|id| id.as_str()) {
                                names.push(id.to_string());
                            }
                        }
                        if !names.is_empty() {
                            return Ok(names);
                        }
                    }
                }
            }
        }
    }

    Ok(KIRO_MODELS.iter().map(|s| s.to_string()).collect())
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut in_esc = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_esc = true;
        } else if in_esc {
            if c.is_ascii_alphabetic() {
                in_esc = false;
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn clean_kiro_output(raw: &str) -> String {
    let clean = strip_ansi(raw);
    let mut content_lines = Vec::new();
    let mut found_content = false;

    for line in clean.lines() {
        let stripped = line.trim();
        if !found_content {
            if let Some(after) = stripped.strip_prefix("> ") {
                found_content = true;
                content_lines.push(after.to_string());
                continue;
            } else if let Some(after) = stripped.strip_prefix('>') {
                found_content = true;
                content_lines.push(after.trim_start().to_string());
                continue;
            } else if stripped.starts_with("WARNING:")
                || stripped.contains("needs to be prepended")
                || stripped.contains("All tools are now trusted")
                || stripped.is_empty()
            {
                continue;
            } else {
                found_content = true;
                content_lines.push(line.to_string());
                continue;
            }
        }

        if (stripped.contains("Credits:") && (stripped.contains("Time:") || stripped.contains('▸')))
            || stripped.starts_with("▸ Credits:")
        {
            break;
        }

        if let Some(after) = stripped.strip_prefix("> ") {
            content_lines.push(after.to_string());
        } else if stripped == ">" {
            content_lines.push(String::new());
        } else {
            content_lines.push(line.to_string());
        }
    }

    content_lines.join("\n").trim().to_string()
}

pub fn extract_thinking(raw: &str) -> (Option<String>, String) {
    if let Some(start) = raw.find("<thinking>").or_else(|| raw.find("<thought>")) {
        let tag_len = if raw[start..].starts_with("<thinking>") {
            "<thinking>".len()
        } else {
            "<thought>".len()
        };
        let end_tag = if tag_len == "<thinking>".len() {
            "</thinking>"
        } else {
            "</thought>"
        };
        if let Some(end) = raw[start + tag_len..].find(end_tag) {
            let thought = raw[start + tag_len..start + tag_len + end].trim().to_string();
            let mut text = String::new();
            text.push_str(&raw[..start]);
            text.push_str(&raw[start + tag_len + end + end_tag.len()..]);
            return (Some(thought), text.trim().to_string());
        }
    }
    (None, raw.trim().to_string())
}

pub fn format_messages_for_kiro(messages: &[Message], tools: &[ToolDefinition]) -> String {
    let mut sections = Vec::new();

    if !tools.is_empty() {
        let mut tool_lines = vec!["Available tools:".to_string()];
        for t in tools {
            tool_lines.push(format!("- {}: {} (parameters: {})", t.name, t.description, t.parameters));
        }
        sections.push(tool_lines.join("\n"));
    }

    for msg in messages {
        let role = match msg.role {
            Role::System => "System Instructions",
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => "Tool Result",
        };

        let mut body = msg.content.clone().unwrap_or_default();
        if let Some(ref r) = msg.reasoning {
            body = format!("<thinking>\n{}\n</thinking>\n{}", r, body);
        }
        for tc in &msg.tool_calls {
            body.push_str(&format!(
                "\n<tool_call>{{\"name\":\"{}\",\"arguments\":{}}}</tool_call>",
                tc.name, tc.arguments
            ));
        }

        sections.push(format!("[{}]\n{}", role, body));
    }

    sections.join("\n\n")
}

pub async fn stream_kiro_completion(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    _options: &CompletionOptions,
) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
    let bin = find_kiro_bin().ok_or_else(|| HarnessError::ProviderError {
        provider: "kiro-cli".to_string(),
        message: "kiro-cli executable not found in PATH or ~/.local/bin/kiro-cli".to_string(),
    })?;

    let prompt = format_messages_for_kiro(messages, tools);
    let resolved_model = if model.is_empty() { "auto" } else { model };

    let mut child = Command::new(&bin)
        .args([
            "chat",
            "--no-interactive",
            "--trust-tools=",
            "--model",
            resolved_model,
        ])
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| HarnessError::ProviderError {
            provider: "kiro-cli".to_string(),
            message: format!("Failed to spawn kiro-cli: {}", e),
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes()).await.map_err(|e| {
            HarnessError::ProviderError {
                provider: "kiro-cli".to_string(),
                message: format!("Failed to write prompt to kiro-cli stdin: {}", e),
            }
        })?;
    }

    let output = child.wait_with_output().await.map_err(|e| {
        HarnessError::ProviderError {
            provider: "kiro-cli".to_string(),
            message: format!("kiro-cli execution failed: {}", e),
        }
    })?;

    let raw_out = String::from_utf8_lossy(&output.stdout).to_string();
    let cleaned = clean_kiro_output(&raw_out);

    if cleaned.is_empty() && !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(HarnessError::ProviderError {
            provider: "kiro-cli".to_string(),
            message: format!("kiro-cli exited with error: {}", err),
        });
    }

    let (thinking, text) = extract_thinking(&cleaned);

    let prompt_tokens = (prompt.len() / 4).max(1) as u32;
    let completion_tokens = (text.len() / 4).max(1) as u32;

    let mut events = Vec::new();
    if let Some(th) = thinking {
        events.push(Ok(StreamEvent::ReasoningDelta(th)));
    }
    if !text.is_empty() {
        events.push(Ok(StreamEvent::ContentDelta(text.clone())));
    }
    events.push(Ok(StreamEvent::Completed {
        finish_reason: Some("stop".to_string()),
        usage: Some(TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        }),
    }));

    Ok(stream::iter(events).boxed())
}

pub async fn execute_kiro_completion(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: &CompletionOptions,
) -> Result<Message, HarnessError> {
    let mut stream = stream_kiro_completion(model, messages, tools, options).await?;
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

    Ok(Message {
        role: Role::Assistant,
        content: if full_text.is_empty() { None } else { Some(full_text) },
        reasoning: if thought.is_empty() { None } else { Some(thought) },
        tool_calls: Vec::new(),
        tool_result: None,
        usage: final_usage,
    })
}
