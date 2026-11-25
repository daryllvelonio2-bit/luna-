//! The session a tool call belongs to.
//!
//! A tool call carries no session argument, but the work it starts has to be reported to the
//! session that started it: a background job or a delegated child that finishes in another
//! session wakes the wrong conversation and writes the wrong transcript. The turn's session
//! rides in a task-local so a tool can read it without a new argument on every tool signature.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

tokio::task_local! {
    static SESSION: SessionScope;
}

/// What a turn hands to the tools it runs.
#[derive(Clone)]
pub struct SessionScope {
    pub session_id: String,
    /// The queue the parent session receives out-of-band messages on. A delegated child
    /// pushes its completion notice here, so the notice reaches the session that asked for
    /// the work even when the user has since switched to another tab.
    pub steer_queue: Option<Arc<Mutex<VecDeque<String>>>>,
}

/// Run `future` as part of `session_id`.
pub async fn scope<F>(
    session_id: impl Into<String>,
    steer_queue: Option<Arc<Mutex<VecDeque<String>>>>,
    future: F,
) -> F::Output
where
    F: Future,
{
    SESSION
        .scope(
            SessionScope {
                session_id: session_id.into(),
                steer_queue,
            },
            future,
        )
        .await
}

/// The session of the task running now, if it was scoped to one.
pub fn current() -> Option<String> {
    SESSION.try_with(|scope| scope.session_id.clone()).ok()
}

/// The steer queue of the session running now, if it was scoped to one.
pub fn current_steer_queue() -> Option<Arc<Mutex<VecDeque<String>>>> {
    SESSION
        .try_with(|scope| scope.steer_queue.clone())
        .ok()
        .flatten()
}
