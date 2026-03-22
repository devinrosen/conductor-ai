//! Central transition tables for workflow FSM state machines.
//!
//! Defines which state transitions are valid for WorkflowRunStatus and
//! WorkflowStepStatus. Used as a warn-only guard initially — invalid
//! transitions are logged but not rejected until the full test suite
//! confirms zero unexpected warnings.
//!
//! Part of: fsm-state-specification-template@1.0.0

use super::status::{WorkflowRunStatus, WorkflowStepStatus};

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

/// Check if a workflow step status transition is valid.
pub fn is_valid_step_transition(from: &WorkflowStepStatus, to: &WorkflowStepStatus) -> bool {
    use WorkflowStepStatus::*;
    matches!(
        (from, to),
        (Pending, Running)
            | (Pending, Skipped)
            | (Pending, Completed) // quality gate direct eval
            | (Pending, Failed)    // quality gate direct fail
            | (Running, Completed)
            | (Running, Failed)
            | (Running, Waiting)
            | (Running, TimedOut)
            | (Running, Skipped) // dry-run skip
            | (Waiting, Completed)
            | (Waiting, Failed)
            | (Failed, Pending)    // resume reset
            | (TimedOut, Pending)  // resume reset
            | (Waiting, Pending)   // resume reset
            | (Completed, Pending) // restart reset
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

/// Log a warning for an invalid step transition (warn-only guard mode).
pub fn warn_invalid_step_transition(
    step_id: &str,
    from: &WorkflowStepStatus,
    to: &WorkflowStepStatus,
) {
    if !is_valid_step_transition(from, to) {
        tracing::warn!(
            step_id = %step_id,
            from = %from,
            to = %to,
            "invalid workflow step status transition detected"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use WorkflowRunStatus::*;
    use WorkflowStepStatus as SS;

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

    // --- Step transition tests ---

    #[test]
    fn valid_step_transitions() {
        let valid = vec![
            (SS::Pending, SS::Running),
            (SS::Pending, SS::Skipped),
            (SS::Pending, SS::Completed),
            (SS::Pending, SS::Failed),
            (SS::Running, SS::Completed),
            (SS::Running, SS::Failed),
            (SS::Running, SS::Waiting),
            (SS::Running, SS::TimedOut),
            (SS::Running, SS::Skipped),
            (SS::Waiting, SS::Completed),
            (SS::Waiting, SS::Failed),
            (SS::Failed, SS::Pending),
            (SS::TimedOut, SS::Pending),
            (SS::Waiting, SS::Pending),
            (SS::Completed, SS::Pending),
        ];
        for (from, to) in valid {
            assert!(
                is_valid_step_transition(&from, &to),
                "{from} -> {to} should be valid"
            );
        }
    }

    #[test]
    fn invalid_step_transitions() {
        let invalid = vec![
            (SS::Completed, SS::Running),
            (SS::Completed, SS::Failed),
            (SS::Completed, SS::Skipped),
            (SS::Failed, SS::Running),
            (SS::Failed, SS::Completed),
            (SS::Skipped, SS::Running),
            (SS::Skipped, SS::Pending),
            (SS::TimedOut, SS::Running),
            (SS::TimedOut, SS::Completed),
        ];
        for (from, to) in invalid {
            assert!(
                !is_valid_step_transition(&from, &to),
                "{from} -> {to} should be invalid"
            );
        }
    }

    #[test]
    fn self_transitions_are_invalid_for_steps() {
        let all = vec![
            SS::Pending,
            SS::Running,
            SS::Completed,
            SS::Failed,
            SS::Skipped,
            SS::Waiting,
            SS::TimedOut,
        ];
        for s in all {
            assert!(
                !is_valid_step_transition(&s, &s),
                "{s} -> {s} self-transition should be invalid"
            );
        }
    }
}
