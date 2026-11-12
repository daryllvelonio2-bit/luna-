use std::collections::HashSet;

use crate::spec::{Node, NodeKind, Screen, SCHEMA_VERSION};
use crate::SpecError;

pub(crate) fn validate(screen: &Screen) -> Result<(), SpecError> {
    if screen.schema_version != SCHEMA_VERSION {
        return Err(SpecError::unsupported_schema_version(screen.schema_version));
    }
    if screen.id.trim().is_empty() {
        return Err(SpecError::EmptyScreenId);
    }
    if screen.title.trim().is_empty() {
        return Err(SpecError::EmptyScreenTitle);
    }

    let mut capabilities = HashSet::new();
    for capability in &screen.capabilities {
        if capability.trim().is_empty() {
            return Err(SpecError::EmptyCapability);
        }
        if !capabilities.insert(capability.as_str()) {
            return Err(SpecError::DuplicateCapability {
                capability: capability.clone(),
            });
        }
    }

    let mut ids = HashSet::new();
    let mut binds = HashSet::new();
    let mut failure = None;
    screen.walk(&mut |node| {
        if failure.is_some() {
            return;
        }
        if let Err(error) = validate_node(screen, node, &capabilities, &mut ids, &mut binds) {
            failure = Some(error);
        }
    });
    failure.map_or(Ok(()), Err)
}

fn validate_node(
    screen: &Screen,
    node: &Node,
    capabilities: &HashSet<&str>,
    ids: &mut HashSet<String>,
    binds: &mut HashSet<String>,
) -> Result<(), SpecError> {
    if node.id.trim().is_empty() {
        return Err(SpecError::EmptyNodeId);
    }
    if !ids.insert(node.id.clone()) {
        return Err(SpecError::DuplicateNodeId {
            id: node.id.clone(),
        });
    }

    if let Some(bind) = node.bind() {
        if bind.trim().is_empty() {
            return Err(SpecError::EmptyBind {
                id: node.id.clone(),
            });
        }
        if !binds.insert(bind.to_string()) {
            return Err(SpecError::DuplicateBind {
                bind: bind.to_string(),
            });
        }
    }

    if let Some(action) = node.action() {
        if action.trim().is_empty() {
            return Err(SpecError::EmptyAction {
                id: node.id.clone(),
            });
        }
        if !capabilities.contains(action) {
            return Err(SpecError::UndeclaredAction {
                screen: screen.id.clone(),
                node: node.id.clone(),
                action: action.to_string(),
            });
        }
    }

    match &node.kind {
        NodeKind::Select { options, .. } => {
            if options.is_empty() {
                return Err(SpecError::EmptySelectOptions {
                    id: node.id.clone(),
                });
            }
            let mut values = HashSet::new();
            for option in options {
                if !values.insert(option.value.as_str()) {
                    return Err(SpecError::DuplicateSelectValue {
                        id: node.id.clone(),
                        value: option.value.clone(),
                    });
                }
            }
        }
        NodeKind::Table { columns, .. } => {
            if columns.is_empty() {
                return Err(SpecError::EmptyTableColumns {
                    id: node.id.clone(),
                });
            }
            let mut keys = HashSet::new();
            for column in columns {
                if !keys.insert(column.key.as_str()) {
                    return Err(SpecError::DuplicateColumnKey {
                        id: node.id.clone(),
                        key: column.key.clone(),
                    });
                }
            }
        }
        NodeKind::Image { src, .. } => {
            if src.trim().is_empty() {
                return Err(SpecError::EmptyImageSource {
                    id: node.id.clone(),
                });
            }
        }
        NodeKind::Button { label, .. } if label.trim().is_empty() => {
            return Err(SpecError::EmptyButtonLabel {
                id: node.id.clone(),
            });
        }
        _ => {}
    }

    Ok(())
}
