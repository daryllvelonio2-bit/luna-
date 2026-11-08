use crate::engine::DesktopEngine;
use crate::envelope::Response;
use crate::tree::options::TreeOptions;

/// Read a tree and mint refs for the elements in it.
pub fn snapshot(engine: &DesktopEngine, app: &str, options: &TreeOptions) -> Response {
    match engine.observe(app, options) {
        Ok(data) => Response::ok("snapshot", data),
        Err(error) => Response::err("snapshot", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_of_an_unknown_app_is_an_error_envelope() {
        struct NoApps;
        impl crate::adapter::observation::ObservationOps for NoApps {}
        impl crate::adapter::action::ActionOps for NoApps {}
        impl crate::adapter::input::InputOps for NoApps {}
        impl crate::adapter::system::SystemOps for NoApps {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoApps));
        let response = snapshot(&engine, "Calculator", &TreeOptions::default());
        assert!(!response.ok);
        assert_eq!(
            response.error.expect("an error").code,
            "PLATFORM_NOT_SUPPORTED"
        );
    }
}
