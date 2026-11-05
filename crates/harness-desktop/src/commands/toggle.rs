use crate::adapter::ActionRequest;
use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Flip the checkable control a ref names, or set it to a given state.
pub fn toggle(engine: &DesktopEngine, ref_id: &str, checked: Option<bool>) -> Response {
    super::act::run(engine, ref_id, ActionRequest::Toggle { checked }, "toggle")
}
