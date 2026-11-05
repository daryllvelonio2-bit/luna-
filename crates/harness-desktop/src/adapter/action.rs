use serde::{Deserialize, Serialize};

use crate::deadline::Deadline;
use crate::error::{DeliveryDisposition, DesktopError};
use crate::tree::identity::ElementIdentity;

/// What the agent asks the desktop to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionRequest {
    /// Activate a control.
    Click,
    /// Replace the contents of a text field.
    Type { text: String },
    /// Set a value on a control that has one.
    SetValue { value: String },
    /// Flip a checkable control, or set it to a given state.
    Toggle { checked: Option<bool> },
}

impl ActionRequest {
    /// The name of the action, for logs and error messages.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::Type { .. } => "type",
            Self::SetValue { .. } => "set-value",
            Self::Toggle { .. } => "toggle",
        }
    }
}

/// What happened when the action ran.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionOutcome {
    /// Whether the action reached the application, and how sure that is.
    pub delivery: DeliveryDisposition,
    /// The steps that ran, in order: "Action.DoAction", "EditableText.SetTextContents".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
    /// A note for the agent: what the application did, when that is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ActionOutcome {
    /// The action reached the application and the effect was confirmed.
    pub fn verified(steps: Vec<String>) -> Self {
        Self {
            delivery: DeliveryDisposition::DeliveredVerified,
            steps,
            message: None,
        }
    }

    /// The action reached the application, but nothing confirmed the effect.
    pub fn unverified(steps: Vec<String>) -> Self {
        Self {
            delivery: DeliveryDisposition::DeliveredUnverified,
            steps,
            message: None,
        }
    }

    /// The action did not reach the application.
    pub fn not_delivered(steps: Vec<String>) -> Self {
        Self {
            delivery: DeliveryDisposition::NotDelivered,
            steps,
            message: None,
        }
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

/// What a platform can do.
pub trait ActionOps: Send + Sync {
    /// Act on the element matching `identity` inside `app`.
    ///
    /// The adapter re-reads the tree before it acts, because the element a ref
    /// names may be gone or may no longer be the only element with that
    /// identity. An adapter that cannot prove uniqueness refuses.
    fn execute(
        &self,
        _app: &str,
        _identity: &ElementIdentity,
        _request: &ActionRequest,
        _deadline: Deadline,
    ) -> Result<ActionOutcome, DesktopError> {
        Err(DesktopError::not_supported("execute"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_request_names_itself() {
        assert_eq!(ActionRequest::Click.name(), "click");
        assert_eq!(
            ActionRequest::Type {
                text: "hi".to_string()
            }
            .name(),
            "type"
        );
        assert_eq!(
            ActionRequest::SetValue {
                value: "1".to_string()
            }
            .name(),
            "set-value"
        );
        assert_eq!(
            ActionRequest::Toggle { checked: None }.name(),
            "toggle"
        );
    }

    #[test]
    fn a_verified_outcome_says_so() {
        let outcome = ActionOutcome::verified(vec!["Action.DoAction".to_string()]);
        assert_eq!(outcome.delivery, DeliveryDisposition::DeliveredVerified);
        assert_eq!(outcome.steps.len(), 1);
    }
}
