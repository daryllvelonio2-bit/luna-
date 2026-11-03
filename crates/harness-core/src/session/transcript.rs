//! What the harness sends: the replayed transcript, and the folds it has retired.
//!
//! A session is stored as rows; a request is a vector of messages. This is the seam between
//! the two: the rows a fold retired stop being sent, and the rows that no longer make sense
//! on their own stop being sent too.

use std::collections::HashSet;

use tracing::debug;

use super::StoredMessage;
use crate::types::{Message, Role};

/// The sequence of the newest fold in `history`, if the session has been folded.
///
/// A summary row is written with the highest `seq` in the session at the moment of the fold, so it
/// is the watermark that separates usage reports made against a transcript that still exists from
/// reports made against one a later fold has replaced.
pub fn last_fold_seq(history: &[StoredMessage]) -> Option<i64> {
    history
        .iter()
        .filter(|stored| stored.is_summary)
        .map(|stored| stored.seq)
        .max()
}

/// The replayed rows as the messages to send, with the size reports a later fold retired dropped.
///
/// [`Message::usage`] is not an annotation on a message: it is the size of the *request* the
/// provider answered, which covers everything up to that reply. Once a fold has written a newer
/// summary, that request's transcript is gone, so the number describes nothing that will be sent
/// again — and both the context gauge and [`crate::AgentConfig::compact_at_tokens`] read it as the
/// size of the session, which makes the readout show history the fold removed and makes the next
/// turn fold against a transcript that is not there. Dropping it makes the estimate fall back to
/// the character heuristic over what is actually sent.
///
/// A tool result whose call is not in the transcript is dropped as well. It answers nothing, and a
/// provider rejects the whole request over it — NVIDIA answers `HTTP 400 {"message":"Invalid
/// request"}`. A fold that cut a tool round in half leaves exactly that behind: the call went into
/// the summary and its answers stayed live. The summary stands in for the round they belonged to,
/// so the answers are what the model can do without.
pub fn sendable_messages(history: &[StoredMessage]) -> Vec<Message> {
    let fold_seq = last_fold_seq(history);
    let mut announced: HashSet<String> = HashSet::new();
    let mut messages = Vec::with_capacity(history.len());
    for stored in history {
        let mut message = stored.message.clone();
        if fold_seq.is_some_and(|fold_seq| stored.seq < fold_seq) {
            message.usage = None;
        }
        if message.role == Role::Tool {
            let call_id = message
                .tool_result
                .as_ref()
                .map(|result| result.call_id.clone())
                .unwrap_or_default();
            if !announced.contains(&call_id) {
                debug!(
                    "Dropping an orphaned tool result: no call in the transcript answers '{}'",
                    call_id
                );
                continue;
            }
        }
        if message.role == Role::Assistant {
            for call in &message.tool_calls {
                announced.insert(call.id.clone());
            }
        }
        messages.push(message);
    }
    elide_replayed_images(&mut messages);
    messages
}

/// How many of the most recent image-bearing user messages keep their pixels when replayed.
///
/// A pasted screenshot belongs to the conversation that was about it. Two user messages later the
/// conversation has moved on, but the pixels were still posted on every request for the rest of the
/// session: one tool-heavy session replayed 2.3 MB of base64 — three screenshots, one of them
/// duplicated — on every one of its tool rounds. The marker keeps the fact that the image was
/// there, so a later turn can still refer to it without the request carrying it.
const REPLAYED_IMAGE_MESSAGES: usize = 2;

/// What replaces an inline image whose pixels are no longer re-sent.
const INLINE_IMAGE_ELIDED: &str = "[inline image from an earlier message: pixels not re-sent]";

/// Replace the inline images of user messages older than the newest [`REPLAYED_IMAGE_MESSAGES`]
/// image-bearing ones with a marker.
fn elide_replayed_images(messages: &mut [Message]) {
    let mut seen = 0usize;
    let mut keep = vec![false; messages.len()];
    for (index, message) in messages.iter().enumerate().rev() {
        if message.role != Role::User {
            continue;
        }
        if message.content.as_deref().is_some_and(has_inline_image) {
            if seen < REPLAYED_IMAGE_MESSAGES {
                keep[index] = true;
            }
            seen += 1;
        }
    }
    for (message, keep) in messages.iter_mut().zip(keep) {
        if keep {
            continue;
        }
        if let Some(content) = message.content.as_mut() {
            *content = without_inline_images(content);
        }
    }
}

/// Whether `text` carries an inline image payload, as opposed to a sentence about one.
fn has_inline_image(text: &str) -> bool {
    let mut rest = text;
    while let Some(start) = rest.find("data:image/") {
        let after_start = &rest[start..];
        let end = after_start
            .find(char::is_whitespace)
            .unwrap_or(after_start.len());
        if is_inline_image_url(&after_start[..end]) {
            return true;
        }
        rest = &after_start[end..];
    }
    false
}

/// Whether a `data:image/...` token is a real payload rather than prose quoting the format.
///
/// Real inline images are kilobytes of base64; a documentation example is a handful of characters.
/// The length floor is what keeps `data:image/png;base64,...` in a sentence from being rewritten.
fn is_inline_image_url(url: &str) -> bool {
    url.strip_prefix("data:image/")
        .and_then(|rest| rest.split_once(";base64,"))
        .is_some_and(|(mime, payload)| !mime.is_empty() && payload.len() >= 64)
}

/// Replace every inline image URL in `text` with [`INLINE_IMAGE_ELIDED`].
///
/// A non-payload occurrence — a link, a path, a sentence about one — is left alone: it costs a few
/// bytes and removing it would change what the user wrote.
fn without_inline_images(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("data:image/") {
        out.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let end = after_start
            .find(char::is_whitespace)
            .unwrap_or(after_start.len());
        let url = &after_start[..end];
        if is_inline_image_url(url) {
            out.push_str(INLINE_IMAGE_ELIDED);
        } else {
            out.push_str(url);
        }
        rest = &after_start[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(seq: i64, message: Message) -> StoredMessage {
        StoredMessage {
            id: seq,
            seq,
            message,
            is_summary: false,
        }
    }

    fn screenshot(tag: char) -> String {
        let payload: String = std::iter::repeat_n(tag, 80).collect();
        format!("look at this\n\ndata:image/png;base64,{payload}\n")
    }

    /// The newest two image-bearing user messages keep their pixels; an older one is replaced by a
    /// marker rather than re-sent on every request for the rest of the session.
    #[test]
    fn only_the_newest_screenshots_are_replayed() {
        let history = vec![
            stored(1, Message::user(screenshot('A'))),
            stored(2, Message::assistant("done")),
            stored(3, Message::user(screenshot('B'))),
            stored(4, Message::assistant("done")),
            stored(5, Message::user(screenshot('C'))),
            stored(6, Message::assistant("done")),
        ];

        let messages = sendable_messages(&history);
        let content = |index: usize| messages[index].content.as_deref().unwrap_or("");

        assert!(
            !content(0).contains("data:image/"),
            "the oldest screenshot is no longer sent: {}",
            content(0)
        );
        assert!(
            content(0).contains(INLINE_IMAGE_ELIDED),
            "the fact that an image was there survives: {}",
            content(0)
        );
        assert!(content(2).contains("data:image/"), "the second is kept");
        assert!(content(4).contains("data:image/"), "the newest is kept");
        assert!(
            content(0).contains("look at this"),
            "the prose around the image is untouched"
        );
    }

    /// The turn just started: its images have not been answered yet and must survive wholesale.
    #[test]
    fn the_current_turns_images_are_never_elided() {
        let history = vec![
            stored(1, Message::user(screenshot('A'))),
            stored(2, Message::assistant("done")),
            stored(3, Message::user(screenshot('B'))),
            stored(4, Message::assistant("done")),
            stored(5, Message::user(screenshot('C'))),
        ];

        let messages = sendable_messages(&history);
        assert!(messages[2]
            .content
            .as_deref()
            .unwrap()
            .contains("data:image/"));
        assert!(messages[4]
            .content
            .as_deref()
            .unwrap()
            .contains("data:image/"));
        assert!(!messages[0]
            .content
            .as_deref()
            .unwrap()
            .contains("data:image/"));
    }

    /// A sentence that merely mentions a data URL is prose, not pixels, and is left as written.
    #[test]
    fn prose_about_an_image_is_not_elided() {
        let history = vec![
            stored(
                1,
                Message::user("the format is data:image/png;base64,... and so on"),
            ),
            stored(2, Message::assistant("done")),
            stored(3, Message::user(screenshot('A'))),
            stored(4, Message::assistant("done")),
            stored(5, Message::user(screenshot('B'))),
        ];

        let messages = sendable_messages(&history);
        assert_eq!(
            messages[0].content.as_deref(),
            Some("the format is data:image/png;base64,... and so on")
        );
    }
}
