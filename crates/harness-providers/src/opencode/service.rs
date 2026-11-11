use reqwest::header::AUTHORIZATION;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;

use super::binary::{find_opencode_binary, opencode_command};
use super::credentials::read_json_file;

/// The local service is on loopback; a connection that takes longer than this is a dead process,
/// not a slow one.
const OPENCODE_CONNECT_TIMEOUT_SECS: u64 = 10;
/// Liveness probe for an already-running service.
const OPENCODE_PROBE_TIMEOUT_MS: u64 = 1_500;
/// Grace period after spawning the service before its endpoint file is expected to exist.
const OPENCODE_START_GRACE_MS: u64 = 1_000;

#[derive(Debug, Clone)]
pub struct OpencodeServiceInfo {
    pub url: String,
    pub password: Option<String>,
}

pub(super) fn base64_encode(input: &[u8]) -> String {
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
            .connect_timeout(std::time::Duration::from_secs(
                OPENCODE_CONNECT_TIMEOUT_SECS,
            ))
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
        .timeout(std::time::Duration::from_millis(OPENCODE_PROBE_TIMEOUT_MS));
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
            tokio::time::sleep(std::time::Duration::from_millis(OPENCODE_START_GRACE_MS)).await;
            if let Some(info) = read_opencode_service_info() {
                if check_service_alive(client, &info).await {
                    return Some(info);
                }
            }
        }
    }

    None
}
