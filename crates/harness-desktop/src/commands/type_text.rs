use serde_json::json;

use crate::adapter::ActionRequest;
use crate::engine::DesktopEngine;
use crate::envelope::Response;

/// Put text into the field a ref names.
///
/// Three modes, because the accessibility path is not always enough:
///
/// * `value` writes through `EditableText.SetTextContents` — semantic, no
///   pointer, no focus change.
/// * `paste` clicks the field with a real pointer, selects its contents and
///   pastes through the clipboard — the path that works in rich web editors
///   (ProseMirror, Lexical, Draft.js) which ignore a property write.
/// * `auto` tries `value`, reads the field back, and falls back to `paste`
///   when the field did not take the text.
pub fn type_text(engine: &DesktopEngine, ref_id: &str, text: &str, mode: &str) -> Response {
    match mode {
        "value" => super::act::run(
            engine,
            ref_id,
            ActionRequest::Type {
                text: text.to_string(),
            },
            "type",
        ),
        "paste" => paste_into(engine, ref_id, text),
        "auto" => auto(engine, ref_id, text),
        other => Response::err(
            "type",
            &crate::error::DesktopError::invalid_args(format!(
                "Unknown mode '{other}'. Use value, paste or auto."
            )),
        ),
    }
}

fn auto(engine: &DesktopEngine, ref_id: &str, text: &str) -> Response {
    if let Ok(outcome) = engine.act(
        ref_id,
        ActionRequest::Type {
            text: text.to_string(),
        },
    ) {
        let read_back = engine.read_value(ref_id).ok().flatten();
        if read_back.as_deref() == Some(text) {
            return Response::ok(
                "type",
                json!({
                    "ref_id": ref_id,
                    "action": "type",
                    "mode": "value",
                    "delivery": outcome.delivery,
                    "steps": outcome.steps,
                    "message": outcome.message,
                }),
            );
        }
    }
    paste_into(engine, ref_id, text)
}

fn paste_into(engine: &DesktopEngine, ref_id: &str, text: &str) -> Response {
    if let Err(error) = engine.click_element(ref_id, 0) {
        return Response::err("type", &error);
    }
    // The field is focused by the click, so Ctrl+A selects its contents and the
    // paste replaces them, which is what "type" promises.
    let _ = engine.keys("ctrl+a");
    match engine.paste(text) {
        Ok(()) => Response::ok(
            "type",
            json!({
                "ref_id": ref_id,
                "action": "paste",
                "mode": "paste",
                "delivery": "delivered_unverified",
                "steps": ["Input.PointerClick", "Input.SelectAll", "Input.ClipboardPaste"],
                "message": "The field was clicked, its contents selected and the text pasted; read the field or take a snapshot to confirm",
            }),
        ),
        Err(error) => Response::err("type", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_mode_is_refused() {
        struct NoInput;
        impl crate::adapter::observation::ObservationOps for NoInput {}
        impl crate::adapter::action::ActionOps for NoInput {}
        impl crate::adapter::input::InputOps for NoInput {}
        impl crate::adapter::system::SystemOps for NoInput {}

        let engine = DesktopEngine::new(std::sync::Arc::new(NoInput));
        let response = type_text(&engine, "@s1:e1", "hi", "telepathy");
        assert!(!response.ok);
        assert_eq!(response.error.expect("an error").code, "INVALID_ARGS");
    }
}
