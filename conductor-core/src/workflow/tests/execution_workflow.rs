use super::*;
use crate::agent::AgentManager;
use crate::error::ConductorError;
use crate::workflow_dsl::{
    AgentRef, CallNode, CallWorkflowNode, ForEachNode, ForeachOver, GateType, OnChildFail,
    WorkflowNode,
};

#[test]
fn test_cannot_start_workflow_run_when_active() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("running-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        worktree_id: Some("w1"),
        ..make_exec_input(
            &conn,
            &config,
            &workflow,
            "/tmp/ws/feat-test",
            "/tmp/repo",
            &exec_config,
        )
    };
    let err = execute_workflow(&input).unwrap_err();
    assert!(
        matches!(err, ConductorError::WorkflowRunAlreadyActive { .. }),
        "expected WorkflowRunAlreadyActive, got: {err}"
    );
}

#[test]
fn test_can_start_workflow_run_after_completion() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("done-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"), None)
        .unwrap();

    let workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        worktree_id: Some("w1"),
        ..make_exec_input(
            &conn,
            &config,
            &workflow,
            "/tmp/ws/feat-test",
            "/tmp/repo",
            &exec_config,
        )
    };
    // Guard should pass; empty workflow completes successfully.
    let result = execute_workflow(&input);
    assert!(
        !matches!(result, Err(ConductorError::WorkflowRunAlreadyActive { .. })),
        "should not be blocked by completed run"
    );
}

#[test]
fn test_child_workflow_not_blocked_by_parent() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("parent-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let workflow = make_empty_workflow();
    // depth = 1 means this is a child workflow — guard must be skipped.
    let input = WorkflowExecInput {
        worktree_id: Some("w1"),
        depth: 1,
        ..make_exec_input(
            &conn,
            &config,
            &workflow,
            "/tmp/ws/feat-test",
            "/tmp/repo",
            &exec_config,
        )
    };
    let result = execute_workflow(&input);
    assert!(
        !matches!(result, Err(ConductorError::WorkflowRunAlreadyActive { .. })),
        "child workflow should not be blocked by active parent run"
    );
}

#[test]
fn test_run_id_notify_slot_is_populated() {
    // Verify that execute_workflow writes the newly-created run ID into
    // run_id_notify before any steps execute. This is the mechanism used
    // by the MCP tool_run_workflow handler to return a run_id immediately.
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    let workflow = make_empty_workflow();

    let slot: RunIdSlot =
        std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));

    let input = WorkflowExecInput {
        run_id_notify: Some(std::sync::Arc::clone(&slot)),
        ..make_exec_input(
            &conn,
            &config,
            &workflow,
            "/tmp/repo",
            "/tmp/repo",
            &exec_config,
        )
    };

    execute_workflow(&input).expect("workflow should complete");

    let notified_id = slot
        .0
        .lock()
        .expect("mutex not poisoned")
        .clone()
        .expect("run_id_notify slot should have been written");

    // The written ID must match the run that was actually created.
    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .get_workflow_run(&notified_id)
        .expect("db query ok")
        .expect("run should exist");
    assert_eq!(run.workflow_name, "test-wf");
}

/// setup_db() creates worktree `w1` with path `/tmp/ws/feat-test` which does not
/// exist on disk. Prior to #816 this would propagate a path-not-found error; after
/// the fix the engine must silently fall back to the repo root and succeed.
#[test]
fn test_execute_workflow_falls_back_to_repo_root_when_worktree_path_missing() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    let input = WorkflowExecInput {
        worktree_id: Some("w1"), // path /tmp/ws/feat-test — does not exist on disk
        ..make_exec_input(
            &conn,
            &config,
            &workflow,
            "/tmp/repo",
            "/tmp/repo",
            &exec_config,
        )
    };

    let result = execute_workflow(&input).expect(
        "execute_workflow must succeed when worktree path is missing (fallback to repo root)",
    );
    assert!(
        result.all_succeeded,
        "empty workflow should complete with all_succeeded=true"
    );
}

#[test]
fn test_execute_workflow_injects_repo_variables() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    // repo `r1` with local_path `/tmp/repo` is inserted by setup_db()
    let input = WorkflowExecInput {
        repo_id: Some("r1"),
        ..make_exec_input(
            &conn,
            &config,
            &workflow,
            "/tmp/repo",
            "/tmp/repo",
            &exec_config,
        )
    };
    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .get_workflow_run(&result.workflow_run_id)
        .unwrap()
        .unwrap();

    assert_eq!(run.inputs.get("repo_id").map(String::as_str), Some("r1"));
    assert_eq!(
        run.inputs.get("repo_path").map(String::as_str),
        Some("/tmp/repo")
    );
    assert_eq!(
        run.inputs.get("repo_name").map(String::as_str),
        Some("test-repo")
    );
    // Assert the repo_id column is persisted on the WorkflowRun record itself.
    assert_eq!(run.repo_id.as_deref(), Some("r1"));
    assert_eq!(run.ticket_id, None);
}

#[test]
fn test_execute_workflow_injects_ticket_variables() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    insert_test_ticket(&conn, "tkt-1", "r1");

    let input = WorkflowExecInput {
        ticket_id: Some("tkt-1"),
        ..make_exec_input(
            &conn,
            &config,
            &workflow,
            "/tmp/repo",
            "/tmp/repo",
            &exec_config,
        )
    };
    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .get_workflow_run(&result.workflow_run_id)
        .unwrap()
        .unwrap();

    assert_eq!(
        run.inputs.get("ticket_id").map(String::as_str),
        Some("tkt-1")
    );
    assert_eq!(
        run.inputs.get("ticket_title").map(String::as_str),
        Some("Test ticket title")
    );
    assert!(
        run.inputs.contains_key("ticket_url"),
        "ticket_url should be injected"
    );
    // Assert the ticket_id column is persisted on the WorkflowRun record itself.
    assert_eq!(run.ticket_id.as_deref(), Some("tkt-1"));
    assert_eq!(run.repo_id, None);
}

#[test]
fn test_execute_workflow_existing_input_not_overwritten_by_injection() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    let mut explicit_inputs = HashMap::new();
    explicit_inputs.insert("repo_name".to_string(), "my-override".to_string());

    let input = WorkflowExecInput {
        repo_id: Some("r1"),
        inputs: explicit_inputs,
        ..make_exec_input(
            &conn,
            &config,
            &workflow,
            "/tmp/repo",
            "/tmp/repo",
            &exec_config,
        )
    };
    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .get_workflow_run(&result.workflow_run_id)
        .unwrap()
        .unwrap();

    // Caller-supplied repo_name must not be overwritten by the injected value.
    assert_eq!(
        run.inputs.get("repo_name").map(String::as_str),
        Some("my-override")
    );
}

#[test]
fn test_execute_workflow_unknown_ticket_id_returns_error() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    let input = WorkflowExecInput {
        ticket_id: Some("nonexistent-ticket"),
        ..make_exec_input(&conn, &config, &workflow, "", "", &exec_config)
    };
    assert!(
        execute_workflow(&input).is_err(),
        "referencing a nonexistent ticket_id must return an error"
    );
}

#[test]
fn test_execute_workflow_unknown_repo_id_returns_error() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    let input = WorkflowExecInput {
        repo_id: Some("nonexistent-repo"),
        ..make_exec_input(&conn, &config, &workflow, "", "", &exec_config)
    };
    assert!(
        execute_workflow(&input).is_err(),
        "referencing a nonexistent repo_id must return an error"
    );
}

#[test]
fn test_execute_workflow_ephemeral_skips_concurrent_guard() {
    // Verify that when worktree_id is None (ephemeral run), a second concurrent
    // call at depth==0 does NOT return WorkflowRunAlreadyActive — the guard is
    // intentionally skipped for ephemeral runs which have no registered worktree.
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    let workflow = make_empty_workflow();

    // First ephemeral call — must succeed (empty workflow, no agents to spawn).
    let input1 = make_exec_input(&conn, &config, &workflow, "", "", &exec_config);
    let result1 = execute_workflow(&input1);
    assert!(
        !matches!(
            result1,
            Err(ConductorError::WorkflowRunAlreadyActive { .. })
        ),
        "first ephemeral call should not be blocked by the concurrent guard"
    );

    // Second ephemeral call — must also not be blocked by the guard, even though
    // the first run's record now exists in the DB (it has no worktree_id, so the
    // guard is skipped entirely for ephemeral runs).
    let input2 = make_exec_input(&conn, &config, &workflow, "", "", &exec_config);
    let result2 = execute_workflow(&input2);
    assert!(
        !matches!(
            result2,
            Err(ConductorError::WorkflowRunAlreadyActive { .. })
        ),
        "second ephemeral call should not be blocked by the concurrent guard"
    );
}

#[test]
fn test_execute_workflow_iteration_persisted() {
    // When iteration > 0, execute_workflow should persist the iteration on the
    // created workflow run record via set_workflow_run_iteration.
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    // Use run_id_notify to capture the workflow run ID.
    let slot: RunIdSlot =
        std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));

    let input = WorkflowExecInput {
        depth: 1,
        iteration: 3,
        run_id_notify: Some(slot.clone()),
        ..make_exec_input(&conn, &config, &workflow, "", "", &exec_config)
    };

    let result = execute_workflow(&input);
    // The workflow will complete (empty body, no agents to spawn).
    assert!(
        result.is_ok(),
        "execute_workflow should succeed: {:?}",
        result
    );

    // Retrieve the run ID from the notify slot.
    let run_id = slot
        .0
        .lock()
        .unwrap()
        .clone()
        .expect("run_id should be set");

    // Verify the run record has iteration == 3.
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .get_workflow_run(&run_id)
        .unwrap()
        .expect("run should exist");
    assert_eq!(
        run.iteration, 3,
        "iteration should be persisted on the workflow run"
    );
}

#[test]
fn test_execute_workflow_fails_on_invalid_schema() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    // Create a temp dir with a valid agent definition so the agent check passes
    let tmp = tempfile::tempdir().unwrap();
    let agents_dir = tmp.path().join(".conductor/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("test-agent.md"), "You are a test agent.").unwrap();
    let working_dir = tmp.path().to_str().unwrap();

    // Build a workflow with a step referencing a schema that doesn't exist
    let mut workflow = make_empty_workflow();
    workflow.body.push(WorkflowNode::Call(CallNode {
        agent: AgentRef::Name("test-agent".into()),
        retries: 0,
        on_fail: None,
        output: Some("broken".into()),
        with: vec![],
        bot_name: None,
        plugin_dirs: vec![],
    }));

    let input = make_exec_input(&conn, &config, &workflow, working_dir, "", &exec_config);

    let err = execute_workflow(&input).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Schema validation failed"),
        "expected schema validation error, got: {msg}"
    );
    assert!(
        msg.contains("broken"),
        "error should mention the bad schema name, got: {msg}"
    );

    // Verify no agent runs were created (zero tokens spent)
    let agent_mgr = AgentManager::new(&conn);
    let runs = agent_mgr.list_agent_runs(None, None, None, 100, 0).unwrap();
    assert!(
        runs.is_empty(),
        "no agent runs should be created when schema validation fails"
    );
}

#[test]
fn test_execute_workflow_fails_on_invalid_schema_parse() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    let tmp = tempfile::tempdir().unwrap();
    let agents_dir = tmp.path().join(".conductor/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("test-agent.md"), "You are a test agent.").unwrap();

    // Create a schema file with invalid YAML so it triggers SchemaIssue::Invalid
    let schemas_dir = tmp.path().join(".conductor/schemas");
    std::fs::create_dir_all(&schemas_dir).unwrap();
    std::fs::write(
        schemas_dir.join("bad-schema.yaml"),
        "fields: [this: is: not: valid\n",
    )
    .unwrap();

    let working_dir = tmp.path().to_str().unwrap();

    let mut workflow = make_empty_workflow();
    workflow.body.push(WorkflowNode::Call(CallNode {
        agent: AgentRef::Name("test-agent".into()),
        retries: 0,
        on_fail: None,
        output: Some("bad-schema".into()),
        with: vec![],
        bot_name: None,
        plugin_dirs: vec![],
    }));

    let input = make_exec_input(
        &conn,
        &config,
        &workflow,
        working_dir,
        working_dir,
        &exec_config,
    );

    let err = execute_workflow(&input).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Schema validation failed"),
        "expected schema validation error, got: {msg}"
    );
    assert!(
        msg.contains("invalid"),
        "error should indicate the schema is invalid, got: {msg}"
    );
    assert!(
        msg.contains("bad-schema"),
        "error should mention the schema name, got: {msg}"
    );

    // Verify no agent runs were created
    let agent_mgr = AgentManager::new(&conn);
    let runs = agent_mgr.list_agent_runs(None, None, None, 100, 0).unwrap();
    assert!(
        runs.is_empty(),
        "no agent runs should be created when schema validation fails"
    );
}

#[test]
fn test_execute_workflow_passes_preflight_with_valid_schema() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    let tmp = tempfile::tempdir().unwrap();
    let agents_dir = tmp.path().join(".conductor/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("test-agent.md"), "You are a test agent.").unwrap();

    // Create a valid schema file
    let schemas_dir = tmp.path().join(".conductor/schemas");
    std::fs::create_dir_all(&schemas_dir).unwrap();
    std::fs::write(
        schemas_dir.join("good-schema.yaml"),
        "fields:\n  summary: string\n",
    )
    .unwrap();

    let working_dir = tmp.path().to_str().unwrap();

    let mut workflow = make_empty_workflow();
    workflow.body.push(WorkflowNode::Call(CallNode {
        agent: AgentRef::Name("test-agent".into()),
        retries: 0,
        on_fail: None,
        output: Some("good-schema".into()),
        with: vec![],
        bot_name: None,
        plugin_dirs: vec![],
    }));

    let input = make_exec_input(
        &conn,
        &config,
        &workflow,
        working_dir,
        working_dir,
        &exec_config,
    );

    // execute_workflow should pass pre-flight validation (schema exists and is valid).
    // It will fail later when trying to actually run the agent (no tmux, etc.),
    // but the error should NOT be about schema validation.
    let result = execute_workflow(&input);
    match result {
        Ok(_) => {} // fine if it somehow succeeds
        Err(e) => {
            let msg = e.to_string();
            assert!(
                !msg.contains("Schema validation failed"),
                "valid schema should not trigger schema validation error, got: {msg}"
            );
        }
    }
}

/// Regression test for #1405: when a worktree has a non-default base branch
/// and no feature is resolved, execute_workflow should inject
/// feature_base_branch from the worktree's effective base.
#[test]
fn test_execute_workflow_worktree_fallback_base_branch() {
    let conn = setup_db();
    let config: &'static Config = Box::leak(Box::new(Config::default()));

    // Insert a worktree with a custom base_branch ("develop") that differs
    // from the repo/config default ("main").
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at) \
         VALUES ('wt-custom-base', 'r1', 'feat-custom', 'feat/custom', 'develop', '/tmp/ws/feat-custom', 'active', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();

    let workflow = make_empty_workflow();
    let exec_config = WorkflowExecConfig::default();

    let input = WorkflowExecInput {
        worktree_id: Some("wt-custom-base"),
        ..make_exec_input(
            &conn,
            config,
            &workflow,
            "/tmp/ws/feat-custom",
            "/tmp/repo",
            &exec_config,
        )
    };

    let result = execute_workflow(&input).unwrap();

    // Fetch the persisted workflow run and verify the injected base branch.
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .get_workflow_run(&result.workflow_run_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        run.inputs.get("feature_base_branch").map(String::as_str),
        Some("develop"),
        "feature_base_branch should equal the worktree's custom base_branch, not default 'main'"
    );
}

/// Regression test for #1539: when `repo_id` is `None` but `worktree_id` is provided,
/// `execute_workflow` should derive `repo_id` from the worktree's parent repo.
#[test]
fn test_execute_workflow_derives_repo_id_from_worktree() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    let input = WorkflowExecInput {
        worktree_id: Some("w1"),
        ..make_exec_input(
            &conn,
            &config,
            &workflow,
            "/tmp/ws/feat-test",
            "/tmp/repo",
            &exec_config,
        )
    };

    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .get_workflow_run(&result.workflow_run_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        run.repo_id.as_deref(),
        Some("r1"),
        "repo_id should be derived from worktree w1's parent repo r1"
    );
}

/// Regression test: when `repo_id` is `None` but `worktree_id` is provided, a
/// `foreach over worktrees` step must not fail with "requires a repo_id in the execution
/// context". The `effective_repo_id` derived from the worktree must be threaded into
/// `ExecutionState`, not just saved to the DB row.
#[test]
fn test_foreach_worktrees_uses_derived_repo_id_from_worktree() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    let foreach_node = ForEachNode {
        name: "fan-out".to_string(),
        over: ForeachOver::Worktrees,
        scope: None,
        filter: Default::default(),
        ordered: false,
        on_cycle: crate::workflow_dsl::OnCycle::Fail,
        max_parallel: 1,
        workflow: "ticket-to-pr".to_string(),
        inputs: Default::default(),
        on_child_fail: OnChildFail::Continue,
    };
    let mut workflow = make_empty_workflow();
    workflow.body = vec![WorkflowNode::ForEach(foreach_node)];

    let input = WorkflowExecInput {
        worktree_id: Some("w1"),
        ..make_exec_input(
            &conn,
            &config,
            &workflow,
            "/tmp/ws/feat-test",
            "/tmp/repo",
            &exec_config,
        )
    };

    let result = execute_workflow(&input);
    assert!(
        !matches!(
            result,
            Err(ConductorError::Workflow(ref msg)) if msg.contains("requires a repo_id")
        ),
        "foreach over worktrees should not fail with missing repo_id when worktree_id is provided"
    );
}

/// Regression test for #1652: always block must run even when fail_fast=true and the
/// body has already failed.  Before the fix, execute_nodes checked
/// `!all_succeeded && fail_fast` unconditionally and broke immediately, skipping
/// every node in the always block.
#[test]
fn test_always_block_runs_on_fail_fast_failure() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);
    state.exec_config.fail_fast = true;
    state.exec_config.dry_run = true;
    // Simulate the state after a body step has failed.
    state.all_succeeded = false;

    let nodes = vec![WorkflowNode::Gate(make_gate_node(
        GateType::HumanApproval,
        OnTimeout::Fail,
    ))];

    let initial_position = state.position;
    // Mirrors the always-block call in run_workflow_engine: respect_fail_fast = false.
    let result = execute_nodes(&mut state, &nodes, false);
    assert!(result.is_ok(), "always block should not return an error");
    assert_eq!(
        state.position - initial_position,
        1,
        "always block gate must execute even when fail_fast=true and body failed"
    );
}

/// Guard for the existing fail_fast body-skip behaviour: when respect_fail_fast=true
/// and the body has already failed, subsequent nodes must be skipped.
#[test]
fn test_body_skips_on_fail_fast_failure() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);
    state.exec_config.fail_fast = true;
    state.exec_config.dry_run = true;
    state.all_succeeded = false;

    let nodes = vec![WorkflowNode::Gate(make_gate_node(
        GateType::HumanApproval,
        OnTimeout::Fail,
    ))];

    let initial_position = state.position;
    // Mirrors the body call in run_workflow_engine: respect_fail_fast = true.
    let result = execute_nodes(&mut state, &nodes, true);
    assert!(result.is_ok());
    assert_eq!(
        state.position, initial_position,
        "body gate must be skipped when fail_fast=true and body already failed"
    );
}

// ---------------------------------------------------------------------------
// parent_step_id — child run ID written back immediately (#2320)
// ---------------------------------------------------------------------------

/// When `parent_step_id` is set, `execute_workflow` must write the new child
/// run ID back to the parent step row immediately after the child run is
/// created, so the TUI can drill into a running child workflow.
#[test]
fn test_parent_step_id_writes_child_run_id_to_step() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    // Set up a parent workflow run with a placeholder "call-child" step.
    let agent_mgr = AgentManager::new(&conn);
    let parent_agent_run = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let parent_run = wf_mgr
        .create_workflow_run(
            "parent-wf",
            Some("w1"),
            &parent_agent_run.id,
            false,
            "manual",
            None,
        )
        .unwrap();
    let parent_step_id = wf_mgr
        .insert_step(&parent_run.id, "call-child", "actor", false, 0, 0)
        .unwrap();

    // child_run_id should be None before execute_workflow is called.
    let step_before = wf_mgr
        .get_step_by_id(&parent_step_id)
        .unwrap()
        .expect("step must exist");
    assert!(
        step_before.child_run_id.is_none(),
        "child_run_id must be None before the child run is created"
    );

    // Execute an empty child workflow with parent_step_id set.
    let child_workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        worktree_id: Some("w1"),
        depth: 1,
        parent_workflow_run_id: Some(&parent_run.id),
        parent_step_id: Some(parent_step_id.clone()),
        ..make_exec_input(
            &conn,
            &config,
            &child_workflow,
            "/tmp/ws/feat-test",
            "/tmp/repo",
            &exec_config,
        )
    };

    let result = execute_workflow(&input).unwrap();
    let child_run_id = result.workflow_run_id;

    // The parent step must now have child_run_id set to the new child run's ID.
    let step_after = wf_mgr
        .get_step_by_id(&parent_step_id)
        .unwrap()
        .expect("step must still exist");
    assert_eq!(
        step_after.child_run_id.as_deref(),
        Some(child_run_id.as_str()),
        "child_run_id must be written back to the parent step immediately on child run creation"
    );
}
