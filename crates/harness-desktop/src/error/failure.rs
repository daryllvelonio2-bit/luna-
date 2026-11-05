use serde_json::{Value, json};

use super::code::ErrorCode;
use super::disposition::DeliverySemantics;

/// A desktop failure, in the shape the JSON envelope carries.
#[derive(Debug, Clone)]
pub struct DesktopError {
    pub code: ErrorCode,
    pub message: String,
    /// What the caller should do next, in one sentence.
    pub suggestion: Option<String>,
    /// Machine-readable context: the matching candidates, the platform's own
    /// error text, the refused flag.
    pub details: Option<Value>,
    /// Whether the action landed, and whether repeating it is safe.
    pub disposition: DeliverySemantics,
}

impl DesktopError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            suggestion: None,
            details: None,
            disposition: DeliverySemantics::unknown(),
        }
    }

    /// A command this platform cannot run at all.
    pub fn not_supported(what: &str) -> Self {
        Self::new(
            ErrorCode::PlatformNotSupported,
            format!("This platform does not support '{what}'"),
        )
        .with_suggestion("Run `desktop_status` to see what this platform supports")
    }

    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgs, message)
            .with_disposition(DeliverySemantics::not_delivered())
    }

    pub fn app_not_found(app: &str) -> Self {
        Self::new(
            ErrorCode::AppNotFound,
            format!("No running application matched '{app}'"),
        )
        .with_suggestion("Run `desktop_list_apps` and use one of the names it prints")
    }

    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    /// The platform's own error text, for a failure the core cannot explain.
    pub fn with_platform_detail(mut self, detail: impl Into<String>) -> Self {
        self.details = Some(match self.details {
            Some(mut details) => {
                details["platform_detail"] = Value::String(detail.into());
                details
            }
            None => json!({ "platform_detail": detail.into() }),
        });
        self
    }

    pub fn with_disposition(mut self, disposition: DeliverySemantics) -> Self {
        self.disposition = disposition;
        self
    }

    /// Marks the failure as one where nothing reached the application.
    pub fn not_delivered(mut self) -> Self {
        self.disposition = DeliverySemantics::not_delivered();
        self
    }

    /// The element behind a ref is gone: the snapshot that minted it no longer
    /// describes the screen.
    pub fn stale_ref(ref_id: &str) -> Self {
        Self::new(
            ErrorCode::StaleRef,
            format!("The element '{ref_id}' is no longer in the window"),
        )
        .with_suggestion("Take a fresh snapshot and use the ref it prints")
        .with_details(json!({ "kind": "saved_ref_absent" }))
        .not_delivered()
    }

    /// More than one live element matches the ref's identity.
    pub fn ambiguous_target(ref_id: &str, matches: usize) -> Self {
        Self::new(
            ErrorCode::AmbiguousTarget,
            format!(
                "{matches} elements match '{ref_id}', so the action was not sent to any of them"
            ),
        )
        .with_suggestion("Act on a control that is unique in the window")
        .with_details(json!({ "kind": "identity_not_unique", "matches": matches }))
        .not_delivered()
    }
}

impl std::fmt::Display for DesktopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for DesktopError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::disposition::RetryDisposition;

    #[test]
    fn a_stale_ref_is_safe_to_retry_because_nothing_was_sent() {
        let error = DesktopError::stale_ref("@s1:e4");
        assert_eq!(error.code, ErrorCode::StaleRef);
        assert_eq!(error.disposition.retry, RetryDisposition::Safe);
        assert!(error.details.is_some());
    }

    #[test]
    fn an_ambiguous_target_refuses_to_pick_one() {
        let error = DesktopError::ambiguous_target("@s1:e4", 2);
        assert_eq!(error.code, ErrorCode::AmbiguousTarget);
        assert_eq!(error.disposition.retry, RetryDisposition::Safe);
        assert_eq!(error.details.expect("details")["matches"], 2);
    }

    #[test]
    fn invalid_arguments_are_never_delivered() {
        let error = DesktopError::invalid_args("bad ref");
        assert_eq!(error.code, ErrorCode::InvalidArgs);
        assert_eq!(error.disposition.retry, RetryDisposition::Safe);
    }
}
