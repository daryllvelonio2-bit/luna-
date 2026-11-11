//! First-run onboarding for the Luna harness: the typewriter mission briefing, the settings
//! screens, and the config write that hands control back to the agent loop.
//!
//! [`flow::run`] owns the sequence and nothing else; every mechanic it needs lives in its own
//! module so it can be tested without a terminal:
//!
//! - [`screen`]: centering and wrapping geometry. Pure functions, no I/O.
//! - [`style`]: the palette and the `Line`/`Span` model the frames are built from.
//! - [`typewriter`]: the animation driver. Cadence comes from a [`typewriter::Pacer`], so tests run
//!   the real code path with the clock removed.
//! - [`menu`]: the shared panel and numbered-list primitives.
//! - [`signal`]: cursor restoration on SIGINT/SIGTERM.
//! - [`store`]: persistence of the selected settings into the harness config file.
//! - [`flow`]: the orchestration of the briefing, settings, and confirmation.
//!
//! There is no persona step. Prompt text is not in this crate: bare `luna` runs the built-in
//! `default` profile from `crates/harness-core/profiles.md`, and custom profiles are created later
//! with `/profile create`.

pub mod compact_step;
pub mod flow;
pub mod gateway_step;
pub mod memory_step;
pub mod menu;
pub mod notes_step;
pub mod screen;
pub mod signal;
pub mod store;
pub mod style;
pub mod typewriter;
pub mod web_step;

pub use flow::{
    needs_onboarding, run, run_compact, run_compact_interactive, run_gateway,
    run_gateway_interactive, run_interactive, run_memory, run_memory_interactive, run_notes,
    run_notes_interactive, run_web, run_web_interactive, Outcome,
};
pub use gateway_step::Choice as GatewayChoice;
pub use memory_step::Choice as MemoryChoice;
pub use screen::Size;
pub use store::{
    persist, persist_auto_compact, persist_gateway, persist_memory, persist_notes, persist_web,
    AutoCompactSelection, GatewaySelection, WebSelection,
};
pub use style::Line;
pub use typewriter::{InstantPacer, Pacer, RealPacer};
