use super::*;
use crate::agent::AgentManager;
use crate::db::{sql_placeholders, sql_placeholders_from};
use crate::workflow::status::{WorkflowRunStatus, WorkflowStepStatus};
use crate::workflow::types::WorkflowRun;
use crate::workflow_dsl::GateType;

fn setup_db() -> rusqlite::Connection {
    let conn = crate::test_helpers::setup_db();
    // Add a second repo and worktrees for cross-repo filtering tests
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
         VALUES ('r2', 'other-repo', '/tmp/repo2', 'https://github.com/test/repo2.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r1', 'fix-bug', 'fix/bug', '/tmp/ws/fix-bug', 'active', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w3', 'r2', 'other-feat', 'feat/other', '/tmp/ws2/other-feat', 'active', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn
}

fn make_parent_id(conn: &rusqlite::Connection, wt_id: &str) -> String {
    AgentManager::new(conn)
        .create_run(Some(wt_id), "workflow", None, None)
        .unwrap()
        .id
}

// Helper to create a run linked to a worktree (worktree_id set, repo_id null — simulates
// the common case where runs are launched from a worktree context).
fn create_worktree_run(conn: &rusqlite::Connection, wt_id: &str) -> WorkflowRun {
    let parent_id = make_parent_id(conn, wt_id);
    WorkflowManager::new(conn)
        .create_workflow_run("wf", Some(wt_id), &parent_id, false, "manual", None)
        .unwrap()
}

// Helper to set a step's status without touching optional fields.
fn set_step_status(mgr: &WorkflowManager, step_id: &str, status: WorkflowStepStatus) {
    mgr.update_step_status(step_id, status, None, None, None, None, None)
        .unwrap();
}

// Helper to create a run linked directly to a repo (repo_id set, worktree_id null).
fn create_repo_run(conn: &rusqlite::Connection, repo_id: &str) -> WorkflowRun {
    // Need a valid parent agent run; use w1 as the worktree for the agent run.
    let parent_id = make_parent_id(conn, "w1");
    WorkflowManager::new(conn)
        .create_workflow_run_with_targets(
            "wf",
            None,
            None,
            Some(repo_id),
            &parent_id,
            false,
            "manual",
            None,
            None,
            None,
            None,
        )
        .unwrap()
}

#[test]
fn test_list_workflow_runs_for_repo_includes_worktree_runs() {
    // Runs linked to a worktree (repo_id NULL) should appear when querying by repo.
    let conn = setup_db();
    let run = create_worktree_run(&conn, "w1");
    let runs = WorkflowManager::new(&conn)
        .list_workflow_runs_for_repo("r1", 50)
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, run.id);
}

#[test]
fn test_list_workflow_runs_for_repo_includes_repo_targeted_runs() {
    // Runs with repo_id set directly should also appear.
    let conn = setup_db();
    let run = create_repo_run(&conn, "r1");
    let runs = WorkflowManager::new(&conn)
        .list_workflow_runs_for_repo("r1", 50)
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, run.id);
}

#[test]
fn test_list_workflow_runs_for_repo_distinct_no_duplicates() {
    // A run that matches both paths (repo_id = r1 AND worktree belongs to r1) should
    // appear exactly once thanks to SELECT DISTINCT.
    let conn = setup_db();
    let parent_id = make_parent_id(&conn, "w1");
    WorkflowManager::new(&conn)
        .create_workflow_run_with_targets(
            "wf",
            Some("w1"),
            None,
            Some("r1"),
            &parent_id,
            false,
            "manual",
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let runs = WorkflowManager::new(&conn)
        .list_workflow_runs_for_repo("r1", 50)
        .unwrap();
    assert_eq!(
        runs.len(),
        1,
        "run matching both join paths must appear only once"
    );
}

#[test]
fn test_list_workflow_runs_for_repo_cross_repo_filtering() {
    // Runs belonging to r2 must not appear when querying r1, and vice versa.
    let conn = setup_db();
    create_worktree_run(&conn, "w1"); // r1
    create_worktree_run(&conn, "w3"); // r2 via worktree
    create_repo_run(&conn, "r2"); // r2 directly

    let r1_runs = WorkflowManager::new(&conn)
        .list_workflow_runs_for_repo("r1", 50)
        .unwrap();
    assert_eq!(r1_runs.len(), 1);
    let r2_runs = WorkflowManager::new(&conn)
        .list_workflow_runs_for_repo("r2", 50)
        .unwrap();
    assert_eq!(r2_runs.len(), 2);
}

#[test]
fn test_list_workflow_runs_for_repo_limit() {
    // Only `limit` most recent runs should be returned.
    let conn = setup_db();
    for _ in 0..5 {
        create_worktree_run(&conn, "w1");
    }
    let runs = WorkflowManager::new(&conn)
        .list_workflow_runs_for_repo("r1", 3)
        .unwrap();
    assert_eq!(runs.len(), 3);
}

#[test]
fn test_list_workflow_runs_for_repo_multiple_worktrees() {
    // Runs from different worktrees of the same repo should all appear.
    let conn = setup_db();
    create_worktree_run(&conn, "w1");
    create_worktree_run(&conn, "w2");
    let runs = WorkflowManager::new(&conn)
        .list_workflow_runs_for_repo("r1", 50)
        .unwrap();
    assert_eq!(runs.len(), 2);
}

#[test]
fn test_list_workflow_runs_for_repo_empty() {
    let conn = setup_db();
    let runs = WorkflowManager::new(&conn)
        .list_workflow_runs_for_repo("r1", 50)
        .unwrap();
    assert!(runs.is_empty());
}

// ── list_active_workflow_runs ────────────────────────────────────────────

#[test]
fn test_list_active_workflow_runs_empty_slice_defaults_to_pending_running_waiting() {
    // Empty status slice should default to [pending, running, waiting].
    // A completed run must NOT appear; pending/running runs must appear.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let pending_run = create_worktree_run(&conn, "w1"); // created as pending
    let running_run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
        .unwrap();
    let completed_run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&completed_run.id, WorkflowRunStatus::Completed, None)
        .unwrap();

    let runs = mgr.list_active_workflow_runs(&[]).unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&pending_run.id.as_str()),
        "pending run must appear"
    );
    assert!(
        ids.contains(&running_run.id.as_str()),
        "running run must appear"
    );
    assert!(
        !ids.contains(&completed_run.id.as_str()),
        "completed run must not appear"
    );
}

#[test]
fn test_list_active_workflow_runs_explicit_status_filter() {
    // When an explicit status slice is given, only runs with those statuses appear.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let pending_run = create_worktree_run(&conn, "w1");
    let running_run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
        .unwrap();

    // Ask only for running — pending must not appear.
    let runs = mgr
        .list_active_workflow_runs(&[WorkflowRunStatus::Running])
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&running_run.id.as_str()),
        "running run must appear"
    );
    assert!(
        !ids.contains(&pending_run.id.as_str()),
        "pending run must not appear when filter is running-only"
    );
}

#[test]
fn test_list_active_workflow_runs_null_worktree_included() {
    // Runs with no worktree_id (ephemeral/repo-targeted runs) must always appear.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let repo_run = create_repo_run(&conn, "r1"); // worktree_id IS NULL

    let runs = mgr
        .list_active_workflow_runs(&[WorkflowRunStatus::Pending])
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&repo_run.id.as_str()),
        "repo-targeted run with NULL worktree_id must be included"
    );
}

#[test]
fn test_list_active_workflow_runs_inactive_worktree_excluded() {
    // Runs linked to a non-active (e.g. merged) worktree must not appear.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");

    // Mark w1 as merged so it no longer counts as active.
    conn.execute("UPDATE worktrees SET status = 'merged' WHERE id = 'w1'", [])
        .unwrap();

    let runs = mgr
        .list_active_workflow_runs(&[WorkflowRunStatus::Pending])
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        !ids.contains(&run.id.as_str()),
        "run linked to a merged worktree must not appear"
    );
}

// --- list_all_waiting_gate_steps ---

#[test]
fn test_list_all_waiting_gate_steps_empty() {
    let conn = setup_db();
    let steps = WorkflowManager::new(&conn)
        .list_all_waiting_gate_steps()
        .unwrap();
    assert!(steps.is_empty(), "no gate steps should exist yet");
}

#[test]
fn test_list_all_waiting_gate_steps_returns_waiting_gate_steps() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_worktree_run(&conn, "w1");

    let step_id = mgr
        .insert_step(&run.id, "approval-gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(
        &step_id,
        GateType::HumanApproval,
        Some("Please approve"),
        "1h",
    )
    .unwrap();
    // Mark step as waiting so it appears in the query.
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    let steps = mgr.list_all_waiting_gate_steps().unwrap();
    assert_eq!(steps.len(), 1, "one waiting gate step should be returned");
    let (step, workflow_name, target_label) = &steps[0];
    assert_eq!(step.id, step_id);
    assert_eq!(step.step_name, "approval-gate");
    assert_eq!(workflow_name, "wf");
    assert!(target_label.is_none(), "no target_label set on this run");
}

#[test]
fn test_list_all_waiting_gate_steps_excludes_non_gate_steps() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_worktree_run(&conn, "w1");

    // Regular step with no gate_type — must not appear.
    let step_id = mgr
        .insert_step(&run.id, "regular-step", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    let steps = mgr.list_all_waiting_gate_steps().unwrap();
    assert!(
        steps.is_empty(),
        "steps without gate_type must not be returned"
    );
}

#[test]
fn test_list_active_workflow_runs_multiple_statuses_dynamic_placeholders() {
    // Passing two explicit statuses exercises the dynamic placeholder builder.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let pending_run = create_worktree_run(&conn, "w1");
    let running_run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
        .unwrap();
    let failed_run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&failed_run.id, WorkflowRunStatus::Failed, None)
        .unwrap();

    let runs = mgr
        .list_active_workflow_runs(&[WorkflowRunStatus::Pending, WorkflowRunStatus::Running])
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(ids.contains(&pending_run.id.as_str()));
    assert!(ids.contains(&running_run.id.as_str()));
    assert!(!ids.contains(&failed_run.id.as_str()));
}

#[test]
fn test_sql_placeholders() {
    assert_eq!(sql_placeholders(0), "");
    assert_eq!(sql_placeholders(1), "?1");
    assert_eq!(sql_placeholders(3), "?1, ?2, ?3");
}

#[test]
fn test_sql_placeholders_from_non_one_start() {
    assert_eq!(sql_placeholders_from(0, 5), "");
    assert_eq!(sql_placeholders_from(1, 2), "?2");
    assert_eq!(sql_placeholders_from(3, 4), "?4, ?5, ?6");
}

#[test]
fn test_get_active_steps_for_runs_groups_by_run_id() {
    // Seed two runs, each with one running step (and one completed step that
    // should be excluded).  Verify that get_active_steps_for_runs returns
    // only the running steps and groups them under the correct run_id.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run1 = create_worktree_run(&conn, "w1");
    let run2 = create_worktree_run(&conn, "w2");

    // run1: one running step, one completed step
    let step1_active = mgr
        .insert_step(&run1.id, "step-a", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step1_active, WorkflowStepStatus::Running);
    let step1_done = mgr
        .insert_step(&run1.id, "step-b", "actor", false, 1, 0)
        .unwrap();
    set_step_status(&mgr, &step1_done, WorkflowStepStatus::Completed);

    // run2: one running step only
    let step2_active = mgr
        .insert_step(&run2.id, "step-c", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step2_active, WorkflowStepStatus::Running);

    let result = mgr
        .get_active_steps_for_runs(&[run1.id.as_str(), run2.id.as_str()])
        .unwrap();

    // Each run should be present with exactly its active step.
    assert_eq!(result.len(), 2, "expected entries for both runs");

    let run1_steps = result.get(&run1.id).expect("run1 missing from result");
    assert_eq!(run1_steps.len(), 1, "run1 should have 1 active step");
    assert_eq!(run1_steps[0].id, step1_active);

    let run2_steps = result.get(&run2.id).expect("run2 missing from result");
    assert_eq!(run2_steps.len(), 1, "run2 should have 1 active step");
    assert_eq!(run2_steps[0].id, step2_active);
}

#[test]
fn test_get_active_steps_for_runs_includes_waiting_steps() {
    // Verify that get_active_steps_for_runs returns Waiting steps (not just
    // Running ones), and excludes Pending steps.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");

    // Insert a step and transition it to Waiting.
    let waiting_step = mgr
        .insert_step(&run.id, "step-a", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &waiting_step, WorkflowStepStatus::Waiting);

    // Insert a second step and leave it Pending — should be excluded.
    let _pending_step = mgr
        .insert_step(&run.id, "step-b", "actor", false, 1, 0)
        .unwrap();

    let result = mgr.get_active_steps_for_runs(&[run.id.as_str()]).unwrap();

    // Only the Waiting step should appear.
    assert_eq!(result.len(), 1, "expected one run entry");
    let steps = result.get(&run.id).expect("run missing from result");
    assert_eq!(steps.len(), 1, "expected exactly one active step");
    assert_eq!(
        steps[0].id, waiting_step,
        "active step should be the Waiting one"
    );
}

#[test]
fn test_get_active_steps_for_runs_empty_slice_returns_empty_map() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let result = mgr.get_active_steps_for_runs(&[]).unwrap();
    assert!(result.is_empty(), "empty run_ids must yield an empty map");
}

#[test]
fn test_get_steps_for_runs_empty_slice_returns_empty_map() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let result = mgr.get_steps_for_runs(&[]).unwrap();
    assert!(result.is_empty(), "empty run_ids must yield an empty map");
}

#[test]
fn test_get_workflow_run_ids_for_agent_runs_empty_slice_returns_empty_map() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let result = mgr.get_workflow_run_ids_for_agent_runs(&[]).unwrap();
    assert!(
        result.is_empty(),
        "empty agent_run_ids must yield an empty map"
    );
}

#[test]
fn test_get_step_summaries_for_runs_empty_slice_returns_empty_map() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let result = mgr.get_step_summaries_for_runs(&[]).unwrap();
    assert!(result.is_empty(), "empty run_ids must yield an empty map");
}

#[test]
fn test_get_steps_for_runs_returns_all_steps_regardless_of_status() {
    // Verify that get_steps_for_runs returns ALL steps (pending, running,
    // completed) for multiple runs, grouped by run_id.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run1 = create_worktree_run(&conn, "w1");
    let run2 = create_worktree_run(&conn, "w2");

    // run1: one running step and one completed step — both should appear
    let step1a = mgr
        .insert_step(&run1.id, "step-a", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step1a, WorkflowStepStatus::Running);
    let step1b = mgr
        .insert_step(&run1.id, "step-b", "actor", false, 1, 0)
        .unwrap();
    set_step_status(&mgr, &step1b, WorkflowStepStatus::Completed);

    // run2: one pending step
    let step2a = mgr
        .insert_step(&run2.id, "step-c", "actor", false, 0, 0)
        .unwrap();

    let result = mgr
        .get_steps_for_runs(&[run1.id.as_str(), run2.id.as_str()])
        .unwrap();

    assert_eq!(result.len(), 2, "expected entries for both runs");

    let run1_steps = result.get(&run1.id).expect("run1 missing from result");
    assert_eq!(run1_steps.len(), 2, "run1 should have both steps");
    let run1_ids: Vec<&str> = run1_steps.iter().map(|s| s.id.as_str()).collect();
    assert!(run1_ids.contains(&step1a.as_str()));
    assert!(run1_ids.contains(&step1b.as_str()));

    let run2_steps = result.get(&run2.id).expect("run2 missing from result");
    assert_eq!(run2_steps.len(), 1, "run2 should have one step");
    assert_eq!(run2_steps[0].id, step2a);
}

// ── list_workflow_runs_filtered ──────────────────────────────────────────

#[test]
fn test_list_workflow_runs_filtered_with_status() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let pending_run = create_worktree_run(&conn, "w1");
    let running_run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
        .unwrap();

    let runs = mgr
        .list_workflow_runs_filtered("w1", Some(WorkflowRunStatus::Running))
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&running_run.id.as_str()),
        "running run must appear"
    );
    assert!(
        !ids.contains(&pending_run.id.as_str()),
        "pending run must not appear"
    );
}

#[test]
fn test_list_workflow_runs_filtered_none_returns_all() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let pending_run = create_worktree_run(&conn, "w1");
    let running_run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
        .unwrap();

    let runs = mgr.list_workflow_runs_filtered("w1", None).unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&pending_run.id.as_str()),
        "pending run must appear"
    );
    assert!(
        ids.contains(&running_run.id.as_str()),
        "running run must appear"
    );
}

// ── list_workflow_runs_by_repo_id_filtered ───────────────────────────────

#[test]
fn test_list_workflow_runs_by_repo_id_filtered_with_status() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Use repo-targeted runs so workflow_runs.repo_id is set.
    let pending_run = create_repo_run(&conn, "r1");
    let running_run = create_repo_run(&conn, "r1");
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
        .unwrap();

    let runs = mgr
        .list_workflow_runs_by_repo_id_filtered("r1", 50, 0, Some(WorkflowRunStatus::Running))
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&running_run.id.as_str()),
        "running run must appear"
    );
    assert!(
        !ids.contains(&pending_run.id.as_str()),
        "pending run must not appear"
    );
}

#[test]
fn test_list_workflow_runs_by_repo_id_filtered_none_returns_all() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let pending_run = create_repo_run(&conn, "r1");
    let running_run = create_repo_run(&conn, "r1");
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
        .unwrap();

    let runs = mgr
        .list_workflow_runs_by_repo_id_filtered("r1", 50, 0, None)
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&pending_run.id.as_str()),
        "pending run must appear"
    );
    assert!(
        ids.contains(&running_run.id.as_str()),
        "running run must appear"
    );
}

// ── list_workflow_runs_filtered_paginated ────────────────────────────────

#[test]
fn test_list_workflow_runs_filtered_paginated_with_status() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Two pending runs plus one running run for the same worktree.
    let _pending1 = create_worktree_run(&conn, "w1");
    let _pending2 = create_worktree_run(&conn, "w1");
    let running = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&running.id, WorkflowRunStatus::Running, None)
        .unwrap();

    // First page: limit=1, offset=0 — exactly one pending run.
    let page1 = mgr
        .list_workflow_runs_filtered_paginated("w1", Some(WorkflowRunStatus::Pending), 1, 0)
        .unwrap();
    assert_eq!(
        page1.len(),
        1,
        "first page must have exactly one pending run"
    );

    // Second page: limit=1, offset=1 — the other pending run.
    let page2 = mgr
        .list_workflow_runs_filtered_paginated("w1", Some(WorkflowRunStatus::Pending), 1, 1)
        .unwrap();
    assert_eq!(
        page2.len(),
        1,
        "second page must have exactly one pending run"
    );

    assert_ne!(page1[0].id, page2[0].id, "pages must return different runs");
    assert!(
        page1[0].id != running.id && page2[0].id != running.id,
        "running run must not appear in pending-filtered results"
    );
}

#[test]
fn test_list_workflow_runs_filtered_paginated_none_delegates() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    for _ in 0..3 {
        create_worktree_run(&conn, "w1");
    }

    // None — no status filter, pagination alone controls results.
    let page1 = mgr
        .list_workflow_runs_filtered_paginated("w1", None, 2, 0)
        .unwrap();
    assert_eq!(page1.len(), 2, "limit=2 must return exactly 2 runs");

    let page2 = mgr
        .list_workflow_runs_filtered_paginated("w1", None, 2, 2)
        .unwrap();
    assert_eq!(
        page2.len(),
        1,
        "offset=2 with limit=2 must return the remaining run"
    );
}

// ── list_all_workflow_runs_filtered_paginated ────────────────────────────

#[test]
fn test_list_all_workflow_runs_filtered_paginated_with_status() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let pending_run = create_worktree_run(&conn, "w1");
    let running_run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
        .unwrap();

    let runs = mgr
        .list_all_workflow_runs_filtered_paginated(Some(WorkflowRunStatus::Running), 50, 0)
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&running_run.id.as_str()),
        "running run must appear"
    );
    assert!(
        !ids.contains(&pending_run.id.as_str()),
        "pending run must not appear"
    );
}

#[test]
fn test_list_all_workflow_runs_filtered_paginated_none() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run1 = create_worktree_run(&conn, "w1");
    let run2 = create_worktree_run(&conn, "w2");
    mgr.update_workflow_status(&run2.id, WorkflowRunStatus::Running, None)
        .unwrap();

    let runs = mgr
        .list_all_workflow_runs_filtered_paginated(None, 50, 0)
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(ids.contains(&run1.id.as_str()), "run1 must appear");
    assert!(ids.contains(&run2.id.as_str()), "run2 must appear");
}

#[test]
fn test_list_all_workflow_runs_filtered_paginated_excludes_inactive_worktrees() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let active_run = create_worktree_run(&conn, "w1");
    let inactive_run = create_worktree_run(&conn, "w2");
    conn.execute("UPDATE worktrees SET status = 'merged' WHERE id = 'w2'", [])
        .unwrap();

    let runs = mgr
        .list_all_workflow_runs_filtered_paginated(None, 50, 0)
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&active_run.id.as_str()),
        "active worktree run must appear"
    );
    assert!(
        !ids.contains(&inactive_run.id.as_str()),
        "merged worktree run must not appear"
    );
}

// ── list_all_workflow_runs ───────────────────────────────────────────────

#[test]
fn test_list_all_workflow_runs_respects_limit() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    for _ in 0..5 {
        create_worktree_run(&conn, "w1");
    }

    let runs = mgr.list_all_workflow_runs(3).unwrap();
    assert_eq!(runs.len(), 3, "limit=3 must return exactly 3 runs");
}

#[test]
fn test_list_all_workflow_runs_excludes_inactive_worktrees() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let active_run = create_worktree_run(&conn, "w1");
    let inactive_run = create_worktree_run(&conn, "w2");
    conn.execute("UPDATE worktrees SET status = 'merged' WHERE id = 'w2'", [])
        .unwrap();

    let runs = mgr.list_all_workflow_runs(50).unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&active_run.id.as_str()),
        "active worktree run must appear"
    );
    assert!(
        !ids.contains(&inactive_run.id.as_str()),
        "merged worktree run must not appear"
    );
}

#[test]
fn test_list_all_waiting_gate_steps_excludes_approved_gate_steps() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_worktree_run(&conn, "w1");

    let step_id = mgr
        .insert_step(&run.id, "gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, None, "1h")
        .unwrap();
    // Mark as completed (approved) — must not appear in waiting list.
    conn.execute(
        "UPDATE workflow_run_steps SET status = 'completed', gate_approved_at = '2024-01-01T00:00:00Z' WHERE id = ?1",
        rusqlite::params![step_id],
    ).unwrap();

    let steps = mgr.list_all_waiting_gate_steps().unwrap();
    assert!(
        steps.is_empty(),
        "approved (completed) gate steps must not be returned"
    );
}

#[test]
fn test_list_all_waiting_gate_steps_includes_target_label() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let parent_id = make_parent_id(&conn, "w1");
    let run = mgr
        .create_workflow_run_with_targets(
            "deploy",
            Some("w1"),
            None,
            None,
            &parent_id,
            false,
            "manual",
            None,
            None,
            Some("conductor-ai/feat-123"),
            None,
        )
        .unwrap();

    let step_id = mgr
        .insert_step(&run.id, "approve-deploy", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, None, "1h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    let steps = mgr.list_all_waiting_gate_steps().unwrap();
    assert_eq!(steps.len(), 1);
    let (step, workflow_name, target_label) = &steps[0];
    assert_eq!(step.id, step_id);
    assert_eq!(workflow_name, "deploy");
    assert_eq!(
        target_label.as_deref(),
        Some("conductor-ai/feat-123"),
        "target_label must be propagated from workflow_runs"
    );
}

// --- list_waiting_gate_steps_for_repo ---

#[test]
fn test_list_waiting_gate_steps_for_repo_empty() {
    let conn = setup_db();
    let steps = WorkflowManager::new(&conn)
        .list_waiting_gate_steps_for_repo("r1")
        .unwrap();
    assert!(steps.is_empty(), "no gate steps should exist yet");
}

#[test]
fn test_list_waiting_gate_steps_for_repo_via_worktree() {
    // Runs linked through a worktree (worktree_id set, repo_id NULL) must appear.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    // w1 belongs to r1 (seeded in setup_db via test_helpers)
    let run = create_worktree_run(&conn, "w1");

    let step_id = mgr
        .insert_step(&run.id, "approval-gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(
        &step_id,
        GateType::HumanApproval,
        Some("Please approve"),
        "1h",
    )
    .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
    assert_eq!(
        steps.len(),
        1,
        "worktree-linked gate step must appear for its repo"
    );
    let row = &steps[0];
    assert_eq!(row.step.id, step_id);
    assert_eq!(row.step.step_name, "approval-gate");
    assert_eq!(row.workflow_name, "wf");
    assert!(row.target_label.is_none());
    assert_eq!(
        row.branch.as_deref(),
        Some("feat/test"),
        "branch must be propagated from the worktree"
    );
    assert!(
        row.ticket_ref.is_none(),
        "no ticket linked to this worktree"
    );
}

#[test]
fn test_list_waiting_gate_steps_for_repo_via_direct_repo_id() {
    // Runs with repo_id set directly (no worktree) must also appear.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_repo_run(&conn, "r1");

    let step_id = mgr
        .insert_step(&run.id, "direct-gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, None, "1h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
    assert_eq!(
        steps.len(),
        1,
        "directly-linked gate step must appear for its repo"
    );
    assert_eq!(steps[0].step.id, step_id);
    assert!(
        steps[0].branch.is_none(),
        "repo-targeted run has no worktree branch"
    );
    assert!(
        steps[0].ticket_ref.is_none(),
        "no ticket linked to this run"
    );
}

#[test]
fn test_list_waiting_gate_steps_for_repo_ticket_ref_populated() {
    // When the workflow run has a linked ticket, ticket_ref must be the ticket's source_id.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_worktree_run(&conn, "w1");

    // Insert a ticket and link it to the workflow run.
    conn.execute(
        "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
         VALUES ('ticket-1', 'r1', 'github', '42', 'Fix bug', '', 'open', '[]', '', '2024-01-01T00:00:00Z', '{}')",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET ticket_id = 'ticket-1' WHERE id = ?1",
        [&run.id],
    )
    .unwrap();

    let step_id = mgr
        .insert_step(&run.id, "approval-gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, None, "1h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(
        steps[0].ticket_ref.as_deref(),
        Some("42"),
        "ticket_ref must be the ticket's source_id"
    );
}

#[test]
fn test_list_waiting_gate_steps_for_repo_excludes_other_repo() {
    // Gate steps from r2 must not appear when querying r1, and vice versa.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    // w3 belongs to r2 (inserted in setup_db)
    let run = create_worktree_run(&conn, "w3");

    let step_id = mgr
        .insert_step(&run.id, "gate-other", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, None, "1h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    let steps_r1 = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
    assert!(steps_r1.is_empty(), "r1 must not see r2's gate steps");

    let steps_r2 = mgr.list_waiting_gate_steps_for_repo("r2").unwrap();
    assert_eq!(steps_r2.len(), 1, "r2 should return its own gate step");
    assert_eq!(steps_r2[0].step.id, step_id);
}

#[test]
fn test_list_waiting_gate_steps_for_repo_excludes_non_gate_steps() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_worktree_run(&conn, "w1");

    // A regular actor step with waiting status must not be returned.
    let step_id = mgr
        .insert_step(&run.id, "regular-step", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
    assert!(steps.is_empty(), "non-gate steps must not be returned");
}

#[test]
fn test_list_waiting_gate_steps_for_repo_excludes_completed_gate_steps() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_worktree_run(&conn, "w1");

    let step_id = mgr
        .insert_step(&run.id, "gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, None, "1h")
        .unwrap();
    conn.execute(
        "UPDATE workflow_run_steps SET status = 'completed', gate_approved_at = '2024-01-01T00:00:00Z' WHERE id = ?1",
        rusqlite::params![step_id],
    ).unwrap();

    let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
    assert!(
        steps.is_empty(),
        "completed gate steps must not be returned"
    );
}

#[test]
fn test_list_waiting_gate_steps_for_repo_excludes_cancelled_run() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_worktree_run(&conn, "w1");

    let step_id = mgr
        .insert_step(&run.id, "gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, None, "1h")
        .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET status = 'cancelled' WHERE id = ?1",
        [&run.id],
    )
    .unwrap();

    let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
    assert!(
        steps.is_empty(),
        "waiting gate steps from cancelled runs must not be returned"
    );
}

#[test]
fn test_list_waiting_gate_steps_for_repo_excludes_failed_run() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_worktree_run(&conn, "w1");

    let step_id = mgr
        .insert_step(&run.id, "gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, None, "1h")
        .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET status = 'failed' WHERE id = ?1",
        [&run.id],
    )
    .unwrap();

    let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
    assert!(
        steps.is_empty(),
        "waiting gate steps from failed runs must not be returned"
    );
}

#[test]
fn test_set_workflow_run_iteration() {
    let conn = setup_db();
    let run = create_worktree_run(&conn, "w1");
    let mgr = WorkflowManager::new(&conn);

    // Default iteration should be 0.
    let fetched = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(fetched.iteration, 0);

    // Set iteration to 3.
    mgr.set_workflow_run_iteration(&run.id, 3).unwrap();
    let fetched = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(fetched.iteration, 3);

    // Set iteration to 0 again.
    mgr.set_workflow_run_iteration(&run.id, 0).unwrap();
    let fetched = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(fetched.iteration, 0);
}

// -----------------------------------------------------------------------
// validate_single tests
// -----------------------------------------------------------------------

/// Helper to build a minimal WorkflowDef for validation tests.
fn minimal_workflow(name: &str) -> crate::workflow_dsl::WorkflowDef {
    crate::workflow_dsl::WorkflowDef {
        name: name.to_string(),
        description: "test workflow".to_string(),
        trigger: crate::workflow_dsl::WorkflowTrigger::Manual,
        targets: vec![],
        group: None,
        inputs: vec![],
        body: vec![],
        always: vec![],
        source_path: "test.wf".to_string(),
    }
}

/// Create a temp dir with a `.conductor/workflows/<name>.wf` file so the
/// loader used by cycle detection can resolve the workflow by name.
fn write_wf_file(dir: &std::path::Path, name: &str, content: &str) {
    let wf_dir = dir.join(".conductor/workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(wf_dir.join(format!("{name}.wf")), content).unwrap();
}

#[test]
fn test_validate_single_returns_entry_for_valid_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    let wf_src = "workflow good-wf {\n  meta {\n    description = \"test\"\n    trigger = \"manual\"\n    targets = [\"worktree\"]\n  }\n}\n";
    write_wf_file(tmp.path(), "good-wf", wf_src);

    let wf = minimal_workflow("good-wf");
    let known_bots = std::collections::HashSet::new();
    let path = tmp.path().to_str().unwrap();

    let entry = WorkflowManager::validate_single(path, path, &wf, &known_bots);

    assert_eq!(entry.name, "good-wf");
    assert!(
        entry.errors.is_empty(),
        "expected no errors: {:?}",
        entry.errors
    );
}

#[test]
fn test_validate_single_surfaces_warnings_for_unknown_bot() {
    use crate::workflow_dsl::{AgentRef, CallNode, WorkflowNode};

    let tmp = tempfile::tempdir().unwrap();
    let wf_src = "workflow bot-wf {\n  meta {\n    description = \"test\"\n    trigger = \"manual\"\n    targets = [\"worktree\"]\n  }\n  call some-step { as = \"unknown-bot\" }\n}\n";
    write_wf_file(tmp.path(), "bot-wf", wf_src);

    let mut wf = minimal_workflow("bot-wf");
    wf.body.push(WorkflowNode::Call(CallNode {
        agent: AgentRef::Name("some-step".to_string()),
        retries: 0,
        on_fail: None,
        output: None,
        with: vec![],
        bot_name: Some("unknown-bot".to_string()),
        plugin_dirs: vec![],
    }));
    // known_bots is empty, so "unknown-bot" should produce a warning
    let known_bots = std::collections::HashSet::new();
    let path = tmp.path().to_str().unwrap();

    let entry = WorkflowManager::validate_single(path, path, &wf, &known_bots);

    assert_eq!(entry.name, "bot-wf");
    assert!(
        !entry.warnings.is_empty(),
        "expected warning for unknown bot name, got none"
    );
    assert!(
        entry.warnings[0].message.contains("unknown-bot"),
        "warning should mention the unknown bot name: {}",
        entry.warnings[0].message
    );
}

#[test]
fn test_validate_single_reports_errors_for_missing_agent() {
    use crate::workflow_dsl::{AgentRef, CallNode, WorkflowNode};

    let tmp = tempfile::tempdir().unwrap();
    let wf_src = "workflow bad-wf {\n  meta {\n    description = \"test\"\n    trigger = \"manual\"\n    targets = [\"worktree\"]\n  }\n  call nonexistent-agent\n}\n";
    write_wf_file(tmp.path(), "bad-wf", wf_src);

    let mut wf = minimal_workflow("bad-wf");
    wf.body.push(WorkflowNode::Call(CallNode {
        agent: AgentRef::Name("nonexistent-agent".to_string()),
        retries: 0,
        on_fail: None,
        output: None,
        with: vec![],
        bot_name: None,
        plugin_dirs: vec![],
    }));
    let known_bots = std::collections::HashSet::new();
    let path = tmp.path().to_str().unwrap();

    let entry = WorkflowManager::validate_single(path, path, &wf, &known_bots);

    assert_eq!(entry.name, "bad-wf");
    assert!(
        !entry.errors.is_empty(),
        "expected validation errors for missing agent"
    );
}

// ── get_step_summaries_for_runs — child-chain traversal ─────────────────

#[test]
fn test_get_step_summaries_for_runs_single_level_running_step() {
    // A root run with a running step (no child chain) should return a summary
    // with an empty workflow_chain.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "build", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Running);

    let summaries = mgr.get_step_summaries_for_runs(&[run.id.as_str()]).unwrap();
    assert_eq!(summaries.len(), 1);
    let summary = summaries.get(&run.id).expect("root run missing");
    assert_eq!(summary.step_name, "build");
    assert!(
        summary.workflow_chain.is_empty(),
        "single-level run should have empty workflow_chain"
    );
}

#[test]
fn test_get_step_summaries_for_runs_child_chain_traversal() {
    // Root → child (running) with a running step.
    // The summary should reflect the child's step and a workflow_chain containing
    // the root workflow name (but not the child's own name).
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Create root run and set it running.
    let root = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&root.id, WorkflowRunStatus::Running, None)
        .unwrap();

    // Create a child workflow run parented under the root.
    let parent_agent_id = make_parent_id(&conn, "w1");
    let child = mgr
        .create_workflow_run_with_targets(
            "child-wf",
            Some("w1"),
            None,
            None,
            &parent_agent_id,
            false,
            "manual",
            None,
            Some(&root.id), // parent_workflow_run_id
            None,
            None,
        )
        .unwrap();
    mgr.update_workflow_status(&child.id, WorkflowRunStatus::Running, None)
        .unwrap();

    // Add a running step on the child.
    let child_step_id = mgr
        .insert_step(&child.id, "deploy", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &child_step_id, WorkflowStepStatus::Running);

    let summaries = mgr
        .get_step_summaries_for_runs(&[root.id.as_str()])
        .unwrap();
    assert_eq!(summaries.len(), 1);
    let summary = summaries.get(&root.id).expect("root run missing");
    assert_eq!(summary.step_name, "deploy");
    assert_eq!(
        summary.workflow_chain,
        vec!["wf"],
        "chain should contain root name only (leaf excluded)"
    );
}

#[test]
fn test_get_step_summaries_for_runs_deep_chain() {
    // Root → child → grandchild (running). Verifies multi-level chain traversal.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let root = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&root.id, WorkflowRunStatus::Running, None)
        .unwrap();

    let agent_id = make_parent_id(&conn, "w1");
    let child = mgr
        .create_workflow_run_with_targets(
            "mid-wf",
            Some("w1"),
            None,
            None,
            &agent_id,
            false,
            "manual",
            None,
            Some(&root.id),
            None,
            None,
        )
        .unwrap();
    mgr.update_workflow_status(&child.id, WorkflowRunStatus::Running, None)
        .unwrap();

    let agent_id2 = make_parent_id(&conn, "w1");
    let grandchild = mgr
        .create_workflow_run_with_targets(
            "leaf-wf",
            Some("w1"),
            None,
            None,
            &agent_id2,
            false,
            "manual",
            None,
            Some(&child.id),
            None,
            None,
        )
        .unwrap();
    mgr.update_workflow_status(&grandchild.id, WorkflowRunStatus::Running, None)
        .unwrap();

    let step_id = mgr
        .insert_step(&grandchild.id, "test", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Running);

    let summaries = mgr
        .get_step_summaries_for_runs(&[root.id.as_str()])
        .unwrap();
    let summary = summaries.get(&root.id).expect("root run missing");
    assert_eq!(summary.step_name, "test");
    assert_eq!(
        summary.workflow_chain,
        vec!["wf", "mid-wf"],
        "chain should contain root + middle names, excluding the leaf"
    );
}

#[test]
fn test_get_step_summaries_for_runs_no_running_step_omitted() {
    // A run with no running step should not appear in the result map.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
        .unwrap();
    // Insert a completed step — not running.
    let step_id = mgr
        .insert_step(&run.id, "done", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Completed);

    let summaries = mgr.get_step_summaries_for_runs(&[run.id.as_str()]).unwrap();
    assert!(
        summaries.is_empty(),
        "run with no running step should be absent from summaries"
    );
}

#[test]
fn test_get_step_summaries_for_runs_multiple_roots() {
    // Two independent root runs, each with a running step. Verifies the batch path
    // returns correct summaries for both roots in a single call.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run1 = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&run1.id, WorkflowRunStatus::Running, None)
        .unwrap();
    let step1_id = mgr
        .insert_step(&run1.id, "compile", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step1_id, WorkflowStepStatus::Running);

    let run2 = create_worktree_run(&conn, "w2");
    mgr.update_workflow_status(&run2.id, WorkflowRunStatus::Running, None)
        .unwrap();
    let step2_id = mgr
        .insert_step(&run2.id, "lint", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step2_id, WorkflowStepStatus::Running);

    let summaries = mgr
        .get_step_summaries_for_runs(&[run1.id.as_str(), run2.id.as_str()])
        .unwrap();

    assert_eq!(summaries.len(), 2);
    let s1 = summaries.get(&run1.id).expect("run1 missing");
    assert_eq!(s1.step_name, "compile");
    assert!(s1.workflow_chain.is_empty());

    let s2 = summaries.get(&run2.id).expect("run2 missing");
    assert_eq!(s2.step_name, "lint");
    assert!(s2.workflow_chain.is_empty());
}

#[test]
fn test_get_step_summaries_for_runs_multiple_roots_with_chains() {
    // Two root runs: one with a child chain (leaf has the running step),
    // one flat (root itself has the running step). Verifies workflow_chain is
    // populated correctly per root using the batch path.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Root A: has an active child with a running step.
    let root_a = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&root_a.id, WorkflowRunStatus::Running, None)
        .unwrap();
    let agent_id_a = make_parent_id(&conn, "w1");
    let child_a = mgr
        .create_workflow_run_with_targets(
            "child-wf-a",
            Some("w1"),
            None,
            None,
            &agent_id_a,
            false,
            "manual",
            None,
            Some(&root_a.id),
            None,
            None,
        )
        .unwrap();
    mgr.update_workflow_status(&child_a.id, WorkflowRunStatus::Running, None)
        .unwrap();
    let step_a_id = mgr
        .insert_step(&child_a.id, "deploy", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step_a_id, WorkflowStepStatus::Running);

    // Root B: no children, running step directly on root.
    let root_b = create_worktree_run(&conn, "w2");
    mgr.update_workflow_status(&root_b.id, WorkflowRunStatus::Running, None)
        .unwrap();
    let step_b_id = mgr
        .insert_step(&root_b.id, "test", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step_b_id, WorkflowStepStatus::Running);

    let summaries = mgr
        .get_step_summaries_for_runs(&[root_a.id.as_str(), root_b.id.as_str()])
        .unwrap();

    assert_eq!(summaries.len(), 2);

    let sa = summaries.get(&root_a.id).expect("root_a missing");
    assert_eq!(sa.step_name, "deploy");
    assert_eq!(
        sa.workflow_chain,
        vec!["wf"],
        "root_a chain should contain root name only (child excluded as leaf)"
    );

    let sb = summaries.get(&root_b.id).expect("root_b missing");
    assert_eq!(sb.step_name, "test");
    assert!(
        sb.workflow_chain.is_empty(),
        "root_b has no children so chain should be empty"
    );
}

// ── cancel_run ──────────────────────────────────────────────────────────

#[test]
fn test_cancel_run_marks_run_cancelled() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
        .unwrap();

    mgr.cancel_run(&run.id, "user requested").unwrap();

    let updated = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(updated.status, WorkflowRunStatus::Cancelled);
    assert_eq!(updated.result_summary.as_deref(), Some("user requested"));
}

#[test]
fn test_cancel_run_fails_on_terminal_state() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None)
        .unwrap();

    let result = mgr.cancel_run(&run.id, "too late");
    assert!(result.is_err(), "cancelling a completed run should fail");
}

#[test]
fn test_cancel_run_marks_active_steps_failed() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
        .unwrap();

    // One running step and one completed step.
    let running_step = mgr
        .insert_step(&run.id, "step-a", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &running_step, WorkflowStepStatus::Running);
    let completed_step = mgr
        .insert_step(&run.id, "step-b", "actor", false, 1, 0)
        .unwrap();
    set_step_status(&mgr, &completed_step, WorkflowStepStatus::Completed);

    mgr.cancel_run(&run.id, "abort").unwrap();

    let steps = mgr.get_workflow_steps(&run.id).unwrap();
    let running = steps.iter().find(|s| s.id == running_step).unwrap();
    assert_eq!(
        running.status,
        WorkflowStepStatus::Failed,
        "active step should be marked failed"
    );
    let completed = steps.iter().find(|s| s.id == completed_step).unwrap();
    assert_eq!(
        completed.status,
        WorkflowStepStatus::Completed,
        "completed step should remain completed"
    );
}

// ── workflow def target filter predicate ─────────────────────────────────

fn make_repo_workflow(name: &str) -> crate::workflow::WorkflowDef {
    crate::workflow::WorkflowDef {
        name: name.to_string(),
        description: "repo-scoped workflow".to_string(),
        trigger: crate::workflow::WorkflowTrigger::Manual,
        targets: vec!["repo".to_string()],
        group: None,
        inputs: vec![],
        body: vec![],
        always: vec![],
        source_path: format!(".conductor/workflows/{name}.wf"),
    }
}

fn make_worktree_workflow(name: &str) -> crate::workflow::WorkflowDef {
    crate::workflow::WorkflowDef {
        name: name.to_string(),
        description: "worktree-scoped workflow".to_string(),
        trigger: crate::workflow::WorkflowTrigger::Manual,
        targets: vec!["worktree".to_string()],
        group: None,
        inputs: vec![],
        body: vec![],
        always: vec![],
        source_path: format!(".conductor/workflows/{name}.wf"),
    }
}

#[test]
fn test_filter_repo_target() {
    // Mixed-target slice: only the repo workflow should survive the "repo" filter.
    let defs = [
        make_repo_workflow("repo-wf"),
        make_worktree_workflow("wt-wf"),
    ];
    let filter = "repo";
    let filtered: Vec<_> = defs
        .iter()
        .filter(|d| d.targets.iter().any(|t| t == filter))
        .collect();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "repo-wf");
}

#[test]
fn test_filter_worktree_target() {
    // Mixed-target slice: only the two worktree workflows should survive the "worktree" filter.
    let defs = [
        make_repo_workflow("repo-wf"),
        make_worktree_workflow("wt-wf-1"),
        make_worktree_workflow("wt-wf-2"),
    ];
    let filter = "worktree";
    let filtered: Vec<_> = defs
        .iter()
        .filter(|d| d.targets.iter().any(|t| t == filter))
        .collect();
    assert_eq!(filtered.len(), 2);
    assert!(filtered.iter().all(|d| d.name.starts_with("wt-wf")));
}

#[test]
fn test_cancel_run_not_found() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let result = mgr.cancel_run("nonexistent-id", "reason");
    assert!(result.is_err(), "cancelling a nonexistent run should fail");
}
