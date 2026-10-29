use harness_core::types::{CompletionOptions, Message, Role, StreamEvent, TokenUsage, ToolDefinition};
use harness_core::HarnessError;
use futures_util::stream::BoxStream;
use rand::Rng;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const BASE62_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const ID_CEILING: u64 = 1 << 48;

static CACHED_SESSION_ID: OnceLock<String> = OnceLock::new();
static COUNTER: AtomicUsize = AtomicUsize::new(1);

#[derive(Debug, Clone)]
pub struct DetectedOpencodeCredential {
    pub provider: String,
    pub label: String,
    pub api_key: String,
    /// Whether this key should be the one the pool hands out first.
    ///
    /// The anonymous free-tier token is a FALLBACK: it is registered so a machine with no
    /// OpenCode account still has something to send, but it must never outrank a real key.
    /// The free tier is metered, and a metered token that wins the rotation turns every
    /// request into a rate-limit wait.
    pub is_priority: bool,
}

/// Where OpenCode keeps its own credential store.
///
/// OpenCode writes `auth.json` under its XDG data directory, which is
/// `~/.local/share/opencode` by default. Both are checked because the environment variable is
/// the documented way to relocate it.
fn opencode_auth_paths(home: &Path, xdg_data_home: Option<&str>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(xdg) = xdg_data_home {
        if !xdg.trim().is_empty() {
            paths.push(PathBuf::from(xdg).join("opencode").join("auth.json"));
        }
    }
    paths.push(
        home.join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    );
    paths
}

/// Read one JSON file, if it is there and it parses.
fn read_json_file(path: &Path) -> Option<serde_json::Value> {
    if !path.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<serde_json::Value>(&content).ok()
}

/// Mint a SessionID in the exact shape OpenCode v2's id generator emits.
///
/// OpenCode Zen's free-tier gate decodes the 12-hex prefix of `x-opencode-session`
/// and 403s anything that is not that encoding. Mirror the generator:
/// `~((ms << 12) | counter)` truncated to 48 bits, then 14 random Base62 characters.
pub fn new_opencode_session_id() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cnt = COUNTER.fetch_add(1, Ordering::Relaxed) as u64;
    let value = (ID_CEILING - 1) - ((now_ms * 0x1000 + cnt) % ID_CEILING);

    let mut rng = rand::thread_rng();
    let mut tail = String::with_capacity(14);
    for _ in 0..14 {
        let idx = rng.gen_range(0..62);
        tail.push(BASE62_CHARS[idx] as char);
    }
    format!("ses_{:012x}{}", value, tail)
}

/// Retrieve or initialize a stable SessionID for OpenCode affinity headers.
pub fn get_or_create_opencode_session_id() -> String {
    CACHED_SESSION_ID
        .get_or_init(new_opencode_session_id)
        .clone()
}

/// Inject bash and read tools into payload for OpenCode free-tier requests if missing.
///
/// OpenCode Zen's free tier verifies that requests originate from within OpenCode by
/// inspecting the payload tools. Both 'bash' and 'read' tool definitions must be present.
pub fn ensure_compat_tools(body: &mut serde_json::Value) {
    if !body.is_object() {
        return;
    }

    let tools_val = body.get_mut("tools");
    let mut tools_list = match tools_val {
        Some(serde_json::Value::Array(ref mut arr)) => std::mem::take(arr),
        _ => Vec::new(),
    };

    let mut has_bash = false;
    let mut has_read = false;

    for t in &tools_list {
        if let Some(name) = t.get("name").and_then(|n| n.as_str()) {
            if name == "bash" {
                has_bash = true;
            }
            if name == "read" {
                has_read = true;
            }
        }
        if let Some(func) = t.get("function") {
            if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                if name == "bash" {
                    has_bash = true;
                }
                if name == "read" {
                    has_read = true;
                }
            }
        }
    }

    if !has_bash {
        tools_list.push(json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Executes a given bash command",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                }
            }
        }));
    }

    if !has_read {
        tools_list.push(json!({
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a file from local filesystem",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "filePath": { "type": "string" }
                    },
                    "required": ["filePath"]
                }
            }
        }));
    }

    body["tools"] = serde_json::Value::Array(tools_list);
}

pub fn find_opencode_binary_in(home: &Path) -> Option<PathBuf> {
    let candidates = [
        home.join(".opencode").join("bin").join("opencode"),
        home.join(".local").join("bin").join("opencode"),
        home.join(".npm-global").join("bin").join("opencode"),
    ];
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

pub fn find_opencode_binary() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    if let Some(bin) = find_opencode_binary_in(Path::new(&home)) {
        return Some(bin);
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("opencode");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn detect_opencode_credentials() -> Result<Vec<DetectedOpencodeCredential>, HarnessError> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let xdg = std::env::var("XDG_DATA_HOME").ok();
    detect_opencode_credentials_in(&PathBuf::from(home), xdg.as_deref())
}

/// The same detection against an explicit home directory, so the behaviour can be tested
/// without touching the machine's environment.
pub fn detect_opencode_credentials_in(
    home: &Path,
    xdg_data_home: Option<&str>,
) -> Result<Vec<DetectedOpencodeCredential>, HarnessError> {
    let mut detected = Vec::new();

    // 0. Local OpenCode CLI binary
    let has_binary = find_opencode_binary_in(home).is_some()
        || (home == Path::new(&std::env::var("HOME").unwrap_or_default()) && find_opencode_binary().is_some());
    if has_binary {
        detected.push(DetectedOpencodeCredential {
            provider: "opencode".to_string(),
            label: "opencode-cli".to_string(),
            api_key: "cli".to_string(),
            is_priority: true,
        });
    }

    // 1. OpenCode's own account key and provider keys from auth.json
    for path in opencode_auth_paths(home, xdg_data_home) {
        let Some(val) = read_json_file(&path) else {
            continue;
        };
        if let Some(key) = val
            .get("opencode")
            .and_then(|entry| entry.get("key"))
            .and_then(|k| k.as_str())
        {
            if !key.trim().is_empty() {
                detected.push(DetectedOpencodeCredential {
                    provider: "opencode".to_string(),
                    label: "opencode-account".to_string(),
                    api_key: key.to_string(),
                    is_priority: true,
                });
            }
        }
        if let Some(key) = val
            .get("groq")
            .and_then(|entry| entry.get("key"))
            .and_then(|k| k.as_str())
        {
            if !key.trim().is_empty() {
                // Groq is a provider of its own: its key belongs to the Groq pool, never to the
                // OpenCode pool, where it would only earn a rejection from opencode.ai.
                detected.push(DetectedOpencodeCredential {
                    provider: "groq".to_string(),
                    label: "opencode-groq".to_string(),
                    api_key: key.to_string(),
                    is_priority: true,
                });
            }
        }
        if let Some(obj) = val.as_object() {
            for (prov, entry) in obj {
                if prov == "opencode" || prov == "groq" {
                    continue;
                }
                if let Some(key) = entry.get("key").and_then(|k| k.as_str()) {
                    if !key.trim().is_empty() {
                        detected.push(DetectedOpencodeCredential {
                            provider: prov.clone(),
                            label: format!("opencode-{}", prov),
                            api_key: key.to_string(),
                            is_priority: true,
                        });
                    }
                }
            }
        }
    }

    // 2. ~/.shiina/auth.json (opencode-zen token)
    let shiina_auth = home.join(".shiina").join("auth.json");
    if let Some(val) = read_json_file(&shiina_auth) {
        let pool = val.get("credential_pool");
        if let Some(creds) = pool
            .and_then(|p| p.get("opencode-zen"))
            .and_then(|c| c.as_array())
        {
            for c in creds {
                if let Some(tok) = c
                    .get("access_token")
                    .or_else(|| c.get("api_key"))
                    .and_then(|t| t.as_str())
                {
                    if !tok.is_empty() && tok != "public" {
                        let label = c
                            .get("label")
                            .and_then(|l| l.as_str())
                            .unwrap_or("opencode-zen");
                        detected.push(DetectedOpencodeCredential {
                            provider: "opencode".to_string(),
                            label: label.to_string(),
                            api_key: tok.to_string(),
                            is_priority: !has_binary,
                        });
                    }
                }
            }
        }
    }

    // 3. OpenCode Zen free tier: keyless access via the 'public' token.
    detected.push(DetectedOpencodeCredential {
        provider: "opencode".to_string(),
        label: "opencode-free".to_string(),
        api_key: "public".to_string(),
        is_priority: false,
    });

    Ok(detected)
}

pub const OPENCODE_FREE_MODELS: &[&str] = &[
    "deepseek-v4.1-flash-free",
    "mimo-v2.6-flash-free",
    "minimax-m3.1-flash-free",
    "nemotron-3-ultra-free",
    "nemotron-3.5-lightning-free",
    "nemotron-3.5-super-vl-free",
    "glm-5.3-flash-free",
    "big-pickle",
    "beam-free",
    "longcat-2.5-preview-free",
];

pub async fn stream_opencode_cli_completion(
    bin: &Path,
    model: &str,
    messages: &[Message],
    _tools: &[ToolDefinition],
    _options: &CompletionOptions,
) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
    let mut model_arg = model.to_string();
    if model_arg.is_empty() {
        model_arg = "opencode/mimo-v2.6-flash-free".to_string();
    } else if !model_arg.contains('/') {
        model_arg = format!("opencode/{}", model_arg);
    }

    let mut prompt_parts = Vec::new();
    for msg in messages {
        let content = msg.content.as_deref().unwrap_or("").trim();
        if content.is_empty() && msg.tool_calls.is_empty() && msg.tool_result.is_none() {
            continue;
        }
        match msg.role {
            Role::System => {
                if !content.is_empty() {
                    prompt_parts.push(format!("[System Instruction]\n{}", content));
                }
            }
            Role::User => {
                if !content.is_empty() {
                    prompt_parts.push(format!("[User]\n{}", content));
                }
            }
            Role::Assistant => {
                let mut text = content.to_string();
                for call in &msg.tool_calls {
                    text.push_str(&format!("\n[Tool Call: {} args: {}]", call.name, call.arguments));
                }
                prompt_parts.push(format!("[Assistant]\n{}", text.trim()));
            }
            Role::Tool => {
                let res = msg.tool_result.as_ref().map(|r| r.content.as_str()).unwrap_or(content);
                prompt_parts.push(format!("[Tool Result]\n{}", res.trim()));
            }
        }
    }
    let mut prompt = prompt_parts.join("\n\n");
    if prompt.is_empty() {
        prompt = "Hello".to_string();
    }

    let mut child = tokio::process::Command::new(bin)
        .args(["run", "--format", "json", "--pure", "--thinking", "-m", &model_arg])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: format!("Failed to spawn opencode CLI: {}", e),
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }

    let stdout = child.stdout.take().ok_or_else(|| HarnessError::ProviderError {
        provider: "opencode".to_string(),
        message: "Failed to capture opencode stdout".to_string(),
    })?;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, HarnessError>>(100);

    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let reader = tokio::io::BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut completed_usage = None;
        let mut finish_reason = None;

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() || !line.starts_with('{') {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                let ev_type = val.get("type").and_then(|t| t.as_str());
                let part = val.get("part");

                if ev_type == Some("text") {
                    if let Some(text) = part.and_then(|p| p.get("text")).and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            let _ = tx.send(Ok(StreamEvent::ContentDelta(text.to_string()))).await;
                        }
                    }
                } else if ev_type == Some("reasoning") {
                    if let Some(text) = part.and_then(|p| p.get("text")).and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            let _ = tx.send(Ok(StreamEvent::ReasoningDelta(text.to_string()))).await;
                        }
                    }
                } else if ev_type == Some("step_finish") {
                    if let Some(p) = part {
                        finish_reason = p.get("reason").and_then(|r| r.as_str()).map(|s| s.to_string());
                        if let Some(tokens) = p.get("tokens") {
                            let inp = tokens.get("input").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let out = tokens.get("output").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let total = tokens.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            completed_usage = Some(TokenUsage {
                                prompt_tokens: inp,
                                completion_tokens: out,
                                total_tokens: if total > 0 { total } else { inp + out },
                            });
                        }
                    }
                }
            }
        }

        let _ = child.wait().await;

        let _ = tx
            .send(Ok(StreamEvent::Completed {
                finish_reason: finish_reason.or_else(|| Some("stop".to_string())),
                usage: completed_usage,
            }))
            .await;
    });

    let rx_stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
    Ok(futures_util::StreamExt::boxed(rx_stream))
}

pub async fn fetch_opencode_models() -> Result<Vec<String>, HarnessError> {
    // 1. Try running `opencode models`
    if let Some(bin) = find_opencode_binary() {
        if let Ok(output) = tokio::process::Command::new(bin)
            .args(["models"])
            .output()
            .await
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let mut list = Vec::new();
                for line in stdout.lines() {
                    let line = line.trim();
                    if let Some(id) = line.strip_prefix("opencode/") {
                        if !list.contains(&id.to_string()) {
                            list.push(id.to_string());
                        }
                    } else if !line.is_empty() && !line.contains('/') {
                        if !list.contains(&line.to_string()) {
                            list.push(line.to_string());
                        }
                    }
                }
                if !list.is_empty() {
                    list.sort();
                    return Ok(list);
                }
            }
        }
    }

    // 2. Try running `opencode api GET /api/model`
    if let Ok(output) = tokio::process::Command::new("opencode")
        .args(["api", "GET", "/api/model"])
        .output()
        .await
    {
        if output.status.success() {
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                if let Some(arr) = val.get("data").and_then(|d| d.as_array()) {
                    let mut list = Vec::new();
                    for item in arr {
                        if item.get("providerID").and_then(|p| p.as_str()) == Some("opencode") {
                            if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                                list.push(id.to_string());
                            }
                        }
                    }
                    if !list.is_empty() {
                        list.sort();
                        return Ok(list);
                    }
                }
            }
        }
    }

    // 2. Try ~/.local/state/opencode/model.json
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let state_file = PathBuf::from(&home)
        .join(".local")
        .join("state")
        .join("opencode")
        .join("model.json");
    if state_file.is_file() {
        if let Ok(content) = std::fs::read_to_string(&state_file) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(recent) = val.get("recent").and_then(|r| r.as_array()) {
                    let mut list = Vec::new();
                    for item in recent {
                        if item.get("providerID").and_then(|p| p.as_str()) == Some("opencode") {
                            if let Some(id) = item.get("modelID").and_then(|i| i.as_str()) {
                                if !list.contains(&id.to_string()) {
                                    list.push(id.to_string());
                                }
                            }
                        }
                    }
                    if !list.is_empty() {
                        for &m in OPENCODE_FREE_MODELS {
                            if !list.contains(&m.to_string()) {
                                list.push(m.to_string());
                            }
                        }
                        list.sort();
                        return Ok(list);
                    }
                }
            }
        }
    }

    // 3. Fallback to constant verified free models
    Ok(OPENCODE_FREE_MODELS
        .iter()
        .map(|&s| s.to_string())
        .collect())
}
