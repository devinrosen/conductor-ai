#![allow(unused_imports)]

use super::*;
use crate::agent::AgentManager;

#[test]
fn test_gate_approve() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "human_review", "reviewer", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateKind::HumanReview, Some("Review?"), "48h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    // Find waiting gate
    let waiting = mgr.find_waiting_gate(&run.id).unwrap();
    assert!(waiting.is_some());
    assert_eq!(waiting.unwrap().id, step_id);

    // Approve
    mgr.approve_gate(&step_id, "user", Some("Looks good!"), None)
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
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "human_approval", "reviewer", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateKind::HumanApproval, Some("Approve?"), "24h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    mgr.reject_gate(&step_id, "user", None).unwrap();

    let steps = mgr.get_workflow_steps(&run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
}

// ---------------------------------------------------------------------------
// PrApproval gate type
// ---------------------------------------------------------------------------

#[test]
fn test_gate_pr_approval_approve() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "pr_approval_gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateKind::PrApproval, None, "48h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    mgr.approve_gate(&step_id, "reviewer-bot", Some("PR approved"), None)
        .unwrap();

    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Completed);
    assert_eq!(step.gate_type, Some(GateKind::PrApproval));
    assert_eq!(step.gate_approved_by.as_deref(), Some("reviewer-bot"));
}

#[test]
fn test_gate_pr_approval_reject() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "pr_approval_gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateKind::PrApproval, None, "24h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    mgr.reject_gate(&step_id, "reviewer", Some("Changes requested"))
        .unwrap();

    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Failed);
    assert_eq!(step.gate_feedback.as_deref(), Some("Changes requested"));
}

// ---------------------------------------------------------------------------
// PrChecks gate type
// ---------------------------------------------------------------------------

#[test]
fn test_gate_pr_checks_approve() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "pr_checks_gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateKind::PrChecks, None, "1h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    mgr.approve_gate(&step_id, "ci-bot", Some("All checks passed"), None)
        .unwrap();

    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Completed);
    assert_eq!(step.gate_type, Some(GateKind::PrChecks));
}

// ---------------------------------------------------------------------------
// Multi-select gate options
// ---------------------------------------------------------------------------

#[test]
fn test_gate_multiselect_options_and_approval() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "pick_items", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateKind::HumanReview, Some("Select items:"), "1h")
        .unwrap();

    // Simulate the executor persisting resolved options
    let options_json = r#"[{"value":"finding-a","label":"finding-a"},{"value":"finding-b","label":"finding-b"},{"value":"finding-c","label":"finding-c"}]"#;
    mgr.set_step_gate_options(&step_id, options_json).unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    // Verify gate_options are stored and step is waiting
    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Waiting);
    assert!(step.gate_options.is_some(), "gate_options should be set");

    // Approve with a subset of selections
    let selections = vec!["finding-a".to_string(), "finding-c".to_string()];
    mgr.approve_gate(&step_id, "user", None, Some(&selections))
        .unwrap();

    // Verify post-approval state
    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Completed);
    assert!(step.gate_approved_at.is_some());
    assert_eq!(step.gate_approved_by.as_deref(), Some("user"));

    // Verify selections were persisted as JSON
    let stored: Vec<String> =
        serde_json::from_str(step.gate_selections.as_deref().unwrap()).unwrap();
    assert_eq!(stored, vec!["finding-a", "finding-c"]);

    // Verify context_out was built from selections
    let ctx = step.context_out.as_deref().unwrap();
    assert!(
        ctx.contains("- finding-a"),
        "context_out missing finding-a: {ctx}"
    );
    assert!(
        ctx.contains("- finding-c"),
        "context_out missing finding-c: {ctx}"
    );
    assert!(
        !ctx.contains("finding-b"),
        "context_out should not include unselected item"
    );
}

#[test]
fn test_gate_approve_empty_selections() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "pick_items", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateKind::HumanReview, Some("Select items:"), "1h")
        .unwrap();

    let options_json = r#"[{"value":"item-x","label":"item-x"}]"#;
    mgr.set_step_gate_options(&step_id, options_json).unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    // Approve with empty selections (skip all)
    mgr.approve_gate(&step_id, "user", None, Some(&[])).unwrap();

    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Completed);
    // Empty selections → no context_out injected
    assert!(
        step.context_out.is_none(),
        "empty selections should not produce context_out"
    );
}
