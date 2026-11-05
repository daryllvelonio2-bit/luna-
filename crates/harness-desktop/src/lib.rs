//! The platform-agnostic half of a computer-use adapter.
//!
//! The split is the one `vercel-labs/agent-desktop` uses, and the reason it uses
//! it: core owns the vocabulary — nodes, refs, the JSON envelope, the error
//! contract — and never names a platform API, while a platform crate implements
//! [`adapter::DesktopAdapter`] against one operating system.
//!
//! Two consequences matter here. A platform adapter reads a tree and returns it
//! without refs; the core mints the refs, because a ref is a promise about a
//! *snapshot*, not about a widget. And an adapter acts on an element by
//! *identity* rather than by address, because Linux exposes no stable address:
//! the adapter re-reads the tree, finds the one element that still matches, and
//! refuses when the match is not unique.

pub mod adapter;
pub mod commands;
pub mod deadline;
pub mod envelope;
pub mod error;
pub mod refs;
pub mod tree;

mod engine;
mod skill;

pub use engine::DesktopEngine;
pub use skill::{
    skill_description, skill_instructions, skill_name, SKILL_MARKDOWN, SKILL_NAME,
};
