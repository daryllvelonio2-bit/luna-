//! The AT-SPI platform adapter.
//!
//! Linux exposes desktop applications through the accessibility bus: the
//! session bus carries `org.a11y.Bus`, which hands out the address of the
//! accessibility bus, and every application on that bus answers the AT-SPI
//! interfaces.
//!
//! The split from the core follows the one `vercel-labs/agent-desktop` uses.
//! This crate knows about `org.a11y.atspi.Accessible`, `Component`, `Action`
//! and `EditableText`; it knows nothing about refs, snapshots or the JSON
//! envelope, because those belong to the core.

pub mod actions;
pub mod adapter;
pub mod bus;
pub mod element;
pub mod input;
pub mod system;
pub mod tree;

pub use adapter::LinuxDesktopAdapter;
pub use element::AtspiElement;
