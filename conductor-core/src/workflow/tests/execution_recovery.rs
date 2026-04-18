use super::*;
use crate::agent::AgentManager;
use std::collections::HashMap;

#[test]
fn test_recover_stuck_steps_syncs_completed() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let wf_mgr = WorkflowManager::new(&conn);

    // Create a parent agent run and a workflow run
    let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let wf_run = wf_mgr
        .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    // Insert a step stuck in 'running' with a child_run_id
    let step_id = wf_mgr
        .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
        .unwrap();
    let child = agent_mgr
        .create_run(Some("w1"), "child-agent", None, None)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&child.id),
            None,
            None,
            None,
            None,
        )
        .unwrap();

    // Mark child run as completed
    agent_mgr
        .update_run_completed(
            &child.id,
            None,
            Some("great output"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let recovered = wf_mgr.recover_stuck_steps().unwrap();
    assert_eq!(recovered, 1);

    let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Completed);
    assert_eq!(steps[0].result_text.as_deref(), Some("great output"));
}

#[test]
fn test_recover_stuck_steps_skips_still_running() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let wf_mgr = WorkflowManager::new(&conn);

    let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let wf_run = wf_mgr
        .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let step_id = wf_mgr
        .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
        .unwrap();
    let child = agent_mgr
        .create_run(Some("w1"), "child-agent", None, None)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&child.id),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    // child run stays in 'running' — should NOT be recovered

    let recovered = wf_mgr.recover_stuck_steps().unwrap();
    assert_eq!(recovered, 0);

    let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Running);
}

#[test]
fn test_recover_stuck_steps_failed_child_marks_step_failed() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let wf_mgr = WorkflowManager::new(&conn);

    let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let wf_run = wf_mgr
        .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let step_id = wf_mgr
        .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
        .unwrap();
    let child = agent_mgr
        .create_run(Some("w1"), "child-agent", None, None)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&child.id),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    agent_mgr
        .update_run_failed(&child.id, "agent crashed")
        .unwrap();

    let recovered = wf_mgr.recover_stuck_steps().unwrap();
    assert_eq!(recovered, 1);

    let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
    assert_eq!(steps[0].result_text.as_deref(), Some("agent crashed"));
}

#[test]
fn test_recover_stuck_steps_skips_step_with_purged_child_run() {
    // A step whose child_run_id references an agent run that no longer exists
    // (e.g. purged) must be left in 'running' status and not cause an error.
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let wf_mgr = WorkflowManager::new(&conn);

    let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let wf_run = wf_mgr
        .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let step_id = wf_mgr
        .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
        .unwrap();

    // Point the step at a child_run_id that does not exist in agent_runs.
    conn.execute(
        "UPDATE workflow_run_steps SET status = 'running', child_run_id = 'nonexistent-run-id' \
         WHERE id = :id",
        rusqlite::named_params! { ":id": step_id },
    )
    .unwrap();

    let recovered = wf_mgr.recover_stuck_steps().unwrap();
    assert_eq!(recovered, 0, "purged child run should not be recovered");

    let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
    assert_eq!(
        steps[0].status,
        WorkflowStepStatus::Running,
        "step must remain in 'running' when child_run_id is missing from agent_runs"
    );
}

#[test]
fn test_fetch_child_final_output_returns_last_completed_step() {
    let conn = setup_db();
    let (mgr, run_id) = create_child_run(&conn);

    // Insert two completed steps; the second (position=1) should be returned
    let step1_id = mgr
        .insert_step(&run_id, "step-a", "actor", false, 0, 0)
        .unwrap();
    mgr.update_step_status(
        &step1_id,
        WorkflowStepStatus::Completed,
        None,
        Some("step-a done"),
        Some("context-a"),
        Some(r#"["marker_a"]"#),
        Some(0),
    )
    .unwrap();

    let step2_id = mgr
        .insert_step(&run_id, "step-b", "actor", false, 1, 0)
        .unwrap();
    mgr.update_step_status(
        &step2_id,
        WorkflowStepStatus::Completed,
        None,
        Some("step-b done"),
        Some("context-b"),
        Some(r#"["marker_b1","marker_b2"]"#),
        Some(0),
    )
    .unwrap();

    let (markers, context) = fetch_child_final_output(&mgr, &run_id);
    assert_eq!(markers, vec!["marker_b1", "marker_b2"]);
    assert_eq!(context, "context-b");
}

#[test]
fn test_fetch_child_final_output_no_completed_steps() {
    let conn = setup_db();
    let (mgr, run_id) = create_child_run(&conn);

    // Insert a failed step only
    let step_id = mgr
        .insert_step(&run_id, "step-a", "actor", false, 0, 0)
        .unwrap();
    mgr.update_step_status(
        &step_id,
        WorkflowStepStatus::Failed,
        None,
        Some("failed"),
        None,
        None,
        Some(0),
    )
    .unwrap();

    let (markers, context) = fetch_child_final_output(&mgr, &run_id);
    assert!(markers.is_empty());
    assert!(context.is_empty());
}

#[test]
fn test_fetch_child_final_output_malformed_markers_json() {
    let conn = setup_db();
    let (mgr, run_id) = create_child_run(&conn);

    let step_id = mgr
        .insert_step(&run_id, "step-a", "actor", false, 0, 0)
        .unwrap();
    mgr.update_step_status(
        &step_id,
        WorkflowStepStatus::Completed,
        None,
        Some("done"),
        Some("some context"),
        Some("not valid json {{{"),
        Some(0),
    )
    .unwrap();

    let (markers, context) = fetch_child_final_output(&mgr, &run_id);
    assert!(markers.is_empty()); // malformed JSON falls back to empty
    assert_eq!(context, "some context");
}

#[test]
fn test_fetch_child_final_output_nonexistent_run() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let (markers, context) = fetch_child_final_output(&mgr, "nonexistent-run-id");
    assert!(markers.is_empty());
    assert!(context.is_empty());
}

#[test]
fn test_bubble_up_child_step_results_basic() {
    let conn = setup_db();
    let (wf_mgr, run_id) = create_child_run(&conn);

    // Insert two completed steps with markers
    let step1 = wf_mgr
        .insert_step(&run_id, "review-aggregator", "reviewer", false, 0, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step1,
            WorkflowStepStatus::Completed,
            None,
            Some("done"),
            Some("some context"),
            Some(r#"["has_review_issues"]"#),
            None,
        )
        .unwrap();

    let step2 = wf_mgr
        .insert_step(&run_id, "lint-checker", "reviewer", false, 1, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step2,
            WorkflowStepStatus::Completed,
            None,
            Some("done"),
            Some("lint ok"),
            Some(r#"["lint_passed"]"#),
            None,
        )
        .unwrap();

    let result = bubble_up_child_step_results(&wf_mgr, &run_id);

    assert_eq!(result.len(), 2);
    let agg = result.get("review-aggregator").unwrap();
    assert!(agg.markers.contains(&"has_review_issues".to_string()));
    let lint = result.get("lint-checker").unwrap();
    assert!(lint.markers.contains(&"lint_passed".to_string()));
}

#[test]
fn test_bubble_up_child_step_results_child_overwrites_parent() {
    let conn = setup_db();
    let config = make_resume_config();
    let (mut state, _run_id) = make_state_with_run(&conn, config);

    // Parent already has a step result for "review-aggregator" (stale from iteration 1)
    state.step_results.insert(
        "review-aggregator".to_string(),
        StepResult {
            step_name: "review-aggregator".to_string(),
            status: WorkflowStepStatus::Completed,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            markers: vec!["parent_marker".to_string()],
            context: "parent context".to_string(),
            child_run_id: None,
            structured_output: None,
            output_file: None,
        },
    );

    // Child run with same step name but different marker (fresh result from iteration 2)
    let (child_wf_mgr, child_run_id) = create_child_run(&conn);
    let step1 = child_wf_mgr
        .insert_step(&child_run_id, "review-aggregator", "reviewer", false, 0, 0)
        .unwrap();
    child_wf_mgr
        .update_step_status(
            &step1,
            WorkflowStepStatus::Completed,
            None,
            Some("done"),
            Some("child context"),
            Some(r#"["child_marker"]"#),
            None,
        )
        .unwrap();

    let child_steps = bubble_up_child_step_results(&child_wf_mgr, &child_run_id);
    for (key, value) in child_steps {
        state.step_results.insert(key, value);
    }

    // Child's fresh value should overwrite the stale parent value
    let result = state.step_results.get("review-aggregator").unwrap();
    assert!(result.markers.contains(&"child_marker".to_string()));
    assert!(!result.markers.contains(&"parent_marker".to_string()));
}

#[test]
fn test_bubble_up_child_step_results_no_completed_steps() {
    let conn = setup_db();
    let (wf_mgr, run_id) = create_child_run(&conn);

    // Insert a failed step — should not be bubbled up
    let step1 = wf_mgr
        .insert_step(&run_id, "some-step", "reviewer", false, 0, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step1,
            WorkflowStepStatus::Failed,
            None,
            Some("failed"),
            None,
            None,
            None,
        )
        .unwrap();

    let result = bubble_up_child_step_results(&wf_mgr, &run_id);
    assert!(result.is_empty());
}

#[test]
fn test_restore_completed_step_basic() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    let step = make_test_step(
        "review",
        WorkflowStepStatus::Completed,
        Some("looks good"),
        Some("reviewed code"),
        Some(r#"["approved"]"#),
        None,
        Some(r#"{"verdict":"approve"}"#),
    );
    let ctx = make_resume_ctx(
        [(("review".to_string(), 0), step)].into_iter().collect(),
        HashMap::new(),
    );

    restore_completed_step(&mut state, &ctx, "review", 0);

    // Verify step_results populated
    let result = state.step_results.get("review").unwrap();
    assert_eq!(result.status, WorkflowStepStatus::Completed);
    assert_eq!(result.result_text.as_deref(), Some("looks good"));
    assert_eq!(result.markers, vec!["approved"]);
    assert_eq!(result.context, "reviewed code");
    assert_eq!(
        result.structured_output.as_deref(),
        Some(r#"{"verdict":"approve"}"#)
    );

    // Verify contexts populated
    assert_eq!(state.contexts.len(), 1);
    assert_eq!(state.contexts[0].step, "review");
    assert_eq!(state.contexts[0].context, "reviewed code");
    assert_eq!(
        state.contexts[0].structured_output.as_deref(),
        Some(r#"{"verdict":"approve"}"#)
    );

    // Verify structured output is accessible via contexts
    assert_eq!(
        state
            .contexts
            .iter()
            .rev()
            .find_map(|c| c.structured_output.as_deref()),
        Some(r#"{"verdict":"approve"}"#)
    );
}

#[test]
fn test_restore_completed_step_not_found() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    let ctx = make_resume_ctx(HashMap::new(), HashMap::new());
    restore_completed_step(&mut state, &ctx, "nonexistent", 0);

    // Should be a no-op (with warning logged)
    assert!(state.step_results.is_empty());
    assert!(state.contexts.is_empty());
}

#[test]
fn test_restore_completed_step_accumulates_costs() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    // Create a child agent run with cost data
    let child_run = agent_mgr
        .create_run(Some("w1"), "test agent", None, None)
        .unwrap();
    agent_mgr
        .update_run_completed(
            &child_run.id,
            None,
            Some("done"),
            Some(0.05),
            Some(3),
            Some(5000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let mut state = make_test_state(&conn);
    state.total_cost = 0.10;
    state.total_turns = 5;
    state.total_duration_ms = 10000;

    // Re-fetch the child run so we have the full AgentRun with costs
    let loaded_run = agent_mgr.get_run(&child_run.id).unwrap().unwrap();

    let step = make_test_step(
        "build",
        WorkflowStepStatus::Completed,
        Some("built"),
        Some("build output"),
        None,
        Some(&child_run.id),
        None,
    );
    let ctx = make_resume_ctx(
        [(("build".to_string(), 0), step)].into_iter().collect(),
        [(child_run.id.clone(), loaded_run)].into_iter().collect(),
    );

    restore_completed_step(&mut state, &ctx, "build", 0);

    // Costs should be accumulated from the child run
    assert!((state.total_cost - 0.15).abs() < 0.001);
    assert_eq!(state.total_turns, 8);
    assert_eq!(state.total_duration_ms, 15000);
}

#[test]
fn test_restore_completed_step_restores_gate_feedback() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    let mut step = make_test_step(
        "approval-gate",
        WorkflowStepStatus::Completed,
        Some("approved"),
        None,
        None,
        None,
        None,
    );
    step.gate_feedback = Some("LGTM, ship it".to_string());

    let ctx = make_resume_ctx(
        [(("approval-gate".to_string(), 0), step)]
            .into_iter()
            .collect(),
        HashMap::new(),
    );

    restore_completed_step(&mut state, &ctx, "approval-gate", 0);

    // Gate feedback should be restored for downstream steps
    assert_eq!(state.last_gate_feedback.as_deref(), Some("LGTM, ship it"));
}

// ── reap_finalization_stuck_workflow_runs tests ─────────────────────────────

#[test]
fn test_reap_finalization_all_completed_marks_completed() {
    let conn = setup_db();
    let wf_mgr = WorkflowManager::new(&conn);

    let (run_id, _) = make_running_wf(&conn, "flow");
    insert_terminal_step(&conn, &run_id, WorkflowStepStatus::Completed, 0);
    insert_terminal_step(&conn, &run_id, WorkflowStepStatus::Completed, 1);

    let count = wf_mgr.reap_finalization_stuck_workflow_runs(-1).unwrap();
    assert_eq!(count, 1);

    let run = wf_mgr.get_workflow_run(&run_id).unwrap().unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Completed);
}

#[test]
fn test_reap_finalization_any_failed_marks_failed() {
    let conn = setup_db();
    let wf_mgr = WorkflowManager::new(&conn);

    let (run_id, _) = make_running_wf(&conn, "flow");
    insert_terminal_step(&conn, &run_id, WorkflowStepStatus::Completed, 0);
    insert_terminal_step(&conn, &run_id, WorkflowStepStatus::Failed, 1);

    let count = wf_mgr.reap_finalization_stuck_workflow_runs(-1).unwrap();
    assert_eq!(count, 1);

    let run = wf_mgr.get_workflow_run(&run_id).unwrap().unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Failed);
}

#[test]
fn test_reap_finalization_timed_out_step_marks_failed() {
    let conn = setup_db();
    let wf_mgr = WorkflowManager::new(&conn);

    let (run_id, _) = make_running_wf(&conn, "flow");
    insert_terminal_step(&conn, &run_id, WorkflowStepStatus::TimedOut, 0);

    let count = wf_mgr.reap_finalization_stuck_workflow_runs(-1).unwrap();
    assert_eq!(count, 1);

    let run = wf_mgr.get_workflow_run(&run_id).unwrap().unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Failed);
}

#[test]
fn test_reap_finalization_step_still_running_not_touched() {
    let conn = setup_db();
    let wf_mgr = WorkflowManager::new(&conn);

    let (run_id, _) = make_running_wf(&conn, "flow");
    // One step still running — run must NOT be reaped
    let step_id = wf_mgr
        .insert_step(&run_id, "step", "actor", false, 0, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let count = wf_mgr.reap_finalization_stuck_workflow_runs(-1).unwrap();
    assert_eq!(count, 0);

    let run = wf_mgr.get_workflow_run(&run_id).unwrap().unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Running);
}

#[test]
fn test_reap_finalization_already_terminal_not_touched() {
    let conn = setup_db();
    let wf_mgr = WorkflowManager::new(&conn);

    let (run_id, _) = make_running_wf(&conn, "flow");
    wf_mgr
        .update_workflow_status(&run_id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    insert_terminal_step(&conn, &run_id, WorkflowStepStatus::Completed, 0);

    let count = wf_mgr.reap_finalization_stuck_workflow_runs(-1).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_reap_finalization_zero_steps_uses_started_at() {
    let conn = setup_db();
    let wf_mgr = WorkflowManager::new(&conn);

    // A running workflow run with NO steps — should fall back to started_at as age ref.
    // Threshold -1 ensures elapsed > -1 is always true, avoiding same-second false negatives.
    let (run_id, _) = make_running_wf(&conn, "flow");

    let count = wf_mgr.reap_finalization_stuck_workflow_runs(-1).unwrap();
    assert_eq!(count, 1);

    let run = wf_mgr.get_workflow_run(&run_id).unwrap().unwrap();
    // No failed steps → Completed
    assert_eq!(run.status, WorkflowRunStatus::Completed);
}

#[test]
fn test_reap_finalization_updates_parent_agent_run() {
    let conn = setup_db();
    let wf_mgr = WorkflowManager::new(&conn);
    let agent_mgr = AgentManager::new(&conn);

    let (run_id, parent_id) = make_running_wf(&conn, "flow");
    insert_terminal_step(&conn, &run_id, WorkflowStepStatus::Completed, 0);

    wf_mgr.reap_finalization_stuck_workflow_runs(-1).unwrap();

    let parent = agent_mgr.get_run(&parent_id).unwrap().unwrap();
    assert_eq!(parent.status, crate::agent::AgentRunStatus::Completed);
}

#[test]
fn test_reap_finalization_updates_parent_agent_run_on_failure() {
    // Verifies that the parent agent_run is marked `failed` (not `completed`) when
    // at least one workflow step has a failed status.
    let conn = setup_db();
    let wf_mgr = WorkflowManager::new(&conn);
    let agent_mgr = AgentManager::new(&conn);

    let (run_id, parent_id) = make_running_wf(&conn, "flow");
    insert_terminal_step(&conn, &run_id, WorkflowStepStatus::Failed, 0);

    wf_mgr.reap_finalization_stuck_workflow_runs(-1).unwrap();

    let parent = agent_mgr.get_run(&parent_id).unwrap().unwrap();
    assert_eq!(parent.status, crate::agent::AgentRunStatus::Failed);
}

#[test]
fn test_reap_finalization_skipped_step_counts_as_success() {
    let conn = setup_db();
    let wf_mgr = WorkflowManager::new(&conn);

    let (run_id, _) = make_running_wf(&conn, "flow");
    insert_terminal_step(&conn, &run_id, WorkflowStepStatus::Skipped, 0);
    insert_terminal_step(&conn, &run_id, WorkflowStepStatus::Completed, 1);

    let count = wf_mgr.reap_finalization_stuck_workflow_runs(-1).unwrap();
    assert_eq!(count, 1);

    let run = wf_mgr.get_workflow_run(&run_id).unwrap().unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Completed);
}

#[test]
fn test_reap_finalization_child_run_not_reaped() {
    // Verifies that child workflow runs (parent_workflow_run_id IS NOT NULL) are excluded.
    // The reaper must only finalize root runs to avoid double-finalization of sub-workflows.
    let conn = setup_db();
    let wf_mgr = WorkflowManager::new(&conn);
    let agent_mgr = AgentManager::new(&conn);

    // Create a root workflow run (parent_workflow_run_id = NULL via make_running_wf).
    let (root_run_id, parent_agent_id) = make_running_wf(&conn, "root");

    // Create a child workflow run with parent_workflow_run_id set to root_run_id.
    let child_agent = agent_mgr
        .create_run(Some("w1"), "child", None, None)
        .unwrap();
    let child_run = wf_mgr
        .create_workflow_run_with_targets(
            "child",
            Some("w1"),
            None,
            None,
            &child_agent.id,
            false,
            "manual",
            None,
            Some(&root_run_id),
            None,
        )
        .unwrap();
    wf_mgr
        .update_workflow_status(&child_run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();
    insert_terminal_step(&conn, &child_run.id, WorkflowStepStatus::Completed, 0);

    // Only the root run (no parent_workflow_run_id) should be reaped.
    insert_terminal_step(&conn, &root_run_id, WorkflowStepStatus::Completed, 0);
    let _ = parent_agent_id;

    let count = wf_mgr.reap_finalization_stuck_workflow_runs(-1).unwrap();
    assert_eq!(count, 1);

    // Root finalized, child left untouched.
    let root = wf_mgr.get_workflow_run(&root_run_id).unwrap().unwrap();
    assert_eq!(root.status, WorkflowRunStatus::Completed);

    let child = wf_mgr.get_workflow_run(&child_run.id).unwrap().unwrap();
    assert_eq!(child.status, WorkflowRunStatus::Running);
}

#[test]
fn test_reap_finalization_respects_threshold() {
    // Verifies that a recently-finished run is NOT reaped when threshold_secs hasn't elapsed.
    // All existing tests use threshold=-1 (bypass), but production uses threshold=60.
    // Using i64::MAX ensures elapsed time will never exceed the threshold.
    let conn = setup_db();
    let wf_mgr = WorkflowManager::new(&conn);

    let (run_id, _) = make_running_wf(&conn, "flow");
    insert_terminal_step(&conn, &run_id, WorkflowStepStatus::Completed, 0);

    let count = wf_mgr
        .reap_finalization_stuck_workflow_runs(i64::MAX)
        .unwrap();
    assert_eq!(count, 0);

    let run = wf_mgr.get_workflow_run(&run_id).unwrap().unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Running);
}
