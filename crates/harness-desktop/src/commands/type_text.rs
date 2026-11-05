use crate::adapter::ActionRequest;
use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Replace the contents of the text field a ref names.
pub fn type_text(engine: &DesktopEngine, ref_id: &str, text: &str) -> Response {
    super::act::run(
        engine,
        ref_id,
        ActionRequest::Type {
            text: text.to_string(),
        },
        "type",
    )
}
