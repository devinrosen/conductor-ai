#![allow(unused_imports)]

use super::*;
use crate::agent::AgentManager;

#[test]
fn test_gate_approve() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let run = crate::workflow::create_workflow_run(
        &conn,
        "test",
        Some("w1"),
        &parent.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "human_review", "reviewer", false, 0, 0)
            .unwrap();
    crate::workflow::set_step_gate_info(
        &conn,
        &step_id,
        &GateType::HumanReview,
        Some("Review?"),
        "48h",
    )
    .unwrap();
    set_step_status(&conn, &step_id, WorkflowStepStatus::Waiting);

    // Find waiting gate
    let waiting = crate::workflow::find_waiting_gate(&conn, &run.id).unwrap();
    assert!(waiting.is_some());
    assert_eq!(waiting.unwrap().id, step_id);

    // Approve
    crate::workflow::approve_gate(&conn, &step_id, "user", Some("Looks good!"), None, None)
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
    let run = crate::workflow::create_workflow_run(
        &conn,
        "test",
        Some("w1"),
        &parent.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "human_approval", "reviewer", false, 0, 0)
            .unwrap();
    crate::workflow::set_step_gate_info(
        &conn,
        &step_id,
        &GateType::HumanApproval,
        Some("Approve?"),
        "24h",
    )
    .unwrap();
    set_step_status(&conn, &step_id, WorkflowStepStatus::Waiting);

    crate::workflow::reject_gate(&conn, &step_id, "user", None).unwrap();

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
    let run = crate::workflow::create_workflow_run(
        &conn,
        "test",
        Some("w1"),
        &parent.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "pr_approval_gate", "gate", false, 0, 0)
            .unwrap();
    crate::workflow::set_step_gate_info(&conn, &step_id, &GateType::PrApproval, None, "48h")
        .unwrap();
    set_step_status(&conn, &step_id, WorkflowStepStatus::Waiting);

    crate::workflow::approve_gate(
        &conn,
        &step_id,
        "reviewer-bot",
        Some("PR approved"),
        None,
        None,
    )
    .unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Completed);
    assert_eq!(step.gate_type, Some(GateType::PrApproval.to_string()));
    assert_eq!(step.gate_approved_by.as_deref(), Some("reviewer-bot"));
}

#[test]
fn test_gate_pr_approval_reject() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let run = crate::workflow::create_workflow_run(
        &conn,
        "test",
        Some("w1"),
        &parent.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "pr_approval_gate", "gate", false, 0, 0)
            .unwrap();
    crate::workflow::set_step_gate_info(&conn, &step_id, &GateType::PrApproval, None, "24h")
        .unwrap();
    set_step_status(&conn, &step_id, WorkflowStepStatus::Waiting);

    crate::workflow::reject_gate(&conn, &step_id, "reviewer", Some("Changes requested")).unwrap();

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
    let run = crate::workflow::create_workflow_run(
        &conn,
        "test",
        Some("w1"),
        &parent.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "pr_checks_gate", "gate", false, 0, 0)
            .unwrap();
    crate::workflow::set_step_gate_info(&conn, &step_id, &GateType::PrChecks, None, "1h").unwrap();
    set_step_status(&conn, &step_id, WorkflowStepStatus::Waiting);

    crate::workflow::approve_gate(
        &conn,
        &step_id,
        "ci-bot",
        Some("All checks passed"),
        None,
        None,
    )
    .unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Completed);
    assert_eq!(step.gate_type, Some(GateType::PrChecks.to_string()));
}

// ---------------------------------------------------------------------------
// Multi-select gate options
// ---------------------------------------------------------------------------

#[test]
fn test_gate_multiselect_options_and_approval() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let run = crate::workflow::create_workflow_run(
        &conn,
        "test",
        Some("w1"),
        &parent.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "pick_items", "gate", false, 0, 0).unwrap();
    crate::workflow::set_step_gate_info(
        &conn,
        &step_id,
        &GateType::HumanReview,
        Some("Select items:"),
        "1h",
    )
    .unwrap();

    // Simulate the executor persisting resolved options
    let options_json = r#"[{"value":"finding-a","label":"finding-a"},{"value":"finding-b","label":"finding-b"},{"value":"finding-c","label":"finding-c"}]"#;
    crate::workflow::set_step_gate_options(&conn, &step_id, options_json).unwrap();
    set_step_status(&conn, &step_id, WorkflowStepStatus::Waiting);

    // Verify gate_options are stored and step is waiting
    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.status, WorkflowStepStatus::Waiting);
    assert!(step.gate_options.is_some(), "gate_options should be set");

    // Approve with a subset of selections
    let selections = vec!["finding-a".to_string(), "finding-c".to_string()];
    let context_out = crate::workflow::helpers::format_gate_selection_context(&selections);
    crate::workflow::approve_gate(
        &conn,
        &step_id,
        "user",
        None,
        Some(&selections),
        Some(context_out),
    )
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
    let run = crate::workflow::create_workflow_run(
        &conn,
        "test",
        Some("w1"),
        &parent.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "pick_items", "gate", false, 0, 0).unwrap();
    crate::workflow::set_step_gate_info(
        &conn,
        &step_id,
        &GateType::HumanReview,
        Some("Select items:"),
        "1h",
    )
    .unwrap();

    let options_json = r#"[{"value":"item-x","label":"item-x"}]"#;
    crate::workflow::set_step_gate_options(&conn, &step_id, options_json).unwrap();
    set_step_status(&conn, &step_id, WorkflowStepStatus::Waiting);

    // Approve with empty selections (skip all)
    crate::workflow::approve_gate(&conn, &step_id, "user", None, Some(&[]), None).unwrap();

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

/// Poll `find_waiting_gate` until the run reaches a gate, panicking after 20s.
fn wait_for_gate(db_path: &std::path::Path, run_id: &str) -> WorkflowRunStep {
    let conn = crate::db::open_database(db_path).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(step) = crate::workflow::find_waiting_gate(&conn, run_id).unwrap() {
            return step;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for a gate on run {run_id}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Regression for #3151: gates started by the real engine must record their
/// type, prompt and timeout so `find_waiting_gate` and `approve_gate` work, both
/// for an unconditional gate and for one nested in an `if` body.
#[test]
fn test_engine_gates_are_findable_and_approvable() {
    use std::os::unix::fs::PermissionsExt;

    let (_tmp, db_path) = make_standalone_db();
    let ws = tempfile::tempdir().unwrap();
    let script = ws.path().join("emit.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho '<<<FLOW_OUTPUT>>>'\n\
         echo '{\"markers\": [\"needs_review\"], \"context\": \"ok\"}'\n\
         echo '<<<END_FLOW_OUTPUT>>>'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let run_path = script.to_str().unwrap();
    let def = runkon_flow::dsl::parse_workflow_str(
        &format!(
            r#"workflow gate-wf {{
                meta {{ trigger = "manual" targets = ["worktree"] }}
                script a {{ run = "{run_path}" }}
                gate human_approval {{ prompt = "First approval" timeout = "1h" }}
                if a.needs_review {{
                    gate human_review {{ prompt = "Conditional approval" timeout = "2h" }}
                }}
            }}"#
        ),
        "<test>",
    )
    .expect("gate workflow should parse");

    let params = crate::workflow::types::WorkflowExecStandalone {
        config: Config::default(),
        workflow: def,
        worktree_id: None,
        working_dir: ws.path().to_str().unwrap().to_string(),
        repo_path: ws.path().to_str().unwrap().to_string(),
        ticket_id: None,
        repo_id: None,
        model: None,
        runtime: None,
        exec_config: crate::workflow::WorkflowExecConfig {
            poll_interval: std::time::Duration::from_millis(50),
            ..Default::default()
        },
        inputs: std::collections::HashMap::new(),
        target_label: None,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
        extra_plugin_dirs: vec![],
        db_path: Some(db_path.clone()),
        parent_workflow_run_id: None,
        depth: 0,
        parent_step_id: None,
        default_bot_name: None,
        iteration: 0,
    };
    let handle = std::thread::spawn(move || crate::workflow::execute_workflow_standalone(&params));

    // The run row is created before any step executes.
    let conn = crate::db::open_database(&db_path).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let run_id = loop {
        if let Ok(id) = conn.query_row("SELECT id FROM workflow_runs LIMIT 1", [], |r| {
            r.get::<_, String>(0)
        }) {
            break id;
        }
        assert!(std::time::Instant::now() < deadline, "run never created");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };

    let first = wait_for_gate(&db_path, &run_id);
    assert_eq!(first.gate_type.as_deref(), Some("human_approval"));
    assert_eq!(first.gate_prompt.as_deref(), Some("First approval"));
    assert_eq!(first.gate_timeout.as_deref(), Some("3600s"));
    crate::workflow::approve_gate(&conn, &first.id, "test", None, None, None).unwrap();

    let second = loop {
        let s = wait_for_gate(&db_path, &run_id);
        if s.id != first.id {
            break s;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(second.gate_type.as_deref(), Some("human_review"));
    assert_eq!(second.gate_prompt.as_deref(), Some("Conditional approval"));
    assert_eq!(second.gate_timeout.as_deref(), Some("7200s"));
    crate::workflow::approve_gate(&conn, &second.id, "test", None, None, None).unwrap();

    let result = handle
        .join()
        .expect("workflow thread panicked")
        .expect("workflow should complete");
    assert!(result.all_succeeded, "run should succeed after approvals");
}
