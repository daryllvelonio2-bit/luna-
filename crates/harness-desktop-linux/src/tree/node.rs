//! Reading one accessible off the bus.

use harness_desktop::error::DesktopError;
use harness_desktop::tree::rect::Rect;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::bus;
use crate::element::AtspiElement;

const ACCESSIBLE_INTERFACE: &str = "org.a11y.atspi.Accessible";
const COMPONENT_INTERFACE: &str = "org.a11y.atspi.Component";
const ACTION_INTERFACE: &str = "org.a11y.atspi.Action";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

const NAME_PROPERTY: &str = "Name";
const DESCRIPTION_PROPERTY: &str = "Description";
const ACCESSIBLE_ID_PROPERTY: &str = "AccessibleId";

/// Screen coordinates, the coordinate system `GetExtents` reports in.
const SCREEN_COORDINATES: u32 = 0;

/// What one read of an accessible found.
#[derive(Debug, Clone)]
pub struct RawNode {
    pub element: AtspiElement,
    pub role: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub accessible_id: Option<String>,
    pub states: Vec<String>,
    pub children: Vec<AtspiElement>,
    pub interfaces: Vec<String>,
}

/// Read one accessible: what it is, what it says, and what it holds.
pub fn read_node(connection: &Connection, element: &AtspiElement) -> Result<RawNode, DesktopError> {
    let role = read_role(connection, element)?;
    let name = read_property(connection, element, NAME_PROPERTY)?;
    let description = read_property(connection, element, DESCRIPTION_PROPERTY)?;
    let accessible_id = read_property(connection, element, ACCESSIBLE_ID_PROPERTY)?;
    let states = read_states(connection, element)?;
    let children = read_children(connection, element)?;
    let interfaces = read_interfaces(connection, element)?;
    Ok(RawNode {
        element: element.clone(),
        role,
        name,
        description,
        accessible_id,
        states,
        children,
        interfaces,
    })
}

/// The role of an element, as the platform names it.
pub fn read_role(connection: &Connection, element: &AtspiElement) -> Result<String, DesktopError> {
    match call::<String>(
        connection,
        element,
        ACCESSIBLE_INTERFACE,
        "GetRoleName",
        &(),
    ) {
        Ok(name) if !name.trim().is_empty() => Ok(name),
        _ => {
            let role: u32 = call(connection, element, ACCESSIBLE_INTERFACE, "GetRole", &())?;
            Ok(format!("role-{role}"))
        }
    }
}

/// The states of an element, as names.
pub fn read_states(
    connection: &Connection,
    element: &AtspiElement,
) -> Result<Vec<String>, DesktopError> {
    let bits: Vec<u32> = call(connection, element, ACCESSIBLE_INTERFACE, "GetState", &())?;
    Ok(super::state::state_names(&bits))
}

/// The children of an element.
pub fn read_children(
    connection: &Connection,
    element: &AtspiElement,
) -> Result<Vec<AtspiElement>, DesktopError> {
    let children: Vec<(String, OwnedObjectPath)> = call(
        connection,
        element,
        ACCESSIBLE_INTERFACE,
        "GetChildren",
        &(),
    )?;
    Ok(children
        .into_iter()
        .filter(|(bus_name, path)| !bus_name.is_empty() && path.as_str() != "/org/a11y/atspi/null")
        .map(|(bus_name, path)| AtspiElement::new(bus_name, path.as_str()))
        .collect())
}

/// The interfaces an element offers.
pub fn read_interfaces(
    connection: &Connection,
    element: &AtspiElement,
) -> Result<Vec<String>, DesktopError> {
    call::<Vec<String>>(
        connection,
        element,
        ACCESSIBLE_INTERFACE,
        "GetInterfaces",
        &(),
    )
}

/// One string property of the accessible interface.
pub fn read_property(
    connection: &Connection,
    element: &AtspiElement,
    key: &str,
) -> Result<Option<String>, DesktopError> {
    let value: OwnedValue = call(
        connection,
        element,
        PROPERTIES_INTERFACE,
        "Get",
        &(ACCESSIBLE_INTERFACE, key),
    )?;
    Ok(owned_string(&value))
}

/// The screen rectangle of an element.
pub fn read_extents(
    connection: &Connection,
    element: &AtspiElement,
) -> Result<Option<Rect>, DesktopError> {
    match call::<(i32, i32, i32, i32)>(
        connection,
        element,
        COMPONENT_INTERFACE,
        "GetExtents",
        &(SCREEN_COORDINATES,),
    ) {
        Ok((x, y, width, height)) => Ok(Some(Rect::new(x, y, width, height))),
        Err(error) if error.code == harness_desktop::error::ErrorCode::ActionNotSupported => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// The actions an element offers, by name.
pub fn read_actions(
    connection: &Connection,
    element: &AtspiElement,
) -> Result<Vec<String>, DesktopError> {
    let listed: Vec<(String, String, String)> =
        call(connection, element, ACTION_INTERFACE, "GetActions", &())?;
    let mut names = Vec::with_capacity(listed.len());
    for (index, (name, _, _)) in listed.into_iter().enumerate() {
        // `GetName` is the only call that returns the machine name on every
        // toolkit: `GetActions` returns empty names on Chromium.
        let machine_name = call::<String>(
            connection,
            element,
            ACTION_INTERFACE,
            "GetName",
            &(index as i32,),
        )
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(name);
        names.push(machine_name);
    }
    Ok(names)
}

/// Call a method on an element and read its answer.
pub fn call<T>(
    connection: &Connection,
    element: &AtspiElement,
    interface: &str,
    method: &str,
    body: &(impl zbus::zvariant::DynamicType + serde::Serialize),
) -> Result<T, DesktopError>
where
    T: for<'de> zbus::zvariant::DynamicDeserialize<'de>,
{
    let message = connection
        .call_method(
            Some(element.bus_name.as_str()),
            element.path.as_str(),
            Some(interface),
            method,
            body,
        )
        .map_err(|error| bus::classify(&error))?;
    message
        .body()
        .deserialize()
        .map_err(|error| bus::classify(&error))
}

/// Read a D-Bus string value.
fn owned_string(value: &OwnedValue) -> Option<String> {
    value
        .downcast_ref::<&str>()
        .ok()
        .map(|text| text.to_string())
        .or_else(|| value.downcast_ref::<String>().ok())
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_empty_name_is_not_a_name() {
        // The shape of the helper: an empty string is absence, because an
        // element with no name and an element named "" must resolve alike.
        assert_eq!(Some(String::new()).filter(|s| !s.is_empty()), None);
    }
}
