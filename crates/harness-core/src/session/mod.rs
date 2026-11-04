//! Session persistence and bounded context.
//!
//! A chat session is a row in `sessions` plus an ordered list of `messages` rows, both in the
//! sqlite file at `paths.sessions_db` (default `~/.luna/sessions.db`, WAL). The agent appends every
//! turn it runs; [`SqliteSessionStore::replay`] reads the recent transcript back so a restarted
//! process can answer "what did I just ask you?".
//!
//! Bounded context is [`compaction`]: when the estimated transcript size crosses the fold
//! threshold — `agent.auto_compact_threshold` when auto-compaction is on, else
//! `agent.compact_at_tokens` — the oldest user turns are folded into a single `system`-role summary
//! row, the configured number of recent turns stay verbatim, and the folded rows are kept in the
//! table with `compacted_at` set (nothing is destroyed — that is the raw material for a rewind
//! spec). [`estimate_tokens`] is the one estimator both the threshold and the tests use.
//!
//! Folding always goes through [`compaction::compact`] and [`SessionStore::record_compaction`]
//! together. Marking rows `compacted_at` *without* writing the summary they were folded into drops
//! those turns out of every later replay with nothing standing in for them, which is
//! indistinguishable — to the model — from the conversation never having happened.

pub mod compaction;
pub mod transcript;

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::HarnessError;
use crate::types::Message;

pub use compaction::{
    compact, compact_percent, estimate_tokens, fold_boundary, fold_boundary_percent,
    heuristic_tokens, summarize_session, CompactionOutcome, Estimate, EstimateSource,
    CHARS_PER_TOKEN, HEURISTIC_IMAGE_CHARS, HEURISTIC_TOLERANCE,
};
pub use transcript::{last_fold_seq, sendable_messages};

/// One chat session as `luna sessions list` reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// First user line, truncated. Empty until the first user message is appended.
    pub title: String,
    /// User-assigned name (`/name`), distinct from the auto-derived `title`. Empty when unset.
    pub name: String,
    /// Messages that are still live (not folded into a summary).
    pub message_count: usize,
    /// The provider and model this session last ran with (`/model` switch, or the model in
    /// use when the session was created by `/new`). `None` until the session has a model of
    /// its own, so a session that never picked one resumes on the config default instead of
    /// resurrecting a stale id.
    pub model: Option<(String, String)>,
    /// The provider and model this session uses for compaction (`/compact-model`). `None`
    /// until a session-specific compaction model is set; defaults to global config or the active model.
    pub compact_model: Option<(String, String)>,
}

/// A message as it is stored: the row id and sequence number matter, because compaction marks the
/// folded rows by id.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMessage {
    pub id: i64,
    pub seq: i64,
    pub message: Message,
    /// A synthetic `system`-role summary written by compaction.
    pub is_summary: bool,
}

/// What [`SessionStore::replay`] hands back.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayOutcome {
    /// Summaries first, then the live transcript in `seq` order, bounded by the resume limit.
    pub history: Vec<StoredMessage>,
    /// Live messages the session holds, before the bound was applied.
    pub total: usize,
    /// How many the bound allowed (`history.len()`).
    pub replayed: usize,
}

impl ReplayOutcome {
    /// True when the resume bound dropped messages.
    pub fn truncated(&self) -> bool {
        self.total > self.replayed
    }
}

/// The session table, behind a trait so the agent loop can be tested without a file.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Create the session when it does not exist, and return its record.
    async fn ensure_session(&self, id: &str) -> Result<SessionRecord, HarnessError>;

    /// The record for an existing session.
    async fn get(&self, id: &str) -> Result<SessionRecord, HarnessError>;

    /// Set the user-assigned name (`/name`); an empty string clears it.
    async fn set_name(&self, id: &str, name: &str) -> Result<(), HarnessError>;

    /// Record the provider and model this session runs with (`/model`). Both parts are stored
    /// together so a resume restores the exact pair, not a half.
    async fn set_model(&self, id: &str, provider: &str, model: &str) -> Result<(), HarnessError>;

    /// Record the provider and model this session uses for compaction (`/compact-model`).
    async fn set_compact_model(
        &self,
        id: &str,
        provider: &str,
        model: &str,
    ) -> Result<(), HarnessError>;

    /// Clear the custom compaction model for this session, reverting to global config or the active model.
    async fn clear_compact_model(&self, id: &str) -> Result<(), HarnessError>;

    /// Append one message, assigning the next `seq` and updating the session's `updated_at`.
    /// Returns the new row id.
    async fn append(
        &self,
        session_id: &str,
        message: &Message,
        is_summary: bool,
    ) -> Result<i64, HarnessError>;

    /// The most recent `limit` live messages (plus every summary row, which is never dropped),
    /// and the live count before the bound.
    async fn replay(&self, session_id: &str, limit: usize) -> Result<ReplayOutcome, HarnessError>;

    /// The most recent messages of the conversation for user display, regardless of whether
    /// past turns have been compacted for the model context. Summary rows are excluded
    /// because they are internal model-compaction artifacts rather than conversation turns.
    async fn replay_transcript(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<ReplayOutcome, HarnessError>;

    /// Write `summary` as a new summary row and mark `folded` rows `compacted_at`, in one
    /// transaction. Returns the summary's sequence number.
    ///
    /// Every size report the session holds is dropped with it: see [`Self::clear_usage_reports`].
    async fn record_compaction(
        &self,
        session_id: &str,
        folded: &[i64],
        summary: &str,
    ) -> Result<i64, HarnessError>;

    /// Drop every size report the session has recorded.
    ///
    /// `usage` on a row is not an annotation on that message: it is the size of the *request* the
    /// provider answered, which covered the whole transcript up to that reply. A fold retires the
    /// rows those requests carried, so every report written before it describes a prompt that will
    /// never be sent again — while both the fold bar and the context gauge read it as the size of
    /// the session. A transcript a fold has already bounded then still looks as large as the one it
    /// replaced: the bar folds it again, and again, chewing the stored transcript down to a fraction
    /// of the configured target. That is not hypothetical: one session folded twenty times inside a
    /// single turn, and its next prompt collapsed from 180k tokens to 11k.
    ///
    /// Called when the fold is written and again when a running turn adopts it, so a report made by
    /// a request sent against the transcript the fold replaced cannot raise the bar a second time.
    async fn clear_usage_reports(&self, session_id: &str) -> Result<(), HarnessError>;

    /// Every session, most recently updated first.
    async fn list(&self) -> Result<Vec<SessionRecord>, HarnessError>;

    /// The most recently updated session id, if any.
    async fn most_recent(&self) -> Result<Option<String>, HarnessError>;

    /// Delete a session and every message it holds. Returns `true` when a session row was
    /// removed, so a caller can tell "deleted" apart from "was already gone".
    async fn delete_session(&self, id: &str) -> Result<bool, HarnessError>;

    /// Delete every session and every message. Returns how many sessions were removed.
    /// Bulk-delete in one statement rather than a `list()` + `delete_session()` loop, so a
    /// caller that has thousands of sessions does not pay a round trip per row, and a
    /// concurrent append cannot slip a half-cleared store between two deletes.
    async fn delete_all_sessions(&self) -> Result<usize, HarnessError>;

    /// Fork an existing session into a new session copy, cloning all message history and metadata.
    async fn fork_session(
        &self,
        source_id: &str,
        new_title: Option<&str>,
    ) -> Result<String, HarnessError>;
}

/// Default `paths.sessions_db`, also used when a caller passes `None`.
///
/// Resolved from [`crate::config::PathsSettings`] rather than re-spelling `$HOME/.luna` here: a
/// relocated `paths.sessions_db` must not be silently bypassed by `SqliteSessionStore::new(None)`.
pub fn default_sessions_db() -> PathBuf {
    crate::config::PathsSettings::default().sessions_db
}

pub use sqlite::SqliteSessionStore;

mod sqlite;
