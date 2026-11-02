//! What every adapter does with a reply: split it, send it, and cope with being refused.
//!
//! Two things went wrong in one place, so they are fixed in one place. A send that came back 429
//! was retried by nobody: the plain-text fallback only logged a transport error, so a rate-limited
//! reply was dropped with no line in the log at all. And the splitter cut at a byte offset, which
//! panicked on a message whose 4000th byte falls inside a multi-byte character, and cut inside a
//! fenced code block, which leaves a chunk with an unclosed fence — Telegram refuses that payload
//! with `400 Bad Request: can't parse entities`, the adapter falls back to plain text, and the
//! formatting of that chunk is lost without a word.

use std::time::Duration;

use harness_core::error::HarnessError;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use tracing::{debug, warn};

/// Attempts per chunk, including the first. A rate limit is worth waiting out; a provider that has
/// failed four times is not going to answer this reply.
const MAX_ATTEMPTS: usize = 4;

/// The longest a `Retry-After` is honoured for before giving up. A provider asking for longer is
/// asking this process to stop replying, and the operator needs to see that rather than wait it out.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// The wait before the second attempt when the provider named no delay; it doubles per attempt.
const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// A send that did not go out, with what the provider said about it.
#[derive(Debug)]
pub(crate) struct SendFailure {
    /// `None` when the request never got a response (DNS, TLS, a timeout).
    pub status: Option<StatusCode>,
    /// The seconds a rate limit asked this process to wait, when it named any.
    pub retry_after: Option<u64>,
    pub detail: String,
}

impl SendFailure {
    /// Whether the provider refused the *payload* rather than the request.
    ///
    /// Telegram answers `400` when `parse_mode` does not match the text, and accepts the same text
    /// without it. That is the one failure a retry can fix by sending something else, so it is the
    /// one the caller is told apart.
    pub fn rejected_the_payload(&self) -> bool {
        self.status == Some(StatusCode::BAD_REQUEST)
    }
}

impl std::fmt::Display for SendFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(f, "HTTP {status}: {}", self.detail),
            None => write!(f, "{}", self.detail),
        }
    }
}

/// Send one JSON payload, waiting out a rate limit and retrying a transient failure.
///
/// `what` is what the log line should call this send ("Telegram message to 12345"), so a dropped
/// reply names the reply rather than a URL.
pub(crate) async fn post_json(
    client: &Client,
    url: &str,
    headers: &[(&str, String)],
    payload: &Value,
    what: &str,
) -> Result<(), SendFailure> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut request = client.post(url).json(payload);
        for (name, value) in headers {
            request = request.header(*name, value);
        }

        let failure = match request.send().await {
            Ok(response) if response.status().is_success() => {
                debug!("{what}: sent");
                return Ok(());
            }
            Ok(response) => {
                let status = response.status();
                let mut retry_after = retry_after(response.headers(), status);
                let body = response.text().await.unwrap_or_default();
                // Telegram names the delay in the body rather than in the header.
                if retry_after.is_none() {
                    retry_after = serde_json::from_str::<Value>(&body)
                        .ok()
                        .as_ref()
                        .and_then(retry_after_from_body);
                }
                SendFailure {
                    status: Some(status),
                    retry_after,
                    detail: body.trim().chars().take(300).collect(),
                }
            }
            Err(error) => SendFailure {
                status: None,
                retry_after: None,
                detail: error.to_string(),
            },
        };

        let Some(wait) = backoff_for(attempt, &failure) else {
            return Err(failure);
        };
        if attempt >= MAX_ATTEMPTS {
            warn!("{what} still failing after {attempt} attempts ({failure}); giving up");
            return Err(failure);
        }
        warn!(
            "{what} failed ({failure}); retrying in {}ms",
            wait.as_millis()
        );
        tokio::time::sleep(wait).await;
    }
}

/// A send that never went out, as the error the caller reports.
///
/// `what` is in the message because a gateway can be talking to several chats at once and "HTTP 429"
/// on its own does not say which reply was lost.
pub(crate) fn send_error(platform: &str, what: &str, failure: SendFailure) -> HarnessError {
    HarnessError::ProviderError {
        provider: platform.to_string(),
        message: format!("{what}: {failure}"),
    }
}

/// How long to wait before trying again, or `None` when trying again cannot help.
fn backoff_for(attempt: usize, failure: &SendFailure) -> Option<Duration> {
    match failure.status {
        // The payload is the problem: the same payload will be refused the same way. The caller
        // decides whether a different payload is worth sending (`rejected_the_payload`).
        Some(status) if status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS => None,
        // A rate limit, a 5xx, or no response at all: waiting is what fixes these.
        _ => {
            let base = failure
                .retry_after
                .map(Duration::from_secs)
                .unwrap_or_else(|| {
                    BASE_BACKOFF.saturating_mul(1u32 << attempt.saturating_sub(1).min(4))
                });
            Some(base.min(MAX_BACKOFF))
        }
    }
}

/// The delay a response asked for: `Retry-After` in seconds.
///
/// `None` when it named none, which is the usual case for a plain 5xx.
pub(crate) fn retry_after(headers: &HeaderMap, status: StatusCode) -> Option<u64> {
    if status != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// The delay Telegram names in a 429 body: `{"parameters": {"retry_after": 5}}`, which it sends
/// instead of the header.
pub(crate) fn retry_after_from_body(body: &Value) -> Option<u64> {
    body.get("parameters")
        .and_then(|parameters| parameters.get("retry_after"))
        .and_then(|secs| secs.as_u64())
}

/// `text` split into the pieces a platform accepts, at most `limit` bytes each.
///
/// Cuts at a line that leaves the text outside a code block when it can, so a fence is not left
/// open; when a single block is longer than the limit it closes the fence it opened and reopens it
/// in the next chunk, which is what keeps the block's formatting instead of losing it to a parse
/// error and a plain-text fallback.
pub(crate) fn chunk_message(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(2);
    let mut chunks: Vec<String> = Vec::new();
    let mut rest = text;
    // A fence the previous chunk had to close and this one has to reopen.
    let mut reopen: Option<String> = None;

    while !rest.is_empty() {
        let prefix = reopen.take();
        let prefix_len = prefix
            .as_ref()
            .map(|line| line.len() + 1)
            .unwrap_or_default();
        let room = limit.saturating_sub(prefix_len).max(2);

        if rest.len() <= room {
            chunks.push(join(prefix, rest.trim_end_matches('\n')));
            break;
        }

        // `reopen.is_some()` is the whole point of passing it: a chunk that starts with a fence the
        // previous one closed is *inside* a block, and a scan of `rest` alone cannot see that — the
        // opening line is in the prefix. Without it the chunk would be cut with no closing fence and
        // arrive at the platform as unbalanced Markdown, which is the 400 this code exists to avoid.
        let (cut, inside_a_fence) = break_index(rest, room, prefix.is_some());
        let (head, tail) = rest.split_at(cut);
        let tail = tail.trim_start_matches('\n');
        let body = head.trim_end_matches('\n');

        if inside_a_fence {
            // The block runs past this chunk: close it here and reopen it on the other side, so
            // both halves are well-formed Markdown.
            let opening = fence_line(rest).unwrap_or_else(|| "```".to_string());
            let piece = join(prefix, &format!("{body}\n```"));
            chunks.push(piece);
            reopen = Some(opening);
        } else {
            chunks.push(join(prefix, body));
        }
        rest = tail;
    }

    chunks.retain(|chunk| !chunk.trim().is_empty());
    chunks
}

/// `prefix` and `body` as one chunk, with the newline between them when there is a prefix.
fn join(prefix: Option<String>, body: &str) -> String {
    match prefix {
        Some(prefix) => format!("{prefix}\n{body}"),
        None => body.to_string(),
    }
}

/// The byte index to cut `text` at, at most `limit` bytes in, and whether that lands inside a fence.
///
/// Never returns an index inside a character: a chunk boundary is also where a multi-byte character
/// would panic the splitter.
fn break_index(text: &str, limit: usize, opens_inside_a_fence: bool) -> (usize, bool) {
    let limit = floor_char_boundary(text, limit);
    let mut fence = opens_inside_a_fence;
    let mut last_outside_a_fence: Option<usize> = None;
    let mut last_line_end: Option<usize> = None;
    let mut position = 0;

    for line in text.split_inclusive('\n') {
        let next = position + line.len();
        if next > limit {
            break;
        }
        if is_fence(line) {
            fence = !fence;
        }
        if !fence {
            last_outside_a_fence = Some(next);
        }
        last_line_end = Some(next);
        position = next;
    }

    if let Some(index) = last_outside_a_fence {
        return (index, false);
    }
    // Nothing fitted outside a block — the whole window is inside one.
    match last_line_end {
        Some(index) => (index, true),
        // One line longer than the limit with no newline in it at all: cut it where the limit says.
        None => (limit, fence),
    }
}

/// The byte index at or before `limit` that a string may be split at.
fn floor_char_boundary(text: &str, limit: usize) -> usize {
    if limit >= text.len() {
        return text.len();
    }
    let mut index = limit;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Whether a line opens or closes a fenced code block.
fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// The fence marker the text opens with, so the second half of a split block reopens it.
fn fence_line(text: &str) -> Option<String> {
    text.lines()
        .find(|line| is_fence(line))
        .map(|line| line.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_message_is_one_chunk() {
        assert_eq!(chunk_message("hello", 100), vec!["hello".to_string()]);
    }

    #[test]
    fn a_message_is_split_at_a_line_and_nothing_is_lost() {
        let text = (1..=40)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_message(&text, 60);
        assert!(chunks.len() > 1, "{chunks:?}");
        let rejoined = chunks.join("\n");
        for line in text.lines() {
            assert!(rejoined.contains(line), "lost {line:?}");
        }
    }

    /// The bug this splitter exists to avoid: a byte offset inside a character panicked.
    #[test]
    fn a_multibyte_character_never_lands_on_a_boundary() {
        let text = "é".repeat(200);
        let chunks = chunk_message(&text, 25);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat(), text);
    }

    /// A split inside a code block has to leave both halves with a closed fence, or the platform
    /// answers 400 and the chunk is sent as plain text with its formatting gone.
    #[test]
    fn a_split_inside_a_code_block_closes_and_reopens_the_fence() {
        let mut text = String::from("before\n```rust\n");
        for line in 0..40 {
            text.push_str(&format!("let x{line} = {line};\n"));
        }
        text.push_str("```\nafter\n");

        let chunks = chunk_message(&text, 80);
        assert!(chunks.len() > 1, "{chunks:#?}");
        for chunk in &chunks {
            let fences = chunk.lines().filter(|line| is_fence(line)).count();
            assert_eq!(fences % 2, 0, "every chunk closes what it opens:\n{chunk}");
        }
        assert!(chunks[1].starts_with("```"), "{:#?}", chunks[1]);
    }

    /// A message that is one fence and nothing else still splits into balanced chunks.
    #[test]
    fn a_block_longer_than_a_chunk_splits_into_balanced_pieces() {
        let mut text = String::from("```\n");
        for line in 0..30 {
            text.push_str(&format!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa {line}\n"));
        }
        text.push_str("```\n");
        let chunks = chunk_message(&text, 100);
        assert!(chunks.len() > 2, "{chunks:#?}");
        for chunk in &chunks {
            let fences = chunk.lines().filter(|line| is_fence(line)).count();
            assert_eq!(fences % 2, 0, "unbalanced chunk:\n{chunk}");
        }
    }

    #[test]
    fn a_rate_limit_names_its_delay() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(
            retry_after(&headers, StatusCode::TOO_MANY_REQUESTS),
            Some(7)
        );
        // The header is meaningless on a success, and is not consulted there.
        assert_eq!(retry_after(&headers, StatusCode::OK), None);
    }

    #[test]
    fn telegram_names_its_delay_in_the_body() {
        let body = serde_json::json!({ "parameters": { "retry_after": 3 } });
        assert_eq!(retry_after_from_body(&body), Some(3));
        assert_eq!(retry_after_from_body(&serde_json::json!({})), None);
    }

    #[test]
    fn a_rejected_payload_is_not_retried_but_a_rate_limit_is() {
        let rejected = SendFailure {
            status: Some(StatusCode::BAD_REQUEST),
            retry_after: None,
            detail: "can't parse entities".to_string(),
        };
        assert!(rejected.rejected_the_payload());
        assert!(backoff_for(1, &rejected).is_none());

        let limited = SendFailure {
            status: Some(StatusCode::TOO_MANY_REQUESTS),
            retry_after: Some(2),
            detail: String::new(),
        };
        assert_eq!(backoff_for(1, &limited), Some(Duration::from_secs(2)));

        let transient = SendFailure {
            status: Some(StatusCode::BAD_GATEWAY),
            retry_after: None,
            detail: String::new(),
        };
        assert_eq!(backoff_for(1, &transient), Some(BASE_BACKOFF));
        assert_eq!(backoff_for(3, &transient), Some(BASE_BACKOFF * 4));
    }
}
