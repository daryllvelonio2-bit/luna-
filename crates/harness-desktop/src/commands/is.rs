use serde_json::json;

use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Ask whether the element a ref names has a state.
pub fn is(engine: &DesktopEngine, ref_id: &str, predicate: &str) -> Response {
    if !matches!(predicate, "enabled" | "focused" | "selected" | "checked") {
        return Response::err(
            "is",
            &crate::error::DesktopError::invalid_args(format!(
                "Unknown predicate '{predicate}'. Use enabled, focused, selected or checked."
            )),
        );
    }
    let entry = match engine.resolve(ref_id) {
        Ok(entry) => entry,
        Err(error) => return Response::err("is", &error),
    };
    match engine
        .adapter()
        .read_state(&entry.app, &entry.identity, engine.deadline())
    {
        Ok(Some(state)) => {
            let matches = match predicate {
                "enabled" => state.is_enabled(),
                "focused" => state.is_focused(),
                "selected" => state.is_selected(),
                "checked" => state.is_checked(),
                other => {
                    return Response::err(
                        "is",
                        &crate::error::DesktopError::invalid_args(format!(
                            "Unknown predicate '{other}'. Use enabled, focused, selected or checked."
                        )),
                    );
                }            };
            Response::ok(
                "is",
                json!({
                    "ref_id": entry.ref_id,
                    "predicate": predicate,
                    "matches": matches,
                    "states": state.states,
                }),
            )
        }
        Ok(None) => Response::ok(
            "is",
            json!({
                "ref_id": entry.ref_id,
                "predicate": predicate,
                "matches": false,
                "states": [],
            }),
        ),
        Err(error) => Response::err("is", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_predicate_is_refused() {
        struct NoReads;
        impl crate::adapter::observation::ObservationOps for NoReads {}
        impl crate::adapter::action::ActionOps for NoReads {}
        impl crate::adapter::input::InputOps for NoReads {}
        impl crate::adapter::system::SystemOps for NoReads {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoReads));
        let response = is(&engine, "@s1:e1", "purple");
        assert!(!response.ok);
        assert_eq!(response.error.expect("an error").code, "INVALID_ARGS");
    }
}
