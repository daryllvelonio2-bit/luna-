use std::path::PathBuf;
use harness_core::HarnessError;

#[derive(Debug, Clone)]
pub struct DetectedOpencodeCredential {
    pub provider: String,
    pub label: String,
    pub api_key: String,
}

pub fn detect_opencode_credentials() -> Result<Vec<DetectedOpencodeCredential>, HarnessError> {
    let mut detected = Vec::new();
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());

    // 1. ~/.local/share/opencode/auth.json
    let local_auth = PathBuf::from(&home).join(".local").join("share").join("opencode").join("auth.json");
    if local_auth.is_file() {
        if let Ok(content) = std::fs::read_to_string(&local_auth) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                // Check for Groq integration
                if let Some(groq) = val.get("groq") {
                    if let Some(key) = groq.get("key").and_then(|k| k.as_str()) {
                        if !key.is_empty() {
                            detected.push(DetectedOpencodeCredential {
                                provider: "opencode".to_string(),
                                label: "opencode-groq".to_string(),
                                api_key: key.to_string(),
                            });
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

    // 2. ~/.shiina/auth.json
    let shiina_auth = PathBuf::from(&home).join(".shiina").join("auth.json");
    if shiina_auth.is_file() {
        if let Ok(content) = std::fs::read_to_string(&shiina_auth) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                let pool = val.get("credential_pool");
                // Check opencode
                if let Some(creds) = pool.and_then(|p| p.get("opencode")).and_then(|c| c.as_array()) {
                    for c in creds {
                        if let Some(tok) = c.get("access_token").or_else(|| c.get("api_key")).and_then(|t| t.as_str()) {
                            if !tok.is_empty() && !detected.iter().any(|d| d.api_key == tok) {
                                let label = c.get("label").and_then(|l| l.as_str()).unwrap_or("shiina-opencode");
                                detected.push(DetectedOpencodeCredential {
                                    provider: "opencode".to_string(),
                                    label: label.to_string(),
                                    api_key: tok.to_string(),
                                });
                            }
                        }
                    }
                }
                // Check opencode-zen
                if let Some(creds) = pool.and_then(|p| p.get("opencode-zen")).and_then(|c| c.as_array()) {
                    for c in creds {
                        if let Some(tok) = c.get("access_token").or_else(|| c.get("api_key")).and_then(|t| t.as_str()) {
                            if !tok.is_empty() && !detected.iter().any(|d| d.api_key == tok) {
                                let label = c.get("label").and_then(|l| l.as_str()).unwrap_or("opencode-zen");
                                detected.push(DetectedOpencodeCredential {
                                    provider: "opencode-zen".to_string(),
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

    if detected.is_empty() {
        return Err(HarnessError::AuthenticationFailed {
            provider: "opencode".to_string(),
            message: "No OpenCode credentials found in ~/.local/share/opencode/auth.json or ~/.shiina/auth.json".to_string(),
        });
    }

    Ok(detected)
}
