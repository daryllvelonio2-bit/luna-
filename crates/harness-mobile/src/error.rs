use harness_core::HarnessError;
use harness_ui_spec::{SpecError, SpecLoadError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MobileError {
    #[error(transparent)]
    Harness(#[from] HarnessError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error(transparent)]
    SpecLoad(#[from] SpecLoadError),
}
