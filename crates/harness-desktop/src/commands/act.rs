use serde_json::json;

use crate::adapter::{ActionOutcome, ActionRequest};
use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Run an action and answer with the envelope.
///
/// The delivery disposition travels with the answer on purpose: a caller that
/// only sees "ok" cannot tell whether the click landed and should verify, or
/// never reached the application and is safe to repeat.
pub(crate) fn run(
    engine: &DesktopEngine,
    ref_id: &str,
    request: ActionRequest,
    command: &str,
) -> Response {
    match engine.act(ref_id, request.clone()) {
        Ok(outcome) => Response::ok(command, outcome_json(ref_id, &request, &outcome)),
        Err(error) => Response::err(command, &error),
    }
}

fn outcome_json(
    ref_id: &str,
    request: &ActionRequest,
    outcome: &ActionOutcome,
) -> serde_json::Value {
    json!({
        "ref_id": ref_id,
        "action": request.name(),
        "delivery": outcome.delivery,
        "steps": outcome.steps,
        "message": outcome.message,
    })
}
