use serde_json::json;

use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// The applications the desktop can see.
pub fn list_apps(engine: &DesktopEngine) -> Response {
    match engine.adapter().list_apps(engine.deadline()) {
        Ok(apps) => Response::ok("list_apps", json!({ "apps": apps, "count": apps.len() })),
        Err(error) => Response::err("list_apps", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_apps_are_listed_with_a_count() {
        struct OneApp;
        impl crate::adapter::observation::ObservationOps for OneApp {
            fn list_apps(
                &self,
                _deadline: crate::deadline::Deadline,
            ) -> Result<Vec<crate::adapter::types::AppInfo>, crate::error::DesktopError> {
                Ok(vec![crate::adapter::types::AppInfo {
                    name: "Calculator".to_string(),
                    pid: Some(42),
                    windows: 1,
                    bundle_id: None,
                }])
            }
        }
        impl crate::adapter::action::ActionOps for OneApp {}
        impl crate::adapter::system::SystemOps for OneApp {}

        let engine = DesktopEngine::new(std::sync::Arc::new(OneApp));
        let response = list_apps(&engine);
        assert!(response.ok);
        let data = response.data.expect("data");
        assert_eq!(data["count"], 1);
        assert_eq!(data["apps"][0]["name"], "Calculator");
    }
}
