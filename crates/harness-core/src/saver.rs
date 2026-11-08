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

/// What a pass did to one stored item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStatus {
    /// The store had no active record for this key.
    Saved,
    /// An active record for the same key was replaced or extended.
    Updated,
    /// The item was archived (notes) or deactivated (memories).
    Deleted,
    /// Two or more items were folded into one.
    Merged,
    /// The operation was refused before it reached the store.
    Skipped,
}

impl SaveStatus {
    /// The word a notification shows: `saved`, `updated`, `deleted`, `merged`, `skipped`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Saved => "saved",
            Self::Updated => "updated",
            Self::Deleted => "deleted",
            Self::Merged => "merged",
            Self::Skipped => "skipped",
        }
    }
}

/// Backwards-compatible name for the status a memory write reports.
pub type MemorySaveStatus = SaveStatus;

/// Which store an outcome belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStorage {
    /// A SQLite memory record.
    Memory,
    /// A markdown note.
    Note,
}

impl SaveStorage {
    /// The word a notification shows: `memory` or `note`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Note => "note",
        }
    }
}

/// One memory a pass wrote, as a UI should announce it.
///
/// The saver is the only place that knows *which* records a detached pass produced; the turn is
/// already over by the time they land. This is the smallest thing that lets the CLI say
/// `memory Astra saved` without inventing a second event per write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedMemory {
    /// Friendly name when the model gave one, otherwise the record's `subject.predicate` id.
    pub name: String,
    /// Created, replaced, deactivated, or merged.
    pub status: MemorySaveStatus,
}

/// One note a pass wrote, as a UI should announce it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedNote {
    /// The note's title (the human name), never its slug.
    pub name: String,
    /// Created, updated, deleted, or merged.
    pub status: SaveStatus,
}

/// One audited outcome: what the pass did, to what, and why.
///
/// Unlike [`SavedMemory`] this also carries refusals, so a pass can explain itself without a log
/// line per operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    pub storage: SaveStorage,
    pub name: String,
    pub status: SaveStatus,
    /// Present when `status` is `Skipped`: what rule refused it.
    pub reason: Option<String>,
}

/// What one distillation pass produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaveReport {
    /// Memory records written, revised, deactivated, or merged.
    pub memories: usize,
    /// Notes written, updated, archived, or merged.
    pub notes: usize,
    /// Items the saver refused (too short, duplicated, unsupported evidence, capped, secret).
    pub skipped: usize,
    /// Operations the model explicitly declined with `noop`.
    pub declined: usize,
    /// Each memory write, in write order, so a UI can report it by name.
    pub saved: Vec<SavedMemory>,
    /// Each note write, in write order, so a UI can report it by title.
    pub saved_notes: Vec<SavedNote>,
    /// Every operation's outcome, including refusals and reasons.
    pub outcomes: Vec<SaveOutcome>,
}

impl SaveReport {
    /// True when the pass left the store exactly as it found it.
    pub fn is_empty(&self) -> bool {
        self.memories == 0 && self.notes == 0
    }

    /// Record one applied memory write and its notification.
    pub fn memory_written(&mut self, name: impl Into<String>, status: SaveStatus) {
        let name = name.into();
        self.memories += 1;
        self.outcomes.push(SaveOutcome {
            storage: SaveStorage::Memory,
            name: name.clone(),
            status,
            reason: None,
        });
        self.saved.push(SavedMemory { name, status });
    }

    /// Record one applied note write and its notification.
    pub fn note_written(&mut self, name: impl Into<String>, status: SaveStatus) {
        let name = name.into();
        self.notes += 1;
        self.outcomes.push(SaveOutcome {
            storage: SaveStorage::Note,
            name: name.clone(),
            status,
            reason: None,
        });
        self.saved_notes.push(SavedNote { name, status });
    }

    /// Record one refused operation and why.
    pub fn skip(&mut self, storage: SaveStorage, name: impl Into<String>, reason: impl Into<String>) {
        self.skipped += 1;
        self.outcomes.push(SaveOutcome {
            storage,
            name: name.into(),
            status: SaveStatus::Skipped,
            reason: Some(reason.into()),
        });
    }

    /// One line for the log: what the pass changed, and what it threw away.
    pub fn summary(&self) -> String {
        if self.is_empty() && self.skipped == 0 && self.declined == 0 {
            return "nothing worth keeping".to_string();
        }
        let mut parts = vec![
            format!("{} memories", self.memories),
            format!("{} notes", self.notes),
        ];
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        if self.declined > 0 {
            parts.push(format!("{} declined", self.declined));
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

    /// Enable or disable note writes at runtime, without touching memory writes.
    ///
    /// The default is a no-op so savers that only write memory need not implement it. A saver
    /// that writes both honours the session's `/notes off`.
    fn set_notes_enabled(&self, _enabled: bool) {}

    /// Enable or disable memory-record writes at runtime, without touching note writes.
    ///
    /// The default is a no-op so savers that only write notes need not implement it. A saver
    /// that writes both honours the session's `/memory off`.
    fn set_memory_enabled(&self, _enabled: bool) {}
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
            ..SaveReport::default()
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
