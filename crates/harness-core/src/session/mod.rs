//! Session persistence and bounded context.
//!
//! A chat session is a row in `sessions` plus an ordered list of `messages` rows, both in the
//! sqlite file at `paths.sessions_db` (default `~/.luna/sessions.db`, WAL). The agent appends every
//! turn it runs; [`SqliteSessionStore::replay`] reads the recent transcript back so a restarted
//! process can answer "what did I just ask you?".
//!
//! Bounded context is [`compaction`]: when the estimated transcript size crosses the fold
//! threshold — `agent.auto_compact_threshold` when auto-compaction is on, else
//! `agent.compact_at_tokens` — the oldest user turns are folded into one structured checkpoint
//! row, the configured number of recent turns stay verbatim, and the folded rows are kept in the
//! table with `compacted_at` set (nothing is destroyed — that is the raw material for a rewind
//! spec). [`estimate_tokens`] is the one estimator both the threshold and the tests use.
//!
//! Folding always goes through [`compaction::compact`] and [`SessionStore::record_compaction`]
//! together. Marking rows `compacted_at` *without* writing the summary they were folded into drops
//! those turns out of every later replay with nothing standing in for them, which is
//! indistinguishable — to the model — from the conversation never having happened.

pub mod compaction;
pub mod context;
pub mod transcript;

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::HarnessError;
use crate::types::Message;

pub use compaction::{
    compact, compact_percent, compact_tokens, estimate_tokens, fold_boundary,
    fold_boundary_percent, fold_boundary_tokens, heuristic_tokens, summarize_session,
    CompactionOutcome, Estimate, EstimateSource, CHARS_PER_TOKEN, CHECKPOINT_SECTIONS,
    COMPACTION_TARGET_KEEP_TOKENS, HEURISTIC_IMAGE_CHARS, HEURISTIC_TOLERANCE,
};
pub use context::{measure as measure_context_parts, ContextParts};
pub use transcript::{last_fold_seq, sendable_messages, trim_messages_to_tokens};

/// One chat session as `luna sessions list` reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// resurrecting a stale id. `/default_model` global scope clears it again.
    pub model: Option<(String, String)>,
    /// The provider and model this session uses for compaction (`/compact-model`). `None`
    /// until a session-specific compaction model is set; defaults to global config or the active model.
    pub compact_model: Option<(String, String)>,
    /// Provider -> credential_id map for session-specific priority credentials.
    pub credential_priorities: std::collections::HashMap<String, String>,
    /// Skill names pinned to this session.
    #[serde(default)]
    pub pinned_skills: Vec<String>,
}

/// A message as it is stored: the row id and sequence number matter, because compaction marks the
/// folded rows by id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    /// Clear this session's model pin so it follows the configured default (`/default_model`
    /// global scope). `None` is what a session that never picked its own model reports.
    async fn clear_model(&self, id: &str) -> Result<(), HarnessError>;

    /// Record the provider and model this session uses for compaction (`/compact-model`).
    async fn set_compact_model(
        &self,
        id: &str,
        provider: &str,
        model: &str,
    ) -> Result<(), HarnessError>;

    /// Clear the custom compaction model for this session, reverting to global config or the active model.
    async fn clear_compact_model(&self, id: &str) -> Result<(), HarnessError>;

    /// Record the priority credential for a provider in this session.
    async fn set_credential_priority(
        &self,
        id: &str,
        provider: &str,
        credential_id: &str,
    ) -> Result<(), HarnessError>;

    /// Clear the priority credential for a provider in this session.
    async fn clear_credential_priority(&self, id: &str, provider: &str)
        -> Result<(), HarnessError>;

    /// Record the pinned skill names for this session.
    async fn set_pinned_skills(&self, id: &str, skills: &[String]) -> Result<(), HarnessError>;

    /// Read the pinned skill names for this session.
    async fn get_pinned_skills(&self, id: &str) -> Result<Vec<String>, HarnessError>;

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

    /// Prune empty sessions that have zero messages, optionally sparing an active session.
    /// Returns the number of empty session rows removed.
    async fn prune_empty_sessions(&self, except_id: Option<&str>) -> Result<usize, HarnessError>;

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

/// Strip internal harness prompt envelopes (plan mode banners, out-of-band steer
/// wrappers, goal-loop iteration wrappers, and background job completion triggers)
/// from a stored user message so UIs and auto-derived session titles display only
/// what the human actually typed.
pub fn clean_user_display_text(raw: &str) -> String {
    let mut text = raw.trim();
    if text.is_empty() {
        return String::new();
    }

    // Internal harness-initiated turns are not user-authored text.
    if text.starts_with("[background job finished]")
        || text.starts_with(crate::GOAL_ITERATION_PREFIX)
    {
        return String::new();
    }

    // Collapse iteration-1 autonomous goal prompts back to `/goal <objective>`.
    // The prompt comes from `prompts.md`; a checkout that carries CRLF is parsed as LF here.
    let normalized;
    let goal_text = if text.contains('\r') {
        normalized = text.replace("\r\n", "\n");
        normalized.as_str()
    } else {
        text
    };
    if goal_text.starts_with("Autonomous Goal Mode (iteration 1/") {
        if let Some(after_obj) = goal_text.split("\n\nObjective:\n").nth(1) {
            let objective = after_obj
                .split("\n\nExecution contract:")
                .next()
                .unwrap_or(after_obj)
                .trim();
            if !objective.is_empty() {
                return format!("/goal {objective}");
            }
        }
    }

    // Unwrap `[OUT-OF-BAND USER MESSAGE ...]` ... `[/OUT-OF-BAND USER MESSAGE]`.
    if let Some(start_pos) = text.find("[OUT-OF-BAND USER MESSAGE") {
        let prefix = text[..start_pos].trim();
        if prefix.is_empty() || prefix.starts_with("[PLAN MODE ACTIVE:") {
            let after_start = &text[start_pos..];
            let body_start = if let Some(nl) = after_start.find('\n') {
                start_pos + nl + 1
            } else if let Some(br) = after_start.find(']') {
                start_pos + br + 1
            } else {
                start_pos + "[OUT-OF-BAND USER MESSAGE".len()
            };
            let body = if let Some(end_pos) = text.find("[/OUT-OF-BAND USER MESSAGE]") {
                if end_pos >= body_start {
                    &text[body_start..end_pos]
                } else {
                    &text[body_start..]
                }
            } else {
                &text[body_start..]
            };
            text = body.trim();
        }
    }

    // Strip `[PLAN MODE ACTIVE: ...]`, `[VOICE MODE ACTIVE: ...]`, etc. (including repeated/nested).
    loop {
        let trimmed = text.trim_start();
        let is_mode_banner =
            trimmed.starts_with("[PLAN MODE ACTIVE:") || trimmed.starts_with("[VOICE MODE ACTIVE:");
        if is_mode_banner {
            if let Some(end) = trimmed.find(']') {
                text = trimmed[end + 1..].trim_start_matches(['\r', '\n', ' ', '\t']);
                continue;
            }
        }
        text = trimmed;
        break;
    }

    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::clean_user_display_text;

    #[test]
    fn strips_plan_mode_and_out_of_band_prefixes() {
        let raw = "[PLAN MODE ACTIVE: Formulate a detailed, step-by-step plan/blueprint. Do not attempt to modify files or execute mutating commands until the plan is approved.]\n\nhi";
        assert_eq!(clean_user_display_text(raw), "hi");

        let steer = "[OUT-OF-BAND USER MESSAGE — a direct message from the user]\nfocus on tests\n[/OUT-OF-BAND USER MESSAGE]";
        assert_eq!(clean_user_display_text(steer), "focus on tests");

        assert_eq!(
            clean_user_display_text("[background job finished]\nsome internal info"),
            ""
        );
    }

    /// `prompts.md` is checked out with LF now (`.gitattributes`), but an older Windows checkout
    /// still carries CRLF, and the collapsed replay line must not depend on that.
    #[test]
    fn collapses_a_goal_prompt_that_arrived_with_crlf() {
        let raw = "Autonomous Goal Mode (iteration 1/4).\r\n\r\nObjective:\r\nfix tests\r\n\r\nExecution contract:\r\n- work\r\n";
        assert_eq!(clean_user_display_text(raw), "/goal fix tests");
    }
}
