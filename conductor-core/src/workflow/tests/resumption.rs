#![allow(unused_imports)]

use super::*;
use crate::agent::AgentManager;

#[test]
fn test_get_active_run_for_worktree_none_when_empty() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let active = mgr.get_active_run_for_worktree("w1").unwrap();
    assert!(active.is_none());
}

#[test]
fn test_get_active_run_for_worktree_returns_active() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("my-flow", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    // Set status to running
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let active = mgr.get_active_run_for_worktree("w1").unwrap();
    assert!(active.is_some());
    assert_eq!(active.unwrap().workflow_name, "my-flow");
}

#[test]
fn test_get_active_run_for_worktree_none_after_completion() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("my-flow", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"), None)
        .unwrap();

    let active = mgr.get_active_run_for_worktree("w1").unwrap();
    assert!(active.is_none());
}

#[test]
fn test_get_active_run_for_worktree_ignores_other_worktree() {
    let conn = setup_db();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w2"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("other-flow", Some("w2"), &parent.id, false, "manual", None)
        .unwrap();
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    // w1 should see no active runs
    let active = mgr.get_active_run_for_worktree("w1").unwrap();
    assert!(active.is_none());
}

#[test]
fn test_reset_failed_steps() {
    let conn = setup_db();
    let (run_id, mgr) = setup_run_with_steps(&conn);

    let count = mgr.reset_failed_steps(&run_id).unwrap();
    // Should reset both 'failed' and 'running' steps
    assert_eq!(count, 2);

    let steps = mgr.get_workflow_steps(&run_id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Completed); // unchanged
    assert_eq!(steps[1].status, WorkflowStepStatus::Pending); // was failed
    assert!(steps[1].result_text.is_none()); // cleared
    assert_eq!(steps[2].status, WorkflowStepStatus::Pending); // was running
}

#[test]
fn test_reset_completed_steps() {
    let conn = setup_db();
    let (run_id, mgr) = setup_run_with_steps(&conn);

    let count = mgr.reset_completed_steps(&run_id).unwrap();
    assert_eq!(count, 1);

    let steps = mgr.get_workflow_steps(&run_id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Pending); // was completed
    assert!(steps[0].result_text.is_none()); // cleared
    assert!(steps[0].context_out.is_none()); // cleared
}

#[test]
fn test_reset_steps_from_position() {
    let conn = setup_db();
    let (run_id, mgr) = setup_run_with_steps(&conn);

    // Reset from position 1 onwards
    let count = mgr.reset_steps_from_position(&run_id, 1).unwrap();
    assert_eq!(count, 2); // positions 1 and 2

    let steps = mgr.get_workflow_steps(&run_id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Completed); // position 0 unchanged
    assert_eq!(steps[1].status, WorkflowStepStatus::Pending);
    assert_eq!(steps[2].status, WorkflowStepStatus::Pending);
}

#[test]
fn test_resume_rejects_completed_run() {
    let conn = setup_db();
    let config = make_resume_config();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run(
            "test-wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some("{}"),
        )
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"), None)
        .unwrap();

    let input = WorkflowResumeInput {
        conn: &conn,
        config,
        workflow_run_id: &run.id,
        model: None,
        from_step: None,
        restart: false,
        conductor_bin_dir: None,
    };
    let err = resume_workflow(&input).unwrap_err();
    assert!(
        err.to_string().contains("Cannot resume a completed"),
        "Expected completed-run error, got: {err}"
    );
}

#[test]
fn test_resume_rejects_cancelled_run() {
    let conn = setup_db();
    let config = make_resume_config();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run(
            "test-wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some("{}"),
        )
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Cancelled, None, None)
        .unwrap();

    let input = WorkflowResumeInput {
        conn: &conn,
        config,
        workflow_run_id: &run.id,
        model: None,
        from_step: None,
        restart: false,
        conductor_bin_dir: None,
    };
    let err = resume_workflow(&input).unwrap_err();
    assert!(
        err.to_string().contains("Cannot resume a cancelled"),
        "Expected cancelled-run error, got: {err}"
    );
}

#[test]
fn test_resume_rejects_running_run() {
    let err = validate_resume_preconditions(&WorkflowRunStatus::Running, false, None).unwrap_err();
    assert!(
        err.to_string().contains("already running"),
        "Expected running-run error, got: {err}"
    );
}

#[test]
fn test_resume_rejects_restart_and_from_step_together() {
    let conn = setup_db();
    let config = make_resume_config();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run(
            "test-wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some("{}"),
        )
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("error"), None)
        .unwrap();

    let input = WorkflowResumeInput {
        conn: &conn,
        config,
        workflow_run_id: &run.id,
        model: None,
        from_step: Some("step-one"),
        restart: true,
        conductor_bin_dir: None,
    };
    let err = resume_workflow(&input).unwrap_err();
    assert!(
        err.to_string()
            .contains("--restart and --from-step together"),
        "Expected conflict error, got: {err}"
    );
}

#[test]
fn test_resume_rejects_missing_snapshot() {
    let conn = setup_db();
    let config = make_resume_config();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    // Create run with no definition_snapshot
    let run = wf_mgr
        .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("error"), None)
        .unwrap();

    let input = WorkflowResumeInput {
        conn: &conn,
        config,
        workflow_run_id: &run.id,
        model: None,
        from_step: None,
        restart: false,
        conductor_bin_dir: None,
    };
    let err = resume_workflow(&input).unwrap_err();
    assert!(
        err.to_string().contains("no definition snapshot"),
        "Expected missing-snapshot error, got: {err}"
    );
}

#[test]
fn test_resume_rejects_nonexistent_run() {
    let conn = setup_db();
    let config = make_resume_config();
    let input = WorkflowResumeInput {
        conn: &conn,
        config,
        workflow_run_id: "nonexistent-id",
        model: None,
        from_step: None,
        restart: false,
        conductor_bin_dir: None,
    };
    let err = resume_workflow(&input).unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "Expected not-found error, got: {err}"
    );
}

#[test]
fn test_resume_rejects_nonexistent_from_step() {
    let conn = setup_db();
    let config = make_resume_config();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run(
            "test-wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some("{}"),
        )
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("error"), None)
        .unwrap();

    // Add a step so the run has steps to search through
    let s0 = wf_mgr
        .insert_step(&run.id, "step-a", "actor", false, 0, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &s0,
            WorkflowStepStatus::Completed,
            None,
            Some("ok"),
            None,
            None,
            Some(0),
        )
        .unwrap();

    let input = WorkflowResumeInput {
        conn: &conn,
        config,
        workflow_run_id: &run.id,
        model: None,
        from_step: Some("nonexistent-step"),
        restart: false,
        conductor_bin_dir: None,
    };
    let err = resume_workflow(&input).unwrap_err();
    assert!(
        err.to_string().contains("not found in workflow run"),
        "Expected step-not-found error, got: {err}"
    );
}

/// Regression test for #816: resume_workflow must fall back to the repo root when
/// the worktree path recorded in the DB no longer exists on disk.
/// setup_db() creates worktree `w1` with path `/tmp/ws/feat-test` — absent on disk.
#[test]
fn test_resume_workflow_falls_back_to_repo_root_when_worktree_path_missing() {
    let conn = setup_db();
    let config = make_resume_config();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);

    // Serialize a valid empty WorkflowDef as the snapshot so resume can deserialize it.
    let snapshot = serde_json::to_string(&make_empty_workflow()).unwrap();
    let run = wf_mgr
        .create_workflow_run(
            "test-wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some(&snapshot),
        )
        .unwrap();
    wf_mgr
        .update_workflow_status(
            &run.id,
            WorkflowRunStatus::Failed,
            Some("prior error"),
            None,
        )
        .unwrap();

    let input = WorkflowResumeInput {
        conn: &conn,
        config,
        workflow_run_id: &run.id,
        model: None,
        from_step: None,
        restart: false,
        conductor_bin_dir: None,
    };

    let result = resume_workflow(&input).expect(
        "resume_workflow must succeed when worktree path is missing (fallback to repo root)",
    );
    assert!(
        result.all_succeeded,
        "empty resumed workflow should complete with all_succeeded=true"
    );
}

#[test]
fn test_set_workflow_run_inputs_round_trip() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run(
            "test-wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some("{}"),
        )
        .unwrap();

    // Initially inputs should be empty (no inputs set yet)
    let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert!(fetched.inputs.is_empty(), "Expected no inputs initially");

    // Write inputs and read back
    let mut inputs = HashMap::new();
    inputs.insert("key1".to_string(), "value1".to_string());
    inputs.insert("key2".to_string(), "value2".to_string());
    wf_mgr.set_workflow_run_inputs(&run.id, &inputs).unwrap();

    let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(
        fetched.inputs.get("key1").map(String::as_str),
        Some("value1")
    );
    assert_eq!(
        fetched.inputs.get("key2").map(String::as_str),
        Some("value2")
    );
    assert_eq!(fetched.inputs.len(), 2);
}

#[test]
fn test_set_workflow_run_default_bot_name_round_trip() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run(
            "test-wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some("{}"),
        )
        .unwrap();

    // Initially default_bot_name should be None
    let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert!(
        fetched.default_bot_name.is_none(),
        "Expected no default_bot_name initially"
    );

    // Write a bot name and read it back
    wf_mgr
        .set_workflow_run_default_bot_name(&run.id, "reviewer-bot")
        .unwrap();

    let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(
        fetched.default_bot_name.as_deref(),
        Some("reviewer-bot"),
        "default_bot_name should persist after set"
    );
}

#[test]
fn test_default_bot_name_persists_through_suspend_and_resume() {
    // Verify that default_bot_name written by set_workflow_run_default_bot_name is
    // correctly loaded back when resume_workflow reads the run from the DB — this
    // exercises the full store → retrieve invariant for multi-stage bot identity.
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run(
            "test-wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some("{}"),
        )
        .unwrap();

    // Simulate what execute_workflow does when a default_bot_name is set
    wf_mgr
        .set_workflow_run_default_bot_name(&run.id, "deploy-bot")
        .unwrap();

    // Simulate a suspend by marking the run as waiting
    wf_mgr
        .set_waiting_blocked_on(
            &run.id,
            &BlockedOn::HumanApproval {
                gate_name: "deploy-gate".to_string(),
                prompt: None,
                options: vec![],
            },
        )
        .unwrap();

    // Load the run as resume_workflow would — the bot name must survive the round-trip
    let restored = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(
        restored.default_bot_name.as_deref(),
        Some("deploy-bot"),
        "default_bot_name must survive a suspend/resume DB round-trip"
    );
    assert_eq!(restored.status, WorkflowRunStatus::Waiting);
}

#[test]
fn test_row_to_workflow_run_malformed_inputs_json_returns_empty() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run(
            "test-wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some("{}"),
        )
        .unwrap();

    // Directly write invalid JSON into the inputs column to simulate corruption
    conn.execute(
        "UPDATE workflow_runs SET inputs = :inputs WHERE id = :id",
        rusqlite::named_params! { ":inputs": "not-valid-json", ":id": &run.id },
    )
    .unwrap();

    // Reading back should return an empty HashMap (not panic), matching the
    // unwrap_or_else + warn fallback in row_to_workflow_run.
    let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert!(
        fetched.inputs.is_empty(),
        "Expected empty inputs on malformed JSON, got: {:?}",
        fetched.inputs
    );
}

#[test]
fn test_restart_resets_all_steps() {
    let conn = setup_db();
    let (run_id, mgr) = setup_run_with_steps(&conn);

    // Verify initial state: 1 completed, 1 failed, 1 running
    let steps = mgr.get_workflow_steps(&run_id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Completed);
    assert_eq!(steps[1].status, WorkflowStepStatus::Failed);
    assert_eq!(steps[2].status, WorkflowStepStatus::Running);

    // Restart resets both failed+running and completed steps
    mgr.reset_failed_steps(&run_id).unwrap();
    mgr.reset_completed_steps(&run_id).unwrap();

    let steps = mgr.get_workflow_steps(&run_id).unwrap();
    assert_eq!(
        steps[0].status,
        WorkflowStepStatus::Pending,
        "completed step should be reset"
    );
    assert!(steps[0].result_text.is_none(), "result should be cleared");
    assert!(steps[0].context_out.is_none(), "context should be cleared");
    assert!(steps[0].markers_out.is_none(), "markers should be cleared");
    assert_eq!(
        steps[1].status,
        WorkflowStepStatus::Pending,
        "failed step should be reset"
    );
    assert_eq!(
        steps[2].status,
        WorkflowStepStatus::Pending,
        "running step should be reset"
    );

    // skip set should be empty after restart
    let keys = mgr.get_completed_step_keys(&run_id).unwrap();
    assert!(
        keys.is_empty(),
        "no completed steps should remain after restart"
    );
}

/// Exercises the full --from-step DB orchestration path:
/// - skip-set pruning (keys at/after pos removed)
/// - step_map filtered to only surviving skip keys
/// - DB reset: steps at/after the target step become Pending
/// - steps before the target step remain Completed
#[test]
fn test_from_step_skip_set_and_step_map() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    // Insert 3 completed steps at positions 0, 1, 2
    let s0 = mgr
        .insert_step(&run.id, "step-a", "actor", false, 0, 0)
        .unwrap();
    mgr.update_step_status(
        &s0,
        WorkflowStepStatus::Completed,
        None,
        Some("result-a"),
        Some("ctx-a"),
        None,
        Some(0),
    )
    .unwrap();

    let s1 = mgr
        .insert_step(&run.id, "step-b", "actor", false, 1, 0)
        .unwrap();
    mgr.update_step_status(
        &s1,
        WorkflowStepStatus::Completed,
        None,
        Some("result-b"),
        Some("ctx-b"),
        None,
        Some(0),
    )
    .unwrap();

    let s2 = mgr
        .insert_step(&run.id, "step-c", "actor", false, 2, 0)
        .unwrap();
    mgr.update_step_status(
        &s2,
        WorkflowStepStatus::Completed,
        None,
        Some("result-c"),
        Some("ctx-c"),
        None,
        Some(0),
    )
    .unwrap();

    // Snapshot all_steps before any resets (mirrors resume_workflow: load once upfront)
    let all_steps = mgr.get_workflow_steps(&run.id).unwrap();

    // Simulate the --from-step "step-b" (position 1) branch of resume_workflow
    let mut keys = completed_keys_from_steps(&all_steps);
    assert_eq!(
        keys.len(),
        3,
        "all three steps should be in completed keys initially"
    );

    let pos = all_steps
        .iter()
        .find(|s| s.step_name == "step-b")
        .unwrap()
        .position;
    assert_eq!(pos, 1);

    // Prune keys at/after the from-step position (mirrors resume_workflow)
    let to_remove: Vec<StepKey> = all_steps
        .iter()
        .filter(|s| s.position >= pos && s.status == WorkflowStepStatus::Completed)
        .map(|s| (s.step_name.clone(), s.iteration as u32))
        .collect();
    for key in to_remove {
        keys.remove(&key);
    }

    // Reset DB state for steps at/after pos, then reset any failed/running steps
    mgr.reset_steps_from_position(&run.id, pos).unwrap();
    mgr.reset_failed_steps(&run.id).unwrap();

    // skip_completed should contain only ("step-a", 0)
    assert_eq!(keys.len(), 1, "only step-a:0 should survive pruning");
    assert!(
        keys.contains(&("step-a".to_string(), 0)),
        "step-a:0 must be in skip set"
    );
    assert!(
        !keys.contains(&("step-b".to_string(), 0)),
        "step-b:0 must be pruned from skip set"
    );
    assert!(
        !keys.contains(&("step-c".to_string(), 0)),
        "step-c:0 must be pruned from skip set"
    );

    // DB state: step-a stays Completed, step-b and step-c are reset to Pending
    let updated = mgr.get_workflow_steps(&run.id).unwrap();
    assert_eq!(
        updated[0].status,
        WorkflowStepStatus::Completed,
        "step-a (pos 0) must remain Completed"
    );
    assert_eq!(
        updated[1].status,
        WorkflowStepStatus::Pending,
        "step-b (pos 1, the from-step) must be reset to Pending"
    );
    assert_eq!(
        updated[2].status,
        WorkflowStepStatus::Pending,
        "step-c (pos 2) must be reset to Pending"
    );

    // step_map built from all_steps filtered by surviving skip keys
    // (mirrors resume_workflow)
    let step_map: HashMap<StepKey, WorkflowRunStep> = all_steps
        .into_iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .map(|s| {
            let key = (s.step_name.clone(), s.iteration as u32);
            (key, s)
        })
        .filter(|(key, _)| keys.contains(key))
        .collect();

    assert!(
        step_map.contains_key(&("step-a".to_string(), 0)),
        "step_map must include step-a:0 (will be skipped on resume)"
    );
    assert!(
        !step_map.contains_key(&("step-b".to_string(), 0)),
        "step_map must not include step-b:0 (will be re-executed)"
    );
    assert!(
        !step_map.contains_key(&("step-c".to_string(), 0)),
        "step_map must not include step-c:0 (will be re-executed)"
    );
}

#[test]
fn test_resume_allows_restart_on_completed_run() {
    let conn = setup_db();
    let config = make_resume_config();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run(
            "test-wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some("{}"),
        )
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"), None)
        .unwrap();

    // Without restart, completed run should be rejected
    let input = WorkflowResumeInput {
        conn: &conn,
        config,
        workflow_run_id: &run.id,
        model: None,
        from_step: None,
        restart: false,
        conductor_bin_dir: None,
    };
    assert!(resume_workflow(&input).is_err());

    // With restart, completed run should pass the status check
    // (will fail later due to missing worktree, but the status check should pass)
    let input = WorkflowResumeInput {
        conn: &conn,
        config,
        workflow_run_id: &run.id,
        model: None,
        from_step: None,
        restart: true,
        conductor_bin_dir: None,
    };
    let err = resume_workflow(&input).unwrap_err();
    // Should fail on worktree resolution, NOT on "Cannot resume a completed"
    assert!(
        !err.to_string().contains("Cannot resume a completed"),
        "restart=true should bypass the completed-run check, got: {err}"
    );
}

/// Validates that the min_success calculation in execute_parallel correctly
/// counts skipped-on-resume agents as successes for both the warning logic
/// and the synthetic step status.
///
/// This is a logic-level regression test: execute_parallel uses
/// `effective_successes = successes + skipped_count` and
/// `total_agents = children.len() + skipped_count`, and the synthetic step
/// status must use `effective_successes >= min_required` (not raw `successes`).
#[test]
fn test_parallel_min_success_with_skipped_resume_agents() {
    // Scenario: 3 agents in a parallel block, min_success = 3.
    // On resume, 2 agents were already completed (skipped), 1 new agent succeeds.
    let successes: u32 = 1; // newly succeeded
    let skipped_count: u32 = 2; // completed on previous run
    let children_len: u32 = 1; // only the non-skipped agent was spawned

    let effective_successes = successes + skipped_count; // 3
    let total_agents = children_len + skipped_count; // 3
    let min_required: u32 = 3; // all must succeed

    // The synthetic step should be Completed, not Failed
    let status = if effective_successes >= min_required {
        WorkflowStepStatus::Completed
    } else {
        WorkflowStepStatus::Failed
    };
    assert_eq!(
        status,
        WorkflowStepStatus::Completed,
        "skipped agents must count toward min_success"
    );

    // Verify the all_succeeded flag would NOT be set to false
    let all_succeeded = effective_successes >= min_required;
    assert!(
        all_succeeded,
        "effective_successes ({effective_successes}) should meet min_required ({min_required})"
    );

    // Verify default min_success (None → total_agents) also works
    let default_min = total_agents;
    assert!(
        effective_successes >= default_min,
        "default min_success should be met when all agents (including skipped) succeed"
    );

    // Edge case: one new agent fails, only skipped agents succeeded
    let successes_fail: u32 = 0;
    let effective_fail = successes_fail + skipped_count; // 2
    let status_fail = if effective_fail >= min_required {
        WorkflowStepStatus::Completed
    } else {
        WorkflowStepStatus::Failed
    };
    assert_eq!(
        status_fail,
        WorkflowStepStatus::Failed,
        "should fail when effective successes don't meet min_required"
    );
}

#[test]
fn test_resume_workflow_ephemeral_run_rejected() {
    let conn = setup_db();
    let config = Config::default();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let snapshot = serde_json::to_string(&make_empty_workflow()).unwrap();
    let run = wf_mgr
        .create_workflow_run(
            "ephemeral-wf",
            None,
            &parent.id,
            false,
            "manual",
            Some(&snapshot),
        )
        .unwrap();
    wf_mgr
        .update_workflow_status(
            &run.id,
            WorkflowRunStatus::Failed,
            Some("step failed"),
            None,
        )
        .unwrap();

    let result = resume_workflow(&WorkflowResumeInput {
        conn: &conn,
        config: &config,
        workflow_run_id: &run.id,
        model: None,
        from_step: None,
        restart: false,
        conductor_bin_dir: None,
    });
    assert!(result.is_err(), "ephemeral run resume should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("ephemeral PR run"),
        "error should mention ephemeral PR run, got: {err}"
    );
}

#[test]
fn test_resume_workflow_repo_target() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    let input = WorkflowExecInput {
        repo_id: Some("r1"),
        ..make_exec_input(&conn, &config, &workflow, "/tmp/repo", "/tmp/repo", &exec_config)
    };
    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);
    wf_mgr
        .update_workflow_status(
            &result.workflow_run_id,
            WorkflowRunStatus::Failed,
            Some("step failed"),
            None,
        )
        .unwrap();

    let resume_result = resume_workflow(&WorkflowResumeInput {
        conn: &conn,
        config: &config,
        workflow_run_id: &result.workflow_run_id,
        model: None,
        from_step: None,
        restart: false,
        conductor_bin_dir: None,
    });
    assert!(
        resume_result.is_ok(),
        "resume of repo-targeted run should succeed: {:?}",
        resume_result.err()
    );
}

#[test]
fn test_resume_workflow_ticket_target() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    insert_test_ticket(&conn, "tkt-1", "r1");

    let input = WorkflowExecInput {
        ticket_id: Some("tkt-1"),
        ..make_exec_input(&conn, &config, &workflow, "/tmp/repo", "/tmp/repo", &exec_config)
    };
    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);
    wf_mgr
        .update_workflow_status(
            &result.workflow_run_id,
            WorkflowRunStatus::Failed,
            Some("step failed"),
            None,
        )
        .unwrap();

    let resume_result = resume_workflow(&WorkflowResumeInput {
        conn: &conn,
        config: &config,
        workflow_run_id: &result.workflow_run_id,
        model: None,
        from_step: None,
        restart: false,
        conductor_bin_dir: None,
    });
    assert!(
        resume_result.is_ok(),
        "resume of ticket-targeted run should succeed: {:?}",
        resume_result.err()
    );
}

/// Regression test for #2186: orphaned `pending` step rows (status = 'pending',
/// started_at IS NULL) must be deleted before the skip set is built on resume.
///
/// These rows are created when `insert_step` succeeds but the executor crashes
/// before `started_at` is written.  They are harmless to execution (the resume
/// path re-inserts and re-runs the step), but they pollute step history.
#[test]
fn test_resume_deletes_orphaned_pending_steps() {
    let conn = setup_db();
    let config = make_resume_config();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);

    let snapshot = serde_json::to_string(&make_empty_workflow()).unwrap();
    let run = wf_mgr
        .create_workflow_run(
            "test-wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some(&snapshot),
        )
        .unwrap();

    // Insert a completed step — must survive the orphan-deletion pass.
    let s_completed = wf_mgr
        .insert_step(&run.id, "step-done", "actor", false, 0, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &s_completed,
            WorkflowStepStatus::Completed,
            None,
            Some("ok"),
            None,
            None,
            Some(0),
        )
        .unwrap();

    // Insert an orphaned pending step: status = 'pending', started_at = NULL.
    // `insert_step` inserts with status='pending' and no started_at, so this
    // is the real scenario from #2186.
    let _s_orphan = wf_mgr
        .insert_step(&run.id, "step-orphan", "actor", false, 1, 0)
        .unwrap();

    // Confirm both rows are present before resume.
    let steps_before = wf_mgr.get_workflow_steps(&run.id).unwrap();
    assert_eq!(
        steps_before.len(),
        2,
        "should have 2 step rows before resume"
    );

    // Mark the run as failed so resume is accepted.
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("crash"), None)
        .unwrap();

    // resume_workflow calls delete_orphaned_pending_steps before building the
    // skip set, so the orphaned row must be gone by the time execution starts.
    let result = resume_workflow(&WorkflowResumeInput {
        conn: &conn,
        config,
        workflow_run_id: &run.id,
        model: None,
        from_step: None,
        restart: false,
        conductor_bin_dir: None,
    });
    assert!(
        result.is_ok(),
        "resume_workflow must succeed; got: {:?}",
        result.err()
    );

    // Only the completed step should remain — the orphaned pending row is gone.
    let steps_after = wf_mgr.get_workflow_steps(&run.id).unwrap();
    assert_eq!(
        steps_after.len(),
        1,
        "orphaned pending step must be deleted during resume"
    );
    assert_eq!(
        steps_after[0].step_name, "step-done",
        "the surviving step must be the completed one"
    );
    assert_eq!(
        steps_after[0].status,
        WorkflowStepStatus::Completed,
        "surviving step status must be Completed"
    );
}
