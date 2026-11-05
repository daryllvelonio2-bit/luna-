//! Compaction folds: the background fold a turn starts without waiting for, the explicit
//! `/compact` fold, and the mid-turn rebind when a background fold lands.

use std::sync::atomic::Ordering;
use tracing::{debug, warn};

use super::{Agent, AgentEvent, CompactionNotice, FoldInFlight, FoldPolicy, FoldTarget};
use crate::error::HarnessError;
use crate::session::{
    compact, compact_percent, estimate_tokens, heuristic_tokens, last_fold_seq, CompactionOutcome,
    SessionStore, StoredMessage,
};
use crate::traits::*;
use crate::types::Message;

/// How many urgent folds one request may run back to back before it is sent anyway.
///
/// A fold normally takes a large fraction of the transcript, so one is enough. A tail of oversized
/// kept turns can need a second, and a fold that only rewrites a summary must not loop; the cap
/// bounds the summariser calls a single model request can trigger.
const MAX_URGENT_FOLDS_PER_REQUEST: usize = 3;

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
                true,
            )
            .await?
        {
            Some((notice, _)) => notice,
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
    ) -> bool {
        if self.session.is_none()
            || self.compaction.is_none()
            || self.fold_disabled_this_turn.load(Ordering::SeqCst)
        {
            return false;
        }

        let estimate = estimate_tokens(messages);
        if estimate.tokens as usize <= policy.threshold {
            debug!(
                "Transcript estimate {} tokens ({:?}) is within the fold threshold = {}",
                estimate.tokens, estimate.source, policy.threshold
            );
            return false;
        }
        if self.fold_in_flight.swap(true, Ordering::SeqCst) {
            debug!("A fold is already in flight; running this turn on the transcript as it stands");
            return false;
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
        true
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
                    *completed_fold.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(outcome.clone());
                    if let Some(observer) = observer {
                        observer(AgentEvent::CompactionFinished {
                            folded: Some(CompactionNotice {
                                session_id: session_id.clone(),
                                turns: outcome.folded_turns,
                                messages: outcome.folded_messages,
                            }),
                            error: None,
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

    /// Adopt a fold that landed while this turn was running: the turn goes on with the transcript
    /// the next request would send, instead of the one the fold just replaced.
    ///
    /// `fold_watermark` — the newest fold in the transcript when this turn began,
    /// [`last_fold_seq`] — moves to this fold's summary, so the bar does not answer a fold it has
    /// already adopted.
    ///
    /// Rebuilt from the store, not spliced out of the copy in hand. An automatic fold inside a long
    /// turn retires rows of *this* turn — an agentic turn is hundreds of rounds and the fold cuts
    /// across them — and the splice this replaced kept every message after the turn's user line,
    /// which is exactly what the fold had removed. The turn then went on sending the unfolded
    /// transcript while the store held the folded one, so the next look at the bar still saw the old
    /// size, folded again, and again, chewing the stored transcript down to a fraction of the
    /// configured target: one real session folded twenty times inside a single turn, and its next
    /// prompt collapsed from 180k tokens to 11k.
    ///
    /// What the store holds is what the next request sends, so that is what the running turn is
    /// handed — with the size reports of the retired requests dropped, so that what this turn
    /// reports from here on describes the transcript it is actually sending.
    pub(super) async fn try_rebind_completed_fold(
        &self,
        messages: &mut Vec<Message>,
        fold_watermark: &mut Option<i64>,
        system_text: &str,
    ) -> bool {
        let outcome = self
            .completed_fold
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let Some(outcome) = outcome else {
            return false;
        };
        debug!("Mid-turn fold outcome arrived; rebuilding the active turn from the folded session");
        if let Some(seq) = outcome.fold_seq {
            *fold_watermark = Some(seq);
        }
        self.rebuild_messages_from_store(messages, system_text)
            .await;
        true
    }

    /// Reload the stored transcript as the vector this turn should send.
    ///
    /// A fold retires the rows of this turn as well as older ones, so the in-memory copy is not a
    /// transcript the store agrees with any more. The usage reports written against the retired
    /// requests are cleared first: they carry sequence numbers newer than the summary and would
    /// make the bar re-measure the transcript the fold just removed.
    async fn rebuild_messages_from_store(&self, messages: &mut Vec<Message>, system_text: &str) {
        let Some(binding) = self.session.as_ref() else {
            return;
        };
        if let Err(error) = binding.store.clear_usage_reports(&binding.session_id).await {
            warn!("Cannot drop the session's retired size reports: {}", error);
        }
        match binding.store.replay(&binding.session_id, usize::MAX).await {
            Ok(replayed) => {
                let mut rebuilt = Vec::with_capacity(replayed.history.len() + 1);
                rebuilt.push(Message::system(system_text.to_string()));
                rebuilt.extend(self.replay_messages(&replayed.history));
                *messages = rebuilt;
            }
            Err(error) => warn!(
                "Cannot re-read session '{}' to adopt the fold; this turn runs on the transcript it holds: {}",
                binding.session_id, error
            ),
        }
    }

    /// Re-check the fold bar against the transcript the session now holds, and start a fold if it is
    /// over. The second look behind [`Self::spawn_fold_if_needed`], which only ever runs at the top
    /// of a turn.
    ///
    /// Measured on the *stored* transcript, not on the in-memory copy the turn is carrying: this is
    /// the transcript a fold will act on, so a bar measured against any other one folds a different
    /// session than the one it sized — and the size it saw decides how much the fold takes. What is
    /// estimated here is exactly what the next turn will send.
    pub(super) async fn spawn_fold_from_stored(
        &self,
        policy: FoldPolicy,
        watermark: &mut Option<i64>,
    ) {
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
            // A failed prompt is not silently an empty one: the estimate below would understate
            // the request and skip a fold that should fire, so the failure is reported.
            let system_text = match self.system_prompt("").await {
                Ok(text) => text,
                Err(error) => {
                    warn!("system prompt unavailable while re-arming a fold; estimating without it: {error}");
                    String::new()
                }
            };
            messages.push(Message::system(system_text));
            messages.extend(self.replay_messages(&history));
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
        let system_text = match self.system_prompt("").await {
            Ok(text) => text,
            Err(error) => {
                warn!("system prompt unavailable while checking the fold bar; estimating without it: {error}");
                String::new()
            }
        };
        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.push(Message::system(system_text));
        messages.extend(self.replay_messages(&history));
        self.spawn_fold_if_needed(&messages, &history, policy);
    }

    /// Fold before the next model call when the in-memory transcript is past the urgent ceiling.
    ///
    /// A long tool round can add more to the request than the replay budget left room for. The
    /// ordinary mid-turn check starts a background fold and lets the round run on the transcript it
    /// holds — fine at the policy bar, fatal at the window. Here the turn waits, then rebuilds from
    /// the store exactly as it would for a fold that landed while it was thinking.
    ///
    /// One fold does not always clear the ceiling: a kept tail of oversized turns can still be
    /// over it. Up to [`MAX_URGENT_FOLDS_PER_REQUEST`] folds run in a row, stopping early when the
    /// transcript fits or a fold no longer makes it smaller.
    ///
    /// Returns `true` when at least one fold ran and `messages` was replaced.
    pub(super) async fn fold_now_from_stored(
        &self,
        messages: &mut Vec<Message>,
        system_text: &str,
        policy: FoldPolicy,
        watermark: &mut Option<i64>,
    ) -> bool {
        let Some(ceiling) = policy.ceiling else {
            return false;
        };
        if self.session.is_none()
            || self.compaction.is_none()
            || self.fold_disabled_this_turn.load(Ordering::SeqCst)
        {
            return false;
        }
        let (Some(binding), Some(setup)) = (&self.session, &self.compaction) else {
            return false;
        };
        // One fold at a time, background or preflight: a second summary over the same tail would
        // race the first one's summary row and could fold it away.
        if self.fold_in_flight.swap(true, Ordering::SeqCst) {
            return false;
        }
        let _guard = FoldInFlight(self.fold_in_flight.clone());
        let mut folded_any = false;
        for _ in 0..MAX_URGENT_FOLDS_PER_REQUEST {
            let before = estimate_tokens(messages).tokens as usize;
            if before <= ceiling {
                break;
            }
            // The fold acts on the store, not on the bounded copy in hand: the whole reason to
            // fold mid-turn is that the live transcript is too large, and the replay budget may
            // already be hiding rows the store still holds.
            let history = match binding.store.replay(&binding.session_id, usize::MAX).await {
                Ok(replayed) => replayed.history,
                Err(error) => {
                    warn!("Cannot re-read the session to fold it mid-turn: {}", error);
                    break;
                }
            };
            debug!(
                "Transcript estimate {} tokens is past the urgent ceiling {}; folding before the next request",
                before, ceiling
            );
            if let Some(ref observer) = self.observer {
                observer(AgentEvent::CompactionStarted { forced: false });
            }
            match run_fold(
                binding.store.as_ref(),
                &binding.session_id,
                setup.provider.as_ref(),
                &setup.model,
                &history,
                policy.target,
            )
            .await
            {
                Ok(Some(outcome)) => {
                    if let Some(seq) = outcome.fold_seq {
                        *watermark = Some(seq);
                    }
                    self.rebuild_messages_from_store(messages, system_text)
                        .await;
                    let notice = CompactionNotice {
                        session_id: binding.session_id.clone(),
                        turns: outcome.folded_turns,
                        messages: outcome.folded_messages,
                    };
                    folded_any = true;
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::CompactionFinished {
                            folded: Some(notice),
                            error: None,
                        });
                        observer(AgentEvent::ContextTokensUpdated {
                            tokens_used: heuristic_tokens(messages),
                            reset: true,
                        });
                    }
                    // A fold that did not make the request smaller will not make the next one
                    // smaller either; stop rather than fold the same tail repeatedly.
                    if estimate_tokens(messages).tokens as usize >= before {
                        break;
                    }
                }
                Ok(None) => {
                    self.fold_disabled_this_turn.store(true, Ordering::SeqCst);
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::CompactionFinished {
                            folded: None,
                            error: None,
                        });
                    }
                    break;
                }
                Err(error) => {
                    // A summariser outage must not become a failed prompt: keep the turn alive on
                    // the transcript it holds and stop asking this turn for another fold.
                    self.fold_disabled_this_turn.store(true, Ordering::SeqCst);
                    warn!(
                        "Mid-turn preflight fold failed; running on the full transcript: {}",
                        error
                    );
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::CompactionFinished {
                            folded: None,
                            error: Some(error.to_string()),
                        });
                    }
                    break;
                }
            }
        }
        folded_any
    }

    /// Fold before the first model call when the transcript is already past the hard ceiling.
    ///
    /// The ordinary fold is background work: it bounds the *next* request, and the turn that
    /// crossed the bar runs on a transcript a previous turn already sent. That reasoning stops
    /// holding near the model's window — the request the turn is about to make is the one the
    /// provider will refuse. At that point the turn waits for the summary and continues on the
    /// bounded vector [`Self::compact_now`] hands back.
    ///
    /// Returns `true` when a fold ran and `messages` was rebuilt. A failed or empty fold leaves
    /// the vector untouched, disables further automatic folds for this turn, and lets the turn
    /// proceed: a summariser outage must not become a failed prompt.
    pub(super) async fn preflight_fold_if_needed(
        &self,
        messages: &mut Vec<Message>,
        history: &[StoredMessage],
        system_text: &str,
        policy: FoldPolicy,
        watermark: &mut Option<i64>,
    ) -> bool {
        let Some(ceiling) = policy.ceiling else {
            return false;
        };
        if self.session.is_none()
            || self.compaction.is_none()
            || self.fold_disabled_this_turn.load(Ordering::SeqCst)
        {
            return false;
        }
        let estimate = estimate_tokens(messages);
        if estimate.tokens as usize <= ceiling {
            return false;
        }
        if self.fold_in_flight.swap(true, Ordering::SeqCst) {
            return false;
        }
        let _guard = FoldInFlight(self.fold_in_flight.clone());
        debug!(
            "Transcript estimate {} tokens is past the urgent ceiling {}; folding before the request",
            estimate.tokens, ceiling
        );
        match self
            .compact_now(history, system_text, messages, policy, false)
            .await
        {
            Ok(Some((_notice, fold_seq))) => {
                if let Some(seq) = fold_seq {
                    *watermark = Some(seq);
                }
                true
            }
            Ok(None) => {
                // Nothing the fold could take; the next turn must not keep asking it to try.
                self.fold_disabled_this_turn.store(true, Ordering::SeqCst);
                false
            }
            Err(error) => {
                // A fold is a bound, not the user's answer. Report it and keep the turn alive on
                // the transcript it holds, the same way the background path does.
                self.fold_disabled_this_turn.store(true, Ordering::SeqCst);
                warn!(
                    "Preflight fold failed; running this turn on the full transcript: {}",
                    error
                );
                false
            }
        }
    }

    /// Fold the oldest turns now and swap the sent vector for the bounded one — for a caller that
    /// asked for it and will report the result (`/compact`, summarize-and-new) or for the urgent
    /// preflight above. Automatic folds otherwise do not come through here: see
    /// [`Self::spawn_fold_if_needed`].
    ///
    /// A provider failure is the caller's to report: the vector is untouched and the error is
    /// returned. The notice carries the fold's sequence number so a running turn can move its
    /// watermark to the fold it just adopted.
    async fn compact_now(
        &self,
        history: &[StoredMessage],
        system_text: &str,
        messages: &mut Vec<Message>,
        policy: FoldPolicy,
        forced: bool,
    ) -> Result<Option<(CompactionNotice, Option<i64>)>, HarnessError> {
        let (Some(binding), Some(setup)) = (&self.session, &self.compaction) else {
            return Ok(None);
        };

        // The observer is told before the summary call is made: the wait is seconds long and a UI
        // that only learns about it afterwards has nothing to show while the user is waiting.
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::CompactionStarted { forced });
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
        let fold_seq = outcome.fold_seq;
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
                reset: true,
            });
        }
        Ok(Some((notice, fold_seq)))
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
