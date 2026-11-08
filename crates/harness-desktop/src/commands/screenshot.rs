use serde_json::json;

use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Capture the screen, or one application's window, to a PNG file.
///
/// The answer carries the absolute path and the image's pixel size. The
/// harness's tool results are text, so a screenshot is a file for the agent to
/// read back with OCR or for the user to open; it is not an inline image.
pub fn screenshot(engine: &DesktopEngine, app: Option<&str>, path: &str) -> Response {
    if path.trim().is_empty() {
        return Response::err(
            "screenshot",
            &crate::error::DesktopError::invalid_args("A screenshot path is required"),
        );
    }
    match engine.screenshot(app, path) {
        Ok(info) => Response::ok("screenshot", json!(info)),
        Err(error) => Response::err("screenshot", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_path_is_refused_before_anything_is_captured() {
        struct NoCapture;
        impl crate::adapter::observation::ObservationOps for NoCapture {}
        impl crate::adapter::action::ActionOps for NoCapture {}
        impl crate::adapter::input::InputOps for NoCapture {}
        impl crate::adapter::system::SystemOps for NoCapture {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoCapture));
        let response = screenshot(&engine, None, "  ");
        assert!(!response.ok);
        assert_eq!(response.error.expect("an error").code, "INVALID_ARGS");
    }
}
