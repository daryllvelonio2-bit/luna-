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

/// Characters attributed to an inline image data URL in heuristic token estimation.
/// At [`CHARS_PER_TOKEN`] = 4, this approximates ~250 tokens per image (Gemini: ~258 tokens,
/// OpenAI/Anthropic vision tiles: ~250-768 tokens).
pub const HEURISTIC_IMAGE_CHARS: usize = 1_000;

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

/// Count effective characters of text for token estimation, substituting a fixed character count
/// for base64 data URLs rather than counting hundreds of thousands of raw base64 bytes as prose tokens.
pub fn text_content_chars(text: &str) -> usize {
    if !text.contains("data:image/") {
        return text.len();
    }
    let mut chars = 0;
    let mut rest = text;
    while let Some(start) = rest.find("data:image/") {
        chars += rest[..start].len();
        let after_start = &rest[start..];
        let end = after_start
            .find(char::is_whitespace)
            .unwrap_or(after_start.len());
        if after_start[..end].contains(";base64,") {
            chars += HEURISTIC_IMAGE_CHARS;
        } else {
            chars += end;
        }
        rest = &after_start[end..];
    }
    chars += rest.len();
    chars
}

/// Characters that will be sent for one message: content, tool call names and arguments, tool
/// result.
fn message_chars(message: &Message) -> usize {
    let mut chars = message
        .content
        .as_ref()
        .map_or(0, |s| text_content_chars(s));
    if let Some(result) = &message.tool_result {
        chars += text_content_chars(&result.content);
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

/// Move the cut back until it no longer separates a tool call from its results.
///
/// The harness sends the transcript verbatim, and a provider rejects a `tool` message that
/// answers no call: the assistant row that asked for the tools and the rows that carry the
/// answers have to travel together, on one side of the cut or the other. NVIDIA answers such
/// a request with `HTTP 400 {"message":"Invalid request"}`.
///
/// Two cuts break that. One leaves the first kept row a tool result whose call sits in the
/// folded part. The other folds an assistant's call while its answers stay in the kept tail —
/// the cut a stale fold snapshot produces, because the answers arrive while the summariser is
/// still thinking, and the fold was planned on the transcript as it was before the call.
fn snap_to_round_boundary(history: &[StoredMessage], boundary: usize) -> usize {
    let mut boundary = boundary.min(history.len());

    // The kept tail may not open on a tool result: walk back to the assistant that asked for
    // it, so the call travels with its answers.
    if boundary < history.len() && history[boundary].message.role == Role::Tool {
        while boundary > 0 {
            boundary -= 1;
            let row = &history[boundary].message;
            if row.role == Role::Assistant && !row.tool_calls.is_empty() {
                break;
            }
        }
        if boundary < history.len() && history[boundary].message.role == Role::Tool {
            // A result with no call above it belongs to no round: the tail cannot open on it
            // at all, so the cut goes to the start of the transcript.
            boundary = 0;
        }
    }

    // The folded part may not end with a call whose answers are kept: those answers would
    // arrive with nothing to answer.
    while boundary > 0 {
        let row = &history[boundary - 1].message;
        if row.role == Role::Assistant && !row.tool_calls.is_empty() {
            boundary -= 1;
        } else {
            break;
        }
    }

    boundary
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
pub fn fold_boundary(
    history: &[StoredMessage],
    keep_recent_turns: impl Into<Option<usize>>,
) -> usize {
    let keep_recent_turns = keep_recent_turns.into();
    if let Some(keep_recent_turns) = keep_recent_turns {
        let user_indices: Vec<usize> = history
            .iter()
            .enumerate()
            .filter(|(_, stored)| stored.message.role == Role::User)
            .map(|(index, _)| index)
            .collect();

        if user_indices.len() > keep_recent_turns {
            let b = snap_to_round_boundary(
                history,
                user_indices[user_indices.len() - keep_recent_turns],
            );
            if !history[..b].iter().all(|s| s.is_summary) {
                return b;
            }
        }
    }

    let b = snap_to_round_boundary(history, budget_boundary(history, FALLBACK_KEEP_TOKENS));
    if b > 0 && !history[..b].iter().all(|s| s.is_summary) {
        b
    } else {
        oldest_foldable_boundary(history, FALLBACK_KEEP_TOKENS)
    }
}

/// Choose a fold boundary that compacts approximately `percent`% of the transcript tokens,
/// keeping whole user turns where possible and falling back to tool-round boundaries
/// when a single turn dominates the context.
pub fn fold_boundary_percent(history: &[StoredMessage], percent: usize) -> usize {
    if history.is_empty() {
        return 0;
    }
    let percent = percent.clamp(1, 90);
    let total: usize = history
        .iter()
        .map(|stored| heuristic_tokens(std::slice::from_ref(&stored.message)) as usize)
        .sum();
    if total == 0 {
        return 0;
    }
    let target_fold = (total * percent) / 100;
    let budget = total.saturating_sub(target_fold);

    let user_indices: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, stored)| stored.message.role == Role::User)
        .map(|(index, _)| index)
        .collect();

    // Prefer keeping whole user turns that fit within the budget
    let mut chosen_user_idx = None;
    for &u_idx in user_indices.iter().rev() {
        let kept_tokens: usize = history[u_idx..]
            .iter()
            .map(|stored| heuristic_tokens(std::slice::from_ref(&stored.message)) as usize)
            .sum();
        if kept_tokens <= budget {
            chosen_user_idx = Some(u_idx);
        } else {
            break;
        }
    }

    if let Some(idx) = chosen_user_idx {
        let b = snap_to_round_boundary(history, idx);
        if !history[..b].iter().all(|s| s.is_summary) {
            return b;
        }
    }

    // When the newest user turn alone exceeds the budget or is the only turn,
    // fold tool rounds inside it.
    //
    // Snapped backward from the raw scan, not through `budget_boundary`: that one advances
    // forward past an assistant tool-call to the next user turn, which is right for the turn
    // rule but wrong here. Mid-turn there is no later user turn, so the advance runs to the
    // final reply and folds everything before it — a 50% fold of a 20k transcript leaving one
    // 3k message. Backward keeps the whole round that straddles the budget.
    let boundary = snap_to_round_boundary(history, budget_boundary_raw(history, budget));
    let boundary_folds_content = boundary > 0 && !history[..boundary].iter().all(|s| s.is_summary);
    if !boundary_folds_content && total > budget {
        oldest_foldable_boundary(history, budget)
    } else {
        boundary
    }
}

/// Tokens of the transcript kept verbatim when the turn rule has nothing to fold.
///
/// The turn rule keeps *turns*; this keeps *tokens*. It is a cap, not a target: a transcript of
/// ordinary messages is far below it, and only a transcript whose messages are large ever feels
/// it.
pub const FALLBACK_KEEP_TOKENS: usize = 16_000;

/// The newest boundary that folds the oldest messages while keeping the tail within `budget`.
///
/// This is the escape hatch for the transcript that is one giant user turn: the turn rule has
/// nothing older to fold and `budget_boundary`'s backward scan resolves to the turn's own start,
/// which folds nothing. The walk here runs FORWARD from the oldest message, so the boundary lands
/// where the tail first fits the budget, and the cut is then snapped to a row the providers accept
/// at the head of a kept tail — an assistant tool-call row whose tool results follow it.
/// Zero means nothing is foldable: the transcript fits the budget on its own.
fn oldest_foldable_boundary(history: &[StoredMessage], budget: usize) -> usize {
    if heuristic_tokens(
        &history
            .iter()
            .map(|s| s.message.clone())
            .collect::<Vec<_>>(),
    ) as usize
        <= budget
    {
        return 0;
    }

    // Walk from the oldest message forward, keeping everything from `index` onward; the boundary
    // is the newest `index` whose kept tail still exceeds the budget by as little as possible —
    // equivalently, the first `index` where the tail fits.
    let mut total = 0usize;
    let tokens: Vec<usize> = history
        .iter()
        .map(|stored| heuristic_tokens(std::slice::from_ref(&stored.message)) as usize)
        .collect();
    for count in &tokens {
        total += count;
    }

    let mut kept = 0usize;
    let mut boundary = 0usize;
    for (index, count) in tokens.iter().enumerate() {
        if total - kept <= budget {
            break;
        }
        kept += count;
        boundary = index + 1;
    }
    // Snap to a round start: the kept tail must open on a row the providers accept — an
    // assistant tool-call row whose results follow it, not an orphaned tool result. Walking
    // backward from the raw boundary to the nearest such row keeps every cut inside whole
    // tool rounds. A snap back over a huge round could exceed the budget, but a round is one
    // call and its result — not the turn.
    //
    // The answers of the last call may not have arrived yet: the fold is planned on the
    // transcript as it was when the fold started, and the tool results land while the
    // summariser is still thinking. Folding that call would leave its answers in the live
    // tail with nothing to answer, and a provider rejects such a request outright.
    let b = snap_to_round_boundary(history, boundary);
    if b == 0 || history[..b].iter().all(|s| s.is_summary) {
        let mut fwd = boundary;
        while fwd < history.len() && history[fwd].message.role == Role::Tool {
            fwd += 1;
        }
        let b_fwd = snap_to_round_boundary(history, fwd);
        if b_fwd > 0 && !history[..b_fwd].iter().all(|s| s.is_summary) {
            return b_fwd;
        }
    }
    b
}

/// The start of the most recent stretch of messages that fits `budget` tokens.
///
/// Walking back from the end, the newest message is always kept: it is the last thing said, and a
/// transcript that cannot spare it cannot be compacted at all.
fn budget_boundary(history: &[StoredMessage], budget: usize) -> usize {
    safe_boundary(history, budget_boundary_raw(history, budget))
}

/// [`budget_boundary`] before provider snapping: the raw backward scan.
///
/// Callers that prefer a backward snap — keeping the round that straddles the budget — use this
/// directly instead of the forward-advancing [`safe_boundary`].
fn budget_boundary_raw(history: &[StoredMessage], budget: usize) -> usize {
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

    boundary
}

/// The first index at or after `desired` whose message can open the kept tail.
///
/// The kept tail is a suffix of the transcript, and neither a tool result nor an assistant
/// message issuing tool calls may stand alone: Gemini strictly requires that a function call
/// turn immediately follow a user turn or function response turn, and other providers reject
/// orphaned tool calls without the user prompt that elicited them. A boundary landing on a
/// tool result or an assistant tool-call therefore advances forward to the next clean turn.
fn safe_boundary(history: &[StoredMessage], desired: usize) -> usize {
    let mut index = desired;
    while index < history.len()
        && (history[index].message.role == Role::Tool
            || (history[index].message.role == Role::Assistant
                && !history[index].message.tool_calls.is_empty()))
    {
        index += 1;
    }
    if index < history.len() {
        return index;
    }
    // If advancing forward reached the end (e.g. desired was inside the latest tool execution),
    // scan backward to find the User prompt that initiated this turn.
    let mut back = desired;
    while back > 0 {
        back -= 1;
        if history[back].message.role == Role::User {
            return back;
        }
    }
    desired
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
    /// The database sequence number of the recorded summary row, if recorded.
    pub fold_seq: Option<i64>,
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

/// Inline image data URLs replaced by a marker, for the summariser's transcript.
///
/// A folded message can begin with `data:image/png;base64,…` — a screenshot, an inline image — and
/// what the clip keeps is the head, so the blob's opening characters are exactly what survives. A
/// provider reads that as an image and decodes it: Gemini answers
/// `HTTP 400 {"message":"Failed to base64-decode image data"}` for a payload the clip has already
/// cut. Every fold over such a session failed, and the session ran on unbounded: six of the ten
/// compaction failures in this machine's log were this. A summary needs to know an image was there,
/// not what its pixels were.
fn elide_image_data(text: &str) -> String {
    if !text.contains("data:image/") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("data:image/") {
        out.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let end = after_start
            .find(char::is_whitespace)
            .unwrap_or(after_start.len());
        let url = &after_start[..end];
        match url.split_once(";base64,") {
            Some((kind, payload)) => {
                out.push_str(&format!(
                    "[inline image {}: {} base64 characters elided]",
                    kind.trim_start_matches("data:"),
                    payload.chars().count()
                ));
            }
            // Not an inline payload: a link, a path, documentation about one. Leave it.
            None => out.push_str(url),
        }
        rest = &after_start[end..];
    }
    out.push_str(rest);
    out
}

/// The head of `text`, cut to `limit` characters, with a marker that says how much was cut.
fn clip(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{}\n… [{} more characters cut]", head, count - limit)
}

/// One stored message rendered as one transcript block, the way the summariser reads it.
fn render_stored(stored: &StoredMessage) -> String {
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
    let mut out = String::new();
    // Elided before the clip, not after: a data URL is one unbroken run of base64, so clipping it
    // first spends the whole budget on the cut blob and drops what the message actually says.
    let body = clip(
        &elide_image_data(message.content.as_deref().unwrap_or_default()),
        TRANSCRIPT_MESSAGE_CHARS,
    );
    out.push_str(&format!("{}: {}\n", label, body));
    for call in &message.tool_calls {
        out.push_str(&format!(
            "  tool call {} {} {}\n",
            call.name,
            call.id,
            clip(
                &elide_image_data(&call.arguments.to_string()),
                TRANSCRIPT_MESSAGE_CHARS
            )
        ));
    }
    if let Some(result) = &message.tool_result {
        out.push_str(&format!(
            "  tool result: {}\n",
            clip(&elide_image_data(&result.content), TRANSCRIPT_MESSAGE_CHARS)
        ));
    }
    out
}

/// The folded messages as the summariser reads them, inside [`TRANSCRIPT_TOTAL_CHARS`].
///
/// Two rules decide what a bounded transcript has to contain, and both are about the *end* of
/// the folded region rather than its start:
///
/// * a **previous summary** is always carried. It is the already-compressed head of the
///   conversation, a handful of lines each, and it is the only trace of the layers it covers —
///   cutting it would lose the oldest history outright, which is the one thing a fold must not do.
/// * the budget is then spent **from the newest message backwards**. The turns immediately
///   before the kept tail are the working state the next turn continues from; rendering the
///   oldest messages first and stopping at the cap throws exactly those away, and the summary
///   then describes how the session opened while saying nothing about what was happening when it
///   was folded. The oldest folded turns are what a summary can afford to lose — they are the
///   ones a prior summary already covers.
fn render_transcript(messages: &[StoredMessage]) -> String {
    let mut out = String::new();
    let mut body: Vec<&StoredMessage> = Vec::with_capacity(messages.len());
    for stored in messages {
        if stored.is_summary {
            out.push_str(&render_stored(stored));
        } else {
            body.push(stored);
        }
    }

    let mut rendered: Vec<String> = Vec::with_capacity(body.len());
    let mut used = out.len();
    let mut cut = body.len();
    for (index, stored) in body.iter().enumerate().rev() {
        let text = render_stored(stored);
        let is_newest = index + 1 == body.len();
        // The newest message is always kept: a transcript that cannot spare the last thing said
        // cannot be summarised at all.
        if !is_newest && used + text.len() > TRANSCRIPT_TOTAL_CHARS {
            break;
        }
        used += text.len();
        rendered.push(text);
        cut = index;
    }
    rendered.reverse();

    if cut > 0 {
        out.push_str(&format!("… [{} older messages cut] …\n", cut));
    }
    out.extend(rendered);
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
    keep_recent_turns: Option<usize>,
) -> Result<Option<CompactionOutcome>, HarnessError> {
    let mut boundary = fold_boundary(history, keep_recent_turns);
    if boundary == 0 || history[..boundary].iter().all(|s| s.is_summary) {
        // The whole transcript is one user turn — an agentic turn that has been running tool
        // rounds for hours. The turn rule has nothing older to fold, and `safe_boundary`'s
        // backward scan resolved the budget boundary to the turn's own start, because a kept tail
        // may not open on an orphaned tool row. Refusing to fold here is how one unbounded turn
        // pinned an unbounded transcript: the session grows forever under an armed fold.
        //
        // Fold the oldest complete tool rounds instead: walk forward from the oldest message to
        // the newest boundary that keeps the newest stretch within the fallback budget, then let
        // `safe_boundary` clean the cut. The kept tail still opens on an assistant tool-call row
        // whose result it carries — a call and its answer stay together.
        boundary = oldest_foldable_boundary(history, FALLBACK_KEEP_TOKENS);
        if boundary == 0 || history[..boundary].iter().all(|s| s.is_summary) {
            return Ok(None);
        }
    }
    compact_at_boundary(provider, model, history, boundary).await
}

/// Summarise approximately `percent`% of `history`'s tokens into one model call.
pub async fn compact_percent(
    provider: &dyn ModelProvider,
    model: &str,
    history: &[StoredMessage],
    percent: usize,
) -> Result<Option<CompactionOutcome>, HarnessError> {
    let mut boundary = fold_boundary_percent(history, percent);
    if boundary == 0 || history[..boundary].iter().all(|s| s.is_summary) {
        let total: usize = history
            .iter()
            .map(|stored| heuristic_tokens(std::slice::from_ref(&stored.message)) as usize)
            .sum();
        let target_fold = (total * percent.clamp(1, 90)) / 100;
        let budget = total.saturating_sub(target_fold);
        boundary = oldest_foldable_boundary(history, budget.max(FALLBACK_KEEP_TOKENS));
        if boundary == 0 || history[..boundary].iter().all(|s| s.is_summary) {
            return Ok(None);
        }
    }
    compact_at_boundary(provider, model, history, boundary).await
}

async fn compact_at_boundary(
    provider: &dyn ModelProvider,
    model: &str,
    history: &[StoredMessage],
    boundary: usize,
) -> Result<Option<CompactionOutcome>, HarnessError> {
    if boundary == 0 || history[..boundary].iter().all(|stored| stored.is_summary) {
        return Ok(None);
    }
    let folded = &history[..boundary];
    let kept = &history[boundary..];
    let transcript = render_transcript(folded);
    debug!(
        "Compacting {} messages ({} turns) with model '{}'",
        folded.len(),
        folded
            .iter()
            .filter(|stored| stored.message.role == Role::User)
            .count(),
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

    let mut kept_messages: Vec<Message> =
        kept.iter().map(|stored| stored.message.clone()).collect();
    for msg in &mut kept_messages {
        msg.usage = None;
    }

    Ok(Some(CompactionOutcome {
        summary,
        kept: kept_messages,
        folded_ids: folded.iter().map(|stored| stored.id).collect(),
        folded_turns: folded
            .iter()
            .filter(|stored| stored.message.role == Role::User)
            .count(),
        folded_messages: folded.len(),
        fold_seq: None,
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

    /// A summariser prompt must carry no base64 image payload.
    ///
    /// A provider reads one as an image and decodes it, and the clip has already cut it, so the
    /// call comes back `HTTP 400 {"message":"Failed to base64-decode image data"}` — which is how
    /// six folds on this machine failed, each leaving the session running on its full transcript.
    #[test]
    fn an_inline_image_is_elided_from_the_summarisers_transcript() {
        let payload = "A".repeat(3_000);
        let rendered = render_transcript(&[stored(
            1,
            Message::tool(crate::types::ToolResult::success(
                "call_1",
                format!("screenshot: data:image/png;base64,{payload}"),
            )),
        )]);
        assert!(
            !rendered.contains(&payload),
            "the base64 payload must not reach the summariser"
        );
        assert!(
            rendered.contains("[inline image image/png: 3000 base64 characters elided]"),
            "the marker says an image was there: {rendered}"
        );
    }

    /// The elision is for inline payloads only: a link, a path, or prose about one is left alone.
    #[test]
    fn a_data_url_that_carries_no_payload_is_left_alone() {
        let text = "see data:image/png;base64 not the format";
        assert_eq!(elide_image_data(text), text);
    }

    /// Eliding first and clipping second is what keeps a message's meaning: a data URL is one
    /// unbroken run, so a clip that ran first would spend the whole budget on the blob.
    #[test]
    fn a_message_keeps_its_text_when_it_opens_with_an_image() {
        let payload = "B".repeat(50_000);
        let rendered = render_transcript(&[stored(
            1,
            Message::user(format!(
                "data:image/jpeg;base64,{payload} and here is what I saw"
            )),
        )]);
        assert!(rendered.contains("and here is what I saw"), "{rendered}");
        assert!(!rendered.contains("characters cut"), "{rendered}");
    }
}
