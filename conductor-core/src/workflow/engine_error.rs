use thiserror::Error;

use super::cancellation_reason::CancellationReason;

#[allow(dead_code)]
#[derive(Debug, Error)]
pub(crate) enum EngineError {
    #[error("workflow cancelled: {0:?}")]
    Cancelled(CancellationReason),
}
