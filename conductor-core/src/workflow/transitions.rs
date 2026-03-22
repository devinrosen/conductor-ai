//! Central transition table for workflow run FSM.
//!
//! Defines which state transitions are valid for WorkflowRunStatus.
//! Used as a warn-only guard (debug builds only) — invalid transitions
//! are logged but not rejected.
//!
//! Step-level transitions will be added when step guards are wired in.
//!
//! Part of: fsm-state-specification-template@1.0.0

use super::status::WorkflowRunStatus;

/// Check if a workflow run status transition is valid.
pub fn is_valid_run_transition(from: &WorkflowRunStatus, to: &WorkflowRunStatus) -> bool {
    use WorkflowRunStatus::*;
    matches!(
        (from, to),
        (Pending, Running)
            | (Running, Completed)
            | (Running, Failed)
            | (Running, Waiting)
            | (Running, Cancelled)
            | (Waiting, Running)
            | (Waiting, Cancelled)
            | (Failed, Running) // resume
            | (Pending, Cancelled)
            | (Completed, Running) // restart
    )
}

/// Log a warning for an invalid transition (warn-only guard mode).
pub fn warn_invalid_run_transition(run_id: &str, from: &WorkflowRunStatus, to: &WorkflowRunStatus) {
    if !is_valid_run_transition(from, to) {
        tracing::warn!(
            run_id = %run_id,
            from = %from,
            to = %to,
            "invalid workflow run status transition detected"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use WorkflowRunStatus::*;
    // --- Run transition tests ---

    #[test]
    fn valid_run_transitions() {
        let valid = vec![
            (Pending, Running),
            (Running, Completed),
            (Running, Failed),
            (Running, Waiting),
            (Running, Cancelled),
            (Waiting, Running),
            (Waiting, Cancelled),
            (Failed, Running),
            (Pending, Cancelled),
            (Completed, Running),
        ];
        for (from, to) in valid {
            assert!(
                is_valid_run_transition(&from, &to),
                "{from} -> {to} should be valid"
            );
        }
    }

    #[test]
    fn invalid_run_transitions() {
        let invalid = vec![
            (Pending, Completed),
            (Pending, Failed),
            (Pending, Waiting),
            (Completed, Failed),
            (Completed, Pending),
            (Completed, Waiting),
            (Completed, Cancelled),
            (Failed, Completed),
            (Failed, Pending),
            (Failed, Waiting),
            (Failed, Cancelled),
            (Cancelled, Running),
            (Cancelled, Completed),
            (Cancelled, Failed),
            (Cancelled, Pending),
            (Cancelled, Waiting),
            (Waiting, Completed),
            (Waiting, Failed),
            (Waiting, Pending),
        ];
        for (from, to) in invalid {
            assert!(
                !is_valid_run_transition(&from, &to),
                "{from} -> {to} should be invalid"
            );
        }
    }

    #[test]
    fn self_transitions_are_invalid_for_runs() {
        let all = vec![Pending, Running, Completed, Failed, Cancelled, Waiting];
        for s in all {
            assert!(
                !is_valid_run_transition(&s, &s),
                "{s} -> {s} self-transition should be invalid"
            );
        }
    }
}
