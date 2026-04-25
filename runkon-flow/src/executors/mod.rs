pub mod call;
pub mod call_workflow;
pub mod control_flow;
pub mod foreach;
pub mod gate;
pub mod parallel;
pub mod script;

use crate::engine_error::EngineError;

#[inline]
pub(super) fn p_err(e: EngineError) -> EngineError {
    EngineError::Persistence(e.to_string())
}
