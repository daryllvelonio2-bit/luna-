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

/// Budget for the summary call. The harness model under test is a reasoning model: a budget of a
/// few dozen tokens is swallowed by `reasoning_content` and yields `content: null` (AGENT_RULES §2).
const COMPACTION_MAX_TOKENS: u32 = 1024;

/// The instruction that turns the folded turns into a summary turn.
const COMPACTION_INSTRUCTION: &str = "You compress conversation history for an AI agent. \
Rewrite the transcript that follows as a compact factual summary that preserves: what the user \
asked for, decisions taken, facts established, file or tool names, and anything still unresolved. \
Drop pleasantries. Answer with the summary only — no preamble, no headings.";

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
pub fn fold_boundary(history: &[StoredMessage], keep_recent_turns: usize) -> usize {
    let user_indices: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, stored)| stored.message.role == Role::User)
        .map(|(index, _)| index)
        .collect();

    if user_indices.len() <= keep_recent_turns {
        return 0;
    }
    user_indices[user_indices.len() - keep_recent_turns]
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

fn render_transcript(messages: &[StoredMessage]) -> String {
    let mut out = String::new();
    for stored in messages {
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
        let body = message.content.clone().unwrap_or_default();
        out.push_str(&format!("{}: {}\n", label, body));
        for call in &message.tool_calls {
            out.push_str(&format!(
                "  tool call {} {} {}\n",
                call.name, call.id, call.arguments
            ));
        }
        if let Some(result) = &message.tool_result {
            out.push_str(&format!("  tool result: {}\n", result.content));
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
        Message::system(COMPACTION_INSTRUCTION),
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
            message: "compaction returned an empty summary".to_string(),
        })?;

    Ok(Some(CompactionOutcome {
        summary,
        kept: kept.iter().map(|stored| stored.message.clone()).collect(),
        folded_ids: folded.iter().map(|stored| stored.id).collect(),
        folded_turns: folded
            .iter()
            .filter(|stored| stored.message.role == Role::User)
            .count(),
    }))
}
