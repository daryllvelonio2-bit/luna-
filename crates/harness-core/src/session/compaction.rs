//! Bounded context: the token estimate, the fold boundary, and the one model call that summarises
//! the turns that no longer fit.

use tracing::debug;

use super::StoredMessage;
use crate::error::HarnessError;
use crate::traits::ModelProvider;
use crate::types::{CompletionOptions, Message, Role};

/// Characters per token in the fallback estimate. Four is the usual ratio for English prose under
/// BPE tokenizers; on prose samples the estimate lands within
/// [`HEURISTIC_TOLERANCE`] of the provider-reported count. It is only ever used when no
/// provider-reported usage is available — the reported figure always wins, so the threshold never
/// compares a guess against a fact.
pub const CHARS_PER_TOKEN: usize = 4;

/// Documented relative tolerance between the character heuristic and a provider-reported count on
/// prose. Asserted by the estimator test.
pub const HEURISTIC_TOLERANCE: f64 = 0.25;

/// Budget for the summary call.
///
/// The harness model under test is a reasoning model: the reasoning tokens come out of the
/// same budget as the answer, and a budget of a few dozen tokens is swallowed by
/// `reasoning_content` and yields `content: null` (AGENT_RULES §2). A thousand tokens is
/// not always enough either — a long transcript makes the model think for thousands of
/// tokens before it writes the summary, and the summary then never arrives. So the call
/// gets room for the reasoning plus the summary, and a summary that comes back empty is
/// retried with more.
const COMPACTION_MAX_TOKENS: u32 = 4096;

/// The budget the retry uses when the first summary came back empty.
const COMPACTION_RETRY_MAX_TOKENS: u32 = 16_384;

/// The summary text of a response, when it carries one.
///
/// A reasoning model can spend its whole budget on reasoning and answer with nothing; the
/// provider layer then fills the gap with a marker that says so. That marker is not a
/// summary: a compaction that wrote it into the transcript would have replaced the whole
/// session with a sentence about reasoning.
fn usable_summary(response: &Message) -> Option<String> {
    let text = response.content.as_deref()?.trim();
    if text.is_empty() || text == crate::types::NO_TEXT_RESPONSE {
        return None;
    }
    Some(text.to_string())
}

/// Where an estimate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstimateSource {
    /// Provider-reported usage: fact.
    Reported,
    /// The [`CHARS_PER_TOKEN`] character heuristic: an approximation.
    Heuristic,
}

/// The transcript size the compaction threshold compares against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    pub tokens: u32,
    pub source: EstimateSource,
}

/// Characters that will be sent for one message: content, tool call names and arguments, tool
/// result.
fn message_chars(message: &Message) -> usize {
    let mut chars = message.content.as_ref().map_or(0, String::len);
    if let Some(result) = &message.tool_result {
        chars += result.content.len();
    }
    for call in &message.tool_calls {
        chars += call.name.len() + call.arguments.to_string().len();
    }
    chars
}

/// The character heuristic over a whole slice. Every message costs at least one token, so an empty
/// vector is the only zero.
pub fn heuristic_tokens(messages: &[Message]) -> u32 {
    let chars: usize = messages.iter().map(message_chars).sum();
    let tokens = chars.div_ceil(CHARS_PER_TOKEN);
    if messages.is_empty() {
        0
    } else {
        tokens.max(1) as u32
    }
}

/// Size of the conversation: provider-reported usage when the transcript carries it, the character
/// heuristic otherwise.
///
/// `Message::usage` on an assistant reply is the usage of the request that produced it, so it
/// covers every message up to and including that reply. Anything appended after it (tool results,
/// the new user line) is added with the heuristic — the result is one number, whether or not a
/// provider made the call, and that number is what [`crate::AgentConfig::compact_at_tokens`]
/// compares against.
pub fn estimate_tokens(messages: &[Message]) -> Estimate {
    if let Some((index, usage)) = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| message.usage.as_ref().map(|usage| (index, usage)))
    {
        let reported = usage.prompt_tokens.saturating_add(usage.completion_tokens);
        let tail = heuristic_tokens(&messages[index + 1..]);
        return Estimate {
            tokens: reported.saturating_add(tail),
            source: EstimateSource::Reported,
        };
    }

    Estimate {
        tokens: heuristic_tokens(messages),
        source: EstimateSource::Heuristic,
    }
}

/// Index of the first message that must stay verbatim: the start of the `keep_recent_turns`-th user
/// turn counted from the end. `0` means nothing can be folded (the transcript holds no more turns
/// than we are keeping).
///
/// A turn is a user message plus everything up to the next user message — the unit the summary
/// line counts.
///
/// The turn rule alone is not enough: a turn is not a fixed size. One turn carrying a large tool
/// result or an inline image holds as many tokens as a hundred short turns, so a transcript can be
/// far over budget while holding no more turns than we keep — and then the turn rule has nothing
/// to fold, which leaves the user with a context they cannot shrink. So when the turn rule comes
/// up empty, the boundary moves to the most recent stretch of *messages* that fits
/// [`FALLBACK_KEEP_TOKENS`], and the rest is folded.
pub fn fold_boundary(history: &[StoredMessage], keep_recent_turns: usize) -> usize {
    let user_indices: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, stored)| stored.message.role == Role::User)
        .map(|(index, _)| index)
        .collect();

    if user_indices.len() > keep_recent_turns {
        return user_indices[user_indices.len() - keep_recent_turns];
    }

    budget_boundary(history, FALLBACK_KEEP_TOKENS)
}

/// Tokens of the transcript kept verbatim when the turn rule has nothing to fold.
///
/// The turn rule keeps *turns*; this keeps *tokens*. It is a cap, not a target: a transcript of
/// ordinary messages is far below it, and only a transcript whose messages are large ever feels
/// it.
pub const FALLBACK_KEEP_TOKENS: usize = 16_000;

/// The start of the most recent stretch of messages that fits `budget` tokens.
///
/// Walking back from the end, the newest message is always kept: it is the last thing said, and a
/// transcript that cannot spare it cannot be compacted at all.
fn budget_boundary(history: &[StoredMessage], budget: usize) -> usize {
    let mut kept: usize = 0;
    let mut boundary = history.len();

    for (index, stored) in history.iter().enumerate().rev() {
        let tokens = heuristic_tokens(std::slice::from_ref(&stored.message)) as usize;
        let is_newest = index + 1 == history.len();
        if !is_newest && kept + tokens > budget {
            break;
        }
        kept += tokens;
        boundary = index;
    }

    safe_boundary(history, boundary)
}

/// The first index at or after `desired` whose message can open the kept tail.
///
/// The kept tail is a suffix of the transcript, and a tool result may not stand alone: the
/// assistant message that called it has to be in the tail too, or the provider rejects the
/// vector. A boundary that lands on a tool result therefore moves forward to the next message
/// that can open a conversation.
fn safe_boundary(history: &[StoredMessage], desired: usize) -> usize {
    let mut index = desired;
    while index < history.len() && history[index].message.role == Role::Tool {
        index += 1;
    }
    index
}

/// What a successful compaction produced.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionOutcome {
    /// The summary text, written back as a synthetic `system` turn.
    pub summary: String,
    /// The messages that stay verbatim, in order.
    pub kept: Vec<Message>,
    /// Row ids of the folded messages, so the caller can mark them compacted.
    pub folded_ids: Vec<i64>,
    /// User turns folded into the summary — the number in the console line.
    pub folded_turns: usize,
    /// Messages folded into the summary. A fold that took no whole turn — one huge
    /// message, a transcript with fewer turns than we keep — is still a fold, and this
    /// is what says so.
    pub folded_messages: usize,
}

impl CompactionOutcome {
    /// The vector to send to the provider: the summary first, then the kept turns verbatim.
    /// `system_text` is the caller's system prompt; it is prepended.
    pub fn bounded_messages(&self, system_text: String) -> Vec<Message> {
        let mut messages = Vec::with_capacity(self.kept.len() + 2);
        messages.push(Message::system(system_text));
        messages.push(Message::system(self.summary.clone()));
        messages.extend(self.kept.iter().cloned());
        messages
    }
}

/// Characters of one message that go into the summariser's transcript.
///
/// A folded message can be megabytes — an inline image is a few million base64 characters — and
/// a summariser call that carried it would be rejected for being too long. The head of a message
/// carries its meaning, so the tail is cut and the cut is marked, so the summary can still say
/// what the message was.
const TRANSCRIPT_MESSAGE_CHARS: usize = 4_000;

/// Characters of the whole transcript that go into the summariser's call.
const TRANSCRIPT_TOTAL_CHARS: usize = 60_000;

/// The head of `text`, cut to `limit` characters, with a marker that says how much was cut.
fn clip(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{}\n… [{} more characters cut]", head, count - limit)
}

fn render_transcript(messages: &[StoredMessage]) -> String {
    let mut out = String::new();
    for stored in messages {
        if out.len() >= TRANSCRIPT_TOTAL_CHARS {
            out.push_str("\n… transcript truncated …\n");
            break;
        }
        let message = &stored.message;
        let label = if stored.is_summary {
            "summary".to_string()
        } else {
            match message.role {
                Role::System => "system".to_string(),
                Role::User => "user".to_string(),
                Role::Assistant => "assistant".to_string(),
                Role::Tool => "tool".to_string(),
            }
        };
        let body = clip(
            message.content.as_deref().unwrap_or_default(),
            TRANSCRIPT_MESSAGE_CHARS,
        );
        out.push_str(&format!("{}: {}\n", label, body));
        for call in &message.tool_calls {
            out.push_str(&format!(
                "  tool call {} {} {}\n",
                call.name,
                call.id,
                clip(&call.arguments.to_string(), TRANSCRIPT_MESSAGE_CHARS)
            ));
        }
        if let Some(result) = &message.tool_result {
            out.push_str(&format!(
                "  tool result: {}\n",
                clip(&result.content, TRANSCRIPT_MESSAGE_CHARS)
            ));
        }
    }
    out
}

/// Summarise the oldest turns of `history` into one model call.
///
/// Returns `Ok(None)` when there is nothing to fold. Any failure — the provider rejects the call,
/// or it answers with no content — is returned as an error and the caller leaves the transcript
/// untouched; the loop then proceeds on the full history.
pub async fn compact(
    provider: &dyn ModelProvider,
    model: &str,
    history: &[StoredMessage],
    keep_recent_turns: usize,
) -> Result<Option<CompactionOutcome>, HarnessError> {
    let boundary = fold_boundary(history, keep_recent_turns);
    if boundary == 0 {
        return Ok(None);
    }

    let folded = &history[..boundary];
    let kept = &history[boundary..];
    let transcript = render_transcript(folded);
    debug!(
        "Compacting {} messages ({} turns) with model '{}'",
        folded.len(),
        folded.iter().filter(|stored| stored.message.role == Role::User).count(),
        model
    );

    let request = vec![
        Message::system(crate::prompts::section(
            crate::prompts::COMPACTION_INSTRUCTION,
        )?),
        Message::user(transcript),
    ];
    let options = CompletionOptions {
        model: Some(model.to_string()),
        temperature: Some(0.0),
        max_tokens: Some(COMPACTION_MAX_TOKENS),
        ..CompletionOptions::default()
    };

    let response = provider.complete(&request, &[], &options).await?;
    let summary = match usable_summary(&response) {
        Some(text) => text,
        None => {
            // The model spent the whole budget on reasoning and wrote nothing. A summary
            // is worth a second call with more room; the placeholder the provider hands
            // back is not a summary, and writing it into the transcript would throw the
            // session away.
            debug!(
                "Compaction summary was empty; retrying with {} tokens",
                COMPACTION_RETRY_MAX_TOKENS
            );
            let retry = CompletionOptions {
                max_tokens: Some(COMPACTION_RETRY_MAX_TOKENS),
                ..options
            };
            let response = provider.complete(&request, &[], &retry).await?;
            usable_summary(&response).ok_or_else(|| HarnessError::ProviderError {
                provider: provider.provider_id().to_string(),
                message: "compaction returned an empty summary".to_string(),
            })?
        }
    };

    Ok(Some(CompactionOutcome {
        summary,
        kept: kept.iter().map(|stored| stored.message.clone()).collect(),
        folded_ids: folded.iter().map(|stored| stored.id).collect(),
        folded_turns: folded
            .iter()
            .filter(|stored| stored.message.role == Role::User)
            .count(),
        folded_messages: folded.len(),
    }))
}

/// Summarise the entire `history` of a session into a concise context summary.
pub async fn summarize_session(
    provider: &dyn ModelProvider,
    model: &str,
    history: &[StoredMessage],
) -> Result<String, HarnessError> {
    if history.is_empty() {
        return Err(HarnessError::SessionError(
            "cannot summarize an empty session".to_string(),
        ));
    }

    let transcript = render_transcript(history);
    debug!(
        "Summarizing session with {} messages using model '{}'",
        history.len(),
        model
    );

    let request = vec![
        Message::system(crate::prompts::section(
            crate::prompts::COMPACTION_INSTRUCTION,
        )?),
        Message::user(transcript),
    ];
    let options = CompletionOptions {
        model: Some(model.to_string()),
        temperature: Some(0.0),
        max_tokens: Some(COMPACTION_MAX_TOKENS),
        ..CompletionOptions::default()
    };

    let response = provider.complete(&request, &[], &options).await?;
    let summary = response
        .content
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| HarnessError::ProviderError {
            provider: provider.provider_id().to_string(),
            message: "session summary returned empty content".to_string(),
        })?;

    Ok(summary)
}
