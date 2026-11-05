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
    // Answers present anywhere in the transcript. A turn interrupted while its tools were running
    // leaves the assistant row that asked for them and only the answers that had landed; a
    // provider rejects a request carrying an unanswered call, so the incomplete round is dropped
    // rather than sent. This is the mirror of the orphaned-result check below: there the answer
    // lost its question, here the question never got its answers.
    let answered: HashSet<&str> = history
        .iter()
        .filter_map(|stored| stored.message.tool_result.as_ref())
        .map(|result| result.call_id.as_str())
        .collect();
    let mut announced: HashSet<String> = HashSet::new();
    let mut messages = Vec::with_capacity(history.len());
    for stored in history {
        let mut message = stored.message.clone();
        if fold_seq.is_some_and(|fold_seq| stored.seq < fold_seq) {
            message.usage = None;
        }
        if message.role == Role::Assistant
            && message
                .tool_calls
                .iter()
                .any(|call| !answered.contains(call.id.as_str()))
        {
            debug!(
                "Dropping an incomplete tool round: the assistant asked for calls that were never answered"
            );
            continue;
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
    elide_stale_tool_results(&mut messages);
    elide_stale_reasoning(&mut messages);
    elide_stale_signatures(&mut messages);
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

/// How many of the newest tool results keep their full text when replayed.
///
/// A result is read once, right after it arrives. The transcript keeps it forever, though, so an
/// older result is re-sent on every request long after the conversation moved past it: measured
/// over recent sessions, the newest ten tool results were 2.7% of the live payload, and the rest
/// was context re-sent on every model call for the rest of the session. Six rounds keeps the work
/// in flight and its immediate context verbatim, and clips only what the model has already used.
const REPLAYED_TOOL_RESULTS: usize = 6;

/// Characters kept from a tool result older than [`REPLAYED_TOOL_RESULTS`]: head, marker, tail.
const STALE_TOOL_RESULT_CHARS: usize = 4_000;

/// Hard ceiling for a tool result that is still among the newest [`REPLAYED_TOOL_RESULTS`].
///
/// The dispatch cap bounds every result produced from now on, but rows written before it existed
/// can be megabytes. A result the model is still acting on keeps more of its text, not all of it:
/// no single answer gets to fill the window alone.
const FRESH_TOOL_RESULT_MAX_CHARS: usize = 100_000;

/// What sits between the head and tail of a clipped tool result.
const TOOL_RESULT_CLIPPED: &str = "\n\n[tool result clipped when replayed: middle omitted]\n\n";

/// How many of the newest assistant reasoning blocks keep their text when replayed.
///
/// Reasoning is re-sent as a thought part by the Antigravity route and as `reasoning_content` by
/// the OpenAI-compatible one. It is valuable for the round it belongs to and dead weight for every
/// later round, which is why only the newest few keep it.
const REPLAYED_REASONING: usize = 4;

/// How many of the newest signed calls keep their thought signature when replayed.
///
/// A Gemini thought signature is opaque, often tens of kilobytes for a single call, and was 59% of
/// all live tool-call JSON. Strict Gemini requires a signature beside a replayed call, but accepts
/// Google's documented dummy value for a call whose original is gone; the provider layer already
/// substitutes it when the field is `None`. Keeping the newest few preserves the chain of thought
/// where it still matters.
const REPLAYED_SIGNATURES: usize = 8;

/// Keep the head and tail of tool results older than the newest few, dropping the middle.
///
/// Both `content` and `tool_result.content` are written, because `Message::tool` stores the same
/// text in both and different routes serialize one or the other.
fn elide_stale_tool_results(messages: &mut [Message]) {
    let mut seen = 0usize;
    for message in messages.iter_mut().rev() {
        let Some(result) = message.tool_result.as_mut() else {
            continue;
        };
        // The newest few keep more of their text; everything older is reduced to a head and tail.
        let cap = if seen < REPLAYED_TOOL_RESULTS {
            seen += 1;
            FRESH_TOOL_RESULT_MAX_CHARS
        } else {
            STALE_TOOL_RESULT_CHARS
        };
        let clipped = crate::tools::clip_middle(&result.content, cap, TOOL_RESULT_CLIPPED);
        if clipped.len() == result.content.len() {
            continue;
        }
        result.content = clipped.clone();
        if message.content.is_some() {
            message.content = Some(clipped);
        }
    }
}

/// Drop the reasoning of assistant messages older than the newest [`REPLAYED_REASONING`].
///
/// A message left with neither content nor a call keeps its reasoning: stripping it would make the
/// message empty, and an empty assistant turn is worse than a little dead context.
fn elide_stale_reasoning(messages: &mut [Message]) {
    let mut seen = 0usize;
    for message in messages.iter_mut().rev() {
        let has_reasoning = matches!(
            message.reasoning.as_deref(),
            Some(reasoning) if !reasoning.is_empty()
        );
        if !has_reasoning {
            continue;
        }
        if seen < REPLAYED_REASONING {
            seen += 1;
            continue;
        }
        if message.content.is_some() || !message.tool_calls.is_empty() {
            message.reasoning = None;
        }
    }
}

/// Clear the thought signature of every signed call older than the newest
/// [`REPLAYED_SIGNATURES`], so the provider sends its documented dummy instead of kilobytes of
/// opaque state for a round that is already behind the conversation.
fn elide_stale_signatures(messages: &mut [Message]) {
    let mut seen = 0usize;
    for message in messages.iter_mut().rev() {
        for call in message.tool_calls.iter_mut().rev() {
            let has_signature = matches!(call.signature.as_deref(), Some(sig) if !sig.is_empty());
            if !has_signature {
                continue;
            }
            if seen < REPLAYED_SIGNATURES {
                seen += 1;
            } else {
                call.signature = None;
            }
        }
    }
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

    fn call(id: &str) -> crate::types::ToolCall {
        crate::types::ToolCall {
            id: id.to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({ "path": "a.rs" }),
            signature: None,
        }
    }

    /// A round the model completed is replayed whole: the call and its answer both stay.
    #[test]
    fn an_answered_tool_round_is_replayed() {
        let history = vec![
            stored(1, Message::user("read the file")),
            stored(
                2,
                Message::assistant_with_tools(vec![call("call-1"), call("call-2")]),
            ),
            stored(
                3,
                Message::tool(crate::types::ToolResult::success("call-1", "one")),
            ),
            stored(
                4,
                Message::tool(crate::types::ToolResult::success("call-2", "two")),
            ),
            stored(5, Message::assistant("both read")),
        ];

        let messages = sendable_messages(&history);
        assert_eq!(messages.len(), 5, "{messages:?}");
        assert_eq!(messages[1].tool_calls.len(), 2);
    }

    /// An interrupted turn leaves the assistant row that asked for tools and not every answer.
    /// The unanswered call would make the next request invalid, so the whole incomplete round is
    /// dropped — including the answers that did land, which have nothing left to answer.
    #[test]
    fn an_unanswered_tool_round_is_not_replayed() {
        let history = vec![
            stored(1, Message::user("read both files")),
            stored(
                2,
                Message::assistant_with_tools(vec![call("call-1"), call("call-2")]),
            ),
            stored(
                3,
                Message::tool(crate::types::ToolResult::success("call-1", "one")),
            ),
            stored(4, Message::user("never mind, do something else")),
        ];

        let messages = sendable_messages(&history);
        assert!(
            messages.iter().all(|message| message.tool_calls.is_empty()),
            "no unanswered call survives: {messages:?}"
        );
        assert!(
            messages.iter().all(|message| message.role != Role::Tool),
            "its partial answers go with it: {messages:?}"
        );
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert_eq!(
            messages[1].content.as_deref(),
            Some("never mind, do something else")
        );
    }

    fn tool_round(seq: i64, id: &str, body: &str) -> Vec<StoredMessage> {
        vec![
            stored(seq, Message::assistant_with_tools(vec![call(id)])),
            stored(
                seq + 1,
                Message::tool(crate::types::ToolResult::success(id, body)),
            ),
        ]
    }

    /// Old tool results are clipped to a head and tail; the newest few stay whole, because they are
    /// the work the model is still acting on.
    #[test]
    fn only_the_newest_tool_results_keep_their_text() {
        let body = format!("begin{}end", "x".repeat(20_000));
        let mut history = vec![stored(1, Message::user("go"))];
        for index in 0..8 {
            let id = format!("call-{index}");
            history.extend(tool_round(2 + index as i64 * 2, &id, &body));
        }

        let messages = sendable_messages(&history);
        let results: Vec<&Message> = messages
            .iter()
            .filter(|message| message.role == Role::Tool)
            .collect();
        assert_eq!(results.len(), 8);

        for result in &results[..2] {
            let text = &result.tool_result.as_ref().unwrap().content;
            assert!(text.len() < body.len(), "older result is clipped");
            assert!(text.contains(TOOL_RESULT_CLIPPED), "{text}");
            assert!(text.starts_with("begin"), "the head survives");
            assert!(text.ends_with("end"), "the tail survives");
            assert_eq!(
                result.content.as_deref(),
                Some(text.as_str()),
                "both copies of the text are clipped"
            );
        }
        for result in &results[2..] {
            assert_eq!(&result.tool_result.as_ref().unwrap().content, &body);
        }
    }

    /// Even the newest result has a ceiling: a row written before the dispatch cap can be
    /// megabytes, and no single answer may fill the window alone.
    #[test]
    fn a_fresh_tool_result_is_still_capped() {
        let body = format!("begin{}end", "x".repeat(250_000));
        let history = vec![
            stored(1, Message::user("go")),
            stored(2, Message::assistant_with_tools(vec![call("call-big")])),
            stored(
                3,
                Message::tool(crate::types::ToolResult::success("call-big", &body)),
            ),
        ];

        let messages = sendable_messages(&history);
        let text = &messages[2].tool_result.as_ref().unwrap().content;
        assert!(text.len() <= FRESH_TOOL_RESULT_MAX_CHARS);
        assert!(text.starts_with("begin"));
        assert!(text.ends_with("end"));
        assert!(text.contains(TOOL_RESULT_CLIPPED));
    }

    /// Old reasoning is no longer replayed; the newest blocks keep theirs.
    #[test]
    fn only_the_newest_reasoning_is_replayed() {
        let mut history = vec![stored(1, Message::user("go"))];
        for index in 0..6 {
            let mut message = Message::assistant(format!("answer {index}"));
            message.reasoning = Some(format!("thought {index} {}", "r".repeat(2_000)));
            history.push(stored(2 + index, message));
        }

        let messages = sendable_messages(&history);
        let assistant: Vec<&Message> = messages
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .collect();
        assert_eq!(assistant.len(), 6);

        for message in &assistant[..2] {
            assert!(message.reasoning.is_none(), "older reasoning is dropped");
            assert!(message.content.as_deref().unwrap().starts_with("answer"));
        }
        for message in &assistant[2..] {
            assert!(message.reasoning.is_some(), "newest reasoning is kept");
        }
    }

    /// Old thought signatures are cleared, so the provider sends its documented dummy instead of
    /// replaying opaque state for a round the conversation has left behind.
    #[test]
    fn only_the_newest_thought_signatures_are_replayed() {
        let mut history = vec![stored(1, Message::user("go"))];
        for index in 0..12 {
            let id = format!("call-{index}");
            let mut tool_call = call(&id);
            tool_call.signature = Some(format!("sig-{index}-{}", "s".repeat(2_000)));
            history.push(stored(
                2 + index * 2,
                Message::assistant_with_tools(vec![tool_call]),
            ));
            history.push(stored(
                3 + index * 2,
                Message::tool(crate::types::ToolResult::success(&id, "ok")),
            ));
        }

        let messages = sendable_messages(&history);
        let calls: Vec<&crate::types::ToolCall> = messages
            .iter()
            .flat_map(|message| message.tool_calls.iter())
            .collect();
        assert_eq!(calls.len(), 12);
        for tool_call in &calls[..4] {
            assert!(tool_call.signature.is_none(), "old signature is cleared");
        }
        for tool_call in &calls[4..] {
            assert!(tool_call.signature.is_some(), "newest keep theirs");
        }
    }
}
