//! An adapter for a platform the harness cannot drive yet.
//!
//! Every command refuses with `PLATFORM_NOT_SUPPORTED` and a note saying
//! which platform was found. A refusal is the honest answer: an empty list of
//! windows on macOS would read as "there are no windows", which is a different
//! claim and a wrong one.

use crate::adapter::action::{ActionOps, ActionOutcome, ActionRequest};
use crate::adapter::observation::ObservationOps;
use crate::adapter::system::{DesktopStatus, SystemOps};
use crate::adapter::types::{AppInfo, ObservedTree, WindowInfo};
use crate::deadline::Deadline;
use crate::error::DesktopError;
use crate::tree::identity::ElementIdentity;
use crate::tree::options::TreeOptions;
use crate::tree::state::ElementState;

/// The adapter for a platform with no implementation yet.
pub struct UnsupportedAdapter {
    platform: &'static str,
}

impl UnsupportedAdapter {
    pub fn new(platform: &'static str) -> Self {
        Self { platform }
    }

    fn refuse(&self, what: &str) -> DesktopError {
        DesktopError::not_supported(what).with_suggestion(format!(
            "Desktop automation is not implemented for {}; the tools work on Linux",
            self.platform
        ))
    }
}

impl Default for UnsupportedAdapter {
    fn default() -> Self {
        Self::new(std::env::consts::OS)
    }
}

impl ObservationOps for UnsupportedAdapter {
    fn list_apps(&self, _deadline: Deadline) -> Result<Vec<AppInfo>, DesktopError> {
        Err(self.refuse("list_apps"))
    }

    fn list_windows(
        &self,
        _app: Option<&str>,
        _deadline: Deadline,
    ) -> Result<Vec<WindowInfo>, DesktopError> {
        Err(self.refuse("list_windows"))
    }

    fn observe(
        &self,
        _app: &str,
        _options: &TreeOptions,
        _deadline: Deadline,
    ) -> Result<ObservedTree, DesktopError> {
        Err(self.refuse("observe"))
    }

    fn read_value(
        &self,
        _app: &str,
        _identity: &ElementIdentity,
        _deadline: Deadline,
    ) -> Result<Option<String>, DesktopError> {
        Err(self.refuse("read_value"))
    }

    fn read_state(
        &self,
        _app: &str,
        _identity: &ElementIdentity,
        _deadline: Deadline,
    ) -> Result<Option<ElementState>, DesktopError> {
        Err(self.refuse("read_state"))
    }

    fn read_actions(
        &self,
        _app: &str,
        _identity: &ElementIdentity,
        _deadline: Deadline,
    ) -> Result<Option<Vec<String>>, DesktopError> {
        Err(self.refuse("read_actions"))
    }
}

impl ActionOps for UnsupportedAdapter {
    fn execute(
        &self,
        _app: &str,
        _identity: &ElementIdentity,
        _request: &ActionRequest,
        _deadline: Deadline,
    ) -> Result<ActionOutcome, DesktopError> {
        Err(self.refuse("execute"))
    }
}

impl SystemOps for UnsupportedAdapter {
    fn platform(&self) -> &'static str {
        self.platform
    }

    fn status(&self, _deadline: Deadline) -> Result<DesktopStatus, DesktopError> {
        Err(self.refuse("status"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn every_capability_refuses_with_the_platform_named() {
        let adapter = UnsupportedAdapter::new("macos");
        let error = adapter.list_apps(Deadline::none()).expect_err("no apps");
        assert_eq!(error.code, ErrorCode::PlatformNotSupported);
        assert!(
            error.suggestion.expect("a suggestion").contains("macos"),
            "the suggestion must name the platform"
        );
    }

    #[test]
    fn the_platform_is_reported_even_when_nothing_works() {
        let adapter = UnsupportedAdapter::new("windows");
        assert_eq!(adapter.platform(), "windows");
    }
}
