use thiserror::Error;

use crate::spec::SCHEMA_VERSION;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpecError {
    #[error("unsupported UI schema version {found}; this build supports version {supported}")]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
    #[error("screen id must not be empty")]
    EmptyScreenId,
    #[error("screen title must not be empty")]
    EmptyScreenTitle,
    #[error("node id must not be empty")]
    EmptyNodeId,
    #[error("duplicate node id `{id}`")]
    DuplicateNodeId { id: String },
    #[error("capability must not be empty")]
    EmptyCapability,
    #[error("duplicate capability `{capability}`")]
    DuplicateCapability { capability: String },
    #[error("node `{id}` binds an empty state key")]
    EmptyBind { id: String },
    #[error("duplicate state binding `{bind}`")]
    DuplicateBind { bind: String },
    #[error("node `{id}` references an empty action")]
    EmptyAction { id: String },
    #[error("screen `{screen}` node `{node}` references undeclared action `{action}`")]
    UndeclaredAction {
        screen: String,
        node: String,
        action: String,
    },
    #[error("select node `{id}` must declare at least one option")]
    EmptySelectOptions { id: String },
    #[error("select node `{id}` repeats option value `{value}`")]
    DuplicateSelectValue { id: String, value: String },
    #[error("table node `{id}` must declare at least one column")]
    EmptyTableColumns { id: String },
    #[error("table node `{id}` repeats column key `{key}`")]
    DuplicateColumnKey { id: String, key: String },
    #[error("image node `{id}` must declare a source")]
    EmptyImageSource { id: String },
    #[error("button node `{id}` must declare a label")]
    EmptyButtonLabel { id: String },
}

impl SpecError {
    pub(crate) fn unsupported_schema_version(found: u32) -> Self {
        Self::UnsupportedSchemaVersion {
            found,
            supported: SCHEMA_VERSION,
        }
    }
}

#[derive(Debug, Error)]
pub enum SpecLoadError {
    #[error("invalid UI spec JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Invalid(#[from] SpecError),
}
