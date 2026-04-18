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
        input_tokens: None,
        output_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        fan_out_total: None,
        fan_out_completed: 0,
        fan_out_failed: 0,
        fan_out_skipped: 0,
        step_error: None,
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
        input_tokens: None,
        output_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        fan_out_total: None,
        fan_out_completed: 0,
        fan_out_failed: 0,
        fan_out_skipped: 0,
        step_error: None,
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
    let child_run_id: String = conn
        .query_row(
            "SELECT id FROM workflow_runs WHERE parent_workflow_run_id = :id",
            rusqlite::named_params! { ":id": result.workflow_run_id },
            |row| row.get("id"),
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
// call_workflow resume regression tests
// ---------------------------------------------------------------------------

/// Build a minimal ExecutionState for a given parent workflow run.
fn make_call_wf_state<'a>(
    conn: &'a rusqlite::Connection,
    config: &'a Config,
    working_dir: &str,
    workflow_run_id: String,
    parent_run_id: String,
) -> ExecutionState<'a> {
    ExecutionState {
        conn,
        config,
        workflow_run_id,
        workflow_name: "parent-wf".into(),
        worktree_id: None,
        working_dir: working_dir.to_string(),
        worktree_slug: String::new(),
        repo_path: working_dir.to_string(),
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: WorkflowExecConfig {
            fail_fast: false,
            ..WorkflowExecConfig::default()
        },
        inputs: HashMap::new(),
        agent_mgr: crate::agent::AgentManager::new(conn),
        wf_mgr: WorkflowManager::new(conn),
        parent_run_id,
        depth: 0,
        target_label: None,
        step_results: HashMap::new(),
        contexts: Vec::new(),
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_input_tokens: 0,
        total_cache_creation_input_tokens: 0,
        last_gate_feedback: None,
        block_output: None,
        block_with: Vec::new(),
        resume_ctx: None,
        default_bot_name: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        extra_plugin_dirs: vec![],
        last_heartbeat_at: ExecutionState::new_heartbeat(),
    }
}

/// Creates a tempdir with `.conductor/workflows/child.wf` on disk.
/// Returns `(TempDir, dir_path_string)` — caller must keep `TempDir` alive.
fn setup_child_wf_dir() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let wf_dir = tmp.path().join(".conductor/workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(
        wf_dir.join("child.wf"),
        "workflow child { meta { targets = [\"worktree\"] } }",
    )
    .unwrap();
    let dir = tmp.path().to_str().unwrap().to_string();
    (tmp, dir)
}

/// Asserts that exactly one child workflow run exists under `parent_run_id`.
fn assert_no_new_child_run(conn: &rusqlite::Connection, parent_run_id: &str, msg: &str) {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM workflow_runs WHERE parent_workflow_run_id = ?1",
            rusqlite::params![parent_run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "{}", msg);
}

/// Regression: when a resumed call_workflow step's child run fails again
/// (all_succeeded=false), execute_call_workflow must NOT fall through to the
/// retry loop. It should record failure and return immediately.
#[test]
fn test_call_workflow_resume_failure_stops_without_new_child() {
    let conn = setup_db();
    let config = Config::default();

    // Temp dir with child.wf on disk (needed for load_workflow_by_name).
    let (_tmp, dir) = setup_child_wf_dir();

    // Register a real repo so the child run is not "ephemeral" and resume_workflow
    // can look up paths. (child.wf calls "nonexistent" → no matching file → body
    // errors → all_succeeded=false, but not an Err from resume_workflow itself.)
    let repo = crate::repo::RepoManager::new(&conn, &config)
        .register("test-repo-resume-fail", &dir, "", None)
        .unwrap();

    // Create parent workflow run.
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent_agent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let parent_run = wf_mgr
        .create_workflow_run("parent-wf", None, &parent_agent.id, false, "manual", None)
        .unwrap();

    // Build a definition_snapshot for the child that calls a non-existent
    // sub-workflow. When resumed, the body will error → all_succeeded=false.
    let child_snap = WorkflowDef {
        name: "child".into(),
        title: None,
        description: String::new(),
        trigger: WorkflowTrigger::Manual,
        targets: vec![],
        group: None,
        inputs: vec![],
        body: vec![WorkflowNode::CallWorkflow(CallWorkflowNode {
            workflow: "nonexistent".into(),
            inputs: HashMap::new(),
            retries: 0,
            on_fail: None,
            bot_name: None,
        })],
        always: vec![],
        source_path: "child.wf".into(),
    };
    let child_snapshot = serde_json::to_string(&child_snap).unwrap();

    // Create child workflow run linked to the parent, with repo_id set.
    let child_agent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    let child_run = wf_mgr
        .create_workflow_run_with_targets(
            "child",
            None,
            None,
            Some(repo.id.as_str()),
            &child_agent.id,
            false,
            "manual",
            Some(&child_snapshot),
            Some(parent_run.id.as_str()),
            None,
        )
        .unwrap();
    // Mark it failed so find_resumable_child_run picks it up.
    conn.execute(
        "UPDATE workflow_runs SET status = 'failed' WHERE id = ?1",
        rusqlite::params![child_run.id],
    )
    .unwrap();

    let mut state = make_call_wf_state(
        &conn,
        &config,
        &dir,
        parent_run.id.clone(),
        parent_agent.id.clone(),
    );
    let node = CallWorkflowNode {
        workflow: "child".into(),
        inputs: HashMap::new(),
        retries: 0,
        on_fail: None,
        bot_name: None,
    };

    execute_call_workflow(&mut state, &node, 0).unwrap();

    assert!(
        !state.all_succeeded,
        "state must be failed after resume failure"
    );
    assert_no_new_child_run(
        &conn,
        &parent_run.id,
        "resume failure must not spawn a new child run",
    );
}

/// Regression: when a resumed call_workflow step's child run errors during
/// resume (Err from resume_workflow), execute_call_workflow must NOT fall
/// through to the retry loop. It should record failure and return immediately.
#[test]
fn test_call_workflow_resume_error_stops_without_new_child() {
    let conn = setup_db();
    let config = Config::default();

    let (_tmp, dir) = setup_child_wf_dir();

    // Create parent workflow run.
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent_agent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let parent_run = wf_mgr
        .create_workflow_run("parent-wf", None, &parent_agent.id, false, "manual", None)
        .unwrap();

    // Insert a child run with no worktree/repo/ticket (ephemeral). When
    // resume_workflow is called on it, it returns Err immediately ("ephemeral PR
    // run with no registered worktree — cannot resume").
    insert_workflow_run(
        &conn,
        "child-run-resume-err",
        "child",
        "failed",
        Some(parent_run.id.as_str()),
    );

    let mut state = make_call_wf_state(
        &conn,
        &config,
        &dir,
        parent_run.id.clone(),
        parent_agent.id.clone(),
    );
    let node = CallWorkflowNode {
        workflow: "child".into(),
        inputs: HashMap::new(),
        retries: 0,
        on_fail: None,
        bot_name: None,
    };

    execute_call_workflow(&mut state, &node, 0).unwrap();

    assert!(
        !state.all_succeeded,
        "state must be failed after resume error"
    );
    assert_no_new_child_run(
        &conn,
        &parent_run.id,
        "resume error must not spawn a new child run",
    );
}

/// When a resume fails and the node has `on_fail` set, `run_on_fail_agent` must
/// be called. Verified by checking that a step for the on_fail agent is inserted
/// into the DB under the parent run.
#[test]
fn test_call_workflow_resume_failure_triggers_on_fail_agent() {
    let conn = setup_db();
    let config = Config::default();

    let (tmp, dir) = setup_child_wf_dir();

    // Create the on_fail agent file so load_agent succeeds and insert_step fires.
    let agents_dir = tmp.path().join(".conductor/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("on-fail-agent.md"), "Handle failure.").unwrap();

    // Create parent workflow run.
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent_agent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let parent_run = wf_mgr
        .create_workflow_run("parent-wf", None, &parent_agent.id, false, "manual", None)
        .unwrap();

    // Insert an ephemeral child run that causes resume_workflow to return Err.
    insert_workflow_run(
        &conn,
        "child-run-on-fail",
        "child",
        "failed",
        Some(parent_run.id.as_str()),
    );

    let mut state = make_call_wf_state(
        &conn,
        &config,
        &dir,
        parent_run.id.clone(),
        parent_agent.id.clone(),
    );
    let node = CallWorkflowNode {
        workflow: "child".into(),
        inputs: HashMap::new(),
        retries: 0,
        on_fail: Some(crate::workflow_dsl::OnFail::Agent(
            crate::workflow_dsl::AgentRef::Name("on-fail-agent".into()),
        )),
        bot_name: None,
    };

    execute_call_workflow(&mut state, &node, 0).unwrap();

    assert!(
        !state.all_succeeded,
        "state must be failed after resume error"
    );
    assert_no_new_child_run(
        &conn,
        &parent_run.id,
        "resume failure must not spawn a new child run",
    );

    // The on_fail agent step must have been inserted under the parent run.
    let on_fail_step_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM workflow_run_steps WHERE workflow_run_id = ?1 AND step_name = 'on-fail-agent'",
            rusqlite::params![parent_run.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        on_fail_step_count, 1,
        "on_fail agent step must be recorded when resume fails"
    );
}

// ---------------------------------------------------------------------------
// evaluate_hooks integration tests
// ---------------------------------------------------------------------------

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
        .update_workflow_status(
            &state.workflow_run_id,
            WorkflowRunStatus::Cancelled,
            None,
            None,
        )
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
