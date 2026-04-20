use thiserror::Error;

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CancellationReason {
    UserRequested(Option<String>),
    Timeout,
    FailFast,
    ParentCancelled,
    EngineShutdown,
}

#[allow(dead_code)]
#[derive(Debug, Error)]
pub(crate) enum EngineError {
    #[error("workflow cancelled: {0:?}")]
    Cancelled(CancellationReason),
}
