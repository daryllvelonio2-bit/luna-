//! Finding the applications on the accessibility bus.

use harness_desktop::adapter::types::AppInfo;
use harness_desktop::error::DesktopError;
use zbus::blocking::Connection;

use crate::bus::{REGISTRY_ROOT_PATH, REGISTRY_SERVICE};
use crate::element::AtspiElement;
use crate::tree::node::{read_children, read_node};

/// The desktop root: the object whose children are the applications.
pub fn desktop_root() -> AtspiElement {
    AtspiElement::new(REGISTRY_SERVICE, REGISTRY_ROOT_PATH)
}

/// The applications the bus knows about.
pub fn list_apps(connection: &Connection) -> Result<Vec<AppInfo>, DesktopError> {
    let root = desktop_root();
    let mut apps = Vec::new();
    for child in read_children(connection, &root)? {
        let Ok(raw) = read_node(connection, &child) else {
            // An application that does not answer is one that is shutting
            // down. It is skipped rather than failing the whole listing.
            continue;
        };
        let Some(name) = raw.name.clone().filter(|name| !name.trim().is_empty()) else {
            continue;
        };
        let windows = count_windows(connection, &child);
        apps.push(AppInfo {
            name,
            pid: None,
            windows,
            bundle_id: raw.accessible_id.filter(|id| !id.trim().is_empty()),
        });
    }
    Ok(apps)
}

/// The element of the application with this name.
///
/// An exact match wins over a substring, because an exact match is what the
/// agent asked for and a substring is a guess.
pub fn find_app(connection: &Connection, name: &str) -> Result<AtspiElement, DesktopError> {
    let root = desktop_root();
    let wanted = name.trim();
    let mut partial = None;

    for child in read_children(connection, &root)? {
        let Ok(raw) = read_node(connection, &child) else {
            continue;
        };
        let Some(app_name) = raw.name else {
            continue;
        };
        if app_name.eq_ignore_ascii_case(wanted) {
            return Ok(child);
        }
        if partial.is_none() && app_name.to_lowercase().contains(&wanted.to_lowercase()) {
            partial = Some(child);
        }
    }

    partial.ok_or_else(|| DesktopError::app_not_found(name))
}

/// How many windows an application has.
fn count_windows(connection: &Connection, app: &AtspiElement) -> usize {
    match read_children(connection, app) {
        Ok(children) => children
            .iter()
            .filter(|child| is_window_role(connection, child))
            .count(),
        Err(_) => 0,
    }
}

/// Whether an element is a top-level window.
///
/// The role is read from the bus; a child that does not answer is not a
/// window, because it cannot say that it is one.
pub fn is_window_role(connection: &Connection, element: &AtspiElement) -> bool {
    match crate::tree::node::read_role(connection, element) {
        Ok(role) => {
            let role = role.to_lowercase();
            role == "frame" || role == "window" || role == "dialog" || role == "alert"
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_root_is_the_registry_root() {
        let root = desktop_root();
        assert_eq!(root.bus_name, REGISTRY_SERVICE);
        assert_eq!(root.path, REGISTRY_ROOT_PATH);
    }
}
