use thiserror::Error;

use super::cancellation_reason::CancellationReason;

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("workflow cancelled: {0:?}")]
    Cancelled(CancellationReason),
    #[error("persistence error: {0}")]
    Persistence(String),
}
