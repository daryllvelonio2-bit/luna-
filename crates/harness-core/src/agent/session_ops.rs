//! Session-store operations: replay, append, hook events, the background memory saver, and the
//! read-side compaction helpers (`/compact`-adjacent reports).

use std::sync::atomic::Ordering;
use tracing::{debug, info, warn};

use super::{Agent, ReplayNotice};
use crate::error::HarnessError;
use crate::hooks::{HookContext, HookEvent};
use crate::saver::transcript_tail;
use crate::session::{estimate_tokens, last_fold_seq, sendable_messages, StoredMessage};
use crate::traits::*;
use crate::types::Message;

impl<P, M, N, R> Agent<P, M, N, R>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    R: ToolRegistry + 'static,
{
    /// Fire an event that has no tool attached (`session_start`, `stop`, …).
    ///
    /// A denial is surfaced as [`HarnessError::ApprovalRejected`] so no caller can mistake a
    /// refused event for a completed one.
    pub async fn emit_hook_event(
        &self,
        event: HookEvent,
        message: Option<&str>,
    ) -> Result<(), HarnessError> {
        let Some(hooks) = self.hooks.as_ref() else {
            return Ok(());
        };
        let session_id = self
            .session
            .as_ref()
            .map(|binding| binding.session_id.as_str());
        let context = HookContext {
            session_id,
            message,
            ..HookContext::default()
        };
        let outcome = hooks.run(event, &context).await;
        for note in &outcome.notes {
            debug!("{} hook: {}", event.as_str(), note);
        }
        match outcome.denial() {
            // A refusal from an event that was only watching is reported, not obeyed: a
            // `session_end` hook exiting 2 must not turn a finished turn into a failed one.
            Some(reason) if event.refusal_blocks_turn() => Err(HarnessError::ApprovalRejected(
                format!("{} hook refused: {reason}", event.as_str()),
            )),
            Some(reason) => {
                warn!(
                    "{} hook refused, which this event cannot do: {}",
                    event.as_str(),
                    reason
                );
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Hand the finished turn to the saver when its interval is due.
    ///
    /// Detached on purpose: the reply is already written by now, so a second model call must not
    /// hold the terminal. A failure is logged, never returned — a saver that cannot reach its
    /// provider must not turn a completed turn into a failed one.
    pub(super) fn spawn_background_saver(&self, messages: &[Message]) {
        let Some(hook) = self.saver.as_ref() else {
            return;
        };
        let completed = self.turns_completed.fetch_add(1, Ordering::Relaxed) + 1;
        if !hook.due(completed) {
            return;
        }
        let tail = transcript_tail(messages, hook.saver.tail_limit());
        if tail.is_empty() {
            return;
        }
        let saver = hook.saver.clone();
        debug!(
            "dispatching background memory saver after turn {} ({} messages, model {})",
            completed,
            tail.len(),
            saver.model()
        );
        tokio::spawn(async move {
            match saver.distil(&tail).await {
                Ok(report) => info!("background memory saver: {}", report.summary()),
                Err(error) => warn!("background memory saver failed: {}", error),
            }
        });
    }

    /// Summarise the current session's conversation history using the configured
    /// compaction model (or the session's active provider and `fallback_model`).
    pub async fn summarize_current_session(
        &self,
        fallback_model: &str,
    ) -> Result<String, HarnessError> {
        let Some(binding) = &self.session else {
            return Err(HarnessError::SessionError(
                "no session bound to agent".to_string(),
            ));
        };
        let replay = binding
            .store
            .replay(&binding.session_id, usize::MAX)
            .await?;
        if replay.history.is_empty() {
            return Err(HarnessError::SessionError(
                "no messages in current session to summarize".to_string(),
            ));
        }
        let (provider, model): (&dyn ModelProvider, &str) = if let Some(setup) = &self.compaction {
            (setup.provider.as_ref(), &setup.model)
        } else {
            (self.provider.as_ref(), fallback_model)
        };
        crate::session::summarize_session(provider, model, &replay.history).await
    }

    /// Get the estimated token count for the current session's conversation history.
    /// This loads the history from the session store and runs the compaction estimator.
    ///
    /// Reported usage wins, *unless* the newest fold is newer than it: the kept tail of a folded
    /// session still carries the usage of the call made against the transcript that was folded
    /// away, and handing that number back draws the gauge with the size of history the fold just
    /// removed — the readout only catches up when the next request reports fresh usage.
    pub async fn estimate_session_tokens(&self) -> Result<u32, HarnessError> {
        let (history, _replay_notice) = self.load_history().await?;

        if history.is_empty() {
            return Ok(0);
        }

        // A summary row is written with the highest `seq` in the session, so any usage reported
        // before it describes a prompt that no longer exists.
        let fold_seq = last_fold_seq(&history);

        // 1. If any prior turn recorded actual usage from the provider, use the most recent report
        if let Some((usage_seq, latest_usage)) = history.iter().rev().find_map(|stored| {
            stored
                .message
                .usage
                .as_ref()
                .map(|usage| (stored.seq, usage))
        }) {
            if !fold_seq.is_some_and(|fold_seq| usage_seq < fold_seq) {
                return Ok(latest_usage
                    .prompt_tokens
                    .saturating_add(latest_usage.completion_tokens));
            }
            debug!(
                "The newest reported usage (seq {}) predates the fold at seq {:?}; estimating from the live transcript",
                usage_seq, fold_seq
            );
        }

        // 2. Otherwise estimate from the messages that would actually be sent, with the retired
        //    reports dropped so the estimator measures the live transcript and not the folded one.
        Ok(estimate_tokens(&sendable_messages(&history)).tokens)
    }

    /// The session's recent transcript, plus the notice the console prints about the bound.
    pub(super) async fn load_history(
        &self,
    ) -> Result<(Vec<StoredMessage>, Option<ReplayNotice>), HarnessError> {
        let Some(binding) = &self.session else {
            return Ok((Vec::new(), None));
        };

        let replay = binding
            .store
            .replay(&binding.session_id, binding.resume_max_messages)
            .await?;
        let notice = ReplayNotice {
            session_id: binding.session_id.clone(),
            replayed: replay.replayed,
            total: replay.total,
        };
        debug!(
            "Replayed {} of {} messages of session '{}'",
            replay.replayed, replay.total, binding.session_id
        );
        Ok((replay.history, Some(notice)))
    }

    /// Append to the session store, if this run is bound to one. A failed write is an error: the
    /// session is the record of what was sent, and a turn that cannot be recorded must not be
    /// reported as a normal turn.
    pub(super) async fn append(&self, message: &Message, is_summary: bool) -> Result<(), HarnessError> {
        let Some(binding) = &self.session else {
            return Ok(());
        };
        binding
            .store
            .append(&binding.session_id, message, is_summary)
            .await
            .map(|_| ())
    }
}
