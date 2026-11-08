use serde_json::json;

use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Click with a real pointer, either at a ref's on-screen center or at a
/// screen coordinate.
///
/// This is the fallback for an element whose accessibility action does nothing
/// or does not exist. It moves the pointer and takes focus, which the semantic
/// `desktop_click` deliberately does not.
pub fn click_at(
    engine: &DesktopEngine,
    reference: Option<&str>,
    x: Option<i32>,
    y: Option<i32>,
    button: u8,
) -> Response {
    if !matches!(button, 0 | 1 | 2) {
        return Response::err(
            "click-at",
            &crate::error::DesktopError::invalid_args(format!(
                "button {button} is not one of 0 (left), 1 (right) or 2 (middle)"
            )),
        );
    }

    let result = match (reference, x, y) {
        (Some(reference), _, _) => engine.click_element(reference, button),
        (None, Some(x), Some(y)) => engine.click_at(x, y, button),
        _ => {
            return Response::err(
                "click-at",
                &crate::error::DesktopError::invalid_args(
                    "Give either a ref, or both x and y",
                ),
            )
        }
    };

    match result {
        Ok(()) => Response::ok(
            "click-at",
            json!({
                "ref_id": reference,
                "x": x,
                "y": y,
                "button": button,
                "delivery": "delivered_unverified",
                "message": "The pointer click was sent; take a fresh snapshot to confirm the effect",
            }),
        ),
        Err(error) => Response::err("click-at", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_without_a_ref_or_coordinates_is_refused() {
        struct NoInput;
        impl crate::adapter::observation::ObservationOps for NoInput {}
        impl crate::adapter::action::ActionOps for NoInput {}
        impl crate::adapter::input::InputOps for NoInput {}
        impl crate::adapter::system::SystemOps for NoInput {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoInput));
        let response = click_at(&engine, None, None, None, 0);
        assert!(!response.ok);
        assert_eq!(response.error.expect("an error").code, "INVALID_ARGS");
    }

    #[test]
    fn an_unknown_button_is_refused() {
        struct NoInput;
        impl crate::adapter::observation::ObservationOps for NoInput {}
        impl crate::adapter::action::ActionOps for NoInput {}
        impl crate::adapter::input::InputOps for NoInput {}
        impl crate::adapter::system::SystemOps for NoInput {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoInput));
        let response = click_at(&engine, None, Some(1), Some(2), 9);
        assert!(!response.ok);
        assert_eq!(response.error.expect("an error").code, "INVALID_ARGS");
    }
}
