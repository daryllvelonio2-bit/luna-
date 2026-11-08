use serde_json::json;

use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Send one or more key chords to the focused window.
///
/// A chord is `+`-joined (`ctrl+v`); chords are separated by spaces
/// (`ctrl+a ctrl+v`). The answer is unverified on purpose: a key event that
/// was delivered is not proof that the application acted on it.
pub fn key(engine: &DesktopEngine, keys: &str) -> Response {
    match engine.keys(keys) {
        Ok(()) => Response::ok(
            "key",
            json!({
                "keys": keys,
                "delivery": "delivered_unverified",
                "message": "The keys were sent to the focused window; read the screen again to confirm the effect",
            }),
        ),
        Err(error) => Response::err("key", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_on_a_platform_without_input_is_an_error_envelope() {
        struct NoInput;
        impl crate::adapter::observation::ObservationOps for NoInput {}
        impl crate::adapter::action::ActionOps for NoInput {}
        impl crate::adapter::input::InputOps for NoInput {}
        impl crate::adapter::system::SystemOps for NoInput {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoInput));
        let response = key(&engine, "ctrl+v");
        assert!(!response.ok);
        assert_eq!(
            response.error.expect("an error").code,
            "PLATFORM_NOT_SUPPORTED"
        );
    }
}
