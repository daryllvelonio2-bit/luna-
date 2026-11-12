//! Who is signed in on the Codex route, and where that answer comes from.
//!
//! The Codex CLI keeps its own login in `$CODEX_HOME/auth.json` (default `~/.codex/auth.json`): a
//! ChatGPT OAuth token pair plus the account it was issued for. Importing "the Codex login"
//! therefore means what it means for Antigravity or OpenCode — copy the credential the other tool
//! already holds, instead of asking the user to paste a token a second time.
//!
//! There is deliberately no refresh here. Renewing a ChatGPT token *rotates* the refresh token, so
//! a refresh done here would have to be written back into the Codex CLI's own `auth.json` or the
//! CLI's next request would present a token it no longer owns and sign the user out. Writing to
//! another tool's credential store is a side effect this route does not take: an expired token is
//! reported with the one-keystroke way to replace it (`/connect` → Import Codex re-reads the file
//! the CLI keeps current), and the CLI refreshes that file itself whenever it is used.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use harness_core::HarnessError;

/// One ChatGPT login, in the shape luna stores it.
///
/// `account_id` is not decoration: the Codex backend requires it as a `chatgpt-account-id` header
/// on every request, and it cannot be derived from the access token at request time without
/// decoding a JWT on the hot path.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodexCredentials {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

impl CodexCredentials {
    /// The stored form: one JSON object, the shape every other JSON credential in the pool uses.
    pub fn to_secret(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| self.access_token.clone())
    }

    /// What the credential is called in `/connect → List connected providers`.
    pub fn label(&self) -> String {
        self.email
            .clone()
            .filter(|email| !email.trim().is_empty())
            .unwrap_or_else(|| "codex-cli".to_string())
    }
}

/// Where the Codex CLI keeps its login: `$CODEX_HOME/auth.json`, else `<home>/.codex/auth.json`.
///
/// The environment variable is the documented way to relocate the directory, so it is checked
/// first; the default is checked second rather than instead, because a stale `CODEX_HOME` in a
/// shell profile should not hide the login that is actually on disk.
pub fn codex_auth_paths(home: &Path, codex_home: Option<&str>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(dir) = codex_home {
        if !dir.trim().is_empty() {
            paths.push(PathBuf::from(dir).join("auth.json"));
        }
    }
    paths.push(home.join(".codex").join("auth.json"));
    paths
}

/// Read the Codex CLI's login off this machine.
pub fn detect_codex_credentials() -> Result<CodexCredentials, HarnessError> {
    let codex_home = std::env::var("CODEX_HOME").ok();
    detect_codex_credentials_in(&harness_core::config::home_dir(), codex_home.as_deref())
}

/// The same detection against an explicit home directory, so it can be tested without touching
/// the machine's environment.
pub fn detect_codex_credentials_in(
    home: &Path,
    codex_home: Option<&str>,
) -> Result<CodexCredentials, HarnessError> {
    let mut searched = Vec::new();
    for path in codex_auth_paths(home, codex_home) {
        searched.push(path.display().to_string());
        let Some(value) = read_json_file(&path) else {
            continue;
        };
        if let Some(credentials) = credentials_from_auth_json(&value) {
            return Ok(credentials);
        }
    }

    Err(HarnessError::AuthenticationFailed {
        provider: "codex".to_string(),
        message: format!(
            "no ChatGPT login found in {}. Sign in with `codex login` (or start the Codex CLI \
             once) and import again.",
            searched.join(" or ")
        ),
    })
}

/// The credential inside a Codex `auth.json`, or `None` when the file holds no OAuth login.
///
/// A CLI configured with a plain `OPENAI_API_KEY` has an `auth.json` with no `tokens` object; that
/// key belongs to the `openai` provider and would be rejected by the ChatGPT backend, so its
/// absence is reported by the caller as "no login" rather than smuggled in as a token.
fn credentials_from_auth_json(value: &serde_json::Value) -> Option<CodexCredentials> {
    let tokens = value.get("tokens")?;
    let access_token = tokens
        .get("access_token")
        .and_then(|token| token.as_str())
        .map(str::trim)
        .filter(|token| !token.is_empty())?;

    let claims = claim_map(tokens.get("id_token").and_then(|token| token.as_str()));
    // The account id is written down by the CLI; the id_token claim is the fallback for a file
    // that predates the field.
    let account_id = tokens
        .get("account_id")
        .and_then(|account| account.as_str())
        .map(str::trim)
        .filter(|account| !account.is_empty())
        .map(str::to_string)
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|auth| auth.get("chatgpt_account_id"))
                .and_then(|account| account.as_str())
                .map(str::to_string)
        });

    Some(CodexCredentials {
        access_token: access_token.to_string(),
        refresh_token: tokens
            .get("refresh_token")
            .and_then(|token| token.as_str())
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string),
        account_id,
        email: claims
            .get("email")
            .and_then(|email| email.as_str())
            .map(str::to_string),
    })
}

/// The claims of a JWT, or an empty object when it is absent or unreadable.
///
/// Every caller treats the claims as optional decoration — a display name, a fallback account id —
/// so a malformed token must not fail the import it is only meant to describe.
fn claim_map(token: Option<&str>) -> serde_json::Map<String, serde_json::Value> {
    token
        .and_then(claims_from_jwt)
        .and_then(|claims| claims.as_object().cloned())
        .unwrap_or_default()
}

fn claims_from_jwt(token: &str) -> Option<serde_json::Value> {
    use base64::Engine as _;

    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

/// The credential a stored secret names, whether it was written as the JSON record above or as a
/// bare token by hand.
pub fn parse_codex_secret(raw: &str) -> CodexCredentials {
    let trimmed = raw.trim();
    if let Ok(parsed) = serde_json::from_str::<CodexCredentials>(trimmed) {
        if !parsed.access_token.trim().is_empty() {
            return parsed;
        }
    }
    CodexCredentials {
        access_token: trimmed.to_string(),
        ..Default::default()
    }
}

fn read_json_file(path: &Path) -> Option<serde_json::Value> {
    if !path.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<serde_json::Value>(&content).ok()
}
