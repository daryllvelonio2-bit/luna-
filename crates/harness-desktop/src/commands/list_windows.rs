use serde_json::json;

use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// The windows the desktop can see.
pub fn list_windows(engine: &DesktopEngine, app: Option<&str>) -> Response {
    match engine.adapter().list_windows(app, engine.deadline()) {
        Ok(windows) => Response::ok(
            "list_windows",
            json!({ "windows": windows, "count": windows.len() }),
        ),
        Err(error) => Response::err("list_windows", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_platform_without_windows_refuses() {
        struct NoWindows;
        impl crate::adapter::observation::ObservationOps for NoWindows {}
        impl crate::adapter::action::ActionOps for NoWindows {}
        impl crate::adapter::input::InputOps for NoWindows {}
        impl crate::adapter::system::SystemOps for NoWindows {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoWindows));
        let response = list_windows(&engine, None);
        assert!(!response.ok);
        assert_eq!(
            response.error.expect("an error").code,
            "PLATFORM_NOT_SUPPORTED"
        );
    }
}
