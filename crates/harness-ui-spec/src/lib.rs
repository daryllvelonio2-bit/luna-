//! Versioned JSON description of a screen that an agent can generate at runtime and a native
//! renderer can draw without recompiling the app.
//!
//! The spec deliberately contains no executable content. Interactive nodes reference actions by
//! name, and [`Screen::capabilities`] is the allowlist the renderer must enforce before dispatch.

mod error;
mod spec;
mod validate;

pub use error::{SpecError, SpecLoadError};
pub use spec::{Column, Node, NodeKind, Screen, SelectOption, TextRole, SCHEMA_VERSION};
