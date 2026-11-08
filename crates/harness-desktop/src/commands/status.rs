use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// What this desktop can do right now.
pub fn status(engine: &DesktopEngine) -> Response {
    match engine.status() {
        Ok(status) => Response::ok(
            "status",
            serde_json::to_value(status).unwrap_or(serde_json::Value::Null),
        ),
        Err(error) => Response::err("status", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_that_cannot_be_read_is_an_error_envelope() {
        struct NoStatus;
        impl crate::adapter::observation::ObservationOps for NoStatus {}
        impl crate::adapter::action::ActionOps for NoStatus {}
        impl crate::adapter::input::InputOps for NoStatus {}
        impl crate::adapter::system::SystemOps for NoStatus {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoStatus));
        let response = status(&engine);
        assert!(!response.ok);
        assert_eq!(
            response.error.expect("an error").code,
            "PLATFORM_NOT_SUPPORTED"
        );
    }
}
