//! What the web tools are allowed to reach, and the one path through which they reach it.
//!
//! A URL handed to `web_fetch` is not a trusted input. It arrives from a model, and the text that
//! put it there may have arrived from a page the model read a moment ago — so "fetch this" is
//! attacker-influenced text, and the interesting targets are the ones that are not on the internet:
//! `http://127.0.0.1:8080/admin`, `http://169.254.169.254/latest/meta-data/` (cloud instance
//! credentials), `http://[::1]:9200/_search`, an internal wiki on a RFC1918 address. Every one of
//! those is a `GET` away from a naive fetcher, and every one of them answers to a model that cannot
//! tell an internal service from a public one.
//!
//! So the reach is decided in one place, and this is it:
//!
//! 1. **Scheme.** `http` and `https` only. `file:`, `data:` and friends are not pages.
//! 2. **Host policy.** The configured allow/deny globs, merged with the per-call lists a search
//!    result set carries. Deny always wins.
//! 3. **Address.** Every address the host resolves to must be publicly routable, unless
//!    `tools.web.allow_private_hosts` says otherwise. *Every*, not "the first one": a name that
//!    answers with a public address and a loopback address is a name that is trying to be chosen
//!    differently on the second lookup.
//! 4. **The connection itself.** The validated address is pinned into the client for that one host,
//!    so the address that was checked is the address that is dialled. Validating one resolution and
//!    then letting the client resolve again is the DNS-rebinding hole, and it is the whole reason
//!    this module builds a client per hop instead of caching one.
//! 5. **Redirects.** Followed by hand, capped, and re-run through 2–4 on every hop: a redirect to
//!    `http://169.254.169.254/` is exactly the attack a redirect policy that only counts hops
//!    would let through.
//! 6. **Body.** A content-type allowlist and a byte ceiling, applied while streaming, so a 4 GB
//!    response with no `Content-Length` cannot fill memory before it is measured.
//!
//! `robots.txt` is consulted here too, for the honest reason rather than the legal one: a page that
//! asks not to be crawled is a page whose operator has already answered the question "is this
//! welcome", and an agent that ignores that answer is a bad guest.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Mutex;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, LOCATION};
use reqwest::{Client, Method, Response, StatusCode};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::config::WebSettings;
use crate::glob::glob_match;

/// Content types that can be turned into something a model should read.
///
/// Deliberately a list and not `text/*`: `text/event-stream` never ends and `text/css` is not a
/// document, and both would otherwise pass as "text".
const TEXTUAL: [&str; 8] = [
    "text/html",
    "application/xhtml+xml",
    "text/plain",
    "text/markdown",
    "application/json",
    "application/ld+json",
    "application/xml",
    "text/xml",
];

/// What the client asks for, in preference order. Sent as the `Accept` header.
const ACCEPT_HEADER: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,application/json;q=0.9,text/plain;q=0.8,*/*;q=0.5";

/// Longest a robots.txt is read for, in bytes. The standard file is a few dozen lines; a
/// multi-megabyte one is not a robots file.
const ROBOTS_MAX_BYTES: usize = 65_536;

/// Longest a `connect()` may take, in seconds. Bounded independently of the request budget so a
/// black-holed address cannot spend the whole timeout before a byte moves.
const CONNECT_TIMEOUT_CEILING_SECS: u64 = 10;

/// What went wrong. Refusals are policy answers the model can act on ("that host is blocked");
/// transport failures are the network's, and both reach the model as a tool result rather than an
/// error the harness aborts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebError {
    /// Refused before a socket was opened, for a stated reason.
    Refused(String),
    /// The request was made and did not complete.
    Transport(String),
    /// The turn was cancelled while the request was in flight.
    Cancelled,
}

impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) | Self::Transport(reason) => f.write_str(reason),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for WebError {}

/// What a successful request produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// Where the response came from, after any redirects.
    pub final_url: String,
    /// The response's content type, essence plus parameters.
    pub content_type: String,
    /// The body as text. A non-UTF-8 body is decoded lossily: this crate carries no charset
    /// tables, and a page that announces its own encoding is still readable for the parts that are
    /// ASCII — which is most of a web page's prose.
    pub body: String,
    /// The body hit the byte ceiling and was cut.
    pub truncated: bool,
}

/// A request to make: method, extra headers, optional body.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    /// Extra headers, e.g. a search API's key header.
    pub headers: Vec<(String, String)>,
    /// `(content type, body)` for a POST.
    pub body: Option<(String, String)>,
}

impl Request {
    /// A plain `GET`.
    pub fn get() -> Self {
        Self {
            method: Method::GET,
            headers: Vec::new(),
            body: None,
        }
    }
}

/// One `robots.txt` group, reduced to path rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RobotsRules {
    /// Path prefixes that are allowed.
    allow: Vec<String>,
    /// Path prefixes that are not.
    disallow: Vec<String>,
}

impl RobotsRules {
    /// The standard's rule: the longest matching path wins, and an allow beats a disallow of the
    /// same length. No match means allowed.
    fn allows(&self, path: &str) -> bool {
        let longest = |rules: &[String]| {
            rules
                .iter()
                .filter(|rule| !rule.is_empty() && path.starts_with(rule.as_str()))
                .map(|rule| rule.len())
                .max()
        };
        match (longest(&self.allow), longest(&self.disallow)) {
            (Some(allow), Some(deny)) => allow >= deny,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => true,
        }
    }
}

/// The reach policy, built once from config and shared by both web tools.
#[derive(Debug)]
pub struct WebPolicy {
    user_agent: String,
    timeout: Duration,
    max_redirects: usize,
    max_bytes: usize,
    respect_robots: bool,
    allowed: Vec<String>,
    blocked: Vec<String>,
    allow_private: bool,
    /// Parsed robots files, by origin. Inside a mutex because both tools share one policy.
    robots: Mutex<HashMap<String, RobotsRules>>,
}

impl WebPolicy {
    /// Build the policy from the `tools.web` settings.
    pub fn from_settings(settings: &WebSettings) -> Self {
        Self {
            user_agent: settings.user_agent.clone(),
            timeout: Duration::from_secs(settings.timeout_secs),
            max_redirects: settings.max_redirects,
            max_bytes: settings.fetch_max_bytes,
            respect_robots: settings.respect_robots,
            allowed: settings.allowed_domains.clone(),
            blocked: settings.blocked_domains.clone(),
            allow_private: settings.allow_private_hosts,
            robots: Mutex::new(HashMap::new()),
        }
    }

    /// The response body ceiling, so a tool can say what it cut.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// The budget for one request, in seconds.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Whether a host passes the merged allow/deny policy.
    ///
    /// Merged, because a per-call list can only ever narrow what config permits: `allowed_domains`
    /// on a `web_search` call takes the results down to the domains the caller named, and cannot
    /// reintroduce one the config blocks.
    pub fn host_allowed(&self, host: &str, allowed: &[String], blocked: &[String]) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        for pattern in self.blocked.iter().chain(blocked.iter()) {
            if host_matches(pattern, &host) {
                return false;
            }
        }
        let allows: Vec<&String> = self.allowed.iter().chain(allowed.iter()).collect();
        if allows.is_empty() {
            return true;
        }
        allows.iter().any(|pattern| host_matches(pattern, &host))
    }

    /// The `User-Agent` these requests carry.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// Whether the configured environment is one where policy refusals should be reported loudly.
    /// Kept next to [`Self::host_allowed`] so the two answer about the same setting.
    pub fn allows_private_hosts(&self) -> bool {
        self.allow_private
    }

    /// Parse and vet a URL before anything is resolved or opened.
    pub fn check_url(&self, raw: &str) -> Result<Url, WebError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(WebError::Refused("empty URL".to_string()));
        }
        let url = Url::parse(trimmed)
            .map_err(|error| WebError::Refused(format!("'{trimmed}' is not a URL: {error}")))?;
        match url.scheme() {
            "http" | "https" => {}
            other => {
                return Err(WebError::Refused(format!(
                    "scheme '{other}' is not fetchable: only http and https are"
                )))
            }
        }
        if url.host_str().map(str::is_empty).unwrap_or(true) {
            return Err(WebError::Refused(format!("'{trimmed}' has no host")));
        }
        // Credentials in a URL are a leak (they reach the access log, the error message and the
        // transcript) and are almost always a redirect target crafted to look legitimate.
        if !url.username().is_empty() || url.password().is_some() {
            return Err(WebError::Refused(
                "the URL carries credentials: send an authenticated API through its key header, \
                 not through userinfo"
                    .to_string(),
            ));
        }
        Ok(url)
    }

    /// Make the request, following redirects by hand.
    pub async fn fetch(
        &self,
        raw: &str,
        request: &Request,
        allowed: &[String],
        blocked: &[String],
        cancel: &CancellationToken,
    ) -> Result<Fetched, WebError> {
        let mut current = self.check_url(raw)?;

        for hop in 0..=self.max_redirects {
            self.check_host(&current, allowed, blocked)?;
            if self.respect_robots && !self.robots_allows(&current, cancel).await {
                return Err(WebError::Refused(format!(
                    "{} disallows this path for '{}' in robots.txt",
                    current.origin().ascii_serialization(),
                    self.user_agent
                )));
            }

            let client = self.client_for(&current).await?;
            let outcome = tokio::select! {
                _ = cancel.cancelled() => return Err(WebError::Cancelled),
                outcome = tokio::time::timeout(self.timeout, self.hop(&client, &current, request)) => {
                    match outcome {
                        Err(_) => {
                            return Err(WebError::Transport(format!(
                                "no response within {}s",
                                self.timeout.as_secs()
                            )))
                        }
                        Ok(Err(error)) => return Err(error),
                        Ok(Ok(outcome)) => outcome,
                    }
                }
            };

            match outcome {
                Hop::Body(fetched) => return Ok(fetched),
                Hop::Redirect(location) => {
                    if hop == self.max_redirects {
                        return Err(WebError::Refused(format!(
                            "more than {} redirects, starting at {}",
                            self.max_redirects, raw
                        )));
                    }
                    // Re-vetted on the next turn of the loop: a redirect is a second URL the model
                    // did not write and the page did.
                    current = current.join(&location).map_err(|error| {
                        WebError::Refused(format!("redirect target '{location}' is not a URL: {error}"))
                    })?;
                }
            }
        }

        Err(WebError::Transport(format!(
            "redirect chain from {raw} ended without a response"
        )))
    }

    /// Vet one URL's host and port before dialling it.
    fn check_host(&self, url: &Url, allowed: &[String], blocked: &[String]) -> Result<(), WebError> {
        let host = url
            .host_str()
            .ok_or_else(|| WebError::Refused("the URL has no host".to_string()))?;
        if !self.host_allowed(host, allowed, blocked) {
            return Err(WebError::Refused(format!(
                "host '{host}' is not allowed by tools.web.allowed_domains / \
                 tools.web.blocked_domains (or the per-call domain lists)"
            )));
        }
        Ok(())
    }

    /// Resolve `url`'s host, vet every address, and return a client bound to the vetted one.
    async fn client_for(&self, url: &Url) -> Result<Client, WebError> {
        let host = url
            .host_str()
            .ok_or_else(|| WebError::Refused("the URL has no host".to_string()))?
            .to_string();
        let port = url.port_or_known_default().unwrap_or(443);
        let literal: Option<IpAddr> = host.parse().ok();

        let addresses: Vec<SocketAddr> = match literal {
            Some(ip) => vec![SocketAddr::new(ip, port)],
            None => tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|error| {
                    WebError::Transport(format!("cannot resolve '{host}': {error}"))
                })?
                .collect(),
        };
        if addresses.is_empty() {
            return Err(WebError::Transport(format!(
                "'{host}' resolved to no address"
            )));
        }
        if !self.allow_private {
            for address in &addresses {
                if !is_public(address.ip()) {
                    return Err(WebError::Refused(format!(
                        "'{host}' resolves to {}, which is not a public address: set \
                         tools.web.allow_private_hosts only to reach a service you run yourself",
                        address.ip()
                    )));
                }
            }
        }

        let mut builder = Client::builder()
            .user_agent(self.user_agent.clone())
            .timeout(self.timeout)
            .connect_timeout(self.timeout.min(Duration::from_secs(CONNECT_TIMEOUT_CEILING_SECS)))
            .redirect(reqwest::redirect::Policy::none())
            // One request per client: a pool would outlive the validation above.
            .pool_max_idle_per_host(0);
        if literal.is_none() {
            // Pin the address that was checked. Without this the client would resolve the name a
            // second time, and the answer to the second lookup is not the answer that was vetted.
            builder = builder.resolve(&host, addresses[0]);
        }
        builder
            .build()
            .map_err(|error| WebError::Transport(format!("cannot build an HTTP client: {error}")))
    }

    /// One request: send it, or read its body.
    async fn hop(&self, client: &Client, url: &Url, request: &Request) -> Result<Hop, WebError> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(ACCEPT_HEADER));
        for (name, value) in &request.headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| WebError::Transport(format!("bad header '{name}': {error}")))?;
            let value = HeaderValue::from_str(value)
                .map_err(|error| WebError::Transport(format!("bad header value: {error}")))?;
            headers.insert(name, value);
        }

        let mut outgoing = client
            .request(request.method.clone(), url.clone())
            .headers(headers);
        if let Some((content_type, body)) = &request.body {
            let content_type = HeaderValue::from_str(content_type)
                .map_err(|error| WebError::Transport(format!("bad content type: {error}")))?;
            outgoing = outgoing
                .header(CONTENT_TYPE, content_type)
                .body(body.clone());
        }

        let response = outgoing
            .send()
            .await
            .map_err(|error| WebError::Transport(describe_transport(&error, url)))?;
        let status = response.status();

        if status.is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            return match location {
                // A 3xx with no Location is a dead end, not a body to read.
                Some(location) => Ok(Hop::Redirect(location)),
                None => Err(WebError::Transport(format!(
                    "{status} from {url} with no Location header"
                ))),
            };
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(WebError::Transport(format!(
                "{status} from {url}: the site refused this request (an API key or a different \
                 User-Agent may be needed)"
            )));
        }
        if !status.is_success() {
            return Err(WebError::Transport(format!("{status} from {url}")));
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let essence = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !TEXTUAL.contains(&essence.as_str()) {
            let shown = if essence.is_empty() {
                "no content type"
            } else {
                essence.as_str()
            };
            return Err(WebError::Refused(format!(
                "{url} is {shown}, which is not text: only {} are read",
                TEXTUAL.join(", ")
            )));
        }

        let (body, truncated) = self.read_body(response).await?;
        Ok(Hop::Body(Fetched {
            final_url: url.to_string(),
            content_type,
            body: String::from_utf8_lossy(&body).into_owned(),
            truncated,
        }))
    }

    /// Read the body up to the byte ceiling, abandoning the rest of the stream.
    async fn read_body(&self, response: Response) -> Result<(Vec<u8>, bool), WebError> {
        let mut stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|error| WebError::Transport(format!("the body failed: {error}")))?;
            let remaining = self.max_bytes.saturating_sub(buffer.len());
            if chunk.len() >= remaining {
                buffer.extend_from_slice(&chunk[..remaining]);
                // The stream is dropped here: the rest of a larger body is not read, so a page that
                // never ends cannot hold the turn open.
                return Ok((buffer, true));
            }
            buffer.extend_from_slice(&chunk);
        }
        Ok((buffer, false))
    }

    /// Whether `robots.txt` for this URL's origin permits the path.
    ///
    /// Fails open: a robots file that cannot be read is not an answer, and treating an outage as a
    /// blanket refusal would make the tool look broken exactly when the network is unreliable.
    async fn robots_allows(&self, url: &Url, cancel: &CancellationToken) -> bool {
        let origin = url.origin().ascii_serialization();
        if let Some(rules) = self.cached_robots(&origin) {
            return rules.allows(url.path());
        }

        let robots_url = format!("{origin}/robots.txt");
        let rules = match self.robots_rules(&robots_url, cancel).await {
            Ok(rules) => rules,
            Err(error) => {
                tracing::debug!("robots.txt for {origin} could not be read ({}): allowing", error);
                return true;
            }
        };
        let allowed = rules.allows(url.path());
        if let Ok(mut cache) = self.robots.lock() {
            cache.insert(origin, rules);
        }
        allowed
    }

    /// The cached rules for an origin, if they were read in this session.
    fn cached_robots(&self, origin: &str) -> Option<RobotsRules> {
        self.robots
            .lock()
            .ok()
            .and_then(|cache| cache.get(origin).cloned())
    }

    /// Fetch and parse a robots file. The request goes through the same address vetting as any
    /// other, but not through the robots check itself, for the obvious reason.
    async fn robots_rules(
        &self,
        robots_url: &str,
        cancel: &CancellationToken,
    ) -> Result<RobotsRules, WebError> {
        let url = self.check_url(robots_url)?;
        let client = self.client_for(&url).await?;
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(WebError::Cancelled),
            response = tokio::time::timeout(self.timeout, client.get(url.clone()).send()) => {
                match response {
                    Err(_) => return Err(WebError::Transport("robots.txt timed out".to_string())),
                    Ok(Err(error)) => {
                        return Err(WebError::Transport(describe_transport(&error, &url)))
                    }
                    Ok(Ok(response)) => response,
                }
            }
        };
        if !response.status().is_success() {
            // No robots file is the common case, and it means "no restrictions".
            return Ok(RobotsRules::default());
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|error| WebError::Transport(format!("robots.txt failed: {error}")))?;
            let remaining = ROBOTS_MAX_BYTES.saturating_sub(body.len());
            if chunk.len() >= remaining {
                body.extend_from_slice(&chunk[..remaining]);
                break;
            }
            body.extend_from_slice(&chunk);
        }
        Ok(parse_robots(&String::from_utf8_lossy(&body), &self.user_agent_token()))
    }

    /// The product token robots groups are matched against: the `User-Agent` up to the first `/`
    /// or space, lowercased. `LunaHarness/0.1 (+https://…)` matches `User-agent: LunaHarness`.
    fn user_agent_token(&self) -> String {
        self.user_agent
            .split(['/', ' '])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    }
}

/// One hop's answer: either the response, or where to look next.
enum Hop {
    Body(Fetched),
    Redirect(String),
}

/// Is `ip` an address the public internet can route to?
///
/// A deny-list, because the set of public addresses is the whole space minus a handful of special
/// ranges — and each range here is denied for a reason worth naming: loopback and the private
/// blocks are the host's own services, link-local holds the cloud metadata endpoint, CGNAT and the
/// documentation ranges are not a destination anyone means, and multicast/broadcast are not a
/// request. Tested as a table, because this is the function a mistake in would be silent.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    let [a, b, _, _] = octets;
    if ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_broadcast() {
        return false;
    }
    if ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    match (a, b) {
        // 100.64.0.0/10: carrier-grade NAT, which is someone's internal network.
        (100, 64..=127) => false,
        // 192.0.0.0/24: IETF protocol assignments, including the 6to4 relay anycast address.
        (192, 0) => false,
        // 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24: the documentation ranges. Never a real
        // service, so reaching one means a typo or a crafted address rather than a page.
        (192, 2) | (198, 51) | (203, 0) => false,
        // 198.18.0.0/15: benchmarking.
        (198, 18..=19) => false,
        // 240.0.0.0/4: reserved, and 255.255.255.255 with it.
        (240..=255, _) => false,
        _ => true,
    }
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    // An IPv4-mapped address is an IPv4 destination wearing an IPv6 hat, and the check must agree
    // with `is_public_v4` or `::ffff:127.0.0.1` becomes a way around it.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    let segments = ip.segments();
    // fc00::/7 unique-local, fe80::/10 link-local, 2001:db8::/32 documentation.
    if segments[0] & 0xfe00 == 0xfc00 || segments[0] & 0xffc0 == 0xfe80 {
        return false;
    }
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return false;
    }
    true
}

/// Whether a configured pattern matches a host. A bare domain also covers its subdomains, because
/// that is what someone writing `docs.rs` means, and a `*.` prefix is accepted for the same reason.
fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().to_ascii_lowercase();
    if pattern.is_empty() {
        return false;
    }
    let bare = pattern.trim_start_matches("*.").to_string();
    glob_match(&pattern, host)
        || glob_match(&bare, host)
        || glob_match(&format!("*.{bare}"), host)
}

/// Parse a robots file for one agent token, keeping only the groups that apply to it.
///
/// Groups that name our token win over `*`; when either is absent the other is used. A `Disallow:`
/// with an empty value is the standard's way of saying "nothing here is disallowed" and is dropped
/// rather than stored as a rule that matches everything.
fn parse_robots(text: &str, agent: &str) -> RobotsRules {
    let mut specific = RobotsRules::default();
    let mut wildcard = RobotsRules::default();
    // `None` while a group header has not been seen; otherwise which group is being read.
    let mut target: Option<bool> = None;

    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        let field = field.trim().to_ascii_lowercase();
        let value = value.trim();
        match field.as_str() {
            "user-agent" => {
                let token = value.to_ascii_lowercase();
                target = Some(!agent.is_empty() && token.starts_with(agent));
            }
            "allow" | "disallow" => {
                let Some(is_specific) = target else { continue };
                if value.is_empty() {
                    continue;
                }
                let rules = if is_specific {
                    &mut specific
                } else {
                    &mut wildcard
                };
                if field == "allow" {
                    rules.allow.push(value.to_string());
                } else {
                    rules.disallow.push(value.to_string());
                }
            }
            _ => {}
        }
    }

    if specific.allow.is_empty() && specific.disallow.is_empty() {
        wildcard
    } else {
        specific
    }
}

/// A transport failure, with the URL it happened on: reqwest's own message names neither.
fn describe_transport(error: &reqwest::Error, url: &Url) -> String {
    if error.is_timeout() {
        return format!("{url} timed out");
    }
    if error.is_connect() {
        return format!("cannot connect to {url}: {error}");
    }
    format!("{url} failed: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WebSettings;

    fn settings() -> WebSettings {
        WebSettings::default()
    }

    fn policy() -> WebPolicy {
        WebPolicy::from_settings(&settings())
    }

    #[test]
    fn loopback_private_link_local_and_mapped_addresses_are_not_public() {
        let refused = [
            "127.0.0.1",
            "127.9.9.9",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "192.0.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "192.0.2.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
        ];
        for address in refused {
            let ip: IpAddr = address.parse().expect("a literal address");
            assert!(!is_public(ip), "{address} must not be reachable");
        }

        let allowed = ["1.1.1.1", "93.184.216.34", "172.32.0.1", "2606:4700::1111"];
        for address in allowed {
            let ip: IpAddr = address.parse().expect("a literal address");
            assert!(is_public(ip), "{address} should be reachable");
        }
    }

    #[test]
    fn only_http_and_https_are_fetchable_and_credentials_are_refused() {
        let policy = policy();
        assert!(policy.check_url("https://example.com/a?b=c").is_ok());
        assert!(policy.check_url("http://example.com").is_ok());
        for bad in [
            "file:///etc/passwd",
            "data:text/plain,hi",
            "ftp://example.com/x",
            "https://user:pass@example.com/x",
            "https://user@example.com/x",
            "not a url",
            "",
            "https://",
        ] {
            assert!(
                matches!(policy.check_url(bad), Err(WebError::Refused(_))),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn the_domain_policy_denies_before_it_allows_and_subdomains_follow_their_domain() {
        let mut settings = settings();
        settings.allowed_domains = vec!["docs.rs".to_string(), "*.example.org".to_string()];
        settings.blocked_domains = vec!["secret.docs.rs".to_string()];
        let policy = WebPolicy::from_settings(&settings);

        assert!(policy.host_allowed("docs.rs", &[], &[]));
        assert!(policy.host_allowed("SUB.docs.rs", &[], &[]));
        assert!(!policy.host_allowed("secret.docs.rs", &[], &[]));
        assert!(policy.host_allowed("www.example.org", &[], &[]));
        assert!(!policy.host_allowed("evil.com", &[], &[]));
        // A per-call allowlist narrows; it cannot widen past the block list.
        assert!(policy.host_allowed("evil.com", &["evil.com".to_string()], &[]));
        assert!(!policy.host_allowed("docs.rs", &[], &["docs.rs".to_string()]));
    }

    #[test]
    fn a_domain_list_that_is_empty_permits_any_host() {
        let policy = policy();
        assert!(policy.host_allowed("anything.example", &[], &[]));
        assert!(!policy.host_allowed("anything.example", &[], &["anything.example".to_string()]));
    }

    #[test]
    fn robots_rules_keep_the_longest_match_and_an_allow_wins_a_tie() {
        let rules = parse_robots(
            "User-agent: *\nDisallow: /private\nAllow: /private/open\nUser-agent: LunaHarness\n\
             Disallow: /draft\n",
            "lunaharness",
        );
        // Our own group exists, so the wildcard group is not consulted.
        assert!(rules.allows("/public"));
        assert!(rules.allows("/private/secret"), "wildcard group is not ours");
        assert!(!rules.allows("/draft"));
        assert!(!rules.allows("/draft/2026"));

        let tie = RobotsRules {
            allow: vec!["/a".to_string()],
            disallow: vec!["/a".to_string()],
        };
        assert!(tie.allows("/a/b"), "an allow beats a disallow of equal length");
    }

    #[test]
    fn a_robots_file_with_only_wildcards_applies_to_everyone() {
        let rules = parse_robots("User-agent: *\nDisallow: /all\n", "lunaharness");
        assert!(!rules.allows("/all"));
        assert!(rules.allows("/other"));
        // An empty Disallow is "nothing is disallowed", not "everything is".
        let empty = parse_robots("User-agent: *\nDisallow:\n", "lunaharness");
        assert!(empty.allows("/anything"));
    }

    #[test]
    fn a_robots_file_that_names_no_one_leaves_everything_open() {
        let rules = parse_robots("# just a comment\nSitemap: https://x/sitemap.xml\n", "lunaharness");
        assert!(rules.allows("/anything"));
    }

    #[test]
    fn the_user_agent_token_is_the_product_name() {
        let policy = WebPolicy::from_settings(&settings());
        assert_eq!(policy.user_agent_token(), "lunaharness");
    }

    #[test]
    fn the_private_address_escape_hatch_is_off_unless_asked_for() {
        assert!(!policy().allows_private_hosts());
        let mut permissive = settings();
        permissive.allow_private_hosts = true;
        assert!(WebPolicy::from_settings(&permissive).allows_private_hosts());
    }
}
