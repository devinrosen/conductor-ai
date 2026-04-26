pub mod call;
pub mod call_workflow;
pub mod control_flow;
pub mod foreach;
pub mod gate;
pub mod parallel;
pub mod script;

use crate::engine_error::EngineError;

#[inline]
pub(super) fn p_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Persistence(e.to_string())
}

/// Insert a step record and emit a StepRetrying event when `attempt > 0`.
/// Returns the new step_id.
pub(super) fn begin_retry_attempt(
    state: &mut crate::engine::ExecutionState,
    step_name: &str,
    role: &str,
    pos: i64,
    iteration: u32,
    attempt: u32,
) -> crate::engine_error::Result<String> {
    use crate::engine::emit_event;
    use crate::events::EngineEvent;
    use crate::traits::persistence::NewStep;
    if attempt > 0 {
        emit_event(
            state,
            EngineEvent::StepRetrying {
                step_name: step_name.to_string(),
                attempt,
            },
        );
    }
    let step_id = state
        .persistence
        .insert_step(NewStep {
            workflow_run_id: state.workflow_run_id.clone(),
            step_name: step_name.to_string(),
            role: role.to_string(),
            can_commit: false,
            position: pos,
            iteration: iteration as i64,
            retry_count: Some(attempt as i64),
        })
        .map_err(p_err)?;
    Ok(step_id)
}

/// Returns `true` and performs skip cleanup if the step has already completed.
/// Callers should `return Ok(())` (or `continue`) immediately when this returns `true`.
pub(super) fn skip_if_already_completed(
    state: &mut crate::engine::ExecutionState,
    step_key: &str,
    iteration: u32,
    label: &str,
) -> bool {
    use crate::engine::{restore_step, should_skip};
    if should_skip(state, step_key, iteration) {
        tracing::info!(
            "Skipping completed step '{}' (iteration {})",
            label,
            iteration
        );
        restore_step(state, step_key, iteration);
        true
    } else {
        false
    }
}
