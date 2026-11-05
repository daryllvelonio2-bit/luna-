//! Choosing the platform adapter.

use std::sync::Arc;

use harness_desktop::adapter::DesktopAdapter;
use harness_desktop::DesktopEngine;

/// The desktop engine for this platform.
pub fn engine() -> Arc<DesktopEngine> {
    Arc::new(DesktopEngine::new(adapter()))
}

/// The adapter for the platform this build runs on.
#[cfg(target_os = "linux")]
fn adapter() -> Arc<dyn DesktopAdapter> {
    Arc::new(harness_desktop_linux::LinuxDesktopAdapter::new())
}

/// The adapter for a platform with no implementation yet.
#[cfg(not(target_os = "linux"))]
fn adapter() -> Arc<dyn DesktopAdapter> {
    Arc::new(harness_desktop::adapter::UnsupportedAdapter::new(
        std::env::consts::OS,
    ))
}
