use eventsource_stream::Eventsource;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use harness_core::approval::{ApprovalDecision, ApprovalRequest};
use harness_core::types::{
    CompletionOptions, Message, Role, StreamEvent, TokenUsage, ToolDefinition,
};
use harness_core::{ClarifyAnswer, ClarifyItem, ClarifyRequest, ClarifyResponse, HarnessError};
use rand::Rng;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Clone)]
pub struct OpencodeUiBridge {
    pub clarify_tx: tokio::sync::mpsc::UnboundedSender<(
        ClarifyRequest,
        tokio::sync::oneshot::Sender<ClarifyResponse>,
    )>,
    pub approval_tx: tokio::sync::mpsc::UnboundedSender<(
        ApprovalRequest,
        tokio::sync::oneshot::Sender<ApprovalDecision>,
    )>,
    pub allow_all: Arc<AtomicBool>,
    pub update_context_used_fn: Option<Arc<dyn Fn(u32) + Send + Sync>>,
}

static UI_BRIDGE: OnceLock<RwLock<Option<OpencodeUiBridge>>> = OnceLock::new();

pub fn set_ui_bridge(bridge: OpencodeUiBridge) {
    let cell = UI_BRIDGE.get_or_init(|| RwLock::new(None));
    if let Ok(mut lock) = cell.write() {
        *lock = Some(bridge);
    }
}

pub fn get_ui_bridge() -> Option<OpencodeUiBridge> {
    UI_BRIDGE
        .get()
        .and_then(|lock| lock.read().ok())
        .and_then(|guard| guard.clone())
}

/// How a form field's answer reaches OpenCode.
///
/// The API accepts a scalar for a string/number/boolean field and an array only for a
/// multiselect. Sending an array for every field — which this bridge used to do — is rejected
/// with `FormInvalidAnswerError`, and the form then stays pending forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormFieldKind {
    Scalar,
    Multi,
}

/// One field of an OpenCode form, kept so the user's answer can be encoded as the API expects.
#[derive(Debug, Clone)]
struct FormFieldMapping {
    key: String,
    field_type: String,
    kind: FormFieldKind,
    /// The option *values* (not the labels shown to the user), in choice order.
    option_values: Vec<String>,
}

/// The marker `prompt_clarify_choice` uses when the user pressed Esc instead of answering.
///
/// A skipped prompt is a cancelled form, not an answer: posting the marker as a value would be
/// invalid and leave the service waiting.
const SKIPPED_ANSWER: &str = "(skipped by user)";

fn parse_opencode_form_to_clarify(
    form: &serde_json::Value,
) -> (ClarifyRequest, Vec<FormFieldMapping>) {
    let mut questions = Vec::new();
    let mut field_mappings = Vec::new();

    if let Some(fields) = form.get("fields").and_then(|f| f.as_array()) {
        for (i, field) in fields.iter().enumerate() {
            let key = field
                .get("key")
                .and_then(|k| k.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("q{}", i));
            let field_type = field
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("string")
                .to_string();

            let title = field.get("title").and_then(|t| t.as_str()).unwrap_or("");
            let desc = field
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("");

            let question_text = if !title.is_empty() && !desc.is_empty() && title != desc {
                format!("{}: {}", title, desc)
            } else if !desc.is_empty() {
                desc.to_string()
            } else if !title.is_empty() {
                title.to_string()
            } else {
                "Please select an option:".to_string()
            };

            let mut choices = Vec::new();
            let mut option_values = Vec::new();

            if let Some(opts) = field.get("options").and_then(|o| o.as_array()) {
                for opt in opts {
                    let value = opt
                        .get("value")
                        .and_then(|v| v.as_str())
                        .or_else(|| opt.get("label").and_then(|l| l.as_str()))
                        .unwrap_or("");
                    if !value.is_empty() {
                        // The reply carries the value; the label is only what the box shows.
                        let label = opt.get("label").and_then(|l| l.as_str()).unwrap_or(value);
                        option_values.push(value.to_string());
                        if let Some(opt_desc) = opt
                            .get("description")
                            .and_then(|d| d.as_str())
                            .filter(|s| !s.is_empty())
                        {
                            choices.push(format!("{} — {}", label, opt_desc));
                        } else {
                            choices.push(label.to_string());
                        }
                    }
                }
            }

            // A boolean field has no options in the wire format, but its answer must still be a
            // bool. Offer the two values so the box is usable instead of asking for free text.
            if choices.is_empty() && field_type == "boolean" {
                choices = vec!["Yes".to_string(), "No".to_string()];
                option_values = vec!["true".to_string(), "false".to_string()];
            }

            questions.push(ClarifyItem {
                question: question_text,
                choices,
            });
            field_mappings.push(FormFieldMapping {
                key,
                kind: if field_type == "multiselect" {
                    FormFieldKind::Multi
                } else {
                    FormFieldKind::Scalar
                },
                field_type,
                option_values,
            });
        }
    }

    (ClarifyRequest { questions }, field_mappings)
}

/// Encode one raw answer as the JSON value the form's field type expects.
fn form_value_from_raw(mapping: &FormFieldMapping, raw: &str) -> serde_json::Value {
    let value = match mapping.field_type.as_str() {
        "boolean" => json!(matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "true" | "yes" | "y" | "1"
        )),
        "number" => match raw.trim().parse::<f64>() {
            Ok(number) => json!(number),
            Err(_) => json!(raw),
        },
        "integer" => match raw.trim().parse::<i64>() {
            Ok(number) => json!(number),
            Err(_) => json!(raw),
        },
        _ => json!(raw),
    };
    // Only multiselect fields take the array shape; everything else is scalar.
    if mapping.kind == FormFieldKind::Multi {
        json!([value])
    } else {
        value
    }
}

/// Turn one answered clarify item into the value for its form field.
fn form_value_for_answer(
    mapping: &FormFieldMapping,
    answer: &ClarifyAnswer,
) -> Option<serde_json::Value> {
    let raw = match answer.choice_index {
        Some(index) => mapping.option_values.get(index)?,
        None => &answer.answer,
    };
    Some(form_value_from_raw(mapping, raw))
}

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
    let home = harness_core::config::home_dir()
        .to_string_lossy()
        .into_owned();
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

/// A tokio command for the OpenCode CLI binary.
///
/// npm and yarn install the CLI as a `.cmd` shim on Windows, and `CreateProcess` cannot execute
/// that directly. Those go through `cmd /C`; a native `.exe` is spawned as-is.
fn opencode_command(bin: &Path) -> Command {
    #[cfg(windows)]
    {
        let is_script = bin
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
        if is_script {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C").arg(bin);
            return cmd;
        }
    }
    Command::new(bin)
}

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
    let home = Some(harness_core::config::home_dir());
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
        // npm/yarn install the CLI as an extension-carrying shim on Windows, so the Unix name
        // alone never matches.
        #[cfg(windows)]
        {
            let windows_candidates = [
                h.join(".opencode").join("bin").join("opencode.exe"),
                h.join("AppData")
                    .join("Roaming")
                    .join("npm")
                    .join("opencode.cmd"),
            ];
            for cand in windows_candidates {
                if cand.is_file() {
                    return Ok(cand);
                }
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
                for ext in ["cmd", "bat", "ps1"] {
                    let shim = dir.join(format!("opencode.{ext}"));
                    if shim.is_file() {
                        return Ok(shim);
                    }
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

/// A prompt in the shape the OpenCode routes take it: the conversation as text, and the images
/// pasted into the newest user turn as attachments beside it.
pub struct OpencodePrompt {
    pub text: String,
    pub files: Vec<serde_json::Value>,
}

/// Formats the conversation history for the OpenCode routes.
///
/// A `data:image/...` URL inside the text is not an image to OpenCode — it is hundreds of
/// kilobytes of base64 the model reads as prose, and, depending on how the server splits the
/// message, it can push the instruction out of what the model actually sees. OpenCode has a
/// dedicated attachment field for pixels (`files`), so the URLs are lifted out of the text and
/// handed over there instead.
///
/// Only the newest user turn's images are attached. Images from earlier turns stay in the
/// transcript as the `[image: WxH]` marker the CLI wrote beside them: re-uploading every pasted
/// screenshot on every request would grow each prompt by hundreds of kilobytes and buy the model
/// a second look it cannot act on anymore.
pub fn format_opencode_prompt(messages: &[Message]) -> OpencodePrompt {
    // The turn's instruction is the newest user message, wherever the tool loop has left the
    // tail of the vector. Images in anything older are history; only this turn's pixels are sent.
    let attach_to = messages.iter().rposition(|msg| msg.role == Role::User);

    let mut parts = Vec::new();
    let mut files = Vec::new();
    for (index, msg) in messages.iter().enumerate() {
        let role_label = match msg.role {
            Role::System => "System Instruction",
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => "Tool Output",
        };
        let mut text = String::new();
        if let Some(ref content) = msg.content {
            let (without_images, images) = split_data_urls(content);
            let trimmed = without_images.trim();
            if !trimmed.is_empty() {
                text.push_str(trimmed);
            }
            if Some(index) == attach_to {
                for url in images {
                    files.push(image_attachment(&url, files.len() + 1));
                }
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
    let text = if parts.is_empty() {
        "Hello".to_string()
    } else {
        parts.join("\n\n")
    };
    OpencodePrompt { text, files }
}

/// The text half of [`format_opencode_prompt`], for callers that cannot forward attachments.
pub fn format_messages_for_opencode(messages: &[Message]) -> String {
    format_opencode_prompt(messages).text
}

/// One `data:image/...;base64,...` URL in the prompt, in the shape OpenCode's `files` field
/// takes: the URL itself is the URI, and the name only exists so a saved attachment has an
/// extension that matches its MIME type.
fn image_attachment(url: &str, ordinal: usize) -> serde_json::Value {
    let subtype = url
        .trim_start_matches("data:image/")
        .split([';', ','])
        .next()
        .unwrap_or("png");
    json!({
        "uri": url,
        "name": format!("pasted-image-{}.{}", ordinal, subtype),
        "description": "image pasted into the chat prompt",
    })
}

/// Split every real inline image URL out of `text`, returning the remaining prose and the URLs.
///
/// A URL runs to the next whitespace and only counts when it carries `;base64,` — the same rule
/// the OpenAI-compatible serializer uses, so prose that mentions `data:image/png` in passing is
/// left alone.
fn split_data_urls(text: &str) -> (String, Vec<String>) {
    if !text.contains("data:image/") {
        return (text.to_string(), Vec::new());
    }

    let mut cleaned = String::with_capacity(text.len());
    let mut urls = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("data:image/") {
        let after = &rest[start..];
        let end = after.find(char::is_whitespace).unwrap_or(after.len());
        if !after[..end].contains(";base64,") {
            cleaned.push_str(&rest[..start + "data:image/".len()]);
            rest = &after["data:image/".len()..];
            continue;
        }
        cleaned.push_str(&rest[..start]);
        urls.push(after[..end].to_string());
        rest = &after[end..];
    }
    cleaned.push_str(rest);
    (cleaned, urls)
}

/// Normalize OpenCode's `tokens` object into the crate's one usage shape.
///
/// OpenCode reports `input` as the *uncached* remainder and puts the cache read beside it under
/// `cache.read` — so `input` and `cache.read` add up to the prompt, they do not overlap. Reasoning
/// is billed as output but counted separately, so it joins `completion`. Folding both here keeps a
/// later cost, cache-share or context check from having to know which route wrote the row.
fn usage_from_tokens(tokens: &serde_json::Value) -> TokenUsage {
    let count = |keys: &[&str]| -> u32 {
        for k in keys {
            if let Some(v) = tokens.get(*k).and_then(|v| v.as_u64()) {
                return v as u32;
            }
        }
        0
    };
    let prompt_tokens = count(&["input", "prompt", "prompt_tokens", "input_tokens"]);
    let completion_tokens = count(&["output", "completion", "completion_tokens", "output_tokens"])
        .saturating_add(count(&["reasoning", "reasoning_tokens"]));
    let total_tokens = count(&["total", "total_tokens"]);
    let cached_tokens = tokens
        .get("cache")
        .and_then(|c| c.get("read").or_else(|| c.get("read_tokens")))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or_else(|| count(&["cache_read", "cached_tokens", "cache_read_input_tokens"]));

    TokenUsage::with_separate_cache(
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cached_tokens,
    )
}

/// Add one step's report to a call total.
///
/// The headless route emits one `step_finish` per inner step and no cumulative event, so the
/// ledger has to sum them itself. The context gauge keeps the per-step figure (only the newest
/// prompt is the context); only the bill is cumulative.
fn add_usage(total: &mut Option<TokenUsage>, usage: &TokenUsage) {
    match total {
        Some(sum) => {
            sum.prompt_tokens = sum.prompt_tokens.saturating_add(usage.prompt_tokens);
            sum.completion_tokens = sum
                .completion_tokens
                .saturating_add(usage.completion_tokens);
            sum.total_tokens = sum.total_tokens.saturating_add(usage.total_tokens);
            sum.cached_tokens = sum.cached_tokens.saturating_add(usage.cached_tokens);
        }
        None => *total = Some(usage.clone()),
    }
}

/// The size of the prompt one native step sent: what the context gauge may show.
///
/// Only `session.step.ended` carries this. `session.usage.updated` looks similar but is the
/// session's running total, so treating it as a context size made the gauge climb past the model
/// window as OpenCode's inner loop ran.
fn step_context_usage(ev_type: &str, data: &serde_json::Value) -> Option<TokenUsage> {
    matches!(ev_type, "session.step.ended" | "session.next.step.ended")
        .then(|| parse_tokens(data))
        .flatten()
}

/// The usage to bill for the whole native call. This *is* the cumulative figure, and the session
/// is created per call, so the newest value describes every step OpenCode ran for it.
fn session_total_usage(ev_type: &str, data: &serde_json::Value) -> Option<TokenUsage> {
    matches!(
        ev_type,
        "session.usage.updated" | "session.next.usage.updated"
    )
    .then(|| parse_tokens(data))
    .flatten()
}

pub fn normalize_tool_arguments(input: &mut serde_json::Value) {
    if let serde_json::Value::Object(ref mut map) = input {
        // Path normalization
        if let Some(v) = map.get("filePath").cloned() {
            map.entry("path".to_string()).or_insert_with(|| v.clone());
            map.entry("filepath".to_string())
                .or_insert_with(|| v.clone());
            map.entry("file".to_string()).or_insert(v);
        } else if let Some(v) = map.get("path").cloned() {
            map.entry("filePath".to_string())
                .or_insert_with(|| v.clone());
            map.entry("filepath".to_string())
                .or_insert_with(|| v.clone());
            map.entry("file".to_string()).or_insert(v);
        } else if let Some(v) = map.get("filepath").cloned() {
            map.entry("filePath".to_string())
                .or_insert_with(|| v.clone());
            map.entry("path".to_string()).or_insert_with(|| v.clone());
            map.entry("file".to_string()).or_insert(v);
        } else if let Some(v) = map.get("file").cloned() {
            map.entry("filePath".to_string())
                .or_insert_with(|| v.clone());
            map.entry("path".to_string()).or_insert_with(|| v.clone());
            map.entry("filepath".to_string()).or_insert(v);
        }

        // Old string normalization
        if let Some(v) = map.get("oldString").cloned() {
            map.entry("old_string".to_string())
                .or_insert_with(|| v.clone());
            map.entry("old_str".to_string())
                .or_insert_with(|| v.clone());
            map.entry("old".to_string()).or_insert(v);
        } else if let Some(v) = map.get("old_string").cloned() {
            map.entry("oldString".to_string())
                .or_insert_with(|| v.clone());
            map.entry("old_str".to_string())
                .or_insert_with(|| v.clone());
            map.entry("old".to_string()).or_insert(v);
        }

        // New string normalization
        if let Some(v) = map.get("newString").cloned() {
            map.entry("new_string".to_string())
                .or_insert_with(|| v.clone());
            map.entry("new_str".to_string())
                .or_insert_with(|| v.clone());
            map.entry("new".to_string()).or_insert(v);
        } else if let Some(v) = map.get("new_string").cloned() {
            map.entry("newString".to_string())
                .or_insert_with(|| v.clone());
            map.entry("new_str".to_string())
                .or_insert_with(|| v.clone());
            map.entry("new".to_string()).or_insert(v);
        }

        // Replace all normalization
        if let Some(v) = map.get("replaceAll").cloned() {
            map.entry("replace_all".to_string()).or_insert(v);
        } else if let Some(v) = map.get("replace_all").cloned() {
            map.entry("replaceAll".to_string()).or_insert(v);
        }

        // Patch / Diff normalization
        if let Some(v) = map.get("patchText").cloned() {
            map.entry("diff".to_string()).or_insert_with(|| v.clone());
            map.entry("patch".to_string()).or_insert(v);
        } else if let Some(v) = map.get("diff").cloned() {
            map.entry("patchText".to_string())
                .or_insert_with(|| v.clone());
            map.entry("patch".to_string()).or_insert(v);
        } else if let Some(v) = map.get("patch").cloned() {
            map.entry("patchText".to_string())
                .or_insert_with(|| v.clone());
            map.entry("diff".to_string()).or_insert(v);
        }
    }
}

fn parse_tokens(part: &serde_json::Value) -> Option<TokenUsage> {
    if let Some(tokens) = part
        .get("tokens")
        .or_else(|| part.get("usage"))
        .or_else(|| part.get("tokenUsage"))
    {
        Some(usage_from_tokens(tokens))
    } else if part.get("input").is_some()
        || part.get("prompt_tokens").is_some()
        || part.get("total").is_some()
    {
        Some(usage_from_tokens(part))
    } else {
        None
    }
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
    {
        let home_path = harness_core::config::home_dir();
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
        if let Ok(mut child) = opencode_command(&bin)
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

    let is_yolo = get_ui_bridge()
        .map(|b| b.allow_all.load(Ordering::SeqCst))
        .unwrap_or(false);

    let permissions_payload = if is_yolo {
        json!([
            { "action": "*", "resource": "*", "effect": "allow" }
        ])
    } else {
        json!([
            { "action": "*", "resource": "*", "effect": "allow" },
            { "action": "edit", "resource": "*", "effect": "ask" },
            { "action": "write", "resource": "*", "effect": "ask" },
            { "action": "patch", "resource": "*", "effect": "ask" },
            { "action": "delete", "resource": "*", "effect": "ask" },
            { "action": "shell", "resource": "*", "effect": "ask" },
            { "action": "terminal", "resource": "*", "effect": "ask" },
            { "action": "bash", "resource": "*", "effect": "ask" }
        ])
    };

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

    // 4. Transform SSE Events
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, HarnessError>>(100);
    let clean_client = http_client.clone();
    let service_url = service.url.trim_end_matches('/').to_string();
    let clean_url = format!("{}/api/session/{}", service_url, session_id);
    let mut clean_headers = auth_headers.clone();
    clean_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let target_session_id = session_id.clone();

    tokio::spawn(async move {
        let mut event_source = sse_resp.bytes_stream().eventsource();
        let mut last_usage = None;
        // `session.usage.updated` reports the running totals for the whole OpenCode session. This
        // connection creates a fresh session per completion, so the newest figure is this call's
        // total and belongs in the ledger; it is *not* the size of the context, which is what the
        // last step reported and what the gauge shows.
        let mut total_usage = None;
        let mut last_finish_reason = Some("stop".to_string());
        let mut ended_successfully = false;
        let mut saw_deltas = false;
        let mut tool_names: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut tool_inputs: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();

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
                        let event_session_id = data
                            .get("sessionID")
                            .or_else(|| data.get("sessionId"))
                            .or_else(|| data.get("session_id"))
                            .or_else(|| data.get("session").and_then(|s| s.get("id")))
                            .or_else(|| {
                                data.get("form")
                                    .and_then(|f| f.get("sessionID").or_else(|| f.get("sessionId")))
                            })
                            .or_else(|| parsed.get("sessionID").or_else(|| parsed.get("sessionId")))
                            .and_then(|s| s.as_str());

                        if event_session_id != Some(&target_session_id) {
                            continue;
                        }

                        match ev_type {
                            "session.reasoning.delta" | "session.next.reasoning.delta" => {
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
                            "session.text.delta" | "session.next.text.delta" => {
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
                            "session.tool.input.started" | "session.next.tool.input.started" => {
                                let id = data
                                    .get("id")
                                    .or_else(|| data.get("callID"))
                                    .or_else(|| data.get("callId"))
                                    .and_then(|v| v.as_str());
                                if let Some(id) = id {
                                    if let Some(name) = data
                                        .get("name")
                                        .or_else(|| data.get("tool"))
                                        .and_then(|v| v.as_str())
                                    {
                                        tool_names.insert(id.to_string(), name.to_string());
                                    }
                                }
                            }
                            "session.tool.input.ended" | "session.next.tool.input.ended" => {
                                let id = data
                                    .get("id")
                                    .or_else(|| data.get("callID"))
                                    .or_else(|| data.get("callId"))
                                    .and_then(|v| v.as_str());
                                if let Some(id) = id {
                                    if let Some(text) = data
                                        .get("text")
                                        .or_else(|| data.get("arguments"))
                                        .and_then(|v| v.as_str())
                                    {
                                        if let Ok(mut parsed_input) =
                                            serde_json::from_str::<serde_json::Value>(text)
                                        {
                                            normalize_tool_arguments(&mut parsed_input);
                                            tool_inputs.insert(id.to_string(), parsed_input);
                                        }
                                    } else if let Some(mut input_obj) =
                                        data.get("input").or_else(|| data.get("arguments")).cloned()
                                    {
                                        normalize_tool_arguments(&mut input_obj);
                                        tool_inputs.insert(id.to_string(), input_obj);
                                    }
                                }
                            }
                            "session.tool.called" | "session.next.tool.called" => {
                                let id = data
                                    .get("id")
                                    .or_else(|| data.get("callID"))
                                    .or_else(|| data.get("callId"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let name = data
                                    .get("name")
                                    .or_else(|| data.get("tool"))
                                    .and_then(|n| n.as_str())
                                    .map(|s| s.to_string())
                                    .or_else(|| tool_names.get(id).cloned())
                                    .unwrap_or_else(|| "tool".to_string());
                                let mut input = data
                                    .get("input")
                                    .or_else(|| data.get("arguments"))
                                    .cloned()
                                    .or_else(|| tool_inputs.get(id).cloned())
                                    .unwrap_or(serde_json::Value::Null);
                                normalize_tool_arguments(&mut input);
                                tool_inputs.insert(id.to_string(), input.clone());
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
                            "session.tool.success" | "session.next.tool.success" => {
                                let id = data
                                    .get("id")
                                    .or_else(|| data.get("callID"))
                                    .or_else(|| data.get("callId"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let name = data
                                    .get("name")
                                    .or_else(|| data.get("tool"))
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
                                    if let Some(out) = data
                                        .get("output")
                                        .or_else(|| data.get("result"))
                                        .and_then(|o| o.as_str())
                                    {
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
                            "session.tool.failed" | "session.next.tool.failed" => {
                                let id = data
                                    .get("id")
                                    .or_else(|| data.get("callID"))
                                    .or_else(|| data.get("callId"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let name = data
                                    .get("name")
                                    .or_else(|| data.get("tool"))
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
                            "session.step.ended" | "session.next.step.ended" => {
                                if let Some(finish) = data
                                    .get("finish")
                                    .or_else(|| data.get("finishReason"))
                                    .or_else(|| data.get("reason"))
                                    .and_then(|f| f.as_str())
                                {
                                    last_finish_reason = Some(finish.to_string());
                                }
                                if let Some(usage) = step_context_usage(ev_type, data) {
                                    last_usage = Some(usage.clone());
                                    let _ = tx.send(Ok(StreamEvent::Usage(usage.clone()))).await;
                                    if let Some(bridge) = get_ui_bridge() {
                                        if let Some(ref update_fn) = bridge.update_context_used_fn {
                                            let total_context = usage
                                                .prompt_tokens
                                                .saturating_add(usage.completion_tokens);
                                            if total_context > 0 {
                                                update_fn(total_context);
                                            }
                                        }
                                    }
                                }
                            }
                            "session.usage.updated" | "session.next.usage.updated" => {
                                // Session-cumulative, not one request's prompt. Feeding it to the
                                // context gauge is what pushed the readout past the window while
                                // an inner loop ran: every step's prompt was added to the last.
                                if let Some(usage) = session_total_usage(ev_type, data) {
                                    total_usage = Some(usage);
                                }
                            }
                            "session.execution.succeeded"
                            | "session.next.execution.succeeded"
                            | "session.idle"
                            | "session.next.idle" => {
                                ended_successfully = true;
                                let _ = tx
                                    .send(Ok(StreamEvent::Completed {
                                        finish_reason: last_finish_reason.clone(),
                                        usage: total_usage.clone().or_else(|| last_usage.clone()),
                                    }))
                                    .await;
                                break;
                            }
                            "session.execution.failed" | "session.next.execution.failed" => {
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
                            "session.status" | "session.next.status" => {
                                let status_type = data
                                    .get("status")
                                    .and_then(|s| s.get("type").or(Some(s)))
                                    .and_then(|t| t.as_str())
                                    .or_else(|| data.get("type").and_then(|t| t.as_str()));
                                if status_type == Some("idle") && saw_deltas {
                                    ended_successfully = true;
                                    let _ = tx
                                        .send(Ok(StreamEvent::Completed {
                                            finish_reason: last_finish_reason.clone(),
                                            usage: total_usage
                                                .clone()
                                                .or_else(|| last_usage.clone()),
                                        }))
                                        .await;
                                    break;
                                }
                            }
                            "form.created"
                            | "session.form.created"
                            | "session.next.form.created" => {
                                let form = data.get("form").unwrap_or(data);
                                let form_id = form
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if !form_id.is_empty() {
                                    let (clarify_req, field_mappings) =
                                        parse_opencode_form_to_clarify(form);
                                    let bridge = get_ui_bridge();
                                    let client = clean_client.clone();
                                    let reply_url = format!(
                                        "{}/api/session/{}/form/{}/reply",
                                        service_url, target_session_id, form_id
                                    );
                                    let cancel_url = format!(
                                        "{}/api/session/{}/form/{}",
                                        service_url, target_session_id, form_id
                                    );
                                    let post_headers = clean_headers.clone();

                                    tokio::spawn(async move {
                                        let maybe_resp = if let Some(bridge) = bridge {
                                            let (resp_tx, resp_rx) =
                                                tokio::sync::oneshot::channel();
                                            if bridge
                                                .clarify_tx
                                                .send((clarify_req, resp_tx))
                                                .is_ok()
                                            {
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
                                                for (idx, mapping) in
                                                    field_mappings.iter().enumerate()
                                                {
                                                    let value = match resp.answers.get(idx) {
                                                        Some(answer) => {
                                                            form_value_for_answer(mapping, answer)
                                                        }
                                                        // No answer for this field: keep the
                                                        // form's first option rather than an
                                                        // empty, invalid value.
                                                        None => mapping.option_values.first().map(
                                                            |first| {
                                                                form_value_from_raw(mapping, first)
                                                            },
                                                        ),
                                                    };
                                                    if let Some(value) = value {
                                                        answer_obj
                                                            .insert(mapping.key.clone(), value);
                                                    }
                                                }
                                                Some(answer_obj)
                                            }
                                            _ => None,
                                        };

                                        if let Some(answer_obj) =
                                            answer_obj.filter(|a| !a.is_empty())
                                        {
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
                                                    tracing::warn!(
                                                        "OpenCode form reply failed (HTTP {status}): {body}"
                                                    );
                                                }
                                                Err(error) => tracing::warn!(
                                                    "OpenCode form reply failed: {error}"
                                                ),
                                            }
                                        } else {
                                            // Esc means "ask me nothing": cancel the form so the
                                            // service resumes instead of waiting on it forever.
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
                                                    tracing::warn!(
                                                        "OpenCode form cancel failed (HTTP {status}): {body}"
                                                    );
                                                }
                                                Err(error) => tracing::warn!(
                                                    "OpenCode form cancel failed: {error}"
                                                ),
                                            }
                                        }
                                    });
                                }
                            }
                            "permission.asked"
                            | "session.permission.asked"
                            | "session.next.permission.asked" => {
                                let request_id = data
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if !request_id.is_empty() {
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
                                    let client = clean_client.clone();
                                    let reply_url = format!(
                                        "{}/api/session/{}/permission/{}/reply",
                                        service_url, target_session_id, request_id
                                    );
                                    let post_headers = clean_headers.clone();

                                    tokio::spawn(async move {
                                        let is_yolo = bridge
                                            .as_ref()
                                            .map(|b| b.allow_all.load(Ordering::SeqCst))
                                            .unwrap_or(false);

                                        let decision = if is_yolo {
                                            "once"
                                        } else if let Some(bridge) = bridge {
                                            let arguments = if action == "shell"
                                                || action == "terminal"
                                                || action == "bash"
                                            {
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
                                            if bridge
                                                .approval_tx
                                                .send((approval_req, dec_tx))
                                                .is_ok()
                                            {
                                                match dec_rx.await {
                                                    Ok(ApprovalDecision::Run)
                                                    | Ok(ApprovalDecision::RunWithArguments(_)) => {
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
                                    });
                                }
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
                    usage: total_usage.or(last_usage),
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

/// Executes a one-shot completion via `opencode run`.
/// Temporary files holding the decoded pasted images for `opencode run --file`.
///
/// The CLI takes paths, not data URLs, so the bytes have to exist somewhere the child can read
/// them. Deleting them on drop keeps a chat session from leaving screenshots behind in the
/// temp directory.
struct AttachmentFiles(Vec<PathBuf>);

impl AttachmentFiles {
    fn write(files: &[serde_json::Value]) -> Result<Self, HarnessError> {
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

    fn paths(&self) -> impl Iterator<Item = &Path> {
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

pub async fn execute_opencode_completion_cli(
    model: &str,
    messages: &[Message],
    _tools: &[ToolDefinition],
    _options: &CompletionOptions,
) -> Result<Message, HarnessError> {
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
                        add_usage(&mut token_usage, &usage);
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

    let stderr = child.stderr.take();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, HarnessError>>(100);

    tokio::spawn(async move {
        // Keep the decoded attachment files alive for as long as the child that reads them.
        let _attachments = attachments;
        let mut reader = BufReader::new(stdout).lines();
        let mut saw_output = false;
        let mut last_error = None;
        let mut last_usage = None;
        let mut total_usage = None;
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
                        let mut input = state
                            .get("input")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        normalize_tool_arguments(&mut input);
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
                        if let Some(usage) = parse_tokens(part).or_else(|| parse_tokens(&val)) {
                            last_usage = Some(usage.clone());
                            add_usage(&mut total_usage, &usage);
                            let _ = tx.send(Ok(StreamEvent::Usage(usage.clone()))).await;
                            if let Some(bridge) = get_ui_bridge() {
                                if let Some(ref update_fn) = bridge.update_context_used_fn {
                                    let total_context =
                                        usage.prompt_tokens.saturating_add(usage.completion_tokens);
                                    if total_context > 0 {
                                        update_fn(total_context);
                                    }
                                }
                            }
                        }
                    }
                    "usage" => {
                        if let Some(usage) = parse_tokens(part).or_else(|| parse_tokens(&val)) {
                            last_usage = Some(usage.clone());
                            let _ = tx.send(Ok(StreamEvent::Usage(usage.clone()))).await;
                            if let Some(bridge) = get_ui_bridge() {
                                if let Some(ref update_fn) = bridge.update_context_used_fn {
                                    let total_context =
                                        usage.prompt_tokens.saturating_add(usage.completion_tokens);
                                    if total_context > 0 {
                                        update_fn(total_context);
                                    }
                                }
                            }
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
                usage: total_usage.or(last_usage),
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
    if let Ok(output) = opencode_command(&bin).args(["models"]).output().await {
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
    if let Ok(output) = opencode_command(&bin)
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
    let home = harness_core::config::home_dir();
    let state_file = home
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

    if let Ok(output) = opencode_command(&bin).args(["models"]).output().await {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut list = Vec::new();
            for line in stdout.lines() {
                let line = line.trim();
                if line.starts_with("opencode/") && !list.contains(&line.to_string()) {
                    list.push(line.to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The live wire puts `input` and `cache.read` side by side; the ledger's contract is that the
    /// cache count is part of the prompt. Row 456 of a real ledger had 56 input and 7,680 cached.
    #[test]
    fn a_cache_read_beside_the_input_is_folded_into_the_prompt() {
        let tokens = serde_json::json!({
            "input": 56,
            "output": 18,
            "reasoning": 3,
            "cache": { "read": 7_680 },
            "total": 77
        });
        let usage = usage_from_tokens(&tokens);
        assert_eq!(usage.prompt_tokens, 7_736, "input + cache.read");
        assert_eq!(usage.completion_tokens, 21, "reasoning is billed as output");
        assert_eq!(usage.cached_tokens, 7_680);
        assert_eq!(usage.total_tokens, 7_757);
    }

    /// The gauge reads `session.step.ended`: one step's prompt is the context. The same numbers on
    /// `session.usage.updated` are the running total for every step the inner loop ran, so that
    /// event must never be mistaken for a context size.
    #[test]
    fn only_a_step_report_is_a_context_size() {
        let step = serde_json::json!({
            "tokens": {
                "input": 929,
                "output": 2,
                "reasoning": 0,
                "cache": { "read": 6_144 }
            }
        });
        let context =
            step_context_usage("session.step.ended", &step).expect("a step reports usage");
        assert_eq!(context.prompt_tokens, 7_073, "input + cache.read");
        assert_eq!(context.completion_tokens, 2);
        assert!(
            step_context_usage("session.usage.updated", &step).is_none(),
            "the cumulative event is not a context size"
        );
    }

    /// The native route bills from `session.usage.updated`, which already covers every step of the
    /// completion; the session is created per call, so its newest value is the call's total.
    #[test]
    fn the_cumulative_report_is_the_ledger_total() {
        let cumulative = serde_json::json!({
            "tokens": {
                "input": 1_015,
                "output": 6,
                "reasoning": 81,
                "cache": { "read": 6_528 }
            }
        });
        let usage =
            session_total_usage("session.usage.updated", &cumulative).expect("a cumulative total");
        assert_eq!(usage.prompt_tokens, 7_543);
        assert_eq!(usage.completion_tokens, 87, "output + reasoning");
        assert!(
            session_total_usage("session.step.ended", &cumulative).is_none(),
            "a single step is not a cumulative total"
        );
    }

    /// Headless emits one `step_finish` per step and no cumulative event, so the call total is the
    /// sum of the steps while the gauge keeps whichever step arrived last.
    #[test]
    fn headless_steps_are_summed_for_the_ledger() {
        let first = usage_from_tokens(&serde_json::json!({
            "input": 14, "output": 41, "reasoning": 21, "cache": { "read": 6_016 }
        }));
        let second = usage_from_tokens(&serde_json::json!({
            "input": 20, "output": 5, "reasoning": 0, "cache": { "read": 7_000 }
        }));
        let mut total = None;
        add_usage(&mut total, &first);
        add_usage(&mut total, &second);
        let total = total.expect("two steps make a total");
        assert_eq!(
            total.prompt_tokens,
            first.prompt_tokens + second.prompt_tokens
        );
        assert_eq!(
            total.completion_tokens,
            first.completion_tokens + second.completion_tokens
        );
        assert_eq!(
            total.cached_tokens,
            first.cached_tokens + second.cached_tokens
        );
        assert_eq!(total.total_tokens, first.total_tokens + second.total_tokens);
        assert_eq!(
            second.prompt_tokens, 7_020,
            "each step still holds its own prompt"
        );
    }

    /// A pasted screenshot is an attachment, not 300 KB of prose. The text keeps the instruction
    /// (and the marker the CLI wrote) and the data URL moves to `files`, where OpenCode turns it
    /// into an image part. Sending it in `text` is what produced a turn whose only content the
    /// model could see was "pasted content" with no instruction.
    #[test]
    fn a_pasted_image_becomes_an_attachment_not_prompt_text() {
        let url = "data:image/png;base64,iVBORw0KGgo=";
        let messages = [
            Message::system("be brief"),
            Message::user(format!(
                "make the output coloured [image: 800x600]\n\n{}",
                url
            )),
        ];

        let prompt = format_opencode_prompt(&messages);

        assert!(
            !prompt.text.contains("base64"),
            "the pixels must not be replayed as text: {}",
            prompt.text
        );
        assert!(
            prompt.text.contains("make the output coloured"),
            "the instruction survives: {}",
            prompt.text
        );
        assert!(prompt.text.contains("[image: 800x600]"));
        assert_eq!(prompt.files.len(), 1);
        assert_eq!(prompt.files[0]["uri"], url);
        assert_eq!(prompt.files[0]["name"], "pasted-image-1.png");
    }

    /// A screenshot from an earlier turn is not uploaded again: the `[image: WxH]` marker stays as
    /// the record of it, and only the newest user turn's pixels are attached.
    #[test]
    fn an_older_pasted_image_is_not_reuploaded() {
        let old = "data:image/jpeg;base64,QUJD";
        let messages = [
            Message::user(format!("look at this [image: 10x10]\n\n{}", old)),
            Message::assistant("That is a chart."),
            Message::user("and now?"),
        ];

        let prompt = format_opencode_prompt(&messages);

        assert!(!prompt.text.contains("base64"));
        assert!(prompt.files.is_empty(), "history is not re-attached");
        assert!(prompt.text.contains("[image: 10x10]"));
    }

    /// The headless route takes `--file <path>`, so the pixels have to reach a real file and that
    /// file must not outlive the turn.
    #[test]
    fn attachment_files_are_written_for_the_cli_then_removed() {
        let files = [serde_json::json!({
            "uri": "data:image/png;base64,aGk=",
            "name": "pasted-image-1.png"
        })];

        let written = AttachmentFiles::write(&files).expect("the image decodes and writes");
        let path = written
            .paths()
            .next()
            .expect("one attachment")
            .to_path_buf();
        assert_eq!(std::fs::read(&path).expect("the file exists"), b"hi");
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));

        drop(written);
        assert!(!path.exists(), "the temp file is removed with the turn");
    }

    /// Text that merely mentions the format is prose: it must not be lifted out, so a prompt
    /// explaining `data:image/png` still reaches the model intact.
    #[test]
    fn a_mention_of_a_data_url_is_not_an_attachment() {
        let messages = [Message::user("the spec says data:image/png is a URL")];

        let prompt = format_opencode_prompt(&messages);

        assert!(prompt.files.is_empty());
        assert!(prompt.text.contains("data:image/png"));
    }

    fn question_form(field_type: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "frm_test",
            "title": "Questions",
            "fields": [{
                "key": "q0",
                "title": "Next step",
                "description": "What should happen next?",
                "type": field_type,
                "custom": true,
                "options": [
                    { "value": "restart", "label": "Restart everything" },
                    { "value": "report", "label": "Just report" }
                ]
            }]
        })
    }

    fn answer(choice_index: Option<usize>, text: &str) -> ClarifyAnswer {
        ClarifyAnswer {
            question: "Next step".to_string(),
            answer: text.to_string(),
            choice_index,
        }
    }

    /// OpenCode types a question field as `string`, so its answer is a scalar. The bridge used to
    /// wrap every answer in an array; the API rejects that with `FormInvalidAnswerError`, which
    /// left the form pending and the turn loading forever.
    #[test]
    fn a_string_field_reply_is_scalar_and_uses_the_option_value() {
        let (request, mappings) = parse_opencode_form_to_clarify(&question_form("string"));

        assert_eq!(request.questions.len(), 1);
        assert_eq!(mappings[0].kind, FormFieldKind::Scalar);
        let value = form_value_for_answer(&mappings[0], &answer(Some(0), "Restart everything"))
            .expect("the chosen option is mapped");
        assert_eq!(value, serde_json::json!("restart"));
    }

    #[test]
    fn a_custom_string_answer_is_sent_as_typed() {
        let (_, mappings) = parse_opencode_form_to_clarify(&question_form("string"));

        let value = form_value_for_answer(&mappings[0], &answer(None, "something else"))
            .expect("a typed answer is mapped");
        assert_eq!(value, serde_json::json!("something else"));
    }

    #[test]
    fn a_multiselect_reply_is_an_array_of_values() {
        let (_, mappings) = parse_opencode_form_to_clarify(&question_form("multiselect"));

        assert_eq!(mappings[0].kind, FormFieldKind::Multi);
        let value = form_value_for_answer(&mappings[0], &answer(Some(1), "Just report"))
            .expect("the chosen option is mapped");
        assert_eq!(value, serde_json::json!(["report"]));
    }

    #[test]
    fn a_boolean_field_offers_yes_no_and_replies_with_a_bool() {
        let form = serde_json::json!({
            "id": "frm_bool",
            "title": "Questions",
            "fields": [{ "key": "enabled", "title": "Enable it?", "type": "boolean" }]
        });
        let (request, mappings) = parse_opencode_form_to_clarify(&form);

        assert_eq!(request.questions[0].choices, vec!["Yes", "No"]);
        let value = form_value_for_answer(&mappings[0], &answer(Some(1), "No"))
            .expect("the chosen option is mapped");
        assert_eq!(value, serde_json::json!(false));
    }
}
