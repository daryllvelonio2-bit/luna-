use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::RwLock;

use axum::http::HeaderMap;
use rand::RngCore;

/// Manages authentication tokens and the Host/Origin trust fence for the web UI.
pub struct WebAuthState {
    /// Cryptographically random process launch token passed in the initial URL query.
    pub launch_token: String,
    /// Active authorized session cookies issued after exchanging the launch token.
    authorized_sessions: RwLock<HashSet<String>>,
    /// Bound listener host (e.g. "127.0.0.1").
    pub bound_host: String,
    /// Extra accepted authorities specified via `--trusted-host`.
    pub trusted_hosts: Vec<String>,
}

impl WebAuthState {
    pub fn new(
        bound_host: String,
        trusted_hosts: Vec<String>,
        static_token: Option<String>,
    ) -> Self {
        let launch_token = static_token.unwrap_or_else(|| {
            let mut bytes = [0u8; 16];
            rand::thread_rng().fill_bytes(&mut bytes);
            format!("{:032x}", u128::from_be_bytes(bytes))
        });

        Self {
            launch_token,
            authorized_sessions: RwLock::new(HashSet::new()),
            bound_host,
            trusted_hosts,
        }
    }

    /// Validate the launch token and issue a new authorized session token.
    pub fn exchange_launch_token(&self, token: &str) -> Option<String> {
        if token.trim() == self.launch_token {
            let mut bytes = [0u8; 16];
            rand::thread_rng().fill_bytes(&mut bytes);
            let session_token = format!("{:032x}", u128::from_be_bytes(bytes));
            if let Ok(mut set) = self.authorized_sessions.write() {
                set.insert(session_token.clone());
            }
            Some(session_token)
        } else {
            None
        }
    }

    /// Check if a session cookie token is valid.
    pub fn is_session_authorized(&self, token: &str) -> bool {
        if let Ok(set) = self.authorized_sessions.read() {
            set.contains(token)
        } else {
            false
        }
    }

    /// Host / Origin trust fence: verify that the incoming Host and Origin headers
    /// match the bound address, localhost loopback, or an explicitly trusted host.
    pub fn verify_trust_fence(&self, headers: &HeaderMap) -> bool {
        if let Some(host_val) = headers.get("host").and_then(|v| v.to_str().ok()) {
            let host_only = host_val.split(':').next().unwrap_or(host_val);
            if !self.is_host_trusted(host_only) {
                return false;
            }
        }

        if let Some(origin_val) = headers.get("origin").and_then(|v| v.to_str().ok()) {
            // An Origin that cannot be parsed (for example `null` from a sandboxed frame) must not
            // slip past the fence by leaving the host unchecked.
            let Ok(parsed) = url::Url::parse(origin_val) else {
                return false;
            };
            if !parsed
                .host_str()
                .is_some_and(|host| self.is_host_trusted(host))
            {
                return false;
            }
        }

        true
    }

    fn is_host_trusted(&self, host: &str) -> bool {
        // Direct loopback checks
        if host == "127.0.0.1" || host == "localhost" || host == "::1" || host == "[::1]" {
            return true;
        }
        if host == self.bound_host {
            return true;
        }
        // Check trusted hosts list
        for trusted in &self.trusted_hosts {
            let trusted_only = trusted.split(':').next().unwrap_or(trusted);
            if host.eq_ignore_ascii_case(trusted_only) {
                return true;
            }
        }
        false
    }
}

/// Helper to test if a string represents an unspecified/wildcard IP address.
pub fn is_wildcard_host(host: &str) -> bool {
    let clean = host.trim().trim_matches('[').trim_matches(']');
    if clean == "0.0.0.0" || clean == "::" {
        return true;
    }
    if let Ok(ip) = clean.parse::<IpAddr>() {
        return ip.is_unspecified();
    }
    false
}

/// Helper to test if a string represents a loopback address.
pub fn is_loopback_host(host: &str) -> bool {
    let clean = host.trim().trim_matches('[').trim_matches(']');
    if clean == "127.0.0.1" || clean == "localhost" || clean == "::1" {
        return true;
    }
    if let Ok(ip) = clean.parse::<IpAddr>() {
        return ip.is_loopback();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_fence_rejects_an_origin_it_cannot_parse() {
        let auth = WebAuthState::new(
            "127.0.0.1".into(),
            Vec::new(),
            Some("launch-token".into()),
        );
        let mut headers = HeaderMap::new();
        headers.insert("host", "localhost:8080".parse().unwrap());

        headers.insert("origin", "null".parse().unwrap());
        assert!(!auth.verify_trust_fence(&headers));

        headers.insert("origin", "http://localhost:8080".parse().unwrap());
        assert!(auth.verify_trust_fence(&headers));

        headers.insert("origin", "https://evil.example".parse().unwrap());
        assert!(!auth.verify_trust_fence(&headers));
    }
}
