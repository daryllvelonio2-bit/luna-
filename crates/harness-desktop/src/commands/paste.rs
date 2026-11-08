use serde_json::json;

use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Put text on the clipboard and paste it into the focused window.
///
/// This is the path that works in rich web editors which ignore
/// `EditableText.SetTextContents`: a paste is a real editing event, not a
/// property write.
pub fn paste(engine: &DesktopEngine, text: &str) -> Response {
    match engine.paste(text) {
        Ok(()) => Response::ok(
            "paste",
            json!({
                "chars": text.chars().count(),
                "delivery": "delivered_unverified",
                "message": "The text is on the clipboard and Ctrl+V was sent; read the field or take a snapshot to confirm it landed",
            }),
        ),
        Err(error) => Response::err("paste", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_on_a_platform_without_a_clipboard_is_an_error_envelope() {
        struct NoInput;
        impl crate::adapter::observation::ObservationOps for NoInput {}
        impl crate::adapter::action::ActionOps for NoInput {}
        impl crate::adapter::input::InputOps for NoInput {}
        impl crate::adapter::system::SystemOps for NoInput {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoInput));
        let response = paste(&engine, "hello");
        assert!(!response.ok);
        assert_eq!(
            response.error.expect("an error").code,
            "PLATFORM_NOT_SUPPORTED"
        );
    }
}
