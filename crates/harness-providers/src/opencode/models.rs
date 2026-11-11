use harness_core::HarnessError;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use std::path::{Path, PathBuf};

use super::binary::{find_opencode_binary, opencode_command};
use super::service::{
    base64_encode, get_http_client, get_or_start_opencode_service, OpencodeServiceInfo,
};

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

/// Models reported by a running OpenCode daemon.
async fn fetch_models_from_service(
    client: &reqwest::Client,
    service: &OpencodeServiceInfo,
) -> Option<Vec<String>> {
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
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let val = resp.json::<serde_json::Value>().await.ok()?;
    let arr = val.get("data")?.as_array()?;
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
    if list.is_empty() {
        return None;
    }
    list.sort();
    Some(list)
}

/// Models parsed from `opencode models` output.
async fn fetch_models_from_cli(bin: &Path) -> Option<Vec<String>> {
    let output = opencode_command(bin).args(["models"]).output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut list = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(id) = line.strip_prefix("opencode/") {
            if !list.contains(&id.to_string()) {
                list.push(id.to_string());
            }
        } else if !line.is_empty() && !line.contains('/') && !list.contains(&line.to_string()) {
            list.push(line.to_string());
        }
    }
    if list.is_empty() {
        return None;
    }
    list.sort();
    Some(list)
}

/// Models parsed from `opencode api GET /api/model`.
async fn fetch_models_from_cli_api(bin: &Path) -> Option<Vec<String>> {
    let output = opencode_command(bin)
        .args(["api", "GET", "/api/model"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let val = serde_json::from_slice::<serde_json::Value>(&output.stdout).ok()?;
    let arr = val.get("data")?.as_array()?;
    let mut list = Vec::new();
    for item in arr {
        if item.get("providerID").and_then(|p| p.as_str()) == Some("opencode") {
            if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                list.push(id.to_string());
            }
        }
    }
    if list.is_empty() {
        return None;
    }
    list.sort();
    Some(list)
}

/// Recently used models from `~/.local/state/opencode/model.json`, plus the known free models.
fn fetch_models_from_state_file() -> Option<Vec<String>> {
    let home = harness_core::config::home_dir();
    let state_file = home
        .join(".local")
        .join("state")
        .join("opencode")
        .join("model.json");
    let content = std::fs::read_to_string(&state_file).ok()?;
    let val = serde_json::from_str::<serde_json::Value>(&content).ok()?;
    let recent = val.get("recent")?.as_array()?;
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
    if list.is_empty() {
        return None;
    }
    for &m in OPENCODE_FREE_MODELS {
        if !list.contains(&m.to_string()) {
            list.push(m.to_string());
        }
    }
    list.sort();
    Some(list)
}

pub async fn fetch_opencode_models() -> Result<Vec<String>, HarnessError> {
    let client = get_http_client();
    if let Some(service) = get_or_start_opencode_service(client).await {
        if let Some(list) = fetch_models_from_service(client, &service).await {
            return Ok(list);
        }
    }

    let bin = find_opencode_binary().unwrap_or_else(|_| PathBuf::from("opencode"));

    // 1. Try running `opencode models`
    if let Some(list) = fetch_models_from_cli(&bin).await {
        return Ok(list);
    }

    // 2. Try running `opencode api GET /api/model`
    if let Some(list) = fetch_models_from_cli_api(&bin).await {
        return Ok(list);
    }

    // 3. Try ~/.local/state/opencode/model.json
    if let Some(list) = fetch_models_from_state_file() {
        return Ok(list);
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
