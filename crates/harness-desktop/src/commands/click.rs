use crate::adapter::ActionRequest;
use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Activate the element a ref names.
pub fn click(engine: &DesktopEngine, ref_id: &str) -> Response {
    super::act::run(engine, ref_id, ActionRequest::Click, "click")
}
