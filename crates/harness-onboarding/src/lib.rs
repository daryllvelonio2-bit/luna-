//! First-run onboarding for the Luna harness: the typewriter mission briefing, the personality
//! core menu, and the config write that hands control back to the agent loop.
//!
//! #[`flow::run`] owns the three phases and nothing else; every mechanic it needs lives in its own
//! module so it can be tested without a terminal:
//!
//! - [`persona`]: the ten personality cores and their `prompts.md` blocks.
//! - [`screen`]: centering and wrapping geometry. Pure functions, no I/O.
//! - [`style`]: the palette and the `Line`/`Span` model the frames are built from.
//! - [`typewriter`]: the animation driver. Cadence comes from a [`typewriter::Pacer`], so tests run
//!   the real code path with the clock removed.
//! - [`menu`]: the menu frame plus selection parsing/validation.
//! - [`signal`]: cursor restoration on SIGINT/SIGTERM.
//! - [`store`]: persistence of the selected persona into the harness config file.
//! - [`flow`]: the orchestration of phases 1-3.
//!
//! Prompt text is not in this crate. A persona *selects* a section of
//! `crates/harness-core/prompts.md`; the block itself is loaded through
//! [`harness_core::prompts::section`], so the prompt file stays the only home for model-facing
//! text and a persona's voice can be edited without touching a config file or a build.

pub mod compact_step;
pub mod flow;
pub mod memory_step;
pub mod menu;
pub mod persona;
pub mod screen;
pub mod signal;
pub mod store;
pub mod style;
pub mod typewriter;

pub use flow::{needs_onboarding, run, run_interactive, Outcome};
pub use memory_step::Choice as MemoryChoice;
pub use persona::{Persona, CATALOG};
pub use screen::Size;
pub use store::persist;
pub use style::Line;
pub use typewriter::{InstantPacer, Pacer, RealPacer};
