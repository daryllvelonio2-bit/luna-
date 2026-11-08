//! Finding and holding the accessibility bus.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use harness_desktop::error::{DesktopError, ErrorCode};
use zbus::blocking::{Connection, Proxy};

/// How long one call on the accessibility bus may wait for a reply.
///
/// The bus is a shared resource and an application can stop answering while it
/// is busy (a modal dialog, a long re-render). Without a bound, a single stuck
/// call hangs the tool — and the turn — forever. Five seconds is longer than
/// any healthy call and short enough that the agent can fall back to a pointer
/// click instead of waiting.
const METHOD_TIMEOUT: Duration = Duration::from_secs(5);


const A11Y_BUS_SERVICE: &str = "org.a11y.Bus";
const A11Y_BUS_PATH: &str = "/org/a11y/bus";
const A11Y_BUS_INTERFACE: &str = "org.a11y.Bus";

/// The registry that owns the desktop root.
pub const REGISTRY_SERVICE: &str = "org.a11y.atspi.Registry";
/// The object path of the desktop root.
pub const REGISTRY_ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";

/// The process-wide connection to the accessibility bus.
///
/// One connection per process, opened on first use. The bus is a shared
/// resource and every call over it costs a round trip, so a second connection
/// would cost more than it saves.
static CONNECTION: Mutex<Option<Connection>> = Mutex::new(None);

/// The accessibility bus, opened if it is not open yet.
pub fn connection() -> Result<Connection, DesktopError> {
    let mut slot = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(connection) = slot.as_ref() {
        return Ok(connection.clone());
    }
    let connection = connect()?;
    *slot = Some(connection.clone());
    Ok(connection)
}

/// Forget the cached connection, so the next call opens a fresh one.
pub fn forget() {
    let mut slot = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
    *slot = None;
}

/// Ask the session bus for the accessibility bus address, then open it.
fn connect() -> Result<Connection, DesktopError> {
    let address = discover_address()?;
    zbus::blocking::connection::Builder::address(address.as_str())
        .map_err(|error| bus_unavailable(&error))?
        .method_timeout(METHOD_TIMEOUT)
        .build()
        .map_err(|error| bus_unavailable(&error))
}

/// The accessibility bus address, as the session bus reports it.
fn discover_address() -> Result<String, DesktopError> {
    let session = Connection::session().map_err(|error| bus_unavailable(&error))?;
    let proxy = Proxy::new(
        &session,
        A11Y_BUS_SERVICE,
        A11Y_BUS_PATH,
        A11Y_BUS_INTERFACE,
    )
    .map_err(|error| bus_unavailable(&error))?;
    proxy
        .call::<_, _, String>("GetAddress", &())
        .map_err(|error| bus_unavailable(&error))
}

/// The error for a bus that is not there.
///
/// The message names the package to install, because "no accessibility bus" is
/// otherwise a dead end: the agent cannot fix it and the user does not know
/// what to do.
fn bus_unavailable(error: &zbus::Error) -> DesktopError {
    DesktopError::new(
        ErrorCode::PlatformNotSupported,
        "The Linux accessibility bus did not answer",
    )
    .with_suggestion(
        "Install at-spi2-core and make sure the session has a running accessibility bus, \
         then run desktop_status again",
    )
    .with_details(serde_json::json!({
        "cause": "a11y_bus_unavailable",
        "retryable": false,
    }))
    .with_platform_detail(error.to_string())
}

/// Classify a failed call on the bus.
///
/// A missing interface or method is not a failure of the whole bus: the
/// element simply does not offer that capability, and the caller should read
/// on without it.
pub fn classify(error: &zbus::Error) -> DesktopError {
    // The connection's method timeout: the application stopped answering. This
    // is the failure that used to hang a tool forever, so it gets its own code
    // and a pointer fallback rather than a generic bus error.
    if let zbus::Error::InputOutput(io) = error {
        if io.kind() == std::io::ErrorKind::TimedOut {
            return DesktopError::new(
                ErrorCode::Timeout,
                "The application did not answer the accessibility bus in time",
            )
            .with_suggestion(
                "The window may be busy or showing a modal dialog; take a screenshot, then use \
                 desktop_click_at or desktop_key instead of a semantic action",
            )
            .not_delivered();
        }
    }
    let text = error.to_string();
    if text.contains("UnknownMethod") || text.contains("UnknownInterface") {
        return DesktopError::new(
            ErrorCode::ActionNotSupported,
            "The element does not offer that capability",
        )
        .with_details(serde_json::json!({ "cause": "unsupported_interface" }))
        .not_delivered();
    }
    if text.contains("ServiceUnknown") || text.contains("NameHasNoOwner") {
        return DesktopError::new(
            ErrorCode::AppNotFound,
            "The application is no longer on the accessibility bus",
        )
        .with_suggestion("Run desktop_list_apps and use an application that is still running")
        .not_delivered();
    }
    DesktopError::new(ErrorCode::Internal, "The accessibility bus returned an error")
        .with_suggestion("Retry the command; report the platform detail if it persists")
        .with_platform_detail(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_bus_names_the_package_to_install() {
        let error = bus_unavailable(&zbus::Error::Failure("no bus".to_string()));
        assert_eq!(error.code, ErrorCode::PlatformNotSupported);
        let suggestion = error.suggestion.expect("a suggestion");
        assert!(suggestion.contains("at-spi2-core"), "{suggestion}");
    }

    #[test]
    fn an_unknown_method_is_an_unsupported_capability_rather_than_a_failure() {
        let error = classify(&zbus::Error::Failure("UnknownMethod".to_string()));
        assert_eq!(error.code, ErrorCode::ActionNotSupported);
    }

    #[test]
    fn a_gone_service_is_reported_as_a_missing_application() {
        let error = classify(&zbus::Error::Failure("ServiceUnknown".to_string()));
        assert_eq!(error.code, ErrorCode::AppNotFound);
    }

    #[test]
    fn a_call_that_times_out_is_a_timeout_with_a_pointer_fallback() {
        let error = classify(&zbus::Error::InputOutput(std::sync::Arc::new(
            std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"),
        )));
        assert_eq!(error.code, ErrorCode::Timeout);
        let suggestion = error.suggestion.expect("a suggestion");
        assert!(suggestion.contains("desktop_click_at"), "{suggestion}");
    }
}
