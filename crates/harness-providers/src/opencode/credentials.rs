use harness_core::HarnessError;
use std::path::{Path, PathBuf};

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
pub(super) fn read_json_file(path: &Path) -> Option<serde_json::Value> {
    if !path.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<serde_json::Value>(&content).ok()
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
