use serde::{Deserialize, Serialize};

/// Whether an action reached the application, and how sure the adapter is.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryDisposition {
    /// Nothing is known: the call failed before it could be classified.
    #[default]
    Unknown,
    /// The action provably did not reach the application.
    NotDelivered,
    /// The action may or may not have landed.
    DeliveryUncertain,
    /// The action landed, but the effect was not confirmed.
    DeliveredUnverified,
    /// The action landed and the effect was confirmed.
    DeliveredVerified,
}

/// Whether repeating the action is safe.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetryDisposition {
    #[default]
    Unknown,
    /// Repeating cannot double-apply: the action did not reach the application.
    Safe,
    /// Repeating may apply the action twice.
    Unsafe,
}

/// The pair an action result carries: what happened, and whether to try again.
///
/// This is the single most important piece of the contract. A refusal that says
/// only "failed" leaves the caller guessing, and a guess that retries a click
/// clicks twice.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct DeliverySemantics {
    pub delivery: DeliveryDisposition,
    pub retry: RetryDisposition,
}

impl DeliverySemantics {
    pub fn unknown() -> Self {
        Self::default()
    }

    pub fn not_delivered() -> Self {
        Self {
            delivery: DeliveryDisposition::NotDelivered,
            retry: RetryDisposition::Safe,
        }
    }

    pub fn uncertain() -> Self {
        Self {
            delivery: DeliveryDisposition::DeliveryUncertain,
            retry: RetryDisposition::Unsafe,
        }
    }

    pub fn delivered_unverified() -> Self {
        Self {
            delivery: DeliveryDisposition::DeliveredUnverified,
            retry: RetryDisposition::Unsafe,
        }
    }

    pub fn delivered_verified() -> Self {
        Self {
            delivery: DeliveryDisposition::DeliveredVerified,
            retry: RetryDisposition::Unsafe,
        }
    }

    /// Whether repeating the action is safe.
    pub fn retry_is_safe(&self) -> bool {
        self.retry == RetryDisposition::Safe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_proven_non_delivery_is_safe_to_retry() {
        assert!(DeliverySemantics::not_delivered().retry_is_safe());
        assert!(!DeliverySemantics::uncertain().retry_is_safe());
        assert!(!DeliverySemantics::delivered_unverified().retry_is_safe());
        assert!(!DeliverySemantics::delivered_verified().retry_is_safe());
        assert!(!DeliverySemantics::unknown().retry_is_safe());
    }

    #[test]
    fn the_wire_form_names_the_delivery_and_the_retry() {
        let value = serde_json::to_value(DeliverySemantics::not_delivered()).expect("serializes");
        assert_eq!(value["delivery"], "not_delivered");
        assert_eq!(value["retry"], "safe");
    }
}
