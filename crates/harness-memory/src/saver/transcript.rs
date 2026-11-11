//! The rendered conversation: exchanges, speaker segments, and the budget that bounds them.

use harness_core::{Message, Role};

use super::evidence::{normalize, segment_matches_quote};
use super::model::{MESSAGE_CHAR_CAP, TRANSCRIPT_CHAR_BUDGET};

/// The transcript as the saver reads it: who spoke, and a bounded amount of what they said.
/// Split a transcript tail into complete user/assistant exchanges.
///
/// An exchange starts at a user message and runs until the next user message. Assistant text is
/// joined with blank lines; tool rows are skipped. A leading assistant fragment with no user
/// message in the window is dropped — it belongs to the previous exchange, which an earlier pass
/// with an overlapping tail has already filed.
pub(super) fn exchanges(messages: &[Message]) -> Vec<String> {
    let mut out = Vec::new();
    let mut user: Option<String> = None;
    let mut replies: Vec<String> = Vec::new();

    for message in messages {
        match message.role {
            Role::User => {
                flush_exchange(&mut out, &mut user, &mut replies);
                user = message
                    .content
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_string);
            }
            Role::Assistant => {
                if let Some(text) = message
                    .content
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    replies.push(text.to_string());
                }
            }
            Role::Tool | Role::System => {}
        }
    }
    flush_exchange(&mut out, &mut user, &mut replies);
    out
}

fn flush_exchange(out: &mut Vec<String>, user: &mut Option<String>, replies: &mut Vec<String>) {
    let Some(user) = user.take() else {
        replies.clear();
        return;
    };
    let mut body = format!("> {user}");
    for reply in replies.drain(..) {
        body.push_str("\n\n");
        body.push_str(&reply);
    }
    out.push(body);
}

/// A short searchable label for a raw exchange: the user's first non-empty line.
pub(super) fn transcript_label(body: &str) -> String {
    let first = body
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches('>')
        .trim();
    truncate(first, 160)
}

/// Who said a line the saver cites as evidence.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Provenance {
    User,
    Tool,
    Assistant,
}

/// Phrases with which a user asks that content be kept. Assistant text cannot become a note
/// without one of these, because an answer to a question is a reply, not a record.
const SAVE_REQUEST_MARKERS: &[&str] = &[
    "remember this",
    "remember that",
    "remember it",
    "save this",
    "save that",
    "save it",
    "keep this",
    "keep that",
    "keep it",
    "write this down",
    "write that down",
    "note this down",
    "make a note",
    "store this",
    "file this",
    "don't forget",
    "do not forget",
];

/// A request to keep something is ignored when the same user message retracts it.
const SAVE_REFUSAL_MARKERS: &[&str] = &[
    "don't save",
    "do not save",
    "don't remember",
    "do not remember",
    "don't keep",
    "do not keep",
    "forget this",
    "forget that",
];

/// One message that reached the transcript, kept so a quote can be attributed to its speaker.
struct Segment {
    provenance: Provenance,
    body: String,
}

/// The rendered conversation plus the segments its text is made of.
pub(super) struct RenderedTranscript {
    pub(super) text: String,
    segments: Vec<Segment>,
}

impl RenderedTranscript {
    /// Which message a quote came from, if one message contains all of it.
    ///
    /// A user or tool line wins over an assistant line even when the assistant repeats it: the
    /// authoritative source is what makes a claim citable. A quote that spans messages, or is not
    /// found, returns `None`.
    pub(super) fn provenance_of(&self, evidence: &str) -> Option<Provenance> {
        if normalize(evidence).is_empty() {
            return None;
        }
        let mut assistant = false;
        for segment in &self.segments {
            if segment_matches_quote(&segment.body, evidence) {
                match segment.provenance {
                    Provenance::User | Provenance::Tool => return Some(segment.provenance),
                    Provenance::Assistant => assistant = true,
                }
            }
        }
        assistant.then_some(Provenance::Assistant)
    }

    /// Whether the user asked to keep something. The assistant's own answer can only become a
    /// note after such a request.
    pub(super) fn user_requested_save(&self) -> bool {
        self.segments.iter().any(|segment| {
            if segment.provenance != Provenance::User {
                return false;
            }
            let body = normalize(&segment.body);
            !SAVE_REFUSAL_MARKERS
                .iter()
                .any(|marker| body.contains(marker))
                && SAVE_REQUEST_MARKERS
                    .iter()
                    .any(|marker| body.contains(marker))
        })
    }
}

impl std::ops::Deref for RenderedTranscript {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for RenderedTranscript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

pub(super) fn render_transcript(messages: &[Message]) -> RenderedTranscript {
    let mut segments: Vec<Segment> = Vec::new();
    for message in messages {
        let provenance = match message.role {
            Role::User => Provenance::User,
            Role::Assistant => Provenance::Assistant,
            Role::Tool => Provenance::Tool,
            Role::System => continue,
        };
        let body = match message.content.as_deref() {
            Some(content) if !content.trim().is_empty() => {
                truncate(content.trim(), MESSAGE_CHAR_CAP)
            }
            _ => {
                let calls: Vec<&str> = message
                    .tool_calls
                    .iter()
                    .map(|call| call.name.as_str())
                    .collect();
                if calls.is_empty() {
                    continue;
                }
                format!("(called {})", calls.join(", "))
            }
        };
        let label = match provenance {
            Provenance::User => "USER",
            Provenance::Assistant => "LUNA",
            Provenance::Tool => "TOOL",
        };
        segments.push(Segment {
            provenance,
            body: format!("{label}: {body}"),
        });
    }

    // Budget from the end: the newest turns carry the decision, the oldest are already reflected in
    // the inventory the model was shown. Whole messages are dropped so the text and the provenance
    // segments stay in step.
    let mut omitted = false;
    let mut text = join_segments(&segments);
    while text.chars().count() > TRANSCRIPT_CHAR_BUDGET && segments.len() > 1 {
        segments.remove(0);
        omitted = true;
        text = join_segments(&segments);
    }
    if text.chars().count() > TRANSCRIPT_CHAR_BUDGET {
        let skip = text.chars().count() - TRANSCRIPT_CHAR_BUDGET;
        text = text.chars().skip(skip).collect();
    }
    if omitted {
        text.insert_str(0, "(earlier turns omitted)\n");
    }
    RenderedTranscript { text, segments }
}

fn join_segments(segments: &[Segment]) -> String {
    let mut text = segments
        .iter()
        .map(|segment| segment.body.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text
}

/// Shorten `text` to `limit` characters, marking that it was cut.
pub(super) fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Shorten a multi-line block to `limit` characters, marking that it was cut.
pub(super) fn truncate_block(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(40)).collect();
    out.push_str("\n(INVENTORY TRUNCATED: remaining lines omitted)\n");
    out
}

/// Today's date, so relative phrases ("yesterday") can be grounded by the model.
pub(super) fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}
