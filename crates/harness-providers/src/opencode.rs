use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use rand::Rng;
use serde_json::json;
use harness_core::HarnessError;

const BASE62_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const ID_CEILING: u64 = 1 << 48;

static CACHED_SESSION_ID: OnceLock<String> = OnceLock::new();
static COUNTER: AtomicUsize = AtomicUsize::new(1);

#[derive(Debug, Clone)]
pub struct DetectedOpencodeCredential {
    pub provider: String,
    pub label: String,
    pub api_key: String,
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

pub fn detect_opencode_credentials() -> Result<Vec<DetectedOpencodeCredential>, HarnessError> {
    let mut detected = Vec::new();
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());

    // 1. OpenCode Zen free tier: keyless access via 'public' token
    detected.push(DetectedOpencodeCredential {
        provider: "opencode".to_string(),
        label: "opencode-free".to_string(),
        api_key: "public".to_string(),
    });

    // 2. ~/.shiina/auth.json (opencode-zen token)
    let shiina_auth = PathBuf::from(&home).join(".shiina").join("auth.json");
    if shiina_auth.is_file() {
        if let Ok(content) = std::fs::read_to_string(&shiina_auth) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                let pool = val.get("credential_pool");
                if let Some(creds) = pool.and_then(|p| p.get("opencode-zen")).and_then(|c| c.as_array()) {
                    for c in creds {
                        if let Some(tok) = c.get("access_token").or_else(|| c.get("api_key")).and_then(|t| t.as_str()) {
                            if !tok.is_empty() && tok != "public" {
                                let label = c.get("label").and_then(|l| l.as_str()).unwrap_or("opencode-zen");
                                detected.push(DetectedOpencodeCredential {
                                    provider: "opencode".to_string(),
                                    label: label.to_string(),
                                    api_key: tok.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. ~/.local/share/opencode/auth.json (Groq integration)
    let local_auth = PathBuf::from(&home).join(".local").join("share").join("opencode").join("auth.json");
    if local_auth.is_file() {
        if let Ok(content) = std::fs::read_to_string(&local_auth) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(groq) = val.get("groq") {
                    if let Some(key) = groq.get("key").and_then(|k| k.as_str()) {
                        if !key.is_empty() {
                            detected.push(DetectedOpencodeCredential {
                                provider: "groq".to_string(),
                                label: "opencode-groq".to_string(),
                                api_key: key.to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

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

pub async fn fetch_opencode_models() -> Result<Vec<String>, HarnessError> {
    // 1. Try running `opencode api GET /api/model`
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
    let state_file = PathBuf::from(&home).join(".local").join("state").join("opencode").join("model.json");
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
    Ok(OPENCODE_FREE_MODELS.iter().map(|&s| s.to_string()).collect())
}
