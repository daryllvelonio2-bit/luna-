use crate::adapter::ActionRequest;
use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Set the value of the control a ref names.
pub fn set_value(engine: &DesktopEngine, ref_id: &str, value: &str) -> Response {
    super::act::run(
        engine,
        ref_id,
        ActionRequest::SetValue {
            value: value.to_string(),
        },
        "set-value",
    )
}
