//! OS-level input and capture, for what the accessibility tree cannot reach.
//!
//! The accessibility path is the right first tool: it is semantic, it does not
//! move the pointer, and it names what it acts on. It also has two blind spots
//! that no amount of AT-SPI work closes:
//!
//! * A rich web editor (ProseMirror, Draft.js, Lexical) can expose an `entry`
//!   whose `EditableText` interface ignores `SetTextContents`, so the field
//!   reads back as an object-replacement character and stays empty.
//! * Some elements expose no usable action at all, so the only way to reach
//!   them is a real pointer at their on-screen rectangle.
//!
//! This trait is the fallback: window focus, real pointer clicks, key chords,
//! clipboard paste and screen capture. It is deliberately separate from
//! [`super::action::ActionOps`] because none of it is addressed by a ref.

use serde::{Deserialize, Serialize};

use crate::deadline::Deadline;
use crate::error::DesktopError;
use crate::tree::identity::ElementIdentity;
use crate::tree::rect::Rect;

/// A window as the window manager knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRef {
    /// The window manager's opaque id.
    pub id: String,
    /// The application class, or the closest name the window manager exposes.
    pub app: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
    /// Whether this window holds keyboard focus right now.
    #[serde(default)]
    pub active: bool,
}

/// What a capture wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotInfo {
    /// The absolute path of the image.
    pub path: String,
    pub width: u32,
    pub height: u32,
    /// "screen", or the window id that was captured.
    pub target: String,
}

/// The OS-level actions that bypass the accessibility tree.
///
/// Every method has a default that refuses, so a platform implements only what
/// it has and a missing capability is an honest `PLATFORM_NOT_SUPPORTED`
/// rather than a silent no-op.
pub trait InputOps: Send + Sync {
    /// Focus the window of `app` and report the window that is now active.
    fn focus(&self, _app: &str, _deadline: Deadline) -> Result<WindowRef, DesktopError> {
        Err(DesktopError::not_supported("focus"))
    }

    /// The window that currently has keyboard focus.
    fn active_window(&self, _deadline: Deadline) -> Result<Option<WindowRef>, DesktopError> {
        Err(DesktopError::not_supported("active_window"))
    }

    /// Send one or more key chords to the focused window.
    ///
    /// A chord is `+`-joined (`ctrl+v`); chords are separated by spaces
    /// (`ctrl+a ctrl+v`).
    fn keys(&self, _keys: &str, _deadline: Deadline) -> Result<(), DesktopError> {
        Err(DesktopError::not_supported("keys"))
    }

    /// Put `text` on the clipboard and paste it into the focused window.
    fn paste(&self, _text: &str, _deadline: Deadline) -> Result<(), DesktopError> {
        Err(DesktopError::not_supported("paste"))
    }

    /// Click the on-screen center of the element `identity` names.
    ///
    /// Unlike a semantic click, this moves the pointer and takes focus.
    fn click_element(
        &self,
        _app: &str,
        _identity: &ElementIdentity,
        _button: u8,
        _deadline: Deadline,
    ) -> Result<(), DesktopError> {
        Err(DesktopError::not_supported("click_element"))
    }

    /// Click a screen coordinate with a real pointer.
    ///
    /// `button` is 0 for left, 1 for right and 2 for middle.
    fn click_at(
        &self,
        _x: i32,
        _y: i32,
        _button: u8,
        _deadline: Deadline,
    ) -> Result<(), DesktopError> {
        Err(DesktopError::not_supported("click_at"))
    }

    /// Capture the screen, or one application's window, to a PNG file.
    fn screenshot(
        &self,
        _app: Option<&str>,
        _path: &str,
        _deadline: Deadline,
    ) -> Result<ScreenshotInfo, DesktopError> {
        Err(DesktopError::not_supported("screenshot"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    struct Noop;

    impl InputOps for Noop {}

    #[test]
    fn an_unimplemented_capability_refuses_instead_of_doing_nothing() {
        let error = Noop.focus("Firefox", Deadline::none()).expect_err("no focus");
        assert_eq!(error.code, ErrorCode::PlatformNotSupported);
        assert!(error.suggestion.is_some());
    }

    #[test]
    fn every_capability_refuses_on_a_platform_that_has_none() {
        assert!(Noop.keys("ctrl+v", Deadline::none()).is_err());
        assert!(Noop.paste("hi", Deadline::none()).is_err());
        assert!(Noop.click_at(1, 2, 0, Deadline::none()).is_err());
        assert!(Noop.screenshot(None, "/tmp/x.png", Deadline::none()).is_err());
    }
}
