use serde_json::json;

use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Read one property of the element a ref names.
pub fn get(engine: &DesktopEngine, ref_id: &str, property: &str) -> Response {
    if !matches!(property, "value" | "text" | "state" | "actions") {
        return Response::err(
            "get",
            &crate::error::DesktopError::invalid_args(format!(
                "Unknown property '{property}'. Use value, state or actions."
            )),
        );
    }
    let entry = match engine.resolve(ref_id) {
        Ok(entry) => entry,
        Err(error) => return Response::err("get", &error),
    };
    let deadline = engine.deadline();
    let result = match property {
        "value" | "text" => engine
            .adapter()
            .read_value(&entry.app, &entry.identity, deadline)
            .map(|value| json!({ "value": value })),
        "state" => engine
            .adapter()
            .read_state(&entry.app, &entry.identity, deadline)
            .map(|state| json!({ "state": state })),
        "actions" => engine
            .adapter()
            .read_actions(&entry.app, &entry.identity, deadline)
            .map(|actions| json!({ "available_actions": actions })),
        _ => unreachable!("the property was checked above"),
    };
    match result {
        Ok(data) => Response::ok(
            "get",
            json!({
                "ref_id": entry.ref_id,
                "app": entry.app,
                "property": property,
                "data": data,
            }),
        ),
        Err(error) => Response::err("get", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_property_is_refused() {
        struct NoReads;
        impl crate::adapter::observation::ObservationOps for NoReads {}
        impl crate::adapter::action::ActionOps for NoReads {}
        impl crate::adapter::system::SystemOps for NoReads {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoReads));
        let response = get(&engine, "@s1:e1", "colour");
        assert!(!response.ok);
        assert_eq!(response.error.expect("an error").code, "INVALID_ARGS");
    }
}
