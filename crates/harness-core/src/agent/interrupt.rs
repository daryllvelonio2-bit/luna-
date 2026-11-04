//! Taking a message while the model is still speaking.
//!
//! A provider fixes the model's context when the request is sent; there is no protocol for adding
//! a line mid-stream. The only way to deliver one *now* is to stop the request and ask again with
//! the message appended. This handle does exactly that much and no more: it stops the model call,
//! keeps everything produced before the cut, and lets [`super::Agent::run_turn`] continue the same
//! turn with the new message.
//!
//! It is deliberately separate from the turn's [`tokio_util::sync::CancellationToken`]:
//!
//! - a **hard** cancel (Ctrl+C, Esc, `/interrupt`) still stops the whole turn, tools included;
//! - a **soft** interrupt stops one model call and nothing else. A tool that is already running is
//!   left to finish; the message waits for the next model call instead.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

/// How a submitted message met the running turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptOutcome {
    /// A model call was streaming; it was stopped so the message can go out now.
    Streaming,
    /// The turn is running but no model call is in flight (a tool batch is). The message waits for
    /// the next model call; the tool finishes undisturbed.
    Queued,
    /// No turn is running. The caller should start one instead.
    Idle,
}

/// The soft-interrupt channel shared by the agent and whoever is typing at it.
///
/// Pending messages are kept outside the turn's own state so that a message can be handed over
/// without taking the agent's lock — taking it would block until the running turn released it,
/// which is the delay this exists to remove.
#[derive(Debug, Default)]
pub struct InterruptHandle {
    pending: Mutex<VecDeque<String>>,
    /// The token of the model call currently streaming, if any. Present only between
    /// [`InterruptHandle::begin_call`] and the end of that call.
    call: Mutex<Option<CancellationToken>>,
    turn_active: AtomicBool,
}

impl InterruptHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hand over a message, stopping the model call that is streaming if there is one.
    ///
    /// The text is queued before the call is cancelled, so the turn loop cannot wake up, drain an
    /// empty queue and go back to waiting.
    pub fn interrupt(&self, text: &str) -> InterruptOutcome {
        let trimmed = text.trim();
        if trimmed.is_empty() || !self.turn_active.load(Ordering::SeqCst) {
            return InterruptOutcome::Idle;
        }
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(trimmed.to_string());
        let token = self.call.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match token {
            Some(token) => {
                token.cancel();
                InterruptOutcome::Streaming
            }
            None => InterruptOutcome::Queued,
        }
    }

    /// Messages deposited by [`InterruptHandle::interrupt`] that the turn has not taken yet.
    pub fn drain(&self) -> Vec<String> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }

    /// Take whatever arrived just as the turn was ending.
    ///
    /// The turn loop drains before it returns, but a message can land in the sliver between that
    /// drain and the guard below clearing the active flag. The caller owns the queue then: it can
    /// start the next turn with the text rather than losing it or folding it into the turn that
    /// already answered.
    pub fn take_leftover(&self) -> Vec<String> {
        self.drain()
    }

    pub fn turn_active(&self) -> bool {
        self.turn_active.load(Ordering::SeqCst)
    }

    /// Mark a turn as running until the returned guard is dropped.
    pub(super) fn begin_turn(&self) -> TurnGuard<'_> {
        // Anything left from an earlier turn belongs to that turn's caller, not this one.
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *self.call.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.turn_active.store(true, Ordering::SeqCst);
        TurnGuard { handle: self }
    }

    /// Publish the token of the model call that is about to stream, until the guard is dropped.
    ///
    /// A message arriving between calls finds no token and is simply queued, which is how a tool
    /// batch is allowed to finish.
    pub(super) fn begin_call(&self) -> CallGuard<'_> {
        let token = CancellationToken::new();
        *self.call.lock().unwrap_or_else(|e| e.into_inner()) = Some(token.clone());
        CallGuard {
            handle: self,
            token,
        }
    }
}

/// Keeps [`InterruptHandle::turn_active`] honest however the turn ends, including a panic.
pub(super) struct TurnGuard<'a> {
    handle: &'a InterruptHandle,
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        *self.handle.call.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.handle.turn_active.store(false, Ordering::SeqCst);
    }
}

/// Holds the token of one model call. Dropping it takes the call out of the interrupt path.
pub(super) struct CallGuard<'a> {
    handle: &'a InterruptHandle,
    token: CancellationToken,
}

impl CallGuard<'_> {
    pub(super) fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        *self.handle.call.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
