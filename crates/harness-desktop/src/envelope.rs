use serde::Serialize;
use serde_json::Value;

use crate::error::{DeliverySemantics, DesktopError};

/// The version of the envelope shape this crate emits.
pub const ENVELOPE_VERSION: &str = "1.0";

/// One command's answer: either data or an error, never both.
///
/// The shape is the one `vercel-labs/agent-desktop` prints, so a model that has
/// read one harness's desktop output reads the other the same way.
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub version: &'static str,
    pub ok: bool,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorPayload>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub disposition: DeliverySemantics,
}

impl Response {
    pub fn ok(command: impl Into<String>, data: Value) -> Self {
        Self {
            version: ENVELOPE_VERSION,
            ok: true,
            command: command.into(),
            data: Some(data),
            error: None,
        }
    }

    pub fn err(command: impl Into<String>, error: &DesktopError) -> Self {
        Self {
            version: ENVELOPE_VERSION,
            ok: false,
            command: command.into(),
            data: None,
            error: Some(ErrorPayload {
                code: error.code.as_str().to_string(),
                message: error.message.clone(),
                suggestion: error.suggestion.clone(),
                details: error.details.clone(),
                disposition: error.disposition,
            }),
        }
    }

    /// The envelope as the JSON text a tool returns.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                "{{\"version\":\"{ENVELOPE_VERSION}\",\"ok\":false,\"command\":\"{}\",\"error\":{{\"code\":\"INTERNAL\",\"message\":\"the response could not be serialized\"}}}}",
                self.command
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn a_success_carries_data_and_no_error() {
        let response = Response::ok("status", serde_json::json!({ "platform": "linux" }));
        let value: Value = serde_json::from_str(&response.to_json()).expect("parses");
        assert_eq!(value["ok"], true);
        assert_eq!(value["command"], "status");
        assert_eq!(value["data"]["platform"], "linux");
        assert!(value.get("error").is_none());
    }

    #[test]
    fn a_failure_carries_the_code_and_the_disposition() {
        let error = DesktopError::new(ErrorCode::StaleRef, "gone").not_delivered();
        let response = Response::err("click", &error);
        let value: Value = serde_json::from_str(&response.to_json()).expect("parses");
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "STALE_REF");
        assert_eq!(value["error"]["disposition"]["delivery"], "not_delivered");
        assert_eq!(value["error"]["disposition"]["retry"], "safe");
        assert!(value.get("data").is_none());
    }

    #[test]
    fn every_envelope_carries_its_version() {
        let value: Value = serde_json::from_str(&Response::ok("x", Value::Null).to_json())
            .expect("parses");
        assert_eq!(value["version"], ENVELOPE_VERSION);
    }
}
