//! The background memory saver: the contract the agent loop calls, not its implementation.
//!
//! A session can be configured so that durable memory is *not* written by the turn (`save_memory`
//! tool calls chosen by the model) but by an independent pass over the conversation that just
//! happened: same records, same engines, second reader. The pass needs nothing the agent loop owns
//! except the transcript, so the loop only needs this trait — the distiller prompt, the provider
//! call and the writes live where the engines do (`harness-memory`).
//!
//! The loop keeps its side of the bargain minimal and non-blocking:
//!
//! - it runs the saver on a *detached task*, so a slow second model call never delays a reply;
//! - it hands over only the transcript *tail*, because a saver shown the whole session would
//!   re-derive the same facts every turn and pay for those tokens twice;
//! - it never blocks a turn on the saver's failure: a mistake there is a log line, not a failed turn.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::HarnessError;
use crate::traits::ModelProvider;
use crate::types::{Message, Role};

/// What one distillation pass produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaveReport {
    /// Memory records written or revised.
    pub memories: usize,
    /// Notes written.
    pub notes: usize,
    /// Items the saver declined to write (too short, duplicated, malformed).
    pub skipped: usize,
}

impl SaveReport {
    /// True when the pass left the store exactly as it found it.
    pub fn is_empty(&self) -> bool {
        self.memories == 0 && self.notes == 0
    }

    /// One line for the log: what the pass changed, and what it threw away.
    pub fn summary(&self) -> String {
        if self.is_empty() && self.skipped == 0 {
            return "nothing worth keeping".to_string();
        }
        let mut parts = vec![
            format!("{} memories", self.memories),
            format!("{} notes", self.notes),
        ];
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        parts.join(", ")
    }
}

/// An independent memory writer: read a transcript, write memories and notes.
#[async_trait]
pub trait BackgroundSaver: Send + Sync {
    /// Distil `tail` (the most recent messages, oldest first) into durable records.
    async fn distil(&self, tail: &[Message]) -> Result<SaveReport, HarnessError>;

    /// How many trailing messages this saver wants to see.
    fn tail_limit(&self) -> usize;

    /// Distil after every Nth completed turn; `0` disables the saver entirely.
    fn every_turns(&self) -> usize;

    /// The model this pass bills to, for the log line.
    fn model(&self) -> &str;
}

/// The trailing messages a saver should read, oldest first.
///
/// The system prompt is dropped: it is rebuilt every turn, carries no new user intent, and is the
/// single largest message in the transcript — feeding it to the saver would dominate the budget.
pub fn transcript_tail(messages: &[Message], limit: usize) -> Vec<Message> {
    let conversational: Vec<Message> = messages
        .iter()
        .filter(|message| !matches!(message.role, Role::System))
        .cloned()
        .collect();
    let start = conversational.len().saturating_sub(limit);
    conversational[start..].to_vec()
}

/// Attach a saver to a session: the pair the agent loop stores.
#[derive(Clone)]
pub struct SaverHook {
    /// The pass to run.
    pub saver: Arc<dyn BackgroundSaver>,
    /// Every how many completed turns it runs.
    pub every_turns: usize,
}

impl SaverHook {
    /// A hook that runs after every `every_turns` completed turns; `0` disables it.
    pub fn new(saver: Arc<dyn BackgroundSaver>, every_turns: usize) -> Self {
        Self { saver, every_turns }
    }

    /// Build the hook from the saver's own policy, which is where the config lands.
    pub fn from_saver(saver: Arc<dyn BackgroundSaver>) -> Self {
        let every_turns = saver.every_turns();
        Self { saver, every_turns }
    }

    /// Whether this completed turn (1-based) is a turn the saver runs on.
    pub fn due(&self, completed_turns: usize) -> bool {
        self.every_turns > 0 && completed_turns.is_multiple_of(self.every_turns)
    }
}

/// Providers are shared with the agent and with the saver; the alias keeps signatures readable.
pub type SharedProvider = Arc<dyn ModelProvider>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;

    fn messages(count: usize) -> Vec<Message> {
        let mut out = vec![Message::system("system prompt")];
        for index in 0..count {
            out.push(Message::user(format!("user {index}")));
            out.push(Message::assistant(format!("assistant {index}")));
        }
        out
    }

    #[test]
    fn the_tail_is_the_most_recent_messages_without_the_system_prompt() {
        let all = messages(3);
        let tail = transcript_tail(&all, 2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].content.as_deref(), Some("user 2"));
        assert_eq!(tail[1].content.as_deref(), Some("assistant 2"));
        assert!(tail.iter().all(|message| message.role != Role::System));
    }

    #[test]
    fn a_short_transcript_is_passed_whole_and_a_zero_limit_yields_nothing() {
        let all = messages(1);
        assert_eq!(transcript_tail(&all, 50).len(), 2);
        assert!(transcript_tail(&all, 0).is_empty());
    }

    #[test]
    fn the_hook_runs_on_every_nth_completed_turn() {
        let saver = Arc::new(FakeSaver { every: 3 });
        let hook = SaverHook::from_saver(saver);
        assert_eq!(hook.every_turns, 3);
        assert!(!hook.due(1));
        assert!(!hook.due(2));
        assert!(hook.due(3));
        assert!(hook.due(6));

        // A zero interval is "off", not "every turn".
        let off = SaverHook::new(Arc::new(FakeSaver { every: 0 }), 0);
        assert!(!off.due(1));
    }

    #[test]
    fn a_report_says_what_it_changed() {
        assert_eq!(SaveReport::default().summary(), "nothing worth keeping");
        assert!(SaveReport::default().is_empty());
        let report = SaveReport {
            memories: 2,
            notes: 1,
            skipped: 3,
        };
        assert_eq!(report.summary(), "2 memories, 1 notes, 3 skipped");
        assert!(!report.is_empty());
    }

    struct FakeSaver {
        every: usize,
    }

    #[async_trait]
    impl BackgroundSaver for FakeSaver {
        async fn distil(&self, tail: &[Message]) -> Result<SaveReport, HarnessError> {
            Ok(SaveReport {
                memories: tail.len(),
                ..SaveReport::default()
            })
        }

        fn tail_limit(&self) -> usize {
            24
        }

        fn every_turns(&self) -> usize {
            self.every
        }

        fn model(&self) -> &str {
            "fake"
        }
    }
}
