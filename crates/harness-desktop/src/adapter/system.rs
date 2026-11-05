use serde::{Deserialize, Serialize};

use crate::deadline::Deadline;
use crate::error::DesktopError;

/// What the desktop can do on this machine, right now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopStatus {
    /// The operating system the adapter speaks to.
    pub platform: String,
    /// Whether the accessibility bus answered.
    pub bus_available: bool,
    /// The surfaces the platform can observe: "window", "menu", "sheet".
    #[serde(default)]
    pub supported_surfaces: Vec<String>,
    /// Anything the agent should know before it tries: a missing bus, a
    /// session without a display, a toolkit that needs a launch flag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The platform's own questions.
pub trait SystemOps: Send + Sync {
    /// The operating system name, as the adapter knows it.
    fn platform(&self) -> &'static str {
        "unknown"
    }

    /// The adapter's version.
    fn version(&self) -> &'static str {
        "0.0.0"
    }

    /// What this platform can do right now.
    fn status(&self, _deadline: Deadline) -> Result<DesktopStatus, DesktopError> {
        Err(DesktopError::not_supported("status"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;

    impl SystemOps for Noop {}

    #[test]
    fn an_unimplemented_status_refuses() {
        let error = Noop.status(Deadline::none()).expect_err("no status");
        assert_eq!(
            error.code,
            crate::error::ErrorCode::PlatformNotSupported
        );
    }

    #[test]
    fn the_defaults_name_no_platform() {
        let noop = Noop;
        assert_eq!(noop.platform(), "unknown");
        assert_eq!(noop.version(), "0.0.0");
    }
}
