//! `web_search`: find pages, through one of three providers.
//!
//! Three providers, because they fail in different ways and a user should be able to choose which
//! failure they can live with:
//!
//! - **DuckDuckGo** ([`DUCKDUCKGO_ENDPOINT`], the default) needs no account and no key, which is
//!   what makes a fresh install work. It is also the least stable contract: the endpoint is an HTML
//!   page meant for browsers, not an API, so a markup change breaks the parser. That is why the
//!   parser is the most defensive thing in this module and why it reports "no results" rather than
//!   inventing any.
//! - **Brave** ([`BRAVE_ENDPOINT`]) is a keyed, documented JSON API. Results are titles, URLs and
//!   descriptions.
//! - **Tavily** ([`TAVILY_ENDPOINT`]) is a keyed JSON API built for agents: it returns extracted
//!   page text with each result, which is useful when the follow-up question is about content
//!   rather than about which page to read.
//!
//! All three are reached through [`WebPolicy::fetch`], so the same address vetting, redirect
//! handling, byte ceiling and timeout apply whichever one is configured — a search backend is a
//! choice of *provider*, never a hole in the policy. The endpoints are constants rather than config
//! because they are facts about the providers, not preferences of the user.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::approval::ToolPreview;
use crate::config::{SearchBackend, WebSettings};
use crate::error::HarnessError;
use crate::traits::Tool;
use crate::types::ToolResult;

use super::html;
use super::policy::{Request, WebError, WebPolicy};

/// DuckDuckGo's HTML results page: no key, no JSON, no promise.
pub const DUCKDUCKGO_ENDPOINT: &str = "https://lite.duckduckgo.com/lite/";
/// Brave Search's web endpoint.
pub const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
/// Tavily's search endpoint.
pub const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";

/// Brave's key header, as its documentation names it.
const BRAVE_KEY_HEADER: &str = "x-subscription-token";
/// Language preference. Sent because a results page served in the wrong language is a wasted turn.
const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";

/// One result, after every provider has been reduced to the same three fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// `web_search`: query the configured backend and return the results as text.
pub struct WebSearchTool {
    policy: Arc<WebPolicy>,
    backend: SearchBackend,
    api_key: Option<String>,
    api_key_env: String,
    default_results: usize,
    /// The sentence to return when the key is missing, from the config that declared it.
    key_problem: Option<String>,
}

impl WebSearchTool {
    /// Build the tool over the settings it was configured with. The key is read from the
    /// environment once, here, so a later change to the variable does not silently alter a running
    /// session.
    pub fn new(policy: Arc<WebPolicy>, settings: &WebSettings) -> Self {
        let env_name = settings.api_key_env().to_string();
        let api_key = std::env::var(&env_name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Self {
            policy,
            backend: settings.search_backend,
            api_key,
            api_key_env: env_name,
            default_results: settings.search_max_results,
            key_problem: settings.api_key_problem(),
        }
    }

    /// How many results this call asked for.
    fn limit(&self, args: &serde_json::Value) -> usize {
        let asked = args
            .get("max_results")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(self.default_results);
        asked.clamp(1, crate::config::WEB_SEARCH_MAX_RESULTS_CEILING)
    }

    /// The query, or the error shown when there is none.
    fn query(args: &serde_json::Value) -> Option<&str> {
        args.get("query")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    /// Ask the configured backend.
    async fn ask(
        &self,
        query: &str,
        limit: usize,
        allowed: &[String],
        blocked: &[String],
        cancel: &CancellationToken,
    ) -> Result<Vec<SearchHit>, WebError> {
        match self.backend {
            SearchBackend::DuckDuckGo => self.duckduckgo(query, limit, cancel).await,
            SearchBackend::Brave => self.brave(query, limit, cancel).await,
            SearchBackend::Tavily => self.tavily(query, limit, allowed, blocked, cancel).await,
            // Registration never builds a search tool for `Disabled`; reaching here means a caller
            // constructed one by hand, and the honest answer is that there is no provider.
            SearchBackend::Disabled => Err(WebError::Refused(
                "no search backend is configured (tools.web.search_backend is 'none')".to_string(),
            )),
        }
    }

    /// The keyed backends need a key that was present at construction. The refusal repeats the
    /// config's own sentence so a user who set this up with `luna setup` reads the same words
    /// wherever they meet the problem.
    fn key(&self) -> Result<&str, WebError> {
        self.api_key.as_deref().ok_or_else(|| {
            WebError::Refused(self.key_problem.clone().unwrap_or_else(|| {
                format!(
                    "the {} backend needs an API key: set {}",
                    self.backend, self.api_key_env
                )
            }))
        })
    }

    /// DuckDuckGo's HTML endpoint answers a form POST.
    async fn duckduckgo(
        &self,
        query: &str,
        _limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<SearchHit>, WebError> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", query)
            .finish();
        let request = Request {
            method: reqwest::Method::POST,
            headers: vec![
                ("accept-language".to_string(), ACCEPT_LANGUAGE.to_string()),
                (
                    "content-type".to_string(),
                    "application/x-www-form-urlencoded".to_string(),
                ),
            ],
            body: Some((
                "application/x-www-form-urlencoded".to_string(),
                body.clone(),
            )),
        };
        let fetched = self
            .policy
            .fetch(DUCKDUCKGO_ENDPOINT, &request, &[], &[], cancel)
            .await?;
        // The endpoint can redirect to the non-lite HTML page; both are parsed the same way.
        Ok(parse_duckduckgo(&fetched.body))
    }

    /// Brave answers a documented JSON API and takes the key in a header.
    async fn brave(
        &self,
        query: &str,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<SearchHit>, WebError> {
        let key = self.key()?;
        let mut url = Url::parse(BRAVE_ENDPOINT)
            .map_err(|error| WebError::Transport(format!("bad endpoint: {error}")))?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("count", &limit.to_string());
        let request = Request {
            method: reqwest::Method::GET,
            headers: vec![
                (BRAVE_KEY_HEADER.to_string(), key.to_string()),
                ("accept".to_string(), "application/json".to_string()),
                ("accept-language".to_string(), ACCEPT_LANGUAGE.to_string()),
            ],
            body: None,
        };
        let fetched = self
            .policy
            .fetch(url.as_str(), &request, &[], &[], cancel)
            .await?;
        let parsed: serde_json::Value = serde_json::from_str(&fetched.body).map_err(|error| {
            WebError::Transport(format!(
                "brave returned something that is not JSON: {error}"
            ))
        })?;
        Ok(parse_brave(&parsed))
    }

    /// Tavily takes the key in the body and can filter domains server-side.
    async fn tavily(
        &self,
        query: &str,
        limit: usize,
        allowed: &[String],
        blocked: &[String],
        cancel: &CancellationToken,
    ) -> Result<Vec<SearchHit>, WebError> {
        let key = self.key()?;
        let mut payload = json!({
            "api_key": key,
            "query": query,
            "max_results": limit,
            "search_depth": "basic",
        });
        // Sent as a hint so the provider returns results the local policy will keep: the local
        // filter still runs afterwards, because a provider list is not a promise.
        if !allowed.is_empty() {
            payload["include_domains"] = json!(allowed);
        }
        if !blocked.is_empty() {
            payload["exclude_domains"] = json!(blocked);
        }
        let request = Request {
            method: reqwest::Method::POST,
            headers: vec![
                ("accept".to_string(), "application/json".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: Some(("application/json".to_string(), payload.to_string())),
        };
        let fetched = self
            .policy
            .fetch(TAVILY_ENDPOINT, &request, &[], &[], cancel)
            .await?;
        let parsed: serde_json::Value = serde_json::from_str(&fetched.body).map_err(|error| {
            WebError::Transport(format!(
                "tavily returned something that is not JSON: {error}"
            ))
        })?;
        Ok(parse_tavily(&parsed))
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web and return a numbered list of results (title, URL, snippet). Read-only. \
         Use it when the answer depends on something the repository cannot tell you: a library's \
         current API, an error message you have not seen, a version's release notes. Follow up with \
         web_fetch on the result that looks most likely, because a snippet is one sentence and \
         often the wrong one. Set allowed_domains when you already know which sites are authoritative."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query, phrased as you would type it into a search engine"
                },
                "max_results": {
                    "type": "integer",
                    "description": "How many results to return. Defaults to tools.web.search_max_results."
                },
                "allowed_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional per-call allowlist of domains (e.g. [\"docs.rs\", \"github.com\"]). Results outside it are dropped."
                },
                "blocked_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional per-call denylist of domains. Always wins."
                }
            },
            "required": ["query"]
        })
    }

    fn is_mutating(&self) -> bool {
        false
    }

    fn is_mutating_call(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        Some(ToolPreview {
            target: Self::query(args).map(|query| format!("web_search \"{query}\"")),
            diff: None,
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let Some(query) = Self::query(args) else {
            return Ok(ToolResult::error(
                call_id,
                "'query' is required and must be a non-empty string".to_string(),
            ));
        };
        let limit = self.limit(args);
        let (allowed, blocked) = super::domain_lists(args);

        let hits = match self
            .ask(query, limit, &allowed, &blocked, cancel_token)
            .await
        {
            Ok(hits) => hits,
            Err(WebError::Cancelled) => return Err(HarnessError::Cancelled),
            Err(error) => return Ok(ToolResult::error(call_id, error.to_string())),
        };

        // The provider's own domain filters are a convenience; this is the filter that counts.
        let before = hits.len();
        let hits: Vec<SearchHit> = hits
            .into_iter()
            .filter(|hit| match Url::parse(&hit.url) {
                Ok(url) => url
                    .host_str()
                    .map(|host| self.policy.host_allowed(host, &allowed, &blocked))
                    .unwrap_or(false),
                // A result whose URL cannot be parsed is not something to hand to `web_fetch`.
                Err(_) => false,
            })
            .take(limit)
            .collect();

        Ok(ToolResult::success(
            call_id,
            render(query, self.backend, &hits, before - hits.len(), limit),
        ))
    }
}

/// The model-facing result: the query, what came back, and what was dropped.
fn render(
    query: &str,
    backend: SearchBackend,
    hits: &[SearchHit],
    filtered: usize,
    limit: usize,
) -> String {
    if hits.is_empty() {
        let mut out = format!(
            "[web_search] \"{query}\" · {} · no results",
            backend.label()
        );
        if filtered > 0 {
            out.push_str(&format!(
                "\nnote: {filtered} result(s) were dropped by the domain policy"
            ));
        }
        return out;
    }

    let mut out = format!(
        "[web_search] \"{query}\" · {} · {} result(s)\n",
        backend.label(),
        hits.len()
    );
    for (index, hit) in hits.iter().enumerate() {
        out.push_str(&format!("{}. {}\n   {}\n", index + 1, hit.title, hit.url));
        if !hit.snippet.is_empty() {
            out.push_str(&format!("   {}\n", hit.snippet));
        }
    }
    if filtered > 0 {
        out.push_str(&format!(
            "note: {filtered} more result(s) were dropped by the domain policy\n"
        ));
    }
    if hits.len() < limit {
        out.push_str(&format!(
            "note: {} of the {} requested results were available\n",
            hits.len(),
            limit
        ));
    }
    out.trim_end().to_string()
}

/// Parse a DuckDuckGo HTML results page.
///
/// The lite page is a table: each result is an `<a class="result-link">` followed by a
/// `<td class="result-snippet">`. Titles and snippets are paired by position, which is the only
/// relationship the markup offers; when one list is shorter than the other the missing half is left
/// empty rather than mis-paired.
fn parse_duckduckgo(body: &str) -> Vec<SearchHit> {
    let anchors: Vec<(String, String)> = html::elements(body, "a")
        .into_iter()
        .filter(|element| element.has_class("result-link"))
        .filter_map(|element| {
            let href = element.attr("href")?;
            let url = absolute_href(href)?;
            Some((element.text, url))
        })
        .filter(|(title, url)| !title.is_empty() && !url.is_empty())
        .collect();
    let snippets: Vec<String> = html::elements(body, "td")
        .into_iter()
        .filter(|element| element.has_class("result-snippet"))
        .map(|element| element.text)
        .collect();

    anchors
        .into_iter()
        .enumerate()
        .map(|(index, (title, url))| SearchHit {
            title,
            url,
            snippet: snippets.get(index).cloned().unwrap_or_default(),
        })
        .collect()
}

/// Turn an `href` from a results page into something `web_fetch` can use.
///
/// Two shapes have to be unwrapped: a protocol-relative URL, and DuckDuckGo's own click tracker
/// (`//duckduckgo.com/l/?uddg=<encoded target>`), which would otherwise make every result look like
/// it came from duckduckgo.com and hide the real host from the domain policy.
fn absolute_href(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('#') {
        return None;
    }
    let absolute = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.to_string()
    };
    let url = Url::parse(&absolute).ok()?;
    if is_duckduckgo_redirect(&url) {
        if let Some((_, target)) = url.query_pairs().find(|(key, _)| key == "uddg") {
            // `query_pairs` percent-decodes, so the target is the real URL again.
            return Some(target.into_owned());
        }
    }
    Some(url.to_string())
}

/// Whether a URL is DuckDuckGo's `/l/` click tracker.
fn is_duckduckgo_redirect(url: &Url) -> bool {
    let host = url.host_str().unwrap_or("");
    host == "duckduckgo.com" || host.ends_with(".duckduckgo.com")
}

/// Parse Brave's `web.results[]`.
fn parse_brave(json: &serde_json::Value) -> Vec<SearchHit> {
    json.get("web")
        .and_then(|web| web.get("results"))
        .and_then(|results| results.as_array())
        .map(|results| {
            results
                .iter()
                .map(|result| SearchHit {
                    title: string_at(result, "title"),
                    url: string_at(result, "url"),
                    snippet: string_at(result, "description"),
                })
                .filter(|hit| !hit.url.is_empty() && !hit.title.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Parse Tavily's `results[]`, whose snippet field is `content`.
fn parse_tavily(json: &serde_json::Value) -> Vec<SearchHit> {
    json.get("results")
        .and_then(|results| results.as_array())
        .map(|results| {
            results
                .iter()
                .map(|result| SearchHit {
                    title: string_at(result, "title"),
                    url: string_at(result, "url"),
                    snippet: string_at(result, "content"),
                })
                .filter(|hit| !hit.url.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// A string field of a JSON object, or the empty string.
fn string_at(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|field| field.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WebSettings;

    fn tool(settings: &WebSettings) -> WebSearchTool {
        WebSearchTool::new(Arc::new(WebPolicy::from_settings(settings)), settings)
    }

    /// A trimmed copy of the shape DDG's lite endpoint returns: tracker links, a snippet cell per
    /// result, and an unrelated link that must not be mistaken for a result.
    const DDG_HTML: &str = r#"
        <html><body><table>
        <tr><td><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2F&amp;rut=abc" class='result-link'>The Rust Book</a></td></tr>
        <tr><td class='result-snippet'>The official Rust book, &#39;the book&#39;, covers the language.</td></tr>
        <tr><td><a rel="nofollow" href="//docs.rs/serde/latest/serde/" class='result-link'>serde 1.0</a></td></tr>
        <tr><td class='result-snippet'>Serde documentation on docs.rs</td></tr>
        <tr><td><a href="https://spam.example/ad" class="nav-link">next page</a></td></tr>
        </table></body></html>
    "#;

    #[test]
    fn the_duckduckgo_parser_unwraps_tracker_links_and_pairs_snippets() {
        let hits = parse_duckduckgo(DDG_HTML);
        assert_eq!(hits.len(), 2, "the nav link is not a result: {hits:?}");
        assert_eq!(hits[0].title, "The Rust Book");
        assert_eq!(
            hits[0].url, "https://doc.rust-lang.org/book/",
            "the tracker URL must be unwrapped, or the real host hides from the domain policy"
        );
        assert!(
            hits[0].snippet.contains("'the book'"),
            "{}",
            hits[0].snippet
        );
        assert_eq!(hits[1].url, "https://docs.rs/serde/latest/serde/");
        assert_eq!(hits[1].snippet, "Serde documentation on docs.rs");
    }

    #[test]
    fn a_results_page_with_nothing_on_it_is_no_results_rather_than_a_panic() {
        for body in [
            "",
            "<html><body>no results found</body></html>",
            "<a class='result-link'>",
        ] {
            assert!(parse_duckduckgo(body).is_empty(), "{body}");
        }
    }

    #[test]
    fn a_result_with_a_snippet_missing_still_keeps_its_title_and_url() {
        let html = "<a class='result-link' href='https://a.example/x'>A</a>\
                    <a class='result-link' href='https://b.example/y'>B</a>\
                    <td class='result-snippet'>only one snippet</td>";
        let hits = parse_duckduckgo(html);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].snippet, "only one snippet");
        assert_eq!(hits[1].snippet, "");
    }

    #[test]
    fn the_brave_parser_reads_its_nested_result_array() {
        let json = json!({
            "web": {
                "results": [
                    {"title": "Serde", "url": "https://docs.rs/serde", "description": "serialization"},
                    {"title": "", "url": "https://empty.example", "description": "no title"},
                    {"title": "No URL", "description": "dropped"},
                ]
            }
        });
        let hits = parse_brave(&json);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].title, "Serde");
        assert_eq!(hits[0].snippet, "serialization");
        assert!(parse_brave(&json!({})).is_empty());
        assert!(parse_brave(&json!({"web": {"results": "nonsense"}})).is_empty());
    }

    #[test]
    fn the_tavily_parser_uses_the_content_field_as_its_snippet() {
        let json = json!({
            "results": [
                {"title": "T", "url": "https://t.example", "content": "extracted text"},
                {"url": "https://untitled.example", "content": "no title, still a hit"},
            ]
        });
        let hits = parse_tavily(&json);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].snippet, "extracted text");
        assert_eq!(hits[1].title, "");
    }

    #[test]
    fn protocol_relative_and_relative_links_are_handled_without_inventing_hosts() {
        assert_eq!(
            absolute_href("//example.com/x").as_deref(),
            Some("https://example.com/x")
        );
        assert_eq!(
            absolute_href("https://example.com/x").as_deref(),
            Some("https://example.com/x")
        );
        assert_eq!(absolute_href(""), None);
        assert_eq!(absolute_href("#anchor"), None);
        assert_eq!(absolute_href("/relative/only"), None);
    }

    #[test]
    fn the_render_names_the_query_the_backend_and_what_was_dropped() {
        let hits = vec![
            SearchHit {
                title: "One".to_string(),
                url: "https://one.example".to_string(),
                snippet: "first".to_string(),
            },
            SearchHit {
                title: "Two".to_string(),
                url: "https://two.example".to_string(),
                snippet: String::new(),
            },
        ];
        let rendered = render("rust async", SearchBackend::DuckDuckGo, &hits, 3, 5);
        assert!(rendered.contains("\"rust async\" · DuckDuckGo · 2 result(s)"));
        assert!(rendered.contains("1. One\n   https://one.example\n   first"));
        assert!(rendered.contains("2. Two\n   https://two.example"));
        assert!(rendered.contains("3 more result(s) were dropped by the domain policy"));
        assert!(rendered.contains("2 of the 5 requested results were available"));
    }

    #[test]
    fn an_empty_result_set_says_so_without_a_numbered_list() {
        let rendered = render("nothing", SearchBackend::Brave, &[], 0, 8);
        assert_eq!(
            rendered,
            "[web_search] \"nothing\" · Brave Search API · no results"
        );
        let filtered = render("nothing", SearchBackend::Brave, &[], 2, 8);
        assert!(filtered.contains("2 result(s) were dropped"), "{filtered}");
    }

    #[test]
    fn the_limit_prefers_the_call_and_is_clamped_to_the_ceiling() {
        let settings = WebSettings {
            search_max_results: 4,
            ..WebSettings::default()
        };
        let tool = tool(&settings);
        assert_eq!(tool.limit(&json!({})), 4);
        assert_eq!(tool.limit(&json!({"max_results": 2})), 2);
        assert_eq!(tool.limit(&json!({"max_results": 0})), 1);
        assert_eq!(
            tool.limit(&json!({"max_results": 9999})),
            crate::config::WEB_SEARCH_MAX_RESULTS_CEILING
        );
    }

    #[tokio::test]
    async fn a_missing_query_is_a_tool_error_and_no_request_is_made() {
        let tool = tool(&WebSettings::default());
        let result = tool
            .execute("c1", &json!({}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.content.contains("'query' is required"),
            "{}",
            result.content
        );

        let result = tool
            .execute("c1", &json!({"query": "  "}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn a_keyed_backend_without_a_key_fails_before_the_network_is_touched() {
        let settings = WebSettings {
            search_backend: SearchBackend::Brave,
            search_api_key_env: "LUNA_TEST_ABSENT_BRAVE_KEY".to_string(),
            ..WebSettings::default()
        };
        let tool = tool(&settings);

        let result = tool
            .execute("c1", &json!({"query": "rust"}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.content.contains("LUNA_TEST_ABSENT_BRAVE_KEY"),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn a_backend_of_none_is_refused_rather_than_guessed_at() {
        let settings = WebSettings {
            search_backend: SearchBackend::Disabled,
            ..WebSettings::default()
        };
        let tool = tool(&settings);
        let result = tool
            .execute("c1", &json!({"query": "rust"}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.content.contains("no search backend"),
            "{}",
            result.content
        );
    }

    #[test]
    fn the_query_is_form_encoded_so_a_query_string_cannot_forge_a_parameter() {
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", "a&b=c d")
            .finish();
        assert_eq!(encoded, "q=a%26b%3Dc+d");
    }
}
