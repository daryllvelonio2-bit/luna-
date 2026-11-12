//! Runtime between generated screens and the capabilities an Android build actually exposes.
//!
//! A screen may only dispatch an action that it declares in [`Screen::capabilities`] *and* that the
//! host registered in [`ActionRegistry`]. Anything else is rejected before a handler runs, so a
//! generated spec can never widen its own authority.
//!
//! [`LunaEngine`] is the facade the UI talks to: it owns a headless `harness-core::Agent`, exposes
//! generated screens, and forwards turn activity as [`EngineEvent`]s.

mod action;
mod engine;
mod error;
mod event;
mod prompt;
mod runtime;
mod screens;
mod tool;

pub use action::{
    ActionCall, ActionContext, ActionError, ActionHandler, ActionId, ActionRegistry, StatePatch,
};
pub use engine::{LunaEngine, MobileConfig};
pub use error::MobileError;
pub use event::EngineEvent;
pub use harness_core::AgentEvent;
pub use runtime::{RuntimeError, ScreenRuntime};
pub use screens::UiSpecStore;
