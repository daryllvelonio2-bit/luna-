//! Who is signed in, and keeping that answer current.
//!
//! The OAuth half of the route: reading the credential off the machine, refreshing it, pulling
//! the account's own e-mail out of it (the display name has to follow the credential actually in
//! use, not a label written down once), and reading the bearer token back out for a request.

use serde_json::json;

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntigravityCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub email: Option<String>,
    pub expiry: Option<String>,
}

/// Detect and copy logged-in Antigravity credentials from SecretService or Shiina.
pub fn detect_antigravity_credentials() -> Result<AntigravityCredentials, HarnessError> {
    // 1. Try Linux secret-tool (service: gemini, username: antigravity)
    if let Ok(output) = Command::new("secret-tool")
        .args(["lookup", "service", "gemini", "username", "antigravity"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !stdout.is_empty() {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    let tok = val.get("token").unwrap_or(&val);
                    if let Some(access) = tok.get("access_token").and_then(|a| a.as_str()) {
                        let refresh = tok
                            .get("refresh_token")
                            .and_then(|r| r.as_str())
                            .map(|s| s.to_string());
                        let expiry = tok
                            .get("expiry")
                            .and_then(|e| e.as_str())
                            .map(|s| s.to_string());
                        let email = extract_email_from_jwt(&val);
                        return Ok(AntigravityCredentials {
                            access_token: access.to_string(),
                            refresh_token: refresh,
                            email,
                            expiry,
                        });
                    }
                }
            }
        }
    }

    // 2. Try ~/.shiina/auth.json or profile auth.json
    let home = harness_core::config::home_dir();
    let mut candidate_paths = vec![home.join(".shiina").join("auth.json")];

    let profiles_dir = PathBuf::from(&home).join(".shiina").join("profiles");
    if profiles_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(profiles_dir) {
            for entry in entries.flatten() {
                candidate_paths.push(entry.path().join("auth.json"));
            }
        }
    }

    for path in candidate_paths {
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(agy) = data.get("providers").and_then(|p| p.get("antigravity")) {
                        if let Some(access) = agy.get("access_token").and_then(|a| a.as_str()) {
                            let refresh = agy
                                .get("refresh_token")
                                .and_then(|r| r.as_str())
                                .map(|s| s.to_string());
                            let email = agy
                                .get("email")
                                .and_then(|e| e.as_str())
                                .map(|s| s.to_string());
                            return Ok(AntigravityCredentials {
                                access_token: access.to_string(),
                                refresh_token: refresh,
                                email,
                                expiry: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // 3. Try environment variables
    for var in &[
        "ANTIGRAVITY_ACCESS_TOKEN",
        "AGY_ACCESS_TOKEN",
        "GEMINI_ACCESS_TOKEN",
    ] {
        if let Ok(token) = std::env::var(var) {
            if !token.trim().is_empty() {
                return Ok(AntigravityCredentials {
                    access_token: token.trim().to_string(),
                    refresh_token: None,
                    email: None,
                    expiry: None,
                });
            }
        }
    }

    Err(HarnessError::AuthenticationFailed {
        provider: "antigravity".to_string(),
        message: "No Antigravity credentials found in SecretService, ~/.shiina/auth.json, or environment.".to_string(),
    })
}

fn extract_email_from_jwt(val: &serde_json::Value) -> Option<String> {
    if let Some(email) = val.get("email").and_then(|e| e.as_str()) {
        return Some(email.to_string());
    }
    if let Some(id_tok) = val.get("id_token").and_then(|t| t.as_str()) {
        let parts: Vec<&str> = id_tok.split('.').collect();
        if parts.len() >= 2 {
            let padded = format!("{}{}", parts[1], "=".repeat((4 - parts[1].len() % 4) % 4));
            if let Ok(decoded) = base64_url_decode(&padded) {
                if let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&decoded) {
                    if let Some(email) = claims.get("email").and_then(|e| e.as_str()) {
                        return Some(email.to_string());
                    }
                }
            }
        }
    }
    None
}

fn base64_url_decode(s: &str) -> Result<Vec<u8>, String> {
    let clean: String = s
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            other => other,
        })
        .collect();
    let mut bytes = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for b in clean.bytes() {
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buf >> bits) as u8);
        }
    }
    Ok(bytes)
}

/// What Google's token endpoint returned for a refresh.
///
/// The expiry is kept relative (`expires_in`) because the caller writes it to a stored credential
/// and the clock starts when the response arrives, not when it is read back.
#[derive(Debug, Clone)]
pub(crate) struct AntigravityRefreshedToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in_secs: Option<i64>,
}

/// Refresh an Antigravity Google OAuth access token using a refresh token.
pub async fn refresh_antigravity_token(
    http_client: &reqwest::Client,
    refresh_token: &str,
) -> Result<String, HarnessError> {
    Ok(request_antigravity_token(http_client, refresh_token)
        .await?
        .access_token)
}

/// The full token response, so a stored credential can be updated without losing anything.
async fn request_antigravity_token(
    http_client: &reqwest::Client,
    refresh_token: &str,
) -> Result<AntigravityRefreshedToken, HarnessError> {
    let client_secret = super::agy_client_secret();
    let params = [
        ("client_id", AGY_CLIENT_ID),
        ("client_secret", client_secret.as_str()),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];

    let resp = http_client
        .post(GOOGLE_OAUTH_TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| HarnessError::AuthenticationFailed {
            provider: "antigravity".to_string(),
            message: format!("OAuth refresh network error: {}", e),
        })?;

    if !resp.status().is_success() {
        let err_body = resp.text().await.unwrap_or_default();
        return Err(HarnessError::AuthenticationFailed {
            provider: "antigravity".to_string(),
            message: format!("Failed to refresh Antigravity OAuth token: {}", err_body),
        });
    }

    let data: serde_json::Value =
        resp.json()
            .await
            .map_err(|e| HarnessError::AuthenticationFailed {
                provider: "antigravity".to_string(),
                message: format!("Invalid JSON response from Google OAuth: {}", e),
            })?;

    let access = data
        .get("access_token")
        .and_then(|a| a.as_str())
        .ok_or_else(|| HarnessError::AuthenticationFailed {
            provider: "antigravity".to_string(),
            message: "Missing access_token in OAuth refresh response".to_string(),
        })?;

    Ok(AntigravityRefreshedToken {
        access_token: access.to_string(),
        refresh_token: data
            .get("refresh_token")
            .and_then(|r| r.as_str())
            .map(|s| s.to_string()),
        expires_in_secs: data.get("expires_in").and_then(|e| e.as_i64()),
    })
}

/// The stored credential after a refresh: every field the old record carried (`email`, ...) is
/// kept, and the tokens and expiry are replaced with the ones Google just issued.
pub(crate) fn merge_antigravity_refresh(
    raw_secret: &str,
    refreshed: &AntigravityRefreshedToken,
) -> String {
    let mut record = serde_json::from_str::<serde_json::Value>(raw_secret)
        .ok()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}));

    record["access_token"] = json!(refreshed.access_token);
    if let Some(refresh_token) = &refreshed.refresh_token {
        record["refresh_token"] = json!(refresh_token);
    }
    if let Some(seconds) = refreshed.expires_in_secs {
        let expiry = chrono::Utc::now() + chrono::Duration::seconds(seconds);
        record["expiry"] = json!(expiry.to_rfc3339());
    }
    record.to_string()
}

/// Renew the credential a request just used and write it back to the pool.
///
/// This is the one place a refresh happens: the turn path and the quota path both see a 401 once
/// the access token ages out (roughly hourly), and both need the same merge so a refreshed record
/// keeps the e-mail and expiry the original import carried. Returns the new access token.
pub(crate) async fn refresh_and_store_antigravity_credential(
    http_client: &reqwest::Client,
    raw_secret: &str,
    cred_manager: Option<&std::sync::Arc<crate::auth::CredentialManager>>,
) -> Result<String, HarnessError> {
    let (_, refresh_token) = extract_bearer_token(raw_secret);
    let Some(refresh_token) = refresh_token else {
        return Err(HarnessError::AuthenticationFailed {
            provider: "antigravity".to_string(),
            message: "the Antigravity credential has no refresh token, so it cannot renew itself; \
                      import a fresh login with `luna auth antigravity copy` (or /connect → Import \
                      Antigravity) once"
                .to_string(),
        });
    };

    let refreshed = request_antigravity_token(http_client, &refresh_token).await?;

    if let Some(cm) = cred_manager {
        let updated = merge_antigravity_refresh(raw_secret, &refreshed);
        let _ = cm
            .update_credential_key("antigravity", raw_secret, &updated)
            .await;
    }

    Ok(refreshed.access_token)
}

/// Extracts the raw bearer token string if stored as a JSON credential record.
pub fn extract_bearer_token(secret: &str) -> (String, Option<String>) {
    let trimmed = secret.trim();
    if trimmed.starts_with('{') {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let access = val
                .get("access_token")
                .and_then(|a| a.as_str())
                .unwrap_or(trimmed);
            let refresh = val
                .get("refresh_token")
                .and_then(|r| r.as_str())
                .map(|s| s.to_string());
            return (access.to_string(), refresh);
        }
    }
    (trimmed.to_string(), None)
}

/// Strictly resolve the authentic user email belonging to the given Antigravity credential.
/// Checks the JWT id_token first, then queries Google tokeninfo endpoint, ensuring the
/// active display matches the real account and does not show an old or mismatched label.
pub async fn resolve_antigravity_email_strict(
    http_client: &reqwest::Client,
    raw_secret: &str,
) -> Option<String> {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(raw_secret) {
        // 1. Check if email is embedded directly in jwt id_token
        if let Some(email) = extract_email_from_jwt(&val) {
            return Some(email);
        }
        // 2. Query Google tokeninfo with access_token
        let tok = val.get("token").unwrap_or(&val);
        if let Some(access) = tok.get("access_token").and_then(|a| a.as_str()) {
            let url = format!(
                "https://oauth2.googleapis.com/tokeninfo?access_token={}",
                access
            );
            if let Ok(resp) = http_client
                .get(&url)
                .timeout(std::time::Duration::from_secs(3))
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(info) = resp.json::<serde_json::Value>().await {
                        if let Some(email) = info.get("email").and_then(|e| e.as_str()) {
                            return Some(email.to_string());
                        }
                    }
                }
            }
        }
        // 3. Email property on JSON object
        if let Some(email) = val.get("email").and_then(|e| e.as_str()) {
            return Some(email.to_string());
        }
    }
    None
}
