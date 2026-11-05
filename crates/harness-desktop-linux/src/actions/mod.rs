//! Acting on an element: the AT-SPI `Action` and `EditableText` interfaces.

pub mod do_action;
pub mod editable_text;

pub use do_action::{action_index, do_action};
pub use editable_text::{read_text, set_text_contents};
