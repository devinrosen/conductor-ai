use super::*;
use crate::workflow_dsl::{CallWorkflowNode, Condition, GateType, IfNode, WorkflowNode};

#[test]
fn test_metadata_fields_basic() {
    let step = WorkflowRunStep {
        id: "s1".into(),
        workflow_run_id: "r1".into(),
        step_name: "lint".into(),
        role: "reviewer".into(),
        can_commit: false,
        condition_expr: None,
        status: WorkflowStepStatus::Completed,
        child_run_id: None,
        position: 1,
        started_at: Some("2025-01-01T00:00:00Z".into()),
        ended_at: Some("2025-01-01T00:01:00Z".into()),
        result_text: None,
        condition_met: None,
        iteration: 1,
        parallel_group_id: None,
        context_out: None,
        markers_out: None,
        retry_count: 0,
        gate_type: None,
        gate_prompt: None,
        gate_timeout: None,
        gate_approved_by: None,
        gate_approved_at: None,
        gate_feedback: None,
        structured_output: None,
        output_file: None,
        gate_options: None,
        gate_selections: None,
    };
    let entries = step.metadata_fields();
    assert_eq!(entries.len(), 6); // 4 always-present + Started + Ended
    assert_eq!(
        entries[0],
        MetadataEntry::Field {
            label: "Status",
            value: "completed".into()
        }
    );
    assert_eq!(
        entries[1],
        MetadataEntry::Field {
            label: "Role",
            value: "reviewer".into()
        }
    );
    assert_eq!(
        entries[2],
        MetadataEntry::Field {
            label: "Can commit",
            value: "false".into()
        }
    );
    assert_eq!(
        entries[3],
        MetadataEntry::Field {
            label: "Iteration",
            value: "1".into()
        }
    );
    assert_eq!(
        entries[4],
        MetadataEntry::Field {
            label: "Started",
            value: "2025-01-01T00:00:00Z".into()
        }
    );
    assert_eq!(
        entries[5],
        MetadataEntry::Field {
            label: "Ended",
            value: "2025-01-01T00:01:00Z".into()
        }
    );
    // No gate or section entries
    assert!(!entries
        .iter()
        .any(|e| matches!(e, MetadataEntry::Section { .. })));
}

#[test]
fn test_metadata_fields_optional_sections() {
    let step = WorkflowRunStep {
        id: "s2".into(),
        workflow_run_id: "r1".into(),
        step_name: "review".into(),
        role: "reviewer".into(),
        can_commit: false,
        condition_expr: None,
        status: WorkflowStepStatus::Running,
        child_run_id: None,
        position: 2,
        started_at: None,
        ended_at: None,
        result_text: Some("All good".into()),
        condition_met: None,
        iteration: 0,
        parallel_group_id: None,
        context_out: Some("ctx data".into()),
        markers_out: Some("marker1".into()),
        retry_count: 0,
        gate_type: Some(GateType::HumanApproval),
        gate_prompt: Some("Please approve".into()),
        gate_timeout: None,
        gate_approved_by: None,
        gate_approved_at: None,
        gate_feedback: Some("Looks good".into()),
        structured_output: None,
        output_file: None,
        gate_options: None,
        gate_selections: None,
    };
    let entries = step.metadata_fields();
    assert!(entries.contains(&MetadataEntry::Field {
        label: "Gate type",
        value: "human_approval".into()
    }));
    assert!(entries.contains(&MetadataEntry::Section {
        heading: "Gate Prompt",
        body: "Please approve".into()
    }));
    assert!(entries.contains(&MetadataEntry::Section {
        heading: "Gate Feedback",
        body: "Looks good".into()
    }));
    assert!(entries.contains(&MetadataEntry::Section {
        heading: "Result",
        body: "All good".into()
    }));
    assert!(entries.contains(&MetadataEntry::Section {
        heading: "Context Out",
        body: "ctx data".into()
    }));
    assert!(entries.contains(&MetadataEntry::Section {
        heading: "Markers Out",
        body: "marker1".into()
    }));
}

#[test]
fn test_get_completed_step_keys() {
    let conn = setup_db();
    let (run_id, mgr) = setup_run_with_steps(&conn);

    let keys = mgr.get_completed_step_keys(&run_id).unwrap();
    assert_eq!(keys.len(), 1);
    assert!(keys.contains(&("step-a".to_string(), 0)));
    // Failed/running steps should not be in the set
    assert!(!keys.contains(&("step-b".to_string(), 0)));
    assert!(!keys.contains(&("step-c".to_string(), 0)));
}

#[test]
fn test_call_workflow_propagates_feature_id_to_child() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    // Create a temp dir with a child workflow file (empty body, so it completes instantly).
    let tmp = tempfile::tempdir().unwrap();
    let wf_dir = tmp.path().join(".conductor/workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(
        wf_dir.join("child.wf"),
        "workflow child { meta { targets = [\"worktree\"] } }",
    )
    .unwrap();
    let working_dir = tmp.path().to_str().unwrap();

    // Insert a feature for repo r1 (created by setup_db).
    conn.execute(
        "INSERT INTO features (id, repo_id, name, branch, base_branch, status, created_at) \
         VALUES ('f1', 'r1', 'my-feature', 'feat/my-feature', 'main', 'active', '2025-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    // Parent workflow that calls the child.
    let mut parent = make_empty_workflow();
    parent
        .body
        .push(WorkflowNode::CallWorkflow(CallWorkflowNode {
            workflow: "child".into(),
            inputs: HashMap::new(),
            retries: 0,
            on_fail: None,
            bot_name: None,
        }));

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &parent,
        worktree_id: None,
        working_dir,
        repo_path: "",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: Some("f1"),
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        extra_plugin_dirs: vec![],
        force: false,
    };
    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);

    // Find the child run by querying for runs whose parent is our parent run.
    use rusqlite::params;
    let child_run_id: String = conn
        .query_row(
            "SELECT id FROM workflow_runs WHERE parent_workflow_run_id = ?1",
            params![result.workflow_run_id],
            |row| row.get(0),
        )
        .expect("child run should exist");
    let child_run = wf_mgr
        .get_workflow_run(&child_run_id)
        .unwrap()
        .expect("child run should exist");
    assert_eq!(
        child_run.feature_id.as_deref(),
        Some("f1"),
        "child run should inherit feature_id from parent"
    );
    assert_eq!(
        child_run.inputs.get("feature_id").map(String::as_str),
        Some("f1"),
        "child run should have feature_id in its inputs"
    );
    assert_eq!(
        child_run.inputs.get("feature_name").map(String::as_str),
        Some("my-feature"),
        "child run should have feature_name in its inputs"
    );
    assert_eq!(
        child_run.inputs.get("feature_branch").map(String::as_str),
        Some("feat/my-feature"),
        "child run should have feature_branch in its inputs"
    );
}

#[test]
fn test_call_workflow_propagates_triggered_by_hook_to_child() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    // Create a temp dir with a child workflow file.
    let tmp = tempfile::tempdir().unwrap();
    let wf_dir = tmp.path().join(".conductor/workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(
        wf_dir.join("child.wf"),
        "workflow child { meta { targets = [\"worktree\"] } }",
    )
    .unwrap();
    let working_dir = tmp.path().to_str().unwrap();

    // Parent workflow that calls the child, triggered by hook.
    let mut parent = make_empty_workflow();
    parent
        .body
        .push(WorkflowNode::CallWorkflow(CallWorkflowNode {
            workflow: "child".into(),
            inputs: HashMap::new(),
            retries: 0,
            on_fail: None,
            bot_name: None,
        }));

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &parent,
        worktree_id: None,
        working_dir,
        repo_path: "",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: true,
        conductor_bin_dir: None,
        extra_plugin_dirs: vec![],
        force: false,
    };
    let result = execute_workflow(&input).unwrap();
    assert!(result.all_succeeded);

    // Parent run must have trigger='hook'.
    let wf_mgr = WorkflowManager::new(&conn);
    let parent_run = wf_mgr
        .get_workflow_run(&result.workflow_run_id)
        .unwrap()
        .expect("parent run should exist");
    assert!(
        parent_run.is_triggered_by_hook(),
        "parent run should have trigger='hook'"
    );

    // Child run must also have trigger='hook' (propagated via triggered_by_hook).
    use rusqlite::params;
    let child_run_id: String = conn
        .query_row(
            "SELECT id FROM workflow_runs WHERE parent_workflow_run_id = ?1",
            params![result.workflow_run_id],
            |row| row.get(0),
        )
        .expect("child run should exist");
    let child_run = wf_mgr
        .get_workflow_run(&child_run_id)
        .unwrap()
        .expect("child run should exist");
    assert_eq!(
        child_run.trigger, "hook",
        "child run should inherit trigger='hook' from parent"
    );
    assert!(
        child_run.is_triggered_by_hook(),
        "child run should be marked as triggered by hook"
    );
}

// ---------------------------------------------------------------------------
// evaluate_hooks integration tests
// ---------------------------------------------------------------------------

/// Helper: set up a temp dir with `.conductor/config.toml` and optional workflow files.
fn setup_hooks_dir(config_toml: &str, workflows: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let conductor_dir = dir.path().join(".conductor");
    std::fs::create_dir_all(conductor_dir.join("workflows")).unwrap();
    std::fs::write(conductor_dir.join("config.toml"), config_toml).unwrap();
    for (name, content) in workflows {
        std::fs::write(conductor_dir.join("workflows").join(name), content).unwrap();
    }
    dir
}

#[test]
fn test_hook_chain_prevention_when_triggered_by_hook() {
    // When triggered_by_hook is true, hooks should NOT fire (prevents infinite chains).
    let dir = setup_hooks_dir(
        r#"
[hooks.test-wf]
on_complete = "should-not-fire"
"#,
        &[(
            "should-not-fire.wf",
            r#"workflow should-not-fire {
  meta {
    description = "should never run"
    trigger = "manual"
    targets = ["worktree"]
  }
}"#,
        )],
    );

    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let dir_path = dir.path().to_str().unwrap();

    let workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: dir_path,
        repo_path: dir_path,
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: true,
        conductor_bin_dir: None,
        extra_plugin_dirs: vec![],
        force: false,
    };

    let result = execute_workflow(&input).unwrap();
    assert!(result.all_succeeded);

    // Verify no hook workflow run was created — only the main run should exist.
    // Query all runs directly (no worktree_id filter).
    let all_runs: Vec<WorkflowRun> = crate::db::query_collect(
        &conn,
        &format!(
            "SELECT {} FROM workflow_runs ORDER BY started_at",
            crate::workflow::constants::RUN_COLUMNS
        ),
        [],
        crate::workflow::manager::row_to_workflow_run,
    )
    .unwrap();
    assert_eq!(
        all_runs.len(),
        1,
        "only the main run should exist (no hook run)"
    );
    assert!(
        all_runs[0].is_triggered_by_hook(),
        "main run should have trigger='hook'"
    );
}

#[test]
fn test_hook_skips_missing_workflow() {
    // When hooks config references a workflow that doesn't exist, the main
    // workflow should still complete successfully.
    let dir = setup_hooks_dir(
        r#"
[hooks.test-wf]
on_complete = "nonexistent-hook-wf"
"#,
        &[], // no workflow files
    );

    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let dir_path = dir.path().to_str().unwrap();

    let workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: dir_path,
        repo_path: dir_path,
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        extra_plugin_dirs: vec![],
        force: false,
    };

    let result = execute_workflow(&input).unwrap();
    assert!(
        result.all_succeeded,
        "main workflow should succeed even when hook workflow is missing"
    );
}

#[test]
fn test_hook_fires_on_complete() {
    // When a top-level workflow completes and hooks config has an on_complete
    // entry, the hook workflow should be triggered with trigger='hook'.
    let dir = setup_hooks_dir(
        r#"
[hooks.test-wf]
on_complete = "post-complete"
"#,
        &[(
            "post-complete.wf",
            r#"workflow post-complete {
  meta {
    description = "post-complete hook"
    trigger = "manual"
    targets = ["worktree"]
  }
}"#,
        )],
    );

    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let dir_path = dir.path().to_str().unwrap();

    let workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: dir_path,
        repo_path: dir_path,
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        extra_plugin_dirs: vec![],
        force: false,
    };

    let result = execute_workflow(&input).unwrap();
    assert!(result.all_succeeded);

    // Verify that a hook workflow run was created with trigger='hook'.
    let all_runs: Vec<WorkflowRun> = crate::db::query_collect(
        &conn,
        &format!(
            "SELECT {} FROM workflow_runs ORDER BY started_at",
            crate::workflow::constants::RUN_COLUMNS
        ),
        [],
        crate::workflow::manager::row_to_workflow_run,
    )
    .unwrap();
    assert_eq!(all_runs.len(), 2, "main + hook run should exist");

    let hook_run = all_runs
        .iter()
        .find(|r| r.workflow_name == "post-complete")
        .expect("hook workflow run should exist");
    assert_eq!(hook_run.trigger, "hook");
    assert!(hook_run.is_triggered_by_hook());
    assert_eq!(
        hook_run.parent_workflow_run_id.as_deref(),
        Some(result.workflow_run_id.as_str()),
        "hook run should link to parent"
    );
}

/// Regression test: execute_nodes must stop immediately when the workflow run
/// has been externally cancelled (e.g. via the TUI or web cancel button).
/// Before the fix, a cancelled run would continue executing until it finished
/// naturally, leaving it stuck in `pending` or `running` status.
#[test]
fn test_execute_nodes_stops_on_external_cancel() {
    let conn = setup_db();
    let config: &'static Config = Box::leak(Box::new(Config::default()));
    let mut state = make_loop_test_state(&conn, config);

    // Mark the run as cancelled before any nodes execute.
    WorkflowManager::new(&conn)
        .update_workflow_status(&state.workflow_run_id, WorkflowRunStatus::Cancelled, None)
        .unwrap();

    // Any node will do — cancellation is detected before execute_single_node is called.
    let nodes = vec![WorkflowNode::If(IfNode {
        condition: Condition::BoolInput { input: "x".into() },
        body: vec![],
    })];

    let result = execute_nodes(&mut state, &nodes, true);
    assert!(result.is_err(), "cancelled run should return Err");
    assert!(
        result.unwrap_err().to_string().contains("cancelled"),
        "error message should mention cancellation"
    );
}
