//! Web access: `web_search` and `web_fetch`, and the policy that decides what they may reach.
//!
//! Four modules, split along the seams that make each one testable on its own:
//!
//! - [`policy`]: scheme, host, address and redirect rules, plus the one `fetch` every request goes
//!   through. This is the security boundary, so it has no knowledge of search engines or HTML.
//! - [`html`]: HTML in, text out. A tag scanner, not a parser — see its own documentation for what
//!   it deliberately does not do.
//! - [`fetch`]: `web_fetch`. One URL in, readable text out, with a short-lived cache.
//! - [`search`]: `web_search`. One query in, a numbered result list out, through DuckDuckGo,
//!   Brave or Tavily.
//!
//! [`register`] is the single entry point `tools::register_core_tools` calls. It owns the two
//! questions the tools themselves should not have to answer: is this pair switched on, and has
//! something else — `tools.network: deny` — already said this machine does not talk to the network.

pub mod fetch;
pub mod html;
pub mod policy;
pub mod search;

use std::sync::Arc;

use crate::config::{Config, SearchBackend};
use crate::error::HarnessError;
use crate::traits::ToolRegistry;

pub use fetch::WebFetchTool;
pub use policy::{Fetched, Request, WebError, WebPolicy};
pub use search::WebSearchTool;

/// The search tool's name, as it appears in `tools.enabled` and to the model.
pub const NAME_SEARCH: &str = "web_search";
/// The fetch tool's name.
pub const NAME_FETCH: &str = "web_fetch";

/// The per-call domain lists both tools accept, trimmed of blanks and non-strings.
///
/// Shared so the two tools cannot disagree about what `allowed_domains: [" "]` means, and so the
/// schema text has one implementation behind it.
pub fn domain_lists(args: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let read = |key: &str| {
        args.get(key)
            .and_then(|value| value.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default()
    };
    (read("allowed_domains"), read("blocked_domains"))
}

/// Register whichever of the two tools are enabled, or explain why neither is.
///
/// Withheld rather than failed: `tools.network: deny` is a statement about the machine, and a
/// start-up error would force a user to edit their config before they could run the harness at all.
/// A warning naming the two keys is the honest answer, and the tools stay absent.
pub async fn register(
    registry: &Arc<dyn ToolRegistry>,
    config: &Config,
) -> Result<(), HarnessError> {
    let wanted = |name: &str| config.tools.enabled.iter().any(|tool| tool == name);
    let want_search = wanted(NAME_SEARCH);
    let want_fetch = wanted(NAME_FETCH);
    if !want_search && !want_fetch {
        return Ok(());
    }

    if let Some(reason) = crate::config::web_tools_blocked_reason(&config.tools) {
        tracing::warn!("web tools are withheld: {reason}");
        return Ok(());
    }

    // One policy, shared: both tools must answer "may I reach this host" the same way, and the
    // robots cache is worth having once rather than twice.
    let policy = Arc::new(WebPolicy::from_settings(&config.tools.web));

    if want_search {
        if config.tools.web.search_backend == SearchBackend::Disabled {
            tracing::warn!(
                "{NAME_SEARCH} is in tools.enabled but tools.web.search_backend is 'none': \
                 not registering it ({NAME_FETCH} is unaffected)"
            );
        } else {
            registry
                .register(Arc::new(WebSearchTool::new(
                    policy.clone(),
                    &config.tools.web,
                )))
                .await?;
        }
    }
    if want_fetch {
        registry
            .register(Arc::new(WebFetchTool::new(policy, &config.tools.web)))
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn per_call_domain_lists_are_read_trimmed_and_filtered() {
        let (allowed, blocked) = domain_lists(&json!({
            "allowed_domains": [" a.example ", "", 7, "b.example"],
            "blocked_domains": "not-an-array",
        }));
        assert_eq!(
            allowed,
            vec!["a.example".to_string(), "b.example".to_string()]
        );
        assert!(blocked.is_empty());

        let (allowed, blocked) = domain_lists(&json!({}));
        assert!(allowed.is_empty() && blocked.is_empty());
    }
}
