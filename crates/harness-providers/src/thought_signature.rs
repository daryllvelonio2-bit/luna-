//! Google's thought signatures: the opaque per-call token a Gemini function call carries.
//!
//! A thinking model that calls a tool emits an encrypted `thoughtSignature` with the call. It is a
//! save state for its reasoning: the next request must replay the signature beside the same
//! function call, or the model loses the chain of thought that justified it. Gemini 3 enforces
//! this strictly and rejects a replayed call that is missing one with HTTP 400 — "Function call is
//! missing a thought_signature in functionCall parts".
//!
//! Two wire shapes carry the value, and both are read and written here so every Google route is
//! covered by one implementation:
//!
//! - the OpenAI-compatible route (`/v1beta/openai/chat/completions`, the `gemini` provider) puts it
//!   at `extra_content.google.thought_signature` on the tool call;
//! - the native Code Assist / `generateContent` shape (`antigravity`) puts `thoughtSignature` on
//!   the part beside `functionCall`.
//!
//! A call we hold no signature for — a row another provider wrote, a session stored before this
//! field existed, a history imported from a text-only store — would otherwise 400 the request it
//! is replayed in. Google documents a dummy value for exactly that case,
//! [`SKIP_THOUGHT_SIGNATURE_VALIDATOR`]: the validator skips the check instead of failing. It is
//! accepted by both the Gemini API and Vertex AI, and it is a documented last resort rather than a
//! fabricated signature, so a real one is always preferred when we have it.

/// Google's documented dummy signature, for a function call replayed without its real one.
///
/// https://ai.google.dev/gemini-api/docs/thought-signatures — "using REST or manual handling".
pub const SKIP_THOUGHT_SIGNATURE_VALIDATOR: &str = "skip_thought_signature_validator";

/// First Gemini major version that validates a replayed signature strictly enough to reject a
/// missing one with HTTP 400. Earlier families tolerate an absent signature (the model only loses
/// reasoning state), which is why the dummy is not sent to them: Google documents that the dummy
/// degrades model performance, so it is reserved for the versions that would fail outright.
const STRICT_SIGNATURE_GEMINI_MAJOR: u32 = 3;

/// The signature a provider attached to one tool call, wherever that provider's wire puts it.
pub fn signature_from_tool_call(call: &serde_json::Value) -> Option<String> {
    [
        // OpenAI-compatible Gemini route.
        call.pointer("/extra_content/google/thought_signature"),
        // Some gateways flatten the extension onto the call itself.
        call.get("thought_signature"),
        call.get("thoughtSignature"),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| value.as_str())
    .filter(|signature| !signature.is_empty())
    .map(|signature| signature.to_string())
}

/// The value to replay for one call: the signature the model issued, or Google's dummy when the
/// model validates signatures and this call has none.
///
/// Returns `None` for a provider with no signature concept, so no field is written where the
/// protocol has no such field.
pub fn signature_for_replay(signature: Option<&str>, enforced: bool) -> Option<String> {
    match signature {
        Some(signature) if !signature.is_empty() => Some(signature.to_string()),
        _ if enforced => Some(SKIP_THOUGHT_SIGNATURE_VALIDATOR.to_string()),
        _ => None,
    }
}

/// Whether a model id is one that strictly validates replayed signatures — Gemini 3 and later.
///
/// Keyed on the model rather than the provider id because the validator is Google's whichever
/// route reaches it: the `gemini` provider, `antigravity`'s Code Assist, or a gateway that proxies
/// a `gemini-3*` model. A model no Gemini version can be read from (Claude on Code Assist, every
/// non-Google model) is never sent the dummy.
pub fn model_enforces_thought_signature(model: &str) -> bool {
    gemini_major_version(model).is_some_and(|major| major >= STRICT_SIGNATURE_GEMINI_MAJOR)
}

/// The major version in a Gemini model id: `gemini-3.8-flash` -> `3`, `google/gemini-2.5-pro` -> `2`.
///
/// `None` when the id names no Gemini version at all, which is how a non-Gemini model is told
/// apart from one whose version simply is not written down.
fn gemini_major_version(model: &str) -> Option<u32> {
    const MARKER: &str = "gemini-";
    let lower = model.to_ascii_lowercase();
    let rest = &lower[lower.find(MARKER)? + MARKER.len()..];
    rest.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_signature_is_read_from_either_wire_shape() {
        assert_eq!(
            signature_from_tool_call(&json!({
                "extra_content": { "google": { "thought_signature": "sig-openai" } }
            }))
            .as_deref(),
            Some("sig-openai")
        );
        assert_eq!(
            signature_from_tool_call(&json!({ "thoughtSignature": "sig-native" })).as_deref(),
            Some("sig-native")
        );
        assert_eq!(
            signature_from_tool_call(&json!({ "thought_signature": "sig-flat" })).as_deref(),
            Some("sig-flat")
        );
        assert_eq!(signature_from_tool_call(&json!({ "id": "call_1" })), None);
        // An empty string is not a signature: it must not be replayed as one.
        assert_eq!(
            signature_from_tool_call(&json!({ "thoughtSignature": "" })),
            None
        );
    }

    #[test]
    fn strict_validation_starts_at_gemini_3() {
        assert!(model_enforces_thought_signature("gemini-3.8-flash"));
        assert!(model_enforces_thought_signature("gemini-3.8-flash-tiered"));
        assert!(model_enforces_thought_signature("gemini-3.5-flash-lite"));
        assert!(model_enforces_thought_signature(
            "google/gemini-3.1-pro-preview"
        ));
        assert!(model_enforces_thought_signature("gemini-4-preview"));

        assert!(!model_enforces_thought_signature("gemini-2.5-flash"));
        assert!(!model_enforces_thought_signature("gemini-1.5-pro"));
        // Non-Gemini models on a Google endpoint validate nothing of the sort.
        assert!(!model_enforces_thought_signature("claude-sonnet-4-6"));
        assert!(!model_enforces_thought_signature("deepseek-v4.1-flash"));
    }

    #[test]
    fn the_real_signature_wins_and_only_a_strict_model_gets_the_dummy() {
        assert_eq!(
            signature_for_replay(Some("sig-real"), true).as_deref(),
            Some("sig-real")
        );
        assert_eq!(
            signature_for_replay(Some("sig-real"), false).as_deref(),
            Some("sig-real")
        );
        assert_eq!(
            signature_for_replay(None, true).as_deref(),
            Some(SKIP_THOUGHT_SIGNATURE_VALIDATOR)
        );
        // A non-validating provider is sent no field at all rather than a foreign one.
        assert_eq!(signature_for_replay(None, false), None);
        assert_eq!(
            signature_for_replay(Some(""), true).as_deref(),
            Some(SKIP_THOUGHT_SIGNATURE_VALIDATOR)
        );
    }
}
