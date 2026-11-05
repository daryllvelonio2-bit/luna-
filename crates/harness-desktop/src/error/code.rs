use serde::Serialize;

/// The failure vocabulary a desktop command can report.
///
/// The set is copied from `vercel-labs/agent-desktop` on purpose: an agent that
/// has read one harness's desktop errors reads the other the same way, and a
/// port that renamed `STALE_REF` would silently break every prompt that
/// branches on it.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    PermDenied,
    ElementNotFound,
    AppNotFound,
    ActionFailed,
    ActionNotSupported,
    StaleRef,
    AmbiguousTarget,
    WindowNotFound,
    PlatformNotSupported,
    Timeout,
    InvalidArgs,
    SnapshotNotFound,
    PolicyDenied,
    AppUnresponsive,
    Internal,
}

impl ErrorCode {
    /// The wire form of the code, as it appears in the JSON envelope.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PermDenied => "PERM_DENIED",
            Self::ElementNotFound => "ELEMENT_NOT_FOUND",
            Self::AppNotFound => "APP_NOT_FOUND",
            Self::ActionFailed => "ACTION_FAILED",
            Self::ActionNotSupported => "ACTION_NOT_SUPPORTED",
            Self::StaleRef => "STALE_REF",
            Self::AmbiguousTarget => "AMBIGUOUS_TARGET",
            Self::WindowNotFound => "WINDOW_NOT_FOUND",
            Self::PlatformNotSupported => "PLATFORM_NOT_SUPPORTED",
            Self::Timeout => "TIMEOUT",
            Self::InvalidArgs => "INVALID_ARGS",
            Self::SnapshotNotFound => "SNAPSHOT_NOT_FOUND",
            Self::PolicyDenied => "POLICY_DENIED",
            Self::AppUnresponsive => "APP_UNRESPONSED",
            Self::Internal => "INTERNAL",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_screaming_snake_wire_form() {
        let codes = [
            (ErrorCode::PermDenied, "PERM_DENIED"),
            (ErrorCode::ElementNotFound, "ELEMENT_NOT_FOUND"),
            (ErrorCode::AppNotFound, "APP_NOT_FOUND"),
            (ErrorCode::ActionFailed, "ACTION_FAILED"),
            (ErrorCode::ActionNotSupported, "ACTION_NOT_SUPPORTED"),
            (ErrorCode::StaleRef, "STALE_REF"),
            (ErrorCode::AmbiguousTarget, "AMBIGUOUS_TARGET"),
            (ErrorCode::WindowNotFound, "WINDOW_NOT_FOUND"),
            (ErrorCode::PlatformNotSupported, "PLATFORM_NOT_SUPPORTED"),
            (ErrorCode::Timeout, "TIMEOUT"),
            (ErrorCode::InvalidArgs, "INVALID_ARGS"),
            (ErrorCode::SnapshotNotFound, "SNAPSHOT_NOT_FOUND"),
            (ErrorCode::PolicyDenied, "POLICY_DENIED"),
            (ErrorCode::AppUnresponsive, "APP_UNRESPONSED"),
            (ErrorCode::Internal, "INTERNAL"),
        ];
        for (code, wire) in codes {
            assert_eq!(code.as_str(), wire);
        }
    }

    #[test]
    fn the_serialized_form_matches_the_wire_form() {
        assert_eq!(
            serde_json::to_string(&ErrorCode::StaleRef).expect("serializes"),
            "\"STALE_REF\""
        );
    }
}
