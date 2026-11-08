//! One file per desktop command.
//!
//! Each command turns an engine call into the JSON envelope the tools return.
//! Keeping them apart means a command's shape is visible in one place, and the
//! envelope is built the same way everywhere.

mod act;
mod click;
mod click_at;
mod find;
mod focus;
mod get;
mod is;
mod key;
mod list_apps;
mod list_windows;
mod paste;
mod screenshot;
mod set_value;
mod snapshot;
mod status;
mod toggle;
mod type_text;

pub use click::click;
pub use click_at::click_at;
pub use find::{find, FindQuery};
pub use focus::focus;
pub use get::get;
pub use is::is;
pub use key::key;
pub use list_apps::list_apps;
pub use list_windows::list_windows;
pub use paste::paste;
pub use screenshot::screenshot;
pub use set_value::set_value;
pub use snapshot::snapshot;
pub use status::status;
pub use toggle::toggle;
pub use type_text::type_text;
