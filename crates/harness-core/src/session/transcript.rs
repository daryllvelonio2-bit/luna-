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
    messages
}
