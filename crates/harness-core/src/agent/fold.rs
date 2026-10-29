//! Compaction folds: the background fold a turn starts without waiting for, the explicit
//! `/compact` fold, and the mid-turn rebind when a background fold lands.

use std::sync::atomic::Ordering;
use tracing::{debug, warn};

use super::{Agent, AgentEvent, CompactionNotice, FoldInFlight, FoldPolicy, FoldTarget};
use crate::error::HarnessError;
use crate::session::{
    compact, compact_percent, estimate_tokens, heuristic_tokens, last_fold_seq, sendable_messages,
    CompactionOutcome, SessionStore, StoredMessage,
};
use crate::traits::*;
use crate::types::{Message, Role};

impl<P, M, N, R> Agent<P, M, N, R>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    R: ToolRegistry + 'static,
{
    /// Fold the oldest turns of the current session into a summary, ignoring
    /// `agent.compact_at_tokens` — this is the `/compact` command. Nothing to fold (the session
    /// holds no more turns than `agent.keep_recent_turns`), no compaction setup, or a failed
    /// summary is an error, because the user asked for the fold explicitly.
    ///
    /// Every outcome is also announced as a [`AgentEvent::CompactionFinished`] with `forced: true`,
    /// including the failures raised here before `compact_now` would have run — a UI that reports
    /// folds from the events alone must never miss the reason a fold it asked for did not happen.
    pub async fn compact_current_session(&self) -> Result<CompactionNotice, HarnessError> {
        if self.compaction.is_none() {
            let error = HarnessError::SessionError(
                "compaction is unavailable: no compaction provider is configured — set agent.compaction_provider and agent.compaction_model, or pick one with /compact-model"
                    .to_string(),
            );
            self.announce_compaction_failure(&error);
            return Err(error);
        }
        let (history, _) = self.load_history().await?;
        let system_text = self.system_prompt("").await?;
        let mut messages = Vec::new();
        let notice = match self
            .compact_now(
                &history,
                &system_text,
                &mut messages,
                self.explicit_fold_policy(),
            )
            .await?
        {
            Some(notice) => notice,
            None => {
                let error = HarnessError::SessionError(
                    "nothing to compact: the transcript holds no more turns than agent.keep_recent_turns and fits the compaction budget"
                        .to_string(),
                );
                self.announce_compaction_failure(&error);
                return Err(error);
            }
        };
        Ok(notice)
    }

    /// Tell the observer a fold the user asked for will not happen, and why.
    fn announce_compaction_failure(&self, error: &HarnessError) {
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::CompactionFinished {
                folded: None,
                error: Some(error.to_string()),
            });
        }
    }

    /// Start an automatic fold and return at once: the turn does not wait for the summariser.
    ///
    /// The fold is a bound on the *next* request, not part of this turn's answer, so the turn runs
    /// on the transcript it already has and the folded transcript is replayed by the next one.
    /// Waiting here is what used to hold the turn: the user's prompt sat idle for the length of a
    /// whole model call before its own model call had started. The transcript that tripped the
    /// threshold is one a previous turn sent successfully, so carrying it once more costs at most
    /// the new user line — the fold's threshold is a policy bar, not the model's window.
    ///
    /// One fold at a time: a turn arriving while a summary is still being written runs unfolded
    /// rather than starting a second fold over the same turns.
    pub(super) fn spawn_fold_if_needed(
        &self,
        messages: &[Message],
        history: &[StoredMessage],
        policy: FoldPolicy,
    ) {
        if self.session.is_none()
            || self.compaction.is_none()
            || self.fold_disabled_this_turn.load(Ordering::SeqCst)
        {
            return;
        }

        let estimate = estimate_tokens(messages);
        if estimate.tokens as usize <= policy.threshold {
            debug!(
                "Transcript estimate {} tokens ({:?}) is within the fold threshold = {}",
                estimate.tokens, estimate.source, policy.threshold
            );
            return;
        }
        if self.fold_in_flight.swap(true, Ordering::SeqCst) {
            debug!("A fold is already in flight; running this turn on the transcript as it stands");
            return;
        }
        debug!(
            "Transcript estimate {} tokens exceeds the fold threshold = {}; folding in the background",
            estimate.tokens, policy.threshold
        );

        // Told before the spawn, so the animation starts while the summary call is in flight rather
        // than after it has already finished.
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::CompactionStarted { forced: false });
        }
        self.spawn_fold_run(history, policy);
    }

    /// Spawn the background fold of `history`, having already taken the in-flight latch and
    /// announced the start.
    fn spawn_fold_run(&self, history: &[StoredMessage], policy: FoldPolicy) {
        let (Some(binding), Some(setup)) = (&self.session, &self.compaction) else {
            return;
        };

        let store = binding.store.clone();
        let session_id = binding.session_id.clone();
        let provider = setup.provider.clone();
        let model = setup.model.clone();
        let history = history.to_vec();
        let target = policy.target;
        let observer = self.observer.clone();
        let completed_fold = self.completed_fold.clone();
        let guard = FoldInFlight(self.fold_in_flight.clone());
        let fold_disabled = self.fold_disabled_this_turn.clone();

        tokio::spawn(async move {
            let _guard = guard;
            match run_fold(
                store.as_ref(),
                &session_id,
                provider.as_ref(),
                &model,
                &history,
                target,
            )
            .await
            {
                Ok(Some(outcome)) => {
                    *completed_fold.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome.clone());
                    let new_tokens = estimate_tokens(&outcome.kept).tokens
                        + estimate_tokens(&[Message::system(outcome.summary.clone())]).tokens;
                    if let Some(observer) = observer {
                        observer(AgentEvent::CompactionFinished {
                            folded: Some(CompactionNotice {
                                session_id: session_id.clone(),
                                turns: outcome.folded_turns,
                                messages: outcome.folded_messages,
                            }),
                            error: None,
                        });
                        observer(AgentEvent::ContextTokensUpdated {
                            tokens_used: new_tokens,
                        });
                    }
                }
                Ok(None) => {
                    fold_disabled.store(true, Ordering::SeqCst);
                    debug!(
                        "Nothing to fold: the transcript holds no more turns than the policy keeps \
                         and fits the compaction budget"
                    );
                    if let Some(observer) = observer {
                        observer(AgentEvent::CompactionFinished {
                            folded: None,
                            error: None,
                        });
                    }
                }
                // Non-fatal by design: the transcript is untouched, and the turn this fold was
                // started for has already run on the full history.
                Err(failure) => {
                    fold_disabled.store(true, Ordering::SeqCst);
                    warn!(
                        "Compaction failed, continuing on the full transcript: {}",
                        failure
                    );
                    if let Some(observer) = observer {
                        observer(AgentEvent::CompactionFinished {
                            folded: None,
                            error: None,
                        });
                    }
                }
            }
            // No `ContextTokensUpdated` here: a turn is in flight and will report the usage of the
            // prompt it actually sent. Announcing the folded size now would describe a transcript
            // the running turn is not using; the next request corrects the gauge for real.
        });
    }

    /// Re-check the fold bar against the transcript the session now holds, and start a fold if it is
    /// over. The second look behind [`Self::spawn_fold_if_needed`], which only ever runs at the top
    /// of a turn.
    ///
    /// Measured on the *stored* transcript, not on the in-memory copy the turn is carrying: a fold
    /// that landed earlier in this turn has already shrunk the store, and estimating the copy the
    /// running turn still holds would start a second fold of rows that are no longer there — and
    /// animate it. What is estimated here is exactly what the next turn will send.
    ///
    /// `watermark` is the newest fold in the transcript when this turn began
    /// ([`last_fold_seq`]). A fold that finished during the turn has rewritten the stored history,
    /// and the numbers a caller can see afterwards describe the transcript it replaced: the turn's
    /// own model calls go on reporting the *unfolded* prompt size, because that is the transcript
    /// the running turn is still carrying. Without this check the turn's last request looks over the
    /// bar again and the session folds a second time — summarising the summary out of a history that
    /// was just bounded.
    pub(super) fn try_rebind_completed_fold(
        &self,
        messages: &mut Vec<Message>,
        turn_user_idx: &mut usize,
        fold_watermark: &mut Option<i64>,
        system_text: &str,
    ) -> bool {
        if let Some(outcome) = self.completed_fold.lock().unwrap_or_else(|e| e.into_inner()).take() {
            debug!("Mid-turn fold outcome arrived; rebinding active turn messages");
            let mut turn_messages = messages.split_off(*turn_user_idx);
            for msg in &mut turn_messages {
                msg.usage = None;
            }
            let user_content = turn_messages.first().and_then(|m| m.content.as_ref());
            let past_kept = match user_content {
                Some(content) => {
                    if let Some(pos) = outcome
                        .kept
                        .iter()
                        .rposition(|m| m.role == Role::User && m.content.as_deref() == Some(content))
                    {
                        &outcome.kept[..pos]
                    } else {
                        &outcome.kept[..]
                    }
                }
                None => &outcome.kept[..],
            };
            let mut new_messages = Vec::with_capacity(past_kept.len() + 2 + turn_messages.len());
            new_messages.push(Message::system(system_text.to_string()));
            new_messages.push(Message::system(outcome.summary.clone()));
            for mut msg in past_kept.iter().cloned() {
                msg.usage = None;
                new_messages.push(msg);
            }
            *turn_user_idx = new_messages.len();
            new_messages.extend(turn_messages);
            *messages = new_messages;
            if outcome.fold_seq.is_some() {
                *fold_watermark = outcome.fold_seq;
            }
            true
        } else {
            false
        }
    }

    pub(super) async fn spawn_fold_from_stored(&self, policy: FoldPolicy, watermark: &mut Option<i64>) {
        if self.session.is_none()
            || self.compaction.is_none()
            || self.fold_disabled_this_turn.load(Ordering::SeqCst)
        {
            return;
        }
        // Nothing worth re-reading for: this crossing is already being folded.
        if self.fold_in_flight.load(Ordering::SeqCst) {
            return;
        }
        let (history, _) = match self.load_history().await {
            Ok(loaded) => loaded,
            Err(error) => {
                // A failed read is not a failed turn: the fold is a bound on the next request, and
                // the next check will find it again.
                warn!("Cannot re-read the session to fold it: {}", error);
                return;
            }
        };
        if last_fold_seq(&history) != *watermark {
            // A fold landed earlier in THIS turn. That fold answered one crossing, not every
            // crossing: a long agentic turn keeps growing after its fold, and pinning the
            // watermark until the next user message would let one unbounded turn pin an unbounded
            // transcript. Re-arm instead: adopt the fold that just landed as the new watermark and
            // measure again, so the next crossing folds on its own merits.
            let rearmed = last_fold_seq(&history);
            debug!(
                "A fold landed during this turn; re-arming the watermark to {:?} for the next crossing",
                rearmed
            );
            *watermark = rearmed;
            let mut messages = Vec::with_capacity(history.len() + 1);
            messages.push(Message::system(
                self.system_prompt("").await.unwrap_or_default(),
            ));
            messages.extend(sendable_messages(&history));
            // The re-armed watermark only counts folds newer than the one just seen: pass the
            // fresh value through a nested call by checking size directly here.
            let estimate = estimate_tokens(&messages);
            if estimate.tokens as usize <= policy.threshold {
                return;
            }
            if self.fold_in_flight.swap(true, Ordering::SeqCst) {
                return;
            }
            if let Some(ref observer) = self.observer {
                observer(AgentEvent::CompactionStarted { forced: false });
            }
            self.spawn_fold_run(&history, policy);
            return;
        }
        // The same shape the next turn sends — system prompt plus the replayed transcript — so this
        // estimate and the one the turn makes at its top measure the same thing.
        let system_text = self.system_prompt("").await.unwrap_or_default();
        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.push(Message::system(system_text));
        messages.extend(sendable_messages(&history));
        self.spawn_fold_if_needed(&messages, &history, policy);
    }

    /// Fold the oldest turns now and swap the sent vector for the bounded one — for a caller that
    /// asked for it and will report the result (`/compact`, summarize-and-new). Automatic folds do
    /// not come through here: see [`Self::spawn_fold_if_needed`].
    ///
    /// A provider failure is the caller's to report: the vector is untouched and the error is
    /// returned.
    async fn compact_now(
        &self,
        history: &[StoredMessage],
        system_text: &str,
        messages: &mut Vec<Message>,
        policy: FoldPolicy,
    ) -> Result<Option<CompactionNotice>, HarnessError> {
        let (Some(binding), Some(setup)) = (&self.session, &self.compaction) else {
            return Ok(None);
        };

        // The observer is told before the summary call is made: the wait is seconds long and a UI
        // that only learns about it afterwards has nothing to show while the user is waiting.
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::CompactionStarted { forced: true });
        }

        let outcome = match run_fold(
            binding.store.as_ref(),
            &binding.session_id,
            setup.provider.as_ref(),
            &setup.model,
            history,
            policy.target,
        )
        .await
        {
            Ok(Some(outcome)) => outcome,
            Ok(None) => {
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::CompactionFinished {
                        folded: None,
                        error: None,
                    });
                }
                debug!(
                    "Nothing to fold: the transcript holds no more turns than the policy keeps \
                     and fits the compaction budget"
                );
                return Ok(None);
            }
            Err(failure) => {
                // The user asked for this fold: a failure is the answer, not a silent "there was
                // nothing to compact". Say what actually went wrong.
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::CompactionFinished {
                        folded: None,
                        error: Some(failure.to_string()),
                    });
                }
                return Err(failure);
            }
        };

        let user_line = messages.pop();
        *messages = outcome.bounded_messages(system_text.to_string());
        messages.extend(user_line);

        let notice = CompactionNotice {
            session_id: binding.session_id.clone(),
            turns: outcome.folded_turns,
            messages: outcome.folded_messages,
        };
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::CompactionFinished {
                folded: Some(notice.clone()),
                error: None,
            });
        }

        // The sent vector just changed size, and the gauge has to hear about it *here*: a fold asked
        // for with `/compact` happens between turns, so nothing else recomputes the figure until the
        // next request reports usage — the readout keeps describing the transcript that was just
        // folded away. The character heuristic, deliberately, not `estimate_tokens`: the kept rows
        // still carry the usage the provider reported for the *pre-fold* prompt, and measuring with
        // it would announce the size the fold just removed. No call has been made against the
        // bounded prompt yet, so this is the honest figure until one has.
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::ContextTokensUpdated {
                tokens_used: heuristic_tokens(messages),
            });
        }
        Ok(Some(notice))
    }
}

/// Fold `history` into a summary and record it: the work every fold does, whether the caller waits
/// for it or not.
///
/// Free rather than a method because the fold a turn does not wait for is spawned as its own task,
/// and that task needs the store, the summarising provider and the transcript — not the agent
/// around them.
///
/// The summary row is what a later resume replays, and the folded rows stay readable
/// (`compacted_at` set) for a future rewind. A store failure is an error, not a warning: running
/// on without the summary would make the next resume replay a transcript no request ever sent.
async fn run_fold(
    store: &dyn SessionStore,
    session_id: &str,
    provider: &dyn ModelProvider,
    model: &str,
    history: &[StoredMessage],
    target: FoldTarget,
) -> Result<Option<CompactionOutcome>, HarnessError> {
    let outcome = match target {
        FoldTarget::Percent(pct) => compact_percent(provider, model, history, pct).await?,
        FoldTarget::Turns(turns) => compact(provider, model, history, Some(turns)).await?,
    };
    let Some(mut outcome) = outcome else {
        return Ok(None);
    };
    let seq = store
        .record_compaction(session_id, &outcome.folded_ids, &outcome.summary)
        .await?;
    outcome.fold_seq = Some(seq);
    Ok(Some(outcome))
}
