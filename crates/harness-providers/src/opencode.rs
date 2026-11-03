use eventsource_stream::Eventsource;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use harness_core::types::{
    CompletionOptions, Message, Role, StreamEvent, TokenUsage, ToolDefinition,
};
use harness_core::HarnessError;
use rand::Rng;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

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

/// Mint a RequestID in the exact shape OpenCode v2's id generator emits.
pub fn new_opencode_request_id() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cnt = COUNTER.fetch_add(1, Ordering::Relaxed) as u64;
    let current = (now_ms * 0x1000 + cnt) % ID_CEILING;

    let mut rng = rand::thread_rng();
    let mut tail = String::with_capacity(14);
    for _ in 0..14 {
        let idx = rng.gen_range(0..62);
        tail.push(BASE62_CHARS[idx] as char);
    }
    format!("msg_{:012x}{}", current, tail)
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
                            is_priority: true,
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

/// Discover the installed OpenCode binary across standard platform locations or env.
pub fn find_opencode_binary() -> Result<PathBuf, HarnessError> {
    // 1. Env vars
    for env_var in ["OPENCODE_BIN", "SHIINA_OPENCODE_COMMAND"] {
        if let Ok(bin_str) = std::env::var(env_var) {
            let path = PathBuf::from(bin_str.trim());
            if path.is_file() {
                return Ok(path);
            }
        }
    }

    // 2. Standard user home locations
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    if let Some(ref h) = home {
        let candidates = [
            h.join(".opencode/bin/opencode"),
            h.join(".local/bin/opencode"),
            h.join(".npm-global/bin/opencode"),
        ];
        for cand in candidates {
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }

    // 3. System locations
    let sys_candidates = [
        PathBuf::from("/usr/local/bin/opencode"),
        PathBuf::from("/usr/bin/opencode"),
    ];
    for cand in sys_candidates {
        if cand.is_file() {
            return Ok(cand);
        }
    }

    // 4. Windows paths
    if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
        let cand = PathBuf::from(local_app).join("opencode/bin/opencode.exe");
        if cand.is_file() {
            return Ok(cand);
        }
    }
    if let Ok(app_data) = std::env::var("APPDATA") {
        let cand = PathBuf::from(app_data).join("npm/opencode.cmd");
        if cand.is_file() {
            return Ok(cand);
        }
    }

    // 5. Look in PATH directories
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let cand = dir.join("opencode");
            if cand.is_file() {
                return Ok(cand);
            }
            #[cfg(windows)]
            {
                let cand_exe = dir.join("opencode.exe");
                if cand_exe.is_file() {
                    return Ok(cand_exe);
                }
            }
        }
    }

    Err(HarnessError::ProviderError {
        provider: "opencode".to_string(),
        message: "OpenCode binary not found. Please install opencode or set OPENCODE_BIN."
            .to_string(),
    })
}

/// Normalizes model ID so OpenCode CLI gets the expected provider/model reference
/// (e.g. `opencode/deepseek-v4.1-flash-free`).
pub fn normalize_opencode_model(model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return "opencode/deepseek-v4.1-flash-free".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("opencode-cli/") {
        return if rest.contains('/') {
            rest.to_string()
        } else {
            format!("opencode/{}", rest)
        };
    }
    if trimmed.contains('/') {
        return trimmed.to_string();
    }
    format!("opencode/{}", trimmed)
}

/// Formats the conversation history into a clear prompt structure for `opencode run`.
pub fn format_messages_for_opencode(messages: &[Message]) -> String {
    let mut parts = Vec::new();
    for msg in messages {
        let role_label = match msg.role {
            Role::System => "System Instruction",
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => "Tool Output",
        };
        let mut text = String::new();
        if let Some(ref content) = msg.content {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                text.push_str(trimmed);
            }
        }
        if let Some(ref result) = msg.tool_result {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!(
                "[tool_call_id: {}] {}",
                result.call_id, result.content
            ));
        }
        if !msg.tool_calls.is_empty() {
            for call in &msg.tool_calls {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("[Tool Call: {}({})]", call.name, call.arguments));
            }
        }
        if !text.is_empty() {
            parts.push(format!("[{}]\n{}", role_label, text));
        }
    }
    if parts.is_empty() {
        "Hello".to_string()
    } else {
        parts.join("\n\n")
    }
}


fn parse_tokens(part: &serde_json::Value) -> Option<TokenUsage> {
    let tokens = part.get("tokens")?;
    let prompt_tokens = tokens.get("input").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let completion_tokens = tokens.get("output").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let total_tokens = tokens
        .get("total")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens));
    let cached_tokens = tokens
        .get("cache")
        .and_then(|c| c.get("read"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    Some(TokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cached_tokens,
    })
}

#[derive(Debug, Clone)]
pub struct OpencodeServiceInfo {
    pub url: String,
    pub password: Option<String>,
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i];
        let b1 = if i + 1 < input.len() { input[i + 1] } else { 0 };
        let b2 = if i + 2 < input.len() { input[i + 2] } else { 0 };
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if i + 1 < input.len() {
            out.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < input.len() {
            out.push(TABLE[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub fn get_http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    })
}

pub fn read_opencode_service_info() -> Option<OpencodeServiceInfo> {
    if let Ok(url) = std::env::var("OPENCODE_SERVICE_URL") {
        if !url.trim().is_empty() {
            let password = std::env::var("OPENCODE_SERVICE_PASSWORD").ok();
            return Some(OpencodeServiceInfo {
                url: url.trim().to_string(),
                password,
            });
        }
    }

    let mut candidates = Vec::new();
    if let Ok(xdg_state) = std::env::var("XDG_STATE_HOME") {
        candidates.push(
            PathBuf::from(xdg_state)
                .join("opencode")
                .join("service.json"),
        );
    }
    if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
        candidates.push(
            PathBuf::from(xdg_config)
                .join("opencode")
                .join("service.json"),
        );
    }
    if let Ok(home) = std::env::var("HOME") {
        let home_path = PathBuf::from(home);
        candidates.push(
            home_path
                .join(".local")
                .join("state")
                .join("opencode")
                .join("service.json"),
        );
        candidates.push(
            home_path
                .join(".config")
                .join("opencode")
                .join("service.json"),
        );
    }

    for path in candidates {
        if let Some(val) = read_json_file(&path) {
            if let Some(url) = val.get("url").and_then(|u| u.as_str()) {
                if !url.trim().is_empty() {
                    let password = val
                        .get("password")
                        .and_then(|p| p.as_str())
                        .map(|s| s.to_string());
                    return Some(OpencodeServiceInfo {
                        url: url.trim().to_string(),
                        password,
                    });
                }
            }
        }
    }

    None
}

async fn check_service_alive(client: &reqwest::Client, info: &OpencodeServiceInfo) -> bool {
    let test_url = format!("{}/api/model", info.url.trim_end_matches('/'));
    let mut req = client
        .get(&test_url)
        .timeout(std::time::Duration::from_millis(1500));
    if let Some(ref pwd) = info.password {
        let auth_str = format!(
            "Basic {}",
            base64_encode(format!("opencode:{}", pwd).as_bytes())
        );
        req = req.header(AUTHORIZATION, auth_str);
    }
    match req.send().await {
        Ok(resp) => {
            resp.status().is_success() || resp.status() == reqwest::StatusCode::UNAUTHORIZED
        }
        Err(_) => false,
    }
}

pub async fn get_or_start_opencode_service(
    client: &reqwest::Client,
) -> Option<OpencodeServiceInfo> {
    if let Some(info) = read_opencode_service_info() {
        if check_service_alive(client, &info).await {
            return Some(info);
        }
    }

    if let Ok(bin) = find_opencode_binary() {
        tracing::debug!(
            "OpenCode background service not active; attempting `opencode service start`"
        );
        if let Ok(mut child) = Command::new(&bin)
            .args(["service", "start"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            let _ = child.wait().await;
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            if let Some(info) = read_opencode_service_info() {
                if check_service_alive(client, &info).await {
                    return Some(info);
                }
            }
        }
    }

    None
}

pub fn parse_opencode_model_ref(model: &str) -> (String, String) {
    let trimmed = model.trim();
    let clean = trimmed.strip_prefix("opencode-cli/").unwrap_or(trimmed);
    if let Some((prov, id)) = clean.split_once('/') {
        (prov.to_string(), id.to_string())
    } else if clean.is_empty() {
        (
            "opencode".to_string(),
            "deepseek-v4.1-flash-free".to_string(),
        )
    } else {
        ("opencode".to_string(), clean.to_string())
    }
}

pub async fn native_opencode_stream(
    http_client: &reqwest::Client,
    service: &OpencodeServiceInfo,
    model: &str,
    messages: &[Message],
    _tools: &[ToolDefinition],
    _options: &CompletionOptions,
) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
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

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let (provider_id, model_id) = parse_opencode_model_ref(model);

    // 1. Establish SSE stream first so no events are missed
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

    // 2. Create Session
    let mut ses_headers = auth_headers.clone();
    ses_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Ok(dir_val) = HeaderValue::from_str(&cwd) {
        ses_headers.insert("x-opencode-directory", dir_val);
    }

    let create_session_url = format!("{}/api/session", service.url.trim_end_matches('/'));
    let create_payload = json!({
        "model": {
            "providerID": provider_id,
            "id": model_id
        }
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

    let session_id = ses_json
        .get("data")
        .and_then(|d| d.get("id"))
        .and_then(|i| i.as_str())
        .ok_or_else(|| HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: "Missing session ID in OpenCode response".to_string(),
        })?
        .to_string();

    // 3. Send Prompt
    let prompt_text = format_messages_for_opencode(messages);
    let prompt_url = format!(
        "{}/api/session/{}/prompt",
        service.url.trim_end_matches('/'),
        session_id
    );
    let prompt_payload = json!({
        "text": prompt_text,
        "model": {
            "providerID": provider_id,
            "id": model_id
        }
    });

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

    // 4. Transform SSE Events
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, HarnessError>>(100);
    let clean_client = http_client.clone();
    let clean_url = format!(
        "{}/api/session/{}",
        service.url.trim_end_matches('/'),
        session_id
    );
    let clean_headers = auth_headers.clone();
    let target_session_id = session_id.clone();

    tokio::spawn(async move {
        let mut event_source = sse_resp.bytes_stream().eventsource();
        let mut last_usage = None;
        let mut last_finish_reason = Some("stop".to_string());
        let mut ended_successfully = false;
        let mut saw_deltas = false;
        let mut tool_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut tool_inputs: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();

        while let Some(item) = event_source.next().await {
            match item {
                Ok(event) => {
                    let data_str = event.data.trim();
                    if data_str.is_empty() || data_str == "[DONE]" {
                        continue;
                    }
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data_str) {
                        let ev_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        let data = parsed.get("data").unwrap_or(&serde_json::Value::Null);
                        let event_session_id = data.get("sessionID").and_then(|s| s.as_str());

                        if event_session_id != Some(&target_session_id) {
                            continue;
                        }

                        match ev_type {
                            "session.reasoning.delta" => {
                                if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
                                    if !delta.is_empty() {
                                        saw_deltas = true;
                                        if tx
                                            .send(Ok(StreamEvent::ReasoningDelta(
                                                delta.to_string(),
                                            )))
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                }
                            }
                            "session.text.delta" => {
                                if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
                                    if !delta.is_empty() {
                                        saw_deltas = true;
                                        if tx
                                            .send(Ok(StreamEvent::ContentDelta(delta.to_string())))
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                }
                            }
                            "session.tool.input.started" => {
                                if let Some(id) = data.get("id").and_then(|v| v.as_str()) {
                                    if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
                                        tool_names.insert(id.to_string(), name.to_string());
                                    }
                                }
                            }
                            "session.tool.input.ended" => {
                                if let Some(id) = data.get("id").and_then(|v| v.as_str()) {
                                    if let Some(text) = data.get("text").and_then(|v| v.as_str()) {
                                        if let Ok(parsed_input) =
                                            serde_json::from_str::<serde_json::Value>(text)
                                        {
                                            tool_inputs.insert(id.to_string(), parsed_input);
                                        }
                                    }
                                }
                            }
                            "session.tool.called" => {
                                let id = data.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                let name = data
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .map(|s| s.to_string())
                                    .or_else(|| tool_names.get(id).cloned())
                                    .unwrap_or_else(|| "tool".to_string());
                                let input = data
                                    .get("input")
                                    .cloned()
                                    .or_else(|| tool_inputs.get(id).cloned())
                                    .unwrap_or(serde_json::Value::Null);
                                saw_deltas = true;
                                if tx
                                    .send(Ok(StreamEvent::ToolExecuting {
                                        name,
                                        arguments: input,
                                    }))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            "session.tool.success" => {
                                let id = data.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                let name = data
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .map(|s| s.to_string())
                                    .or_else(|| tool_names.get(id).cloned())
                                    .unwrap_or_else(|| "tool".to_string());
                                let mut output = String::new();
                                if let Some(content) =
                                    data.get("content").and_then(|c| c.as_array())
                                {
                                    for item in content {
                                        if let Some(text) =
                                            item.get("text").and_then(|t| t.as_str())
                                        {
                                            output.push_str(text);
                                        }
                                    }
                                }
                                if output.is_empty() {
                                    if let Some(out) = data.get("output").and_then(|o| o.as_str()) {
                                        output.push_str(out);
                                    }
                                }
                                saw_deltas = true;
                                if tx
                                    .send(Ok(StreamEvent::ToolExecuted {
                                        name,
                                        output,
                                        is_error: false,
                                    }))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            "session.tool.failed" => {
                                let id = data.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                let name = data
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .map(|s| s.to_string())
                                    .or_else(|| tool_names.get(id).cloned())
                                    .unwrap_or_else(|| "tool".to_string());
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
                                saw_deltas = true;
                                if tx
                                    .send(Ok(StreamEvent::ToolExecuted {
                                        name,
                                        output: error_msg,
                                        is_error: true,
                                    }))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            "session.step.ended" => {
                                if let Some(finish) = data.get("finish").and_then(|f| f.as_str()) {
                                    last_finish_reason = Some(finish.to_string());
                                }
                                if let Some(tokens) = data.get("tokens") {
                                    let prompt_tokens =
                                        tokens.get("input").and_then(|v| v.as_u64()).unwrap_or(0)
                                            as u32;
                                    let completion_tokens =
                                        tokens.get("output").and_then(|v| v.as_u64()).unwrap_or(0)
                                            as u32;
                                    let reasoning_tokens = tokens
                                        .get("reasoning")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0)
                                        as u32;
                                    let cached_tokens = tokens
                                        .get("cache")
                                        .and_then(|c| c.get("read"))
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0)
                                        as u32;
                                    let total_tokens = prompt_tokens
                                        .saturating_add(completion_tokens)
                                        .saturating_add(reasoning_tokens);
                                    last_usage = Some(TokenUsage {
                                        prompt_tokens,
                                        completion_tokens,
                                        total_tokens,
                                        cached_tokens,
                                    });
                                }
                            }
                            "session.usage.updated" => {
                                if let Some(tokens) = data.get("tokens") {
                                    let prompt_tokens =
                                        tokens.get("input").and_then(|v| v.as_u64()).unwrap_or(0)
                                            as u32;
                                    let completion_tokens =
                                        tokens.get("output").and_then(|v| v.as_u64()).unwrap_or(0)
                                            as u32;
                                    let reasoning_tokens = tokens
                                        .get("reasoning")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0)
                                        as u32;
                                    let cached_tokens = tokens
                                        .get("cache")
                                        .and_then(|c| c.get("read"))
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0)
                                        as u32;
                                    let total_tokens = prompt_tokens
                                        .saturating_add(completion_tokens)
                                        .saturating_add(reasoning_tokens);
                                    last_usage = Some(TokenUsage {
                                        prompt_tokens,
                                        completion_tokens,
                                        total_tokens,
                                        cached_tokens,
                                    });
                                }
                            }
                            "session.execution.succeeded" => {
                                ended_successfully = true;
                                let _ = tx
                                    .send(Ok(StreamEvent::Completed {
                                        finish_reason: last_finish_reason.clone(),
                                        usage: last_usage.clone(),
                                    }))
                                    .await;
                                break;
                            }
                            "session.execution.failed" => {
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
                                break;
                            }
                            _ => {}
                        }
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

        if !ended_successfully && saw_deltas {
            let _ = tx
                .send(Ok(StreamEvent::Completed {
                    finish_reason: last_finish_reason,
                    usage: last_usage,
                }))
                .await;
        }

        let _ = clean_client
            .delete(&clean_url)
            .headers(clean_headers)
            .send()
            .await;
    });

    let rx_stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
    Ok(Box::pin(rx_stream))
}

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
            StreamEvent::Completed { usage, .. } => {
                if usage.is_some() {
                    token_usage = usage;
                }
            }
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

/// Executes a one-shot completion via `opencode run`.
pub async fn execute_opencode_completion_cli(
    model: &str,
    messages: &[Message],
    _tools: &[ToolDefinition],
    _options: &CompletionOptions,
) -> Result<Message, HarnessError> {
    let binary = find_opencode_binary()?;
    let target_model = normalize_opencode_model(model);
    let prompt = format_messages_for_opencode(messages);

    let mut cmd = Command::new(&binary);
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

    let mut child = cmd.spawn().map_err(|e| HarnessError::ProviderError {
        provider: "opencode".to_string(),
        message: format!(
            "Failed to spawn opencode process ({}): {}",
            binary.display(),
            e
        ),
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes()).await;
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

    let stderr = child.stderr.take();

    let mut reader = BufReader::new(stdout).lines();
    let mut full_text = String::new();
    let mut full_reasoning = String::new();
    let mut token_usage = None;
    let mut last_error = None;

    while let Ok(Some(line)) = reader.next_line().await {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }

        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
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
                        token_usage = Some(usage);
                    }
                }
                "error" => {
                    let msg = val
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .or_else(|| val.get("message").and_then(|m| m.as_str()))
                        .unwrap_or("Unknown OpenCode error");
                    last_error = Some(msg.to_string());
                }
                _ => {}
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| HarnessError::ProviderError {
            provider: "opencode".to_string(),
            message: format!("Failed to wait on opencode process: {}", e),
        })?;

    if !status.success() && full_text.trim().is_empty() {
        let stderr_msg = if let Some(stderr) = stderr {
            let mut err_reader = BufReader::new(stderr).lines();
            let mut err_lines = Vec::new();
            while let Ok(Some(line)) = err_reader.next_line().await {
                err_lines.push(line);
            }
            err_lines.join("\n")
        } else {
            String::new()
        };

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

/// Executes a streaming completion via `opencode run`.
pub async fn stream_opencode_completion_cli(
    model: &str,
    messages: &[Message],
    _tools: &[ToolDefinition],
    _options: &CompletionOptions,
) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
    let binary = find_opencode_binary()?;
    let target_model = normalize_opencode_model(model);
    let prompt = format_messages_for_opencode(messages);

    let mut cmd = Command::new(&binary);
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

    let mut child = cmd.spawn().map_err(|e| HarnessError::ProviderError {
        provider: "opencode".to_string(),
        message: format!(
            "Failed to spawn opencode process ({}): {}",
            binary.display(),
            e
        ),
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes()).await;
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

    let stderr = child.stderr.take();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, HarnessError>>(100);

    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        let mut saw_output = false;
        let mut last_error = None;
        let mut last_usage = None;
        let mut last_finish_reason = Some("stop".to_string());

        while let Ok(Some(line)) = reader.next_line().await {
            let line = line.trim();
            if line.is_empty() || !line.starts_with('{') {
                continue;
            }

            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                let ev_type = val.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let part = val.get("part").unwrap_or(&serde_json::Value::Null);

                match ev_type {
                    "text" => {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                saw_output = true;
                                if tx
                                    .send(Ok(StreamEvent::ContentDelta(text.to_string())))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                    "reasoning" => {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                saw_output = true;
                                if tx
                                    .send(Ok(StreamEvent::ReasoningDelta(text.to_string())))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                    "tool_use" => {
                        let tool_name = part
                            .get("tool")
                            .and_then(|t| t.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        let state = part.get("state").unwrap_or(&serde_json::Value::Null);
                        let input = state
                            .get("input")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let status = state.get("status").and_then(|s| s.as_str());
                        let output = state
                            .get("output")
                            .and_then(|o| o.as_str())
                            .unwrap_or("")
                            .to_string();
                        let is_error = status == Some("failed") || status == Some("error");

                        saw_output = true;
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
                    "step_finish" => {
                        if let Some(reason) = part.get("reason").and_then(|r| r.as_str()) {
                            last_finish_reason = Some(reason.to_string());
                        }
                        if let Some(usage) = parse_tokens(part) {
                            last_usage = Some(usage);
                        }
                    }
                    "error" => {
                        let msg = val
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .or_else(|| val.get("message").and_then(|m| m.as_str()))
                            .unwrap_or("Unknown OpenCode error");
                        last_error = Some(msg.to_string());
                    }
                    _ => {}
                }
            }
        }

        let status = child.wait().await;
        let success = status.as_ref().map(|s| s.success()).unwrap_or(false);

        if (!success || last_error.is_some()) && !saw_output {
            let stderr_msg = if let Some(stderr) = stderr {
                let mut err_reader = BufReader::new(stderr).lines();
                let mut err_lines = Vec::new();
                while let Ok(Some(line)) = err_reader.next_line().await {
                    err_lines.push(line);
                }
                err_lines.join("\n")
            } else {
                String::new()
            };

            let err_detail = last_error.unwrap_or_else(|| {
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
                finish_reason: last_finish_reason,
                usage: last_usage,
            }))
            .await;
    });

    let rx_stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
    Ok(Box::pin(rx_stream))
}

pub async fn fetch_opencode_models() -> Result<Vec<String>, HarnessError> {
    let client = get_http_client();
    if let Some(service) = get_or_start_opencode_service(client).await {
        let test_url = format!("{}/api/model", service.url.trim_end_matches('/'));
        let mut req = client.get(&test_url);
        if let Some(ref pwd) = service.password {
            let auth_str = format!(
                "Basic {}",
                base64_encode(format!("opencode:{}", pwd).as_bytes())
            );
            if let Ok(val) = HeaderValue::from_str(&auth_str) {
                req = req.header(AUTHORIZATION, val);
            }
        }
        if let Ok(resp) = req.send().await {
            if resp.status().is_success() {
                if let Ok(val) = resp.json::<serde_json::Value>().await {
                    if let Some(arr) = val.get("data").and_then(|d| d.as_array()) {
                        let mut list = Vec::new();
                        for item in arr {
                            if item.get("providerID").and_then(|p| p.as_str()) == Some("opencode") {
                                if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                                    if !list.contains(&id.to_string()) {
                                        list.push(id.to_string());
                                    }
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
    }

    let bin = find_opencode_binary().unwrap_or_else(|_| PathBuf::from("opencode"));

    // 1. Try running `opencode models`
    if let Ok(output) = tokio::process::Command::new(&bin)
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
                } else if !line.is_empty()
                    && !line.contains('/')
                    && !list.contains(&line.to_string())
                {
                    list.push(line.to_string());
                }
            }
            if !list.is_empty() {
                list.sort();
                return Ok(list);
            }
        }
    }

    // 2. Try running `opencode api GET /api/model`
    if let Ok(output) = tokio::process::Command::new(&bin)
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

    // 3. Try ~/.local/state/opencode/model.json
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

    // 4. Fallback to constant verified free models
    Ok(OPENCODE_FREE_MODELS
        .iter()
        .map(|&s| s.to_string())
        .collect())
}

/// Fetches models available via the OpenCode CLI (`opencode models`).
pub async fn fetch_opencode_cli_models() -> Result<Vec<String>, HarnessError> {
    let bin = find_opencode_binary().unwrap_or_else(|_| PathBuf::from("opencode"));

    if let Ok(output) = tokio::process::Command::new(&bin)
        .args(["models"])
        .output()
        .await
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut list = Vec::new();
            for line in stdout.lines() {
                let line = line.trim();
                if line.starts_with("opencode/") {
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

    Ok(OPENCODE_FREE_MODELS
        .iter()
        .map(|&s| format!("opencode/{}", s))
        .collect())
}
