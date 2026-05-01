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
    mgr.set_step_gate_info(&step_id, GateType::HumanReview, Some("Review?"), "48h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    // Find waiting gate
    let waiting = crate::workflow::find_waiting_gate(&conn, &run.id).unwrap();
    assert!(waiting.is_some());
    assert_eq!(waiting.unwrap().id, step_id);

    // Approve
    mgr.approve_gate(&step_id, "user", Some("Looks good!"), None, None)
        .unwrap();

    // Verify
    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
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
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, Some("Approve?"), "24h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    mgr.reject_gate(&step_id, "user", None).unwrap();

    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
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
    mgr.set_step_gate_info(&step_id, GateType::PrApproval, None, "48h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    mgr.approve_gate(&step_id, "reviewer-bot", Some("PR approved"), None, None)
        .unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Completed);
    assert_eq!(step.gate_type, Some(GateType::PrApproval));
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
    mgr.set_step_gate_info(&step_id, GateType::PrApproval, None, "24h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    mgr.reject_gate(&step_id, "reviewer", Some("Changes requested"))
        .unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
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
    mgr.set_step_gate_info(&step_id, GateType::PrChecks, None, "1h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    mgr.approve_gate(&step_id, "ci-bot", Some("All checks passed"), None, None)
        .unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Completed);
    assert_eq!(step.gate_type, Some(GateType::PrChecks));
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
    mgr.set_step_gate_info(&step_id, GateType::HumanReview, Some("Select items:"), "1h")
        .unwrap();

    // Simulate the executor persisting resolved options
    let options_json = r#"[{"value":"finding-a","label":"finding-a"},{"value":"finding-b","label":"finding-b"},{"value":"finding-c","label":"finding-c"}]"#;
    mgr.set_step_gate_options(&step_id, options_json).unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    // Verify gate_options are stored and step is waiting
    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Waiting);
    assert!(step.gate_options.is_some(), "gate_options should be set");

    // Approve with a subset of selections
    let selections = vec!["finding-a".to_string(), "finding-c".to_string()];
    let context_out = crate::workflow::helpers::format_gate_selection_context(&selections);
    mgr.approve_gate(&step_id, "user", None, Some(&selections), Some(context_out))
        .unwrap();

    // Verify post-approval state
    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
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
    mgr.set_step_gate_info(&step_id, GateType::HumanReview, Some("Select items:"), "1h")
        .unwrap();

    let options_json = r#"[{"value":"item-x","label":"item-x"}]"#;
    mgr.set_step_gate_options(&step_id, options_json).unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    // Approve with empty selections (skip all)
    mgr.approve_gate(&step_id, "user", None, Some(&[]), None)
        .unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Completed);
    // Empty selections → no context_out injected
    assert!(
        step.context_out.is_none(),
        "empty selections should not produce context_out"
    );
}
