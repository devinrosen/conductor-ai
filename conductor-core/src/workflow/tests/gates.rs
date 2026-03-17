#![allow(unused_imports)]

use super::*;
use crate::agent::AgentManager;

#[test]
fn test_gate_approve() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "human_review", "reviewer", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, "human_review", Some("Review?"), "48h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    // Find waiting gate
    let waiting = mgr.find_waiting_gate(&run.id).unwrap();
    assert!(waiting.is_some());
    assert_eq!(waiting.unwrap().id, step_id);

    // Approve
    mgr.approve_gate(&step_id, "user", Some("Looks good!"))
        .unwrap();

    // Verify
    let steps = mgr.get_workflow_steps(&run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Completed);
    assert!(steps[0].gate_approved_at.is_some());
    assert_eq!(steps[0].gate_approved_by.as_deref(), Some("user"));
    assert_eq!(steps[0].gate_feedback.as_deref(), Some("Looks good!"));
}

#[test]
fn test_gate_reject() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "human_approval", "reviewer", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, "human_approval", Some("Approve?"), "24h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    mgr.reject_gate(&step_id, "user", None).unwrap();

    let steps = mgr.get_workflow_steps(&run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
}

#[test]
fn test_gate_timeout_fail() {
    let conn = setup_db();
    let config = make_resume_config();
    let (mut state, run_id) = make_state_with_run(&conn, config);

    let wf_mgr = WorkflowManager::new(&conn);
    let step_id = wf_mgr
        .insert_step(&run_id, "test_gate", "gate", false, 0, 0)
        .unwrap();
    set_step_status(&wf_mgr, &step_id, WorkflowStepStatus::Waiting);

    let node = make_gate_node(GateType::HumanApproval, OnTimeout::Fail);
    let result = handle_gate_timeout(&mut state, &step_id, &node);

    assert!(result.is_err());
    let steps = wf_mgr.get_workflow_steps(&run_id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
    assert!(!state.all_succeeded);
}

#[test]
fn test_gate_timeout_continue() {
    let conn = setup_db();
    let config = make_resume_config();
    let (mut state, run_id) = make_state_with_run(&conn, config);

    let wf_mgr = WorkflowManager::new(&conn);
    let step_id = wf_mgr
        .insert_step(&run_id, "test_gate", "gate", false, 0, 0)
        .unwrap();
    set_step_status(&wf_mgr, &step_id, WorkflowStepStatus::Waiting);

    let node = make_gate_node(GateType::HumanApproval, OnTimeout::Continue);
    let result = handle_gate_timeout(&mut state, &step_id, &node);

    assert!(result.is_ok(), "on_timeout=continue should return Ok");
    let steps = wf_mgr.get_workflow_steps(&run_id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::TimedOut);
    assert!(
        state.all_succeeded,
        "on_timeout=continue should not set all_succeeded=false"
    );
}
