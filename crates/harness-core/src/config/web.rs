//! The `tools.web` settings: what the `web_search` / `web_fetch` pair may do, and what it may not.
//!
//! It lives in its own module for the same reason [`crate::tools::netguard`] does: reach is a
//! policy, a policy has a shape, and the shape is easier to audit when it is not buried among the
//! twelve other settings structs. `config.rs` owns precedence, the flat key view and the file
//! format; this module owns the vocabulary a web policy is written in.
//!
//! Two switches guard the surface, and both are meant to be flipped deliberately: [`WebSettings::enabled`]
//! decides whether the tools exist at all, and `tools.network: deny` removes them again. The second
//! is not redundant — the seccomp filter behind `tools.network` confines a `terminal` *child*, not
//! the harness itself, so without [`web_tools_blocked_reason`] a machine that has declared itself
//! offline would still let the agent open sockets.
//!
//! Keys are never stored here. [`WebSettings::search_api_key_env`] names the *environment variable*
//! that holds the key, so a config file — which gets copied, shared and committed far more often
//! than a shell profile — never becomes a place a credential leaks from.

use serde::{Deserialize, Serialize};

use crate::error::HarnessError;

use super::ToolsSettings;

/// The `User-Agent` a fresh install sends. Identifying the harness is both polite and useful: an
/// operator reading their own logs can tell agent traffic from a browser's, and `robots.txt` groups
/// can name us instead of having to name `*`.
pub const DEFAULT_WEB_USER_AGENT: &str =
    "LunaHarness/0.1 (+https://github.com/luna-ai/rust-agent-harness)";

/// Every accepted spelling of a search backend, in the order the setup flow offers them.
pub const SEARCH_BACKENDS: [&str; 4] = ["duckduckgo", "brave", "tavily", "none"];

/// Longest `fetch_max_chars` the config may ask for: a tool result larger than this is a transcript
/// the user pays for on every subsequent turn of the session.
pub const WEB_FETCH_MAX_CHARS_CEILING: usize = 400_000;
/// Shortest useful `fetch_max_chars`: below this a page is a headline with no article.
pub const WEB_FETCH_MIN_CHARS: usize = 1_000;
/// Largest response body the config may accept, in bytes.
pub const WEB_FETCH_MAX_BYTES_CEILING: usize = 8_388_608;
/// Smallest response body worth accepting, in bytes.
pub const WEB_FETCH_MIN_BYTES: usize = 1_024;
/// Most results one `web_search` call may ask its backend for.
pub const WEB_SEARCH_MAX_RESULTS_CEILING: usize = 25;
/// Longest per-request budget, in seconds.
pub const WEB_TIMEOUT_CEILING_SECS: u64 = 300;
/// Longest cache lifetime, in seconds (a day). Beyond that a "recent" page is a stale page.
pub const WEB_CACHE_CEILING_SECS: u64 = 86_400;
/// Most redirects a fetch may follow. Each hop is a fresh request and a fresh check.
pub const WEB_MAX_REDIRECT_CEILING: usize = 5;

/// Web access: the `web_search` / `web_fetch` tool pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebSettings {
    /// Whether the pair is registered. On by default: the tools are read-only, and an agent that
    /// has to be told where to look is an agent that reaches for `curl` instead.
    pub enabled: bool,
    /// Which search provider backs `web_search`.
    pub search_backend: SearchBackend,
    /// Name of the environment variable holding the provider's API key. Only consulted for a
    /// backend that needs one; empty means "use the backend's default variable".
    pub search_api_key_env: String,
    /// Results `web_search` may return in one call.
    pub search_max_results: usize,
    /// Ceiling on the decoded response body, in bytes, before it is turned into text.
    pub fetch_max_bytes: usize,
    /// Ceiling on the characters `web_fetch` hands to the model.
    pub fetch_max_chars: usize,
    /// Budget for one request, response body included.
    pub timeout_secs: u64,
    /// Redirects followed before the chain is abandoned. Every hop is re-checked.
    pub max_redirects: usize,
    /// How long a fetched page is reused before it is fetched again. `0` disables reuse.
    pub cache_ttl_secs: u64,
    /// Sent as `User-Agent`, and the token `robots.txt` is matched against.
    pub user_agent: String,
    /// Hosts `web_fetch` and `web_search` may reach. Empty means "any host that passes the
    /// private-address check". Glob semantics, as `search_files`'s `file_glob`.
    pub allowed_domains: Vec<String>,
    /// Hosts that are refused even when an allowlist would permit them. Deny wins.
    pub blocked_domains: Vec<String>,
    /// Allow loopback, link-local and RFC1918 addresses. Off by default: an agent that can be
    /// pointed at `169.254.169.254` is an agent that can read cloud instance credentials, and a
    /// URL is attacker-controlled text when the page that supplied it was not trustworthy.
    /// Turn it on only to reach a local documentation server you control.
    pub allow_private_hosts: bool,
    /// Honour `robots.txt` before fetching a page.
    pub respect_robots: bool,
}

/// The search provider behind `web_search`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchBackend {
    /// DuckDuckGo's HTML endpoint: no account, no API key, and no contract either — the markup can
    /// change without notice. The default because it works on a fresh install.
    #[default]
    DuckDuckGo,
    /// Brave Search API. Needs a key, and publishes one.
    Brave,
    /// Tavily Search API. Needs a key; returns extracted page text alongside each result.
    Tavily,
    /// No search backend: `web_search` is not registered while `web_fetch` still is.
    Disabled,
}

impl SearchBackend {
    /// Read a backend name. Aliases are accepted because this is user-facing text typed by hand
    /// (`luna config set tools.web.search_backend ddg`), and a rejected spelling costs a round trip.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "duckduckgo" | "ddg" | "ddg_lite" => Ok(Self::DuckDuckGo),
            "brave" | "brave_search" => Ok(Self::Brave),
            "tavily" => Ok(Self::Tavily),
            "none" | "off" | "disabled" | "false" => Ok(Self::Disabled),
            other => Err(format!(
                "invalid search backend '{other}': expected one of {}",
                SEARCH_BACKENDS.join(", ")
            )),
        }
    }

    /// The canonical name, which is what gets written to the config file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "duckduckgo",
            Self::Brave => "brave",
            Self::Tavily => "tavily",
            Self::Disabled => "none",
        }
    }

    /// Whether this backend is configured by an API key. A backend that needs a key and cannot find
    /// one fails at startup rather than at the first search the user waits on.
    pub fn requires_api_key(self) -> bool {
        matches!(self, Self::Brave | Self::Tavily)
    }

    /// The environment variable consulted when `tools.web.search_api_key_env` is empty.
    pub fn default_api_key_env(self) -> &'static str {
        match self {
            Self::Brave => "BRAVE_SEARCH_API_KEY",
            Self::Tavily => "TAVILY_API_KEY",
            // Keyless and disabled backends have no variable to name; the empty string keeps
            // `search_api_key_env` a plain `String` instead of an `Option` nobody reads.
            Self::DuckDuckGo | Self::Disabled => "",
        }
    }

    /// Whether this backend answers without an account.
    pub fn is_keyless(self) -> bool {
        !self.requires_api_key()
    }

    /// The name a person reads, as opposed to the value a config file stores: `brave` is the key,
    /// "Brave Search API" is the product, and a confirmation screen should say the second one.
    pub fn label(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Brave => "Brave Search API",
            Self::Tavily => "Tavily Search API",
            Self::Disabled => "Disabled",
        }
    }
}

impl std::fmt::Display for SearchBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SearchBackend {
    type Err = String;
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

impl serde::Serialize for SearchBackend {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for SearchBackend {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        // One parser, so the file format and the command line cannot drift apart: whatever
        // `parse` accepts, the YAML accepts, and vice versa.
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl Default for WebSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            search_backend: SearchBackend::default(),
            search_api_key_env: String::new(),
            search_max_results: 8,
            fetch_max_bytes: 262_144,
            fetch_max_chars: 40_000,
            timeout_secs: 30,
            max_redirects: 3,
            cache_ttl_secs: 900,
            user_agent: DEFAULT_WEB_USER_AGENT.to_string(),
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
            allow_private_hosts: false,
            respect_robots: true,
        }
    }
}

impl WebSettings {
    /// The environment variable a key-requiring backend reads, defaulted when unset.
    pub fn api_key_env(&self) -> &str {
        let named = self.search_api_key_env.trim();
        if named.is_empty() {
            self.search_backend.default_api_key_env()
        } else {
            named
        }
    }

    /// Whether a search tool can be built at all: a backend was chosen *and* its key is present.
    pub fn search_configured(&self) -> bool {
        if self.search_backend == SearchBackend::Disabled {
            return false;
        }
        !self.search_backend.requires_api_key() || self.api_key_present()
    }

    /// Whether the key the configured backend needs is in the environment, non-empty.
    pub fn api_key_present(&self) -> bool {
        let variable = self.api_key_env();
        if variable.is_empty() {
            return false;
        }
        std::env::var(variable)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    }

    /// Reject a web policy that cannot do what it claims: out-of-range budgets, an empty
    /// `User-Agent` (`robots.txt` cannot be matched against nothing), and a key-requiring backend
    /// with no key to read.
    pub fn validate(&self) -> Result<(), HarnessError> {
        let bounds = [
            (
                "tools.web.search_max_results",
                self.search_max_results,
                1,
                WEB_SEARCH_MAX_RESULTS_CEILING,
            ),
            (
                "tools.web.fetch_max_bytes",
                self.fetch_max_bytes,
                WEB_FETCH_MIN_BYTES,
                WEB_FETCH_MAX_BYTES_CEILING,
            ),
            (
                "tools.web.fetch_max_chars",
                self.fetch_max_chars,
                WEB_FETCH_MIN_CHARS,
                WEB_FETCH_MAX_CHARS_CEILING,
            ),
            (
                "tools.web.max_redirects",
                self.max_redirects,
                0,
                WEB_MAX_REDIRECT_CEILING,
            ),
        ];
        for (key, value, low, high) in bounds {
            if value < low || value > high {
                return Err(HarnessError::ConfigurationError(format!(
                    "{key} must be between {low} and {high}, got {value}"
                )));
            }
        }
        if self.timeout_secs == 0 || self.timeout_secs > WEB_TIMEOUT_CEILING_SECS {
            return Err(HarnessError::ConfigurationError(format!(
                "tools.web.timeout_secs must be between 1 and {WEB_TIMEOUT_CEILING_SECS}, got {}",
                self.timeout_secs
            )));
        }
        if self.cache_ttl_secs > WEB_CACHE_CEILING_SECS {
            return Err(HarnessError::ConfigurationError(format!(
                "tools.web.cache_ttl_secs must be at most {WEB_CACHE_CEILING_SECS}, got {}",
                self.cache_ttl_secs
            )));
        }
        if self.user_agent.trim().is_empty() {
            return Err(HarnessError::ConfigurationError(
                "tools.web.user_agent must not be empty: it identifies the agent and is what \
                 robots.txt is matched against"
                    .to_string(),
            ));
        }
        // A missing key is *reported* rather than fatal, and that is a deliberate difference from
        // the bounds above. Both `luna config set tools.web.search_backend brave` and the setup flow
        // write this file before the shell that will hold the key has exported it, so a hard failure
        // here would refuse to save the choice the user just made. The cost of a warning is one
        // clear tool result the first time `web_search` runs — the same sentence, from
        // [`WebSettings::api_key_problem`], which is also what the tool returns.
        if let Some(problem) = self.api_key_problem() {
            tracing::warn!("{problem}");
        }
        Ok(())
    }

    /// What is wrong with the configured search key, or `None` when there is nothing to fix.
    ///
    /// Written once and used three times — the warning at start-up, the tool's refusal, and the
    /// message a user reads — so the three cannot drift apart.
    pub fn api_key_problem(&self) -> Option<String> {
        if !self.enabled || self.search_backend == SearchBackend::Disabled {
            return None;
        }
        if !self.search_backend.requires_api_key() || self.api_key_present() {
            return None;
        }
        Some(format!(
            "tools.web.search_backend is '{}', which needs an API key: set {} (or point \
             tools.web.search_api_key_env at the variable that holds it), or use 'duckduckgo', \
             which needs none",
            self.search_backend,
            self.api_key_env()
        ))
    }
}

/// Why the web tools should not be registered, or `None` when they should be.
///
/// The reason is a string and not a bool because both answers are worth reporting: a user who set
/// `tools.network: deny` and then wonders why `web_search` is missing is owed the sentence that
/// connects the two.
pub fn web_tools_blocked_reason(tools: &ToolsSettings) -> Option<String> {
    if !tools.web.enabled {
        return Some("tools.web.enabled is false".to_string());
    }
    if tools.network.denies_sockets() {
        return Some(
            "tools.network is 'deny', which web_search/web_fetch would bypass (that policy confines \
             a terminal child, not the harness itself): set tools.network to 'allow' to use them"
                .to_string(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::netguard::NetworkPolicy;

    #[test]
    fn the_default_is_a_keyless_search_and_a_fetch_that_honours_robots() {
        let settings = WebSettings::default();
        assert!(settings.enabled);
        assert_eq!(settings.search_backend, SearchBackend::DuckDuckGo);
        assert!(settings.search_backend.is_keyless());
        assert!(settings.respect_robots);
        assert!(!settings.allow_private_hosts, "reaching the LAN is opt-in");
        assert!(
            settings.allowed_domains.is_empty(),
            "no list means any public host"
        );
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn every_backend_name_round_trips_through_the_file_format() {
        for name in SEARCH_BACKENDS {
            let backend = SearchBackend::parse(name).expect("a documented name");
            assert_eq!(backend.as_str(), name);
            let yaml = format!("search_backend: {name}\n");
            let parsed: WebSettings = serde_yaml::from_str(&yaml).expect("the file format agrees");
            assert_eq!(parsed.search_backend, backend, "{name}");
        }
    }

    #[test]
    fn a_backend_name_is_accepted_with_the_spellings_a_person_types() {
        assert_eq!(
            SearchBackend::parse(" DDG ").unwrap(),
            SearchBackend::DuckDuckGo
        );
        assert_eq!(SearchBackend::parse("Brave").unwrap(), SearchBackend::Brave);
        assert_eq!(
            SearchBackend::parse("off").unwrap(),
            SearchBackend::Disabled
        );
        let error = SearchBackend::parse("google").unwrap_err();
        assert!(error.contains("duckduckgo, brave, tavily, none"), "{error}");
    }

    #[test]
    fn the_wrong_backend_is_rejected_with_every_valid_option_named() {
        let error = serde_yaml::from_str::<WebSettings>("search_backend: altavista\n")
            .expect_err("an unknown backend is a parse error, not a silent default");
        assert!(error.to_string().contains("altavista"), "{error}");
    }

    #[test]
    fn the_key_env_falls_back_to_the_backend_default_and_can_be_overridden() {
        let mut settings = WebSettings {
            search_backend: SearchBackend::Brave,
            ..WebSettings::default()
        };
        assert_eq!(settings.api_key_env(), "BRAVE_SEARCH_API_KEY");
        settings.search_backend = SearchBackend::Tavily;
        assert_eq!(settings.api_key_env(), "TAVILY_API_KEY");
        settings.search_api_key_env = " LUNA_BRAVE_KEY ".to_string();
        assert_eq!(settings.api_key_env(), "LUNA_BRAVE_KEY");
        settings.search_backend = SearchBackend::DuckDuckGo;
        assert_eq!(settings.api_key_env(), "LUNA_BRAVE_KEY");
    }

    type BudgetCase = (fn(&mut WebSettings), &'static str);

    #[test]
    fn out_of_range_budgets_are_refused_with_the_key_and_the_range() {
        let cases: [BudgetCase; 7] = [
            (|s| s.search_max_results = 0, "tools.web.search_max_results"),
            (
                |s| s.search_max_results = 99,
                "tools.web.search_max_results",
            ),
            (|s| s.fetch_max_bytes = 10, "tools.web.fetch_max_bytes"),
            (|s| s.fetch_max_chars = 10, "tools.web.fetch_max_chars"),
            (|s| s.timeout_secs = 0, "tools.web.timeout_secs"),
            (|s| s.max_redirects = 9, "tools.web.max_redirects"),
            (|s| s.cache_ttl_secs = u64::MAX, "tools.web.cache_ttl_secs"),
        ];
        for (mutate, key) in cases {
            let mut settings = WebSettings::default();
            mutate(&mut settings);
            let error = settings.validate().expect_err("out of range");
            assert!(error.to_string().contains(key), "{error}");
        }
    }

    #[test]
    fn an_empty_user_agent_is_refused_because_robots_cannot_match_it() {
        let settings = WebSettings {
            user_agent: "   ".to_string(),
            ..WebSettings::default()
        };
        let error = settings.validate().expect_err("empty agent");
        assert!(
            error.to_string().contains("tools.web.user_agent"),
            "{error}"
        );
    }

    #[test]
    fn an_enabled_keyed_backend_without_a_key_reports_the_variable_and_still_validates() {
        let mut settings = WebSettings {
            search_backend: SearchBackend::Brave,
            search_api_key_env: "LUNA_TEST_ABSENT_BRAVE_KEY".to_string(),
            ..WebSettings::default()
        };
        let problem = settings.api_key_problem().expect("a key is missing");
        assert!(problem.contains("LUNA_TEST_ABSENT_BRAVE_KEY"), "{problem}");
        assert!(
            settings.validate().is_ok(),
            "the choice must still be writable: the key lives in the shell, not in the config"
        );

        // Disabled tools cannot have a key problem nobody will hit.
        settings.enabled = false;
        assert!(settings.api_key_problem().is_none());
        assert!(settings.validate().is_ok());

        // And a backend that needs no key never asks for one.
        settings.enabled = true;
        settings.search_backend = SearchBackend::DuckDuckGo;
        assert!(settings.api_key_problem().is_none());
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn a_search_backend_of_none_leaves_search_unconfigured_but_not_an_error() {
        let settings = WebSettings {
            search_backend: SearchBackend::Disabled,
            ..WebSettings::default()
        };
        assert!(!settings.search_configured());
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn the_network_deny_policy_withholds_the_web_tools_with_a_reason() {
        let mut tools = ToolsSettings::default();
        assert!(web_tools_blocked_reason(&tools).is_none());

        tools.network = NetworkPolicy::Deny;
        let reason = web_tools_blocked_reason(&tools).expect("deny withholds them");
        assert!(reason.contains("tools.network"), "{reason}");

        tools.network = NetworkPolicy::Allow;
        tools.web.enabled = false;
        let reason = web_tools_blocked_reason(&tools).expect("disabled withholds them");
        assert!(reason.contains("tools.web.enabled"), "{reason}");
    }

    #[test]
    fn the_web_tools_are_registered_by_default_and_the_config_survives_a_round_trip() {
        let tools = ToolsSettings::default();
        assert!(tools.enabled.iter().any(|name| name == "web_search"));
        assert!(tools.enabled.iter().any(|name| name == "web_fetch"));

        let yaml = serde_yaml::to_string(&tools).expect("serializable");
        assert!(yaml.contains("search_backend: duckduckgo"), "{yaml}");
        let parsed: ToolsSettings = serde_yaml::from_str(&yaml).expect("round trips");
        assert_eq!(parsed.web, tools.web);
    }
}
