//! Walking an accessible tree into the core's node model.

use harness_desktop::error::DesktopError;
use harness_desktop::tree::identity::NodeIdentity;
use harness_desktop::tree::node::AccessibilityNode;
use harness_desktop::tree::options::TreeOptions;
use harness_desktop::tree::presentation::NodePresentation;
use zbus::blocking::Connection;

use crate::element::AtspiElement;
use crate::tree::node::{read_actions, read_extents, read_node};

const COMPONENT_INTERFACE: &str = "org.a11y.atspi.Component";
const ACTION_INTERFACE: &str = "org.a11y.atspi.Action";

/// Read the tree below an element.
///
/// The walk is depth-first and every node is read once. A node's actions are
/// read because they decide whether the node can be acted on, and its extents
/// are read only when the platform says it has a geometry interface — a call
/// the element would refuse is a call not worth making.
pub fn walk(
    connection: &Connection,
    root: &AtspiElement,
    options: &TreeOptions,
) -> Result<AccessibilityNode, DesktopError> {
    walk_at(connection, root, options, 0)
}

fn walk_at(
    connection: &Connection,
    element: &AtspiElement,
    options: &TreeOptions,
    depth: u32,
) -> Result<AccessibilityNode, DesktopError> {
    let raw = read_node(connection, element)?;

    let available_actions = if raw
        .interfaces
        .iter()
        .any(|interface| interface == ACTION_INTERFACE)
    {
        read_actions(connection, element).unwrap_or_default()
    } else {
        Vec::new()
    };

    let bounds = if !options.compact
        && raw
            .interfaces
            .iter()
            .any(|interface| interface == COMPONENT_INTERFACE)
    {
        read_extents(connection, element).ok().flatten()
    } else {
        None
    };

    let mut node = AccessibilityNode {
        ref_id: None,
        role: raw.role.clone(),
        identity: NodeIdentity {
            name: raw.name.clone().filter(|s| !s.trim().is_empty()),
            value: None,
            description: raw.description.clone().filter(|s| !s.trim().is_empty()),
            native_id: raw
                .accessible_id
                .clone()
                .filter(|id| !id.trim().is_empty()),
        },
        presentation: NodePresentation {
            states: raw.states.clone(),
            available_actions,
            bounds,
        },
        handle: Some(harness_desktop::tree::node::ElementHandle::new(
            element.encode(),
        )),
        children_count: None,
        subtree_truncated: false,
        children: Vec::new(),
    };

    let cut_by_depth = options
        .max_depth
        .is_some_and(|limit| depth >= limit);
    let cut_by_skeleton = options.skeleton && depth >= 1;

    if cut_by_depth || cut_by_skeleton {
        node.subtree_truncated = !raw.children.is_empty();
        if node.subtree_truncated {
            node.children_count = Some(raw.children.len() as u32);
        }
        return Ok(node);
    }

    for child in &raw.children {
        let child_node = walk_at(connection, child, options, depth + 1)?;
        if options.interactive_only && !child_node.is_addressable() {
            // A container that is not itself actionable still holds actionable
            // descendants, so its children are kept and it is dropped.
            node.children.extend(child_node.children);
            continue;
        }
        node.children.push(child_node);
    }

    Ok(node)
}
