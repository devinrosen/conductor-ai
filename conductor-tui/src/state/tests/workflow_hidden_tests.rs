use super::*;
use conductor_core::workflow::WorkflowRunStatus;

#[test]
fn init_collapse_state_running_not_collapsed() {
    let mut state = AppState::new();
    state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Running, None)];
    state.init_collapse_state();
    assert!(!state.collapsed_workflow_run_ids.contains("p1"));
}

#[test]
fn init_collapse_state_terminal_statuses_collapsed() {
    for status in [
        WorkflowRunStatus::Completed,
        WorkflowRunStatus::Failed,
        WorkflowRunStatus::Cancelled,
    ] {
        let mut state = AppState::new();
        state.data.workflow_runs = vec![make_wf_run_full("p1", status.clone(), None)];
        state.init_collapse_state();
        assert!(
            state.collapsed_workflow_run_ids.contains("p1"),
            "expected p1 collapsed for {status:?}"
        );
    }
}

#[test]
fn init_collapse_state_idempotent() {
    let mut state = AppState::new();
    state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Completed, None)];
    state.init_collapse_state();
    assert!(state.collapsed_workflow_run_ids.contains("p1"));
    state.collapsed_workflow_run_ids.remove("p1");
    state.init_collapse_state();
    assert!(
        !state.collapsed_workflow_run_ids.contains("p1"),
        "second init_collapse_state call must not re-collapse an already-initialized run"
    );
}

#[test]
fn init_collapse_state_child_runs_not_collapsed() {
    let mut state = AppState::new();
    state.data.workflow_runs = vec![make_wf_run_full(
        "c1",
        WorkflowRunStatus::Completed,
        Some("p1"),
    )];
    state.init_collapse_state();
    assert!(!state.collapsed_workflow_run_ids.contains("c1"));
}

#[test]
fn init_collapse_state_running_leaf_auto_expanded() {
    let mut state = AppState::new();
    state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Running, None)];
    state.init_collapse_state();
    assert!(
        state.expanded_step_run_ids.contains("p1"),
        "running leaf run must be auto-expanded into expanded_step_run_ids"
    );
    assert!(!state.collapsed_workflow_run_ids.contains("p1"));
}

#[test]
fn init_collapse_state_running_non_leaf_not_auto_expanded() {
    let mut state = AppState::new();
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
    ];
    state.init_collapse_state();
    assert!(
        !state.expanded_step_run_ids.contains("p1"),
        "running non-leaf run must NOT be auto-expanded into expanded_step_run_ids"
    );
}
