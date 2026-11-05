//! Listing the windows of an application.

use harness_desktop::adapter::types::WindowInfo;
use harness_desktop::error::DesktopError;
use zbus::blocking::Connection;

use crate::element::AtspiElement;
use crate::system::apps::{find_app, is_window_role};
use crate::tree::node::{read_children, read_extents, read_node};

/// The windows the desktop can see, optionally of one application.
pub fn list_windows(
    connection: &Connection,
    app: Option<&str>,
) -> Result<Vec<WindowInfo>, DesktopError> {
    let apps: Vec<AtspiElement> = match app {
        Some(name) => vec![find_app(connection, name)?],
        None => crate::system::apps::list_apps(connection)?
            .into_iter()
            .filter_map(|info| find_app(connection, &info.name).ok())
            .collect(),
    };

    let mut windows = Vec::new();
    for app in apps {
        let app_name = read_node(connection, &app)
            .ok()
            .and_then(|raw| raw.name)
            .unwrap_or_default();
        for child in read_children(connection, &app)? {
            if !is_window_role(connection, &child) {
                continue;
            }
            let raw = read_node(connection, &child)?;
            windows.push(WindowInfo {
                id: child.id(),
                app: app_name.clone(),
                title: raw.name,
                role: raw.role,
                bounds: read_extents(connection, &child).ok().flatten(),
            });
        }
    }
    Ok(windows)
}
