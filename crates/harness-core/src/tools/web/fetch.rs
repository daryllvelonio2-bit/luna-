//! `web_fetch`: read one page, and hand back its text.
//!
//! The tool is the read side of web access, and it is deliberately a *trampoline* and not a
//! question-answerer. Anthropic's hosted `web_fetch` takes a `prompt` and returns a model's answer
//! about the page; this one takes the same argument, ignores it, and says so in the result. The
//! reason is not laziness: the harness already runs a model, and a fetch that asks a second, hidden
//! model to summarise a page is a call the user cannot see, cannot cache, cannot price, and whose
//! answer the calling model cannot check against the source. Returning the text keeps the whole
//! exchange in one context, where the model that acts on the page is the model that read it.
//!
//! Everything about *reach* lives in [`super::policy`]. What is here is the reading: HTML becomes
//! text, the text is bounded, and a page fetched a moment ago is reused rather than re-requested —
//! a search result set routinely points at the same page twice, and a loop that re-reads its own
//! sources should cost nothing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::approval::ToolPreview;
use crate::config::WebSettings;
use crate::error::HarnessError;
use crate::traits::Tool;
use crate::types::ToolResult;

use super::html;
use super::policy::{Fetched, Request, WebError, WebPolicy};

/// Pages kept for reuse in one session. A transcript that reads two dozen pages does not need a
/// third dozen cached; the point is to stop the second read of the *same* page.
const CACHE_MAX_ENTRIES: usize = 32;

/// Content types that get the HTML treatment. Everything else textual is returned as it arrived.
const HTML_TYPES: [&str; 2] = ["text/html", "application/xhtml+xml"];

/// How a cached page is held.
struct Cached {
    fetched: Fetched,
    title: Option<String>,
    text: String,
    stored: Instant,
}

/// Everything the result renderer needs, gathered so the call site reads as one sentence.
struct Page<'a> {
    requested: &'a str,
    fetched: &'a Fetched,
    title: Option<&'a str>,
    text: &'a str,
    prompt: Option<&'a str>,
    reused: bool,
    max_chars: usize,
    max_bytes: usize,
}

/// `web_fetch`: fetch a URL and return its readable text.
pub struct WebFetchTool {
    policy: Arc<WebPolicy>,
    max_chars: usize,
    cache_ttl: Duration,
    cache: Mutex<HashMap<String, Cached>>,
}

impl WebFetchTool {
    /// Build the tool over the settings it was configured with.
    pub fn new(policy: Arc<WebPolicy>, settings: &WebSettings) -> Self {
        Self {
            policy,
            max_chars: settings.fetch_max_chars,
            cache_ttl: Duration::from_secs(settings.cache_ttl_secs),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// A cached page for this URL, when it is still fresh.
    fn cached(&self, url: &str) -> Option<(Fetched, Option<String>, String)> {
        if self.cache_ttl.is_zero() {
            return None;
        }
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(url)?;
        if entry.stored.elapsed() >= self.cache_ttl {
            return None;
        }
        Some((
            entry.fetched.clone(),
            entry.title.clone(),
            entry.text.clone(),
        ))
    }

    /// Remember a page, dropping the oldest entry when the cache is full.
    fn remember(&self, url: &str, fetched: &Fetched, title: Option<&str>, text: &str) {
        if self.cache_ttl.is_zero() {
            return;
        }
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        if cache.len() >= CACHE_MAX_ENTRIES {
            let oldest = cache
                .iter()
                .min_by_key(|(_, entry)| entry.stored)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            url.to_string(),
            Cached {
                fetched: fetched.clone(),
                title: title.map(str::to_string),
                text: text.to_string(),
                stored: Instant::now(),
            },
        );
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL over HTTP(S) and return the page as readable text (HTML is converted; script, \
         style and markup are removed). Read-only: it changes nothing on this machine. Redirects are \
         followed up to the configured limit, private and link-local addresses are refused, and \
         robots.txt is honoured. Results are cached briefly, so re-reading a URL you already fetched \
         in this session costs nothing. Use it after web_search, or on any URL the user names. The \
         'prompt' argument is accepted for compatibility and ignored: the page text comes back and \
         you do the reading."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The absolute URL to fetch (http or https)"
                },
                "prompt": {
                    "type": "string",
                    "description": "Ignored. Accepted for parity with other harnesses; this tool returns the page text instead of answering a question about it."
                },
                "allowed_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional per-call allowlist of domains. Narrows what the configured policy already permits; it cannot re-enable a blocked host."
                },
                "blocked_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional per-call denylist of domains. Always wins."
                }
            },
            "required": ["url"]
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
            target: args
                .get("url")
                .and_then(|url| url.as_str())
                .map(str::to_string),
            diff: None,
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let Some(raw_url) = args
            .get("url")
            .and_then(|url| url.as_str())
            .map(str::trim)
            .filter(|url| !url.is_empty())
        else {
            return Ok(ToolResult::error(
                call_id,
                "'url' is required and must be a non-empty string".to_string(),
            ));
        };
        let prompt = args
            .get("prompt")
            .and_then(|prompt| prompt.as_str())
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty());
        let (allowed, blocked) = super::domain_lists(args);

        // Vetted before the cache is consulted at all: a host the policy refuses must not be served
        // from a body this process happens to be holding.
        let url = match self.policy.check_url(raw_url) {
            Ok(url) => url,
            Err(error) => return Ok(ToolResult::error(call_id, error.to_string())),
        };
        let host = url.host_str().unwrap_or("").to_string();
        if !self.policy.host_allowed(&host, &allowed, &blocked) {
            return Ok(ToolResult::error(
                call_id,
                format!("host '{host}' is not allowed by the configured domain policy"),
            ));
        }

        let cache_key = url.to_string();
        let hit = self.cached(&cache_key);
        let reused = hit.is_some();
        let (fetched, title, text) = match hit {
            Some(hit) => hit,
            None => {
                let fetched = match self
                    .policy
                    .fetch(raw_url, &Request::get(), &allowed, &blocked, cancel_token)
                    .await
                {
                    Ok(fetched) => fetched,
                    Err(WebError::Cancelled) => return Err(HarnessError::Cancelled),
                    Err(error) => return Ok(ToolResult::error(call_id, error.to_string())),
                };
                let essence = content_essence(&fetched.content_type);
                let (title, text) = if HTML_TYPES.contains(&essence.as_str()) {
                    let document = html::extract(&fetched.body);
                    (document.title, document.text)
                } else {
                    (None, fetched.body.clone())
                };
                self.remember(&cache_key, &fetched, title.as_deref(), &text);
                (fetched, title, text)
            }
        };

        let page = Page {
            requested: raw_url,
            fetched: &fetched,
            title: title.as_deref(),
            text: &text,
            prompt,
            reused,
            max_chars: self.max_chars,
            max_bytes: self.policy.max_bytes(),
        };
        Ok(ToolResult::success(call_id, render(&page)))
    }
}

/// The essence of a content type: everything before the `;` parameters, lowercased.
fn content_essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// The model-facing result: a header, the text, and any caveat that changes how it should be read.
fn render(page: &Page<'_>) -> String {
    let length = page.text.chars().count();
    let mut out = format!("[web_fetch] {}\n", page.requested);
    if page.fetched.final_url != page.requested {
        out.push_str(&format!("redirected to: {}\n", page.fetched.final_url));
    }
    if let Some(title) = page.title {
        out.push_str(&format!("title: {title}\n"));
    }
    out.push_str(&format!("content-type: {}\n", page.fetched.content_type));
    out.push_str(&format!("length: {length} characters\n"));
    out.push_str("---\n");

    let truncated_chars = length > page.max_chars;
    if truncated_chars {
        out.extend(page.text.chars().take(page.max_chars));
    } else {
        out.push_str(page.text);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }

    let mut notes: Vec<String> = Vec::new();
    if page.fetched.truncated {
        notes.push(format!(
            "the response body was cut at the {} byte ceiling (tools.web.fetch_max_bytes): the rest \
             of the page was not read",
            page.max_bytes
        ));
    }
    if truncated_chars {
        notes.push(format!(
            "content truncated to {} characters: raise tools.web.fetch_max_chars to read further",
            page.max_chars
        ));
    }
    if page.reused {
        notes.push("served from this session's fetch cache (tools.web.cache_ttl_secs)".to_string());
    }
    if page.prompt.is_some() {
        notes.push(
            "the 'prompt' argument was ignored: this tool returns the page text and leaves the \
             question to you"
                .to_string(),
        );
    }
    if !notes.is_empty() {
        out.push('\n');
        for note in notes {
            out.push_str(&format!("note: {note}\n"));
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WebSettings;

    fn tool(settings: &WebSettings) -> WebFetchTool {
        WebFetchTool::new(Arc::new(WebPolicy::from_settings(settings)), settings)
    }

    fn fetched(body: &str, content_type: &str, truncated: bool) -> Fetched {
        Fetched {
            final_url: "https://example.com/page".to_string(),
            content_type: content_type.to_string(),
            body: body.to_string(),
            truncated,
        }
    }

    /// Render with a title, the way a real HTML fetch does.
    fn page(
        requested: &str,
        fetched: &Fetched,
        text: &str,
        prompt: Option<&str>,
        reused: bool,
    ) -> String {
        render(&Page {
            requested,
            fetched,
            title: Some("Example Page"),
            text,
            prompt,
            reused,
            max_chars: 40_000,
            max_bytes: 262_144,
        })
    }

    #[tokio::test]
    async fn a_missing_url_is_a_tool_error_and_nothing_is_requested() {
        let tool = tool(&WebSettings::default());
        let result = tool
            .execute("c1", &json!({}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.content.contains("'url' is required"),
            "{}",
            result.content
        );

        let result = tool
            .execute("c1", &json!({"url": "   "}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn a_refused_scheme_or_host_comes_back_as_a_tool_error() {
        let settings = WebSettings {
            allowed_domains: vec!["allowed.example".to_string()],
            ..WebSettings::default()
        };
        let tool = tool(&settings);

        let result = tool
            .execute(
                "c1",
                &json!({"url": "file:///etc/passwd"}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.content.contains("not fetchable"),
            "{}",
            result.content
        );

        let result = tool
            .execute(
                "c1",
                &json!({"url": "https://other.example/x"}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("not allowed"), "{}", result.content);
    }

    #[test]
    fn the_header_names_the_source_the_title_the_type_and_the_length() {
        let body = fetched("<p>hi</p>", "text/html; charset=utf-8", false);
        let rendered = page("https://example.com/page", &body, "hi", None, false);
        assert!(rendered.contains("[web_fetch] https://example.com/page"));
        assert!(rendered.contains("title: Example Page"));
        assert!(rendered.contains("content-type: text/html; charset=utf-8"));
        assert!(rendered.contains("length: 2 characters"));
        assert!(rendered.contains("---\nhi"));
        assert!(!rendered.contains("note:"));
    }

    #[test]
    fn truncation_and_the_ignored_prompt_are_stated_in_the_result() {
        // A character that appears nowhere else, so "how much of the body survived" is a count and
        // not a coincidence with the URL in the header.
        let long = "Z".repeat(5_000);
        let body = fetched(&long, "text/plain", false);
        let rendered = render(&Page {
            requested: "https://example.com/page",
            fetched: &body,
            title: None,
            text: &long,
            prompt: Some("summarise this"),
            reused: false,
            max_chars: 1_000,
            max_bytes: 262_144,
        });

        assert!(rendered.contains("length: 5000 characters"));
        assert!(rendered.contains("content truncated to 1000 characters"));
        assert!(rendered.contains("'prompt' argument was ignored"));
        assert!(!rendered.contains("fetch cache"));
        // The body really is cut, not merely annotated.
        assert_eq!(rendered.matches('Z').count(), 1_000);
    }

    #[test]
    fn the_byte_ceiling_and_the_cache_are_reported_when_they_apply() {
        let body = fetched("body", "text/html", true);
        let rendered = page("https://example.com/page", &body, "body", None, true);
        assert!(
            rendered.contains("cut at the 262144 byte ceiling"),
            "{rendered}"
        );
        assert!(rendered.contains("tools.web.fetch_max_bytes"));
        assert!(rendered.contains("fetch cache"));
        assert!(!rendered.contains("redirected to"));
    }

    #[test]
    fn a_redirect_is_named_so_the_model_cites_the_real_source() {
        let mut body = fetched("body", "text/plain", false);
        body.final_url = "https://example.com/moved".to_string();
        let rendered = page("https://example.com/page", &body, "body", None, false);
        assert!(
            rendered.contains("redirected to: https://example.com/moved"),
            "{rendered}"
        );
    }

    #[test]
    fn a_zero_ttl_disables_reuse_and_the_cache_stays_bounded() {
        let settings = WebSettings {
            cache_ttl_secs: 0,
            ..WebSettings::default()
        };
        let disabled = tool(&settings);
        let body = fetched("body", "text/plain", false);
        disabled.remember("https://example.com/page", &body, None, "body");
        assert!(disabled.cached("https://example.com/page").is_none());

        let bounded = tool(&WebSettings::default());
        for index in 0..(CACHE_MAX_ENTRIES + 5) {
            bounded.remember(&format!("https://example.com/{index}"), &body, None, "body");
        }
        assert!(bounded.cache.lock().unwrap().len() <= CACHE_MAX_ENTRIES);
    }

    #[test]
    fn a_cached_page_keeps_its_title_and_is_served_within_the_ttl() {
        let tool = tool(&WebSettings::default());
        let body = fetched("body", "text/html", false);
        tool.remember("https://example.com/page", &body, Some("Title"), "text");

        let (fetched_back, title, text) = tool
            .cached("https://example.com/page")
            .expect("fresh entry");
        assert_eq!(fetched_back.final_url, "https://example.com/page");
        assert_eq!(title.as_deref(), Some("Title"));
        assert_eq!(text, "text");
    }

    #[test]
    fn the_content_type_essence_ignores_parameters_and_case() {
        assert_eq!(content_essence("Text/HTML; charset=UTF-8"), "text/html");
        assert_eq!(content_essence("application/json"), "application/json");
        assert_eq!(content_essence(""), "");
    }
}
