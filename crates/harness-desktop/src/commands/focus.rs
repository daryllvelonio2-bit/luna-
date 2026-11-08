use serde_json::json;

use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Focus an application's window, or report the window that is focused now.
///
/// Focusing is what makes the next OS-level action (a key chord or a paste)
/// land in the intended window. The semantic tools do not need it; the raw
/// input tools do.
pub fn focus(engine: &DesktopEngine, app: Option<&str>) -> Response {
    match app {
        Some(app) => match engine.focus(app) {
            Ok(window) => {
                let verified = window.active;
                Response::ok(
                    "focus",
                    json!({
                        "window": window,
                        "delivery": if verified { "delivered_verified" } else { "delivered_unverified" },
                        "message": if verified {
                            None
                        } else {
                            Some("The window manager did not report this window as active; keys and pastes may go elsewhere")
                        },
                    }),
                )
            }
            Err(error) => Response::err("focus", &error),
        },
        None => match engine.active_window() {
            Ok(window) => Response::ok("focus", json!({ "window": window })),
            Err(error) => Response::err("focus", &error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_on_a_platform_without_windows_is_an_error_envelope() {
        struct NoInput;
        impl crate::adapter::observation::ObservationOps for NoInput {}
        impl crate::adapter::action::ActionOps for NoInput {}
        impl crate::adapter::input::InputOps for NoInput {}
        impl crate::adapter::system::SystemOps for NoInput {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoInput));
        let response = focus(&engine, Some("Firefox"));
        assert!(!response.ok);
        assert_eq!(
            response.error.expect("an error").code,
            "PLATFORM_NOT_SUPPORTED"
        );
    }
}
