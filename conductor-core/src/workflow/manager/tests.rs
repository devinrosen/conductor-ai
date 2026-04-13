use super::*;
use crate::agent::AgentManager;
use crate::db::{sql_placeholders, sql_placeholders_from};
use crate::workflow::status::{WorkflowRunStatus, WorkflowStepStatus};
use crate::workflow::types::{TimeGranularity, WorkflowRun};
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
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();
    let completed_run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&completed_run.id, WorkflowRunStatus::Completed, None, None)
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
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();
    let failed_run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&failed_run.id, WorkflowRunStatus::Failed, None, None)
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
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&running.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&run2.id, WorkflowRunStatus::Running, None, None)
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
        title: None,
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
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&root.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&child.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&root.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&child.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&grandchild.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&run1.id, WorkflowRunStatus::Running, None, None)
        .unwrap();
    let step1_id = mgr
        .insert_step(&run1.id, "compile", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step1_id, WorkflowStepStatus::Running);

    let run2 = create_worktree_run(&conn, "w2");
    mgr.update_workflow_status(&run2.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&root_a.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&child_a.id, WorkflowRunStatus::Running, None, None)
        .unwrap();
    let step_a_id = mgr
        .insert_step(&child_a.id, "deploy", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step_a_id, WorkflowStepStatus::Running);

    // Root B: no children, running step directly on root.
    let root_b = create_worktree_run(&conn, "w2");
    mgr.update_workflow_status(&root_b.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
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
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    let result = mgr.cancel_run(&run.id, "too late");
    assert!(result.is_err(), "cancelling a completed run should fail");
}

#[test]
fn test_cancel_run_marks_active_steps_failed() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
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
        title: None,
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
        title: None,
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

/// `fail_workflow_run` marks the workflow run as failed and returns the parent run ID.
/// Callers should handle updating the parent agent run separately.
#[test]
fn test_fail_workflow_run_returns_parent_id() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let wf_mgr = WorkflowManager::new(&conn);

    let parent_run = agent_mgr
        .create_run(Some("w1"), "workflow parent", None, None)
        .unwrap();
    let wf_run = wf_mgr
        .create_workflow_run("wf", Some("w1"), &parent_run.id, false, "manual", None)
        .unwrap();

    let returned_parent_id = wf_mgr
        .fail_workflow_run(&wf_run.id, "engine panic")
        .unwrap();
    assert_eq!(returned_parent_id, parent_run.id);

    // Workflow run should be marked as failed
    let updated_wf = wf_mgr.get_workflow_run(&wf_run.id).unwrap().unwrap();
    assert_eq!(updated_wf.status, WorkflowRunStatus::Failed);
    assert_eq!(updated_wf.result_summary.as_deref(), Some("engine panic"));

    // Parent agent run update is handled separately by caller
    let updated_parent = agent_mgr.get_run(&parent_run.id).unwrap().unwrap();
    assert_eq!(
        updated_parent.status,
        crate::agent::status::AgentRunStatus::Running
    );

    // Now caller can update parent separately
    agent_mgr
        .update_run_failed(&returned_parent_id, "engine panic")
        .unwrap();
    let final_parent = agent_mgr.get_run(&parent_run.id).unwrap().unwrap();
    assert_eq!(
        final_parent.status,
        crate::agent::status::AgentRunStatus::Failed
    );
}

// ── list_active_workflow_runs_for_repo ───────────────────────────────────

#[test]
fn test_list_active_workflow_runs_for_repo_includes_worktree_runs() {
    // Runs linked to a worktree whose repo_id = r1 must appear.
    let conn = setup_db();
    let run = create_worktree_run(&conn, "w1"); // w1 belongs to r1
    let runs = WorkflowManager::new(&conn)
        .list_active_workflow_runs_for_repo("r1", &[WorkflowRunStatus::Pending])
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&run.id.as_str()), "worktree run must appear");
}

#[test]
fn test_list_active_workflow_runs_for_repo_includes_repo_targeted_runs() {
    // Runs with repo_id = r1 set directly must appear.
    let conn = setup_db();
    let run = create_repo_run(&conn, "r1");
    let runs = WorkflowManager::new(&conn)
        .list_active_workflow_runs_for_repo("r1", &[WorkflowRunStatus::Pending])
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    assert!(
        ids.contains(&run.id.as_str()),
        "repo-targeted run must appear"
    );
}

#[test]
fn test_list_active_workflow_runs_for_repo_excludes_other_repo() {
    // Runs belonging to r2 must not appear when querying r1.
    let conn = setup_db();
    let _r2_wt_run = create_worktree_run(&conn, "w3"); // w3 belongs to r2
    let _r2_repo_run = create_repo_run(&conn, "r2");
    let runs = WorkflowManager::new(&conn)
        .list_active_workflow_runs_for_repo("r1", &[WorkflowRunStatus::Pending])
        .unwrap();
    assert!(runs.is_empty(), "r1 query must not return r2 runs");
}

#[test]
fn test_list_active_workflow_runs_for_repo_inactive_worktree_excluded() {
    // Runs linked to a merged worktree must not appear.
    let conn = setup_db();
    let run = create_worktree_run(&conn, "w1");
    conn.execute("UPDATE worktrees SET status = 'merged' WHERE id = 'w1'", [])
        .unwrap();
    let runs = WorkflowManager::new(&conn)
        .list_active_workflow_runs_for_repo("r1", &[WorkflowRunStatus::Pending])
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    assert!(
        !ids.contains(&run.id.as_str()),
        "run linked to a merged worktree must not appear"
    );
}

#[test]
fn test_list_active_workflow_runs_for_repo_empty_slice_defaults_to_active() {
    // Empty status slice defaults to [pending, running, waiting]; completed must not appear.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let pending = create_repo_run(&conn, "r1");
    let completed = create_repo_run(&conn, "r1");
    mgr.update_workflow_status(&completed.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    let runs = mgr.list_active_workflow_runs_for_repo("r1", &[]).unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&pending.id.as_str()),
        "pending run must appear"
    );
    assert!(
        !ids.contains(&completed.id.as_str()),
        "completed run must not appear"
    );
}

#[test]
fn test_list_active_workflow_runs_for_repo_explicit_status_filter() {
    // Only runs with the requested status should appear.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let pending = create_repo_run(&conn, "r1");
    let running = create_repo_run(&conn, "r1");
    mgr.update_workflow_status(&running.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let runs = mgr
        .list_active_workflow_runs_for_repo("r1", &[WorkflowRunStatus::Running])
        .unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

    assert!(
        ids.contains(&running.id.as_str()),
        "running run must appear"
    );
    assert!(
        !ids.contains(&pending.id.as_str()),
        "pending run must not appear"
    );
}

#[test]
fn test_list_active_workflow_runs_for_repo_distinct_no_duplicates() {
    // A run matching both join paths (repo_id = r1 AND worktree belongs to r1) must appear once.
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
        .list_active_workflow_runs_for_repo("r1", &[WorkflowRunStatus::Pending])
        .unwrap();
    assert_eq!(
        runs.len(),
        1,
        "run matching both join paths must appear exactly once"
    );
}

// ── helpers shared by analytics query tests ──────────────────────────────────

fn create_named_worktree_run(
    conn: &rusqlite::Connection,
    wt_id: &str,
    wf_name: &str,
) -> WorkflowRun {
    let parent_id = make_parent_id(conn, wt_id);
    WorkflowManager::new(conn)
        .create_workflow_run(wf_name, Some(wt_id), &parent_id, false, "manual", None)
        .unwrap()
}

fn create_named_repo_run(conn: &rusqlite::Connection, repo_id: &str, wf_name: &str) -> WorkflowRun {
    let parent_id = make_parent_id(conn, "w1");
    WorkflowManager::new(conn)
        .create_workflow_run_with_targets(
            wf_name,
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

fn complete_with_metrics(conn: &rusqlite::Connection, run_id: &str, input: i64, output: i64) {
    let mgr = WorkflowManager::new(conn);
    mgr.update_workflow_status(run_id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.persist_workflow_metrics(run_id, input, output, 0, 0, 1, 0.0, 1000, None)
        .unwrap();
}

// ── get_workflow_token_aggregates ─────────────────────────────────────────────

#[test]
fn test_token_aggregates_empty_when_no_completed_runs() {
    let conn = setup_db();
    let result = WorkflowManager::new(&conn)
        .get_workflow_token_aggregates(None)
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_token_aggregates_excludes_non_terminal_runs() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // pending run — should never appear
    create_named_worktree_run(&conn, "w1", "wf-a");

    // running run — should never appear
    let running = create_named_worktree_run(&conn, "w1", "wf-a");
    mgr.update_workflow_status(&running.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let result = mgr.get_workflow_token_aggregates(None).unwrap();
    // pending and running are not terminal — they are excluded
    assert!(result.is_empty());
}

#[test]
fn test_token_aggregates_includes_completed_without_tokens() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // completed run with no token data — terminal, so it appears with 0 token averages
    let completed_no_tokens = create_named_worktree_run(&conn, "w1", "wf-a");
    mgr.update_workflow_status(
        &completed_no_tokens.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();

    let result = mgr.get_workflow_token_aggregates(None).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].workflow_name, "wf-a");
    assert_eq!(result[0].run_count, 1);
    assert!((result[0].avg_input - 0.0).abs() < 0.01);
    assert!((result[0].success_rate - 100.0).abs() < 0.01);
}

#[test]
fn test_token_aggregates_includes_failed_runs() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // one completed + one failed — both are terminal
    complete_with_metrics(
        &conn,
        &create_named_worktree_run(&conn, "w1", "wf-a").id,
        100,
        200,
    );
    let failed = create_named_worktree_run(&conn, "w1", "wf-a");
    mgr.update_workflow_status(&failed.id, WorkflowRunStatus::Failed, None, None)
        .unwrap();

    let result = mgr.get_workflow_token_aggregates(None).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].run_count, 2);
    // token averages come from completed runs only
    assert!((result[0].avg_input - 100.0).abs() < 0.01);
    // 1 of 2 completed → 50% success rate
    assert!((result[0].success_rate - 50.0).abs() < 0.01);
}

#[test]
fn test_token_aggregates_groups_and_averages() {
    let conn = setup_db();

    complete_with_metrics(
        &conn,
        &create_named_worktree_run(&conn, "w1", "wf-a").id,
        100,
        200,
    );
    complete_with_metrics(
        &conn,
        &create_named_worktree_run(&conn, "w1", "wf-a").id,
        300,
        400,
    );
    complete_with_metrics(
        &conn,
        &create_named_worktree_run(&conn, "w1", "wf-b").id,
        50,
        50,
    );

    let result = WorkflowManager::new(&conn)
        .get_workflow_token_aggregates(None)
        .unwrap();

    assert_eq!(result.len(), 2);
    // wf-a has higher avg total (200+300=500 avg), wf-b has 100 avg — ordered desc
    assert_eq!(result[0].workflow_name, "wf-a");
    assert_eq!(result[0].run_count, 2);
    assert!((result[0].avg_input - 200.0).abs() < 0.01);
    assert!((result[0].avg_output - 300.0).abs() < 0.01);

    assert_eq!(result[1].workflow_name, "wf-b");
    assert_eq!(result[1].run_count, 1);
    assert!((result[1].avg_input - 50.0).abs() < 0.01);
}

#[test]
fn test_token_aggregates_repo_filter_some() {
    let conn = setup_db();

    // r1 run — use create_named_repo_run so repo_id is set directly (worktree runs have repo_id NULL)
    complete_with_metrics(
        &conn,
        &create_named_repo_run(&conn, "r1", "wf-a").id,
        100,
        200,
    );
    // r2 run
    complete_with_metrics(
        &conn,
        &create_named_repo_run(&conn, "r2", "wf-a").id,
        999,
        999,
    );

    let result = WorkflowManager::new(&conn)
        .get_workflow_token_aggregates(Some("r1"))
        .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].workflow_name, "wf-a");
    assert!((result[0].avg_input - 100.0).abs() < 0.01);
}

#[test]
fn test_token_aggregates_repo_filter_none_includes_all() {
    let conn = setup_db();

    complete_with_metrics(
        &conn,
        &create_named_worktree_run(&conn, "w1", "wf-a").id,
        100,
        200,
    );
    complete_with_metrics(
        &conn,
        &create_named_repo_run(&conn, "r2", "wf-b").id,
        50,
        50,
    );

    let result = WorkflowManager::new(&conn)
        .get_workflow_token_aggregates(None)
        .unwrap();

    assert_eq!(result.len(), 2);
}

#[test]
fn test_token_aggregates_ordered_by_total_desc() {
    let conn = setup_db();

    complete_with_metrics(
        &conn,
        &create_named_worktree_run(&conn, "w1", "low-wf").id,
        10,
        10,
    );
    complete_with_metrics(
        &conn,
        &create_named_worktree_run(&conn, "w1", "high-wf").id,
        500,
        500,
    );
    complete_with_metrics(
        &conn,
        &create_named_worktree_run(&conn, "w1", "mid-wf").id,
        100,
        100,
    );

    let result = WorkflowManager::new(&conn)
        .get_workflow_token_aggregates(None)
        .unwrap();

    assert_eq!(result[0].workflow_name, "high-wf");
    assert_eq!(result[1].workflow_name, "mid-wf");
    assert_eq!(result[2].workflow_name, "low-wf");
}

// ── get_workflow_token_trend ──────────────────────────────────────────────────

#[test]
fn test_token_trend_empty_for_unknown_workflow() {
    let conn = setup_db();
    let result = WorkflowManager::new(&conn)
        .get_workflow_token_trend("no-such-wf", TimeGranularity::Daily)
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_token_trend_excludes_non_completed_and_null_token_runs() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // pending — excluded
    create_named_worktree_run(&conn, "w1", "trend-wf");

    // completed but no tokens — excluded
    let no_tokens = create_named_worktree_run(&conn, "w1", "trend-wf");
    mgr.update_workflow_status(&no_tokens.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    let result = mgr
        .get_workflow_token_trend("trend-wf", TimeGranularity::Daily)
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_token_trend_daily_granularity() {
    let conn = setup_db();

    let run1 = create_named_worktree_run(&conn, "w1", "trend-wf");
    let run2 = create_named_worktree_run(&conn, "w1", "trend-wf");
    complete_with_metrics(&conn, &run1.id, 100, 200);
    complete_with_metrics(&conn, &run2.id, 50, 80);

    // Force both runs onto the same specific day
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-03-15T10:00:00Z' WHERE id = ?1",
        rusqlite::params![run1.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-03-15T18:00:00Z' WHERE id = ?1",
        rusqlite::params![run2.id],
    )
    .unwrap();

    let result = WorkflowManager::new(&conn)
        .get_workflow_token_trend("trend-wf", TimeGranularity::Daily)
        .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].period, "2024-03-15");
    assert_eq!(result[0].total_input, 150);
    assert_eq!(result[0].total_output, 280);
}

#[test]
fn test_token_trend_daily_multiple_periods_ordered_desc() {
    let conn = setup_db();

    let run1 = create_named_worktree_run(&conn, "w1", "trend-wf");
    let run2 = create_named_worktree_run(&conn, "w1", "trend-wf");
    complete_with_metrics(&conn, &run1.id, 100, 200);
    complete_with_metrics(&conn, &run2.id, 50, 80);

    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-03-14T10:00:00Z' WHERE id = ?1",
        rusqlite::params![run1.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-03-15T10:00:00Z' WHERE id = ?1",
        rusqlite::params![run2.id],
    )
    .unwrap();

    let result = WorkflowManager::new(&conn)
        .get_workflow_token_trend("trend-wf", TimeGranularity::Daily)
        .unwrap();

    assert_eq!(result.len(), 2);
    // ordered DESC — newer period first
    assert_eq!(result[0].period, "2024-03-15");
    assert_eq!(result[1].period, "2024-03-14");
}

#[test]
fn test_token_trend_weekly_granularity() {
    let conn = setup_db();

    let run1 = create_named_worktree_run(&conn, "w1", "trend-wf");
    let run2 = create_named_worktree_run(&conn, "w1", "trend-wf");
    complete_with_metrics(&conn, &run1.id, 100, 200);
    complete_with_metrics(&conn, &run2.id, 50, 80);

    // Both in same ISO week (2024-W11)
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-03-11T10:00:00Z' WHERE id = ?1",
        rusqlite::params![run1.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-03-13T10:00:00Z' WHERE id = ?1",
        rusqlite::params![run2.id],
    )
    .unwrap();

    let result = WorkflowManager::new(&conn)
        .get_workflow_token_trend("trend-wf", TimeGranularity::Weekly)
        .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].period, "2024-11");
    assert_eq!(result[0].total_input, 150);
    assert_eq!(result[0].total_output, 280);
}

// ── get_step_token_heatmap ────────────────────────────────────────────────────

#[test]
fn test_step_heatmap_empty_for_unknown_workflow() {
    let conn = setup_db();
    let result = WorkflowManager::new(&conn)
        .get_step_token_heatmap("no-such-wf", 10)
        .unwrap();
    assert!(result.is_empty());
}

// Insert a stub agent_run with token data and return its id.
fn insert_agent_run_with_tokens(
    conn: &rusqlite::Connection,
    id: &str,
    input: i64,
    output: i64,
    cache_read: i64,
) {
    conn.execute(
        "INSERT INTO agent_runs (id, worktree_id, prompt, status, started_at, \
         input_tokens, output_tokens, cache_read_input_tokens) \
         VALUES (?1, 'w1', 'test', 'completed', '2024-01-01T00:00:00Z', ?2, ?3, ?4)",
        rusqlite::params![id, input, output, cache_read],
    )
    .unwrap();
}

// Insert a workflow_run_step linking a workflow run to an agent run.
fn insert_workflow_step(
    conn: &rusqlite::Connection,
    step_id: &str,
    run_id: &str,
    step_name: &str,
    position: i64,
    child_run_id: &str,
) {
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, child_run_id) \
         VALUES (?1, ?2, ?3, 'actor', ?4, ?5)",
        rusqlite::params![step_id, run_id, step_name, position, child_run_id],
    )
    .unwrap();
}

#[test]
fn test_step_heatmap_basic_per_step_averages() {
    let conn = setup_db();

    // Two completed runs of "heat-wf", each with a step "step-a"
    let run1 = create_named_worktree_run(&conn, "w1", "heat-wf");
    let run2 = create_named_worktree_run(&conn, "w1", "heat-wf");
    WorkflowManager::new(&conn)
        .update_workflow_status(&run1.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    WorkflowManager::new(&conn)
        .update_workflow_status(&run2.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    insert_agent_run_with_tokens(&conn, "ar-r1-a", 100, 200, 10);
    insert_agent_run_with_tokens(&conn, "ar-r2-a", 200, 400, 20);

    insert_workflow_step(&conn, "s1", &run1.id, "step-a", 0, "ar-r1-a");
    insert_workflow_step(&conn, "s2", &run2.id, "step-a", 0, "ar-r2-a");

    let result = WorkflowManager::new(&conn)
        .get_step_token_heatmap("heat-wf", 10)
        .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].step_name, "step-a");
    assert_eq!(result[0].run_count, 2);
    assert!((result[0].avg_input - 150.0).abs() < 0.01);
    assert!((result[0].avg_output - 300.0).abs() < 0.01);
    assert!((result[0].avg_cache_read - 15.0).abs() < 0.01);
}

#[test]
fn test_step_heatmap_limit_runs_respected() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Create 3 completed runs; limit_runs=2 should only count the 2 most recent.
    let run1 = create_named_worktree_run(&conn, "w1", "heat-wf");
    let run2 = create_named_worktree_run(&conn, "w1", "heat-wf");
    let run3 = create_named_worktree_run(&conn, "w1", "heat-wf");

    for r in [&run1, &run2, &run3] {
        mgr.update_workflow_status(&r.id, WorkflowRunStatus::Completed, None, None)
            .unwrap();
    }

    // Force started_at so ordering is deterministic: run1 oldest, run3 newest
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-01-01T00:00:00Z' WHERE id = ?1",
        rusqlite::params![run1.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-01-02T00:00:00Z' WHERE id = ?1",
        rusqlite::params![run2.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-01-03T00:00:00Z' WHERE id = ?1",
        rusqlite::params![run3.id],
    )
    .unwrap();

    // run1 (oldest, excluded by limit): 999 tokens — should NOT affect avg
    insert_agent_run_with_tokens(&conn, "ar-old", 999, 999, 0);
    insert_workflow_step(&conn, "s-old", &run1.id, "step-a", 0, "ar-old");

    // run2 and run3 (the 2 most recent): 100 and 200 tokens
    insert_agent_run_with_tokens(&conn, "ar-r2", 100, 100, 0);
    insert_agent_run_with_tokens(&conn, "ar-r3", 200, 200, 0);
    insert_workflow_step(&conn, "s-r2", &run2.id, "step-a", 0, "ar-r2");
    insert_workflow_step(&conn, "s-r3", &run3.id, "step-a", 0, "ar-r3");

    let result = WorkflowManager::new(&conn)
        .get_step_token_heatmap("heat-wf", 2)
        .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].step_name, "step-a");
    assert_eq!(result[0].run_count, 2);
    // avg of 100 and 200 = 150, NOT 433 (which would include run1's 999)
    assert!((result[0].avg_input - 150.0).abs() < 0.01);
}

#[test]
fn test_step_heatmap_ordered_by_avg_total_tokens_desc() {
    let conn = setup_db();

    let run = create_named_worktree_run(&conn, "w1", "heat-wf");
    WorkflowManager::new(&conn)
        .update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    // step-high: 500+500 = 1000 total; step-low: 10+10 = 20 total
    insert_agent_run_with_tokens(&conn, "ar-high", 500, 500, 0);
    insert_agent_run_with_tokens(&conn, "ar-low", 10, 10, 0);
    insert_workflow_step(&conn, "s-high", &run.id, "step-high", 0, "ar-high");
    insert_workflow_step(&conn, "s-low", &run.id, "step-low", 1, "ar-low");

    let result = WorkflowManager::new(&conn)
        .get_step_token_heatmap("heat-wf", 10)
        .unwrap();

    assert_eq!(result.len(), 2);
    assert_eq!(result[0].step_name, "step-high");
    assert_eq!(result[1].step_name, "step-low");
}

// ── get_run_metrics ────────────────────────────────────────────────────────────

#[test]
fn test_run_metrics_empty_when_no_runs() {
    let conn = setup_db();
    let result = WorkflowManager::new(&conn)
        .get_run_metrics("no-such-wf", 30)
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_run_metrics_excludes_non_completed_runs() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // pending — should not appear
    create_named_worktree_run(&conn, "w1", "metrics-wf");

    let result = mgr.get_run_metrics("metrics-wf", 30).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_run_metrics_returns_completed_run_data() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_named_worktree_run(&conn, "w1", "metrics-wf");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.persist_workflow_metrics(&run.id, 100, 200, 0, 0, 1, 0.0, 5000, None)
        .unwrap();

    let result = mgr.get_run_metrics("metrics-wf", 30).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].input_tokens, Some(100));
    assert_eq!(result[0].output_tokens, Some(200));
    assert_eq!(result[0].duration_ms, Some(5000));
    assert!(!result[0].run_id.is_empty());
    assert!(!result[0].started_at.is_empty());
}

#[test]
fn test_run_metrics_filters_by_workflow_name() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run_a = create_named_worktree_run(&conn, "w1", "wf-a");
    mgr.update_workflow_status(&run_a.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.persist_workflow_metrics(&run_a.id, 100, 200, 0, 0, 1, 0.0, 1000, None)
        .unwrap();

    let run_b = create_named_worktree_run(&conn, "w1", "wf-b");
    mgr.update_workflow_status(&run_b.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.persist_workflow_metrics(&run_b.id, 999, 999, 0, 0, 1, 0.0, 9999, None)
        .unwrap();

    let result = mgr.get_run_metrics("wf-a", 30).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].input_tokens, Some(100));
    assert!(!result[0].run_id.is_empty());
    assert!(!result[0].started_at.is_empty());
}

#[test]
fn test_run_metrics_excludes_null_metric_runs() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // All-null run: completed but no metrics persisted — should be excluded
    let null_run = create_named_worktree_run(&conn, "w1", "metrics-wf");
    mgr.update_workflow_status(&null_run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    // Duration-only run: set total_duration_ms but leave tokens null — should be included
    let dur_run = create_named_worktree_run(&conn, "w1", "metrics-wf");
    mgr.update_workflow_status(&dur_run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET total_duration_ms = 3000 WHERE id = ?1",
        rusqlite::params![dur_run.id],
    )
    .unwrap();

    let result = mgr.get_run_metrics("metrics-wf", 30).unwrap();
    assert_eq!(result.len(), 1, "all-null run should be excluded");
    assert_eq!(result[0].run_id, dur_run.id);
    assert_eq!(result[0].duration_ms, Some(3000));
    assert_eq!(result[0].input_tokens, None);
    assert_eq!(result[0].output_tokens, None);
}

#[test]
fn test_run_metrics_excludes_zero_metric_runs() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // All-zero run: completed with metrics all set to 0 — should be excluded
    let zero_run = create_named_worktree_run(&conn, "w1", "metrics-wf");
    mgr.update_workflow_status(&zero_run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.persist_workflow_metrics(&zero_run.id, 0, 0, 0, 0, 0, 0.0, 0, None)
        .unwrap();

    let result = mgr.get_run_metrics("metrics-wf", 30).unwrap();
    assert!(result.is_empty(), "all-zero metric run should be excluded");
}

#[test]
fn test_run_metrics_includes_partial_nonzero_run() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Zero tokens but nonzero duration — should be included
    let run = create_named_worktree_run(&conn, "w1", "metrics-wf");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.persist_workflow_metrics(&run.id, 0, 0, 0, 0, 1, 0.0, 2500, None)
        .unwrap();

    let result = mgr.get_run_metrics("metrics-wf", 30).unwrap();
    assert_eq!(
        result.len(),
        1,
        "run with nonzero duration should be included"
    );
    assert_eq!(result[0].duration_ms, Some(2500));
    assert_eq!(result[0].input_tokens, Some(0));
    assert_eq!(result[0].output_tokens, Some(0));
}

#[test]
fn test_run_metrics_includes_worktree_and_repo_id() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_named_worktree_run(&conn, "w1", "metrics-wf");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.persist_workflow_metrics(&run.id, 100, 200, 0, 0, 1, 0.0, 5000, None)
        .unwrap();

    let result = mgr.get_run_metrics("metrics-wf", 30).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].worktree_id.as_deref(), Some("w1"));
    // repo_id is null when created via create_named_worktree_run (worktree context)
    assert_eq!(result[0].repo_id, None);
}

// ── get_workflow_failure_rate_trend ────────────────────────────────────────────

#[test]
fn test_failure_rate_trend_empty_when_no_terminal_runs() {
    let conn = setup_db();
    let result = WorkflowManager::new(&conn)
        .get_workflow_failure_rate_trend("no-such-wf", TimeGranularity::Daily)
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_failure_rate_trend_excludes_non_terminal_runs() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // pending run — should not appear
    create_named_worktree_run(&conn, "w1", "trend-wf");

    // running run — should not appear
    let running = create_named_worktree_run(&conn, "w1", "trend-wf");
    mgr.update_workflow_status(&running.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let result = mgr
        .get_workflow_failure_rate_trend("trend-wf", TimeGranularity::Daily)
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_failure_rate_trend_completed_only_period() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_named_worktree_run(&conn, "w1", "trend-wf");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    // Pin started_at to a known date so period is deterministic
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-03-15T10:00:00Z' WHERE id = ?1",
        rusqlite::params![run.id],
    )
    .unwrap();

    let result = mgr
        .get_workflow_failure_rate_trend("trend-wf", TimeGranularity::Daily)
        .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].period, "2024-03-15");
    assert_eq!(result[0].total_runs, 1);
    assert_eq!(result[0].failed_runs, 0);
    assert!((result[0].success_rate - 100.0).abs() < 0.01);
}

#[test]
fn test_failure_rate_trend_mixed_completed_and_failed() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // 2 completed + 1 failed all on the same day → success_rate = 66.67%
    for _ in 0..2 {
        let run = create_named_worktree_run(&conn, "w1", "trend-wf");
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
            .unwrap();
        conn.execute(
            "UPDATE workflow_runs SET started_at = '2024-03-15T10:00:00Z' WHERE id = ?1",
            rusqlite::params![run.id],
        )
        .unwrap();
    }
    let failed = create_named_worktree_run(&conn, "w1", "trend-wf");
    mgr.update_workflow_status(&failed.id, WorkflowRunStatus::Failed, None, None)
        .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-03-15T12:00:00Z' WHERE id = ?1",
        rusqlite::params![failed.id],
    )
    .unwrap();

    let result = mgr
        .get_workflow_failure_rate_trend("trend-wf", TimeGranularity::Daily)
        .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].total_runs, 3);
    assert_eq!(result[0].failed_runs, 1);
    // 2 completed / 3 total × 100 = 66.67
    assert!((result[0].success_rate - 66.666_666).abs() < 0.01);
}

#[test]
fn test_failure_rate_trend_filters_by_workflow_name() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run_a = create_named_worktree_run(&conn, "w1", "wf-a");
    mgr.update_workflow_status(&run_a.id, WorkflowRunStatus::Failed, None, None)
        .unwrap();

    let run_b = create_named_worktree_run(&conn, "w1", "wf-b");
    mgr.update_workflow_status(&run_b.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    let result = mgr
        .get_workflow_failure_rate_trend("wf-a", TimeGranularity::Daily)
        .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].failed_runs, 1);
    assert!((result[0].success_rate - 0.0).abs() < 0.01);
}

#[test]
fn test_failure_rate_trend_weekly_granularity() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Two runs on the same ISO week but different days → single period row
    for ts in ["2024-03-11T00:00:00Z", "2024-03-13T00:00:00Z"] {
        let run = create_named_worktree_run(&conn, "w1", "trend-wf");
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
            .unwrap();
        conn.execute(
            "UPDATE workflow_runs SET started_at = ?1 WHERE id = ?2",
            rusqlite::params![ts, run.id],
        )
        .unwrap();
    }

    let result = mgr
        .get_workflow_failure_rate_trend("trend-wf", TimeGranularity::Weekly)
        .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].total_runs, 2);
}

// ── get_step_failure_heatmap ───────────────────────────────────────────────────

// Helper: insert a workflow_run_step with an explicit status (no child_run_id required).
fn insert_step_with_status(
    conn: &rusqlite::Connection,
    step_id: &str,
    run_id: &str,
    step_name: &str,
    position: i64,
    status: &str,
) {
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status) \
         VALUES (?1, ?2, ?3, 'actor', ?4, ?5)",
        rusqlite::params![step_id, run_id, step_name, position, status],
    )
    .unwrap();
}

#[test]
fn test_step_failure_heatmap_empty_when_no_runs() {
    let conn = setup_db();
    let result = WorkflowManager::new(&conn)
        .get_step_failure_heatmap("no-such-wf", 10)
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_step_failure_heatmap_basic_failure_rate() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Two terminal runs of "heat-wf"
    let run1 = create_named_worktree_run(&conn, "w1", "heat-wf");
    let run2 = create_named_worktree_run(&conn, "w1", "heat-wf");
    mgr.update_workflow_status(&run1.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.update_workflow_status(&run2.id, WorkflowRunStatus::Failed, None, None)
        .unwrap();

    // step-a completed in run1, failed in run2 → failure_rate = 50%
    insert_step_with_status(&conn, "s1", &run1.id, "step-a", 0, "completed");
    insert_step_with_status(&conn, "s2", &run2.id, "step-a", 0, "failed");

    let result = mgr.get_step_failure_heatmap("heat-wf", 10).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].step_name, "step-a");
    assert_eq!(result[0].total_executions, 2);
    assert_eq!(result[0].failed_executions, 1);
    assert!((result[0].failure_rate - 50.0).abs() < 0.01);
}

#[test]
fn test_step_failure_heatmap_excludes_pending_and_skipped_steps() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_named_worktree_run(&conn, "w1", "heat-wf");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    // pending and skipped steps must not appear in results
    insert_step_with_status(&conn, "s-pending", &run.id, "step-a", 0, "pending");
    insert_step_with_status(&conn, "s-skipped", &run.id, "step-b", 1, "skipped");
    // only this completed step should show up
    insert_step_with_status(&conn, "s-done", &run.id, "step-c", 2, "completed");

    let result = mgr.get_step_failure_heatmap("heat-wf", 10).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].step_name, "step-c");
}

#[test]
fn test_step_failure_heatmap_limit_runs_respected() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // 3 completed runs; oldest should be excluded by limit_runs=2
    let run1 = create_named_worktree_run(&conn, "w1", "heat-wf");
    let run2 = create_named_worktree_run(&conn, "w1", "heat-wf");
    let run3 = create_named_worktree_run(&conn, "w1", "heat-wf");
    for r in [&run1, &run2, &run3] {
        mgr.update_workflow_status(&r.id, WorkflowRunStatus::Completed, None, None)
            .unwrap();
    }

    // Force deterministic ordering: run1 oldest, run3 newest
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-01-01T00:00:00Z' WHERE id = ?1",
        rusqlite::params![run1.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-01-02T00:00:00Z' WHERE id = ?1",
        rusqlite::params![run2.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-01-03T00:00:00Z' WHERE id = ?1",
        rusqlite::params![run3.id],
    )
    .unwrap();

    // run1 (oldest, excluded): step-a failed — should NOT inflate failure rate
    insert_step_with_status(&conn, "s-old", &run1.id, "step-a", 0, "failed");
    // run2 and run3 (the 2 most recent): step-a completed
    insert_step_with_status(&conn, "s-r2", &run2.id, "step-a", 0, "completed");
    insert_step_with_status(&conn, "s-r3", &run3.id, "step-a", 0, "completed");

    let result = mgr.get_step_failure_heatmap("heat-wf", 2).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].total_executions, 2);
    assert_eq!(result[0].failed_executions, 0);
    assert!((result[0].failure_rate - 0.0).abs() < 0.01);
}

#[test]
fn test_step_failure_heatmap_ordered_by_failure_rate_desc() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_named_worktree_run(&conn, "w1", "heat-wf");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    // step-never-fails: completed; step-always-fails: failed → second row first
    insert_step_with_status(&conn, "s-ok", &run.id, "step-never-fails", 0, "completed");
    insert_step_with_status(&conn, "s-bad", &run.id, "step-always-fails", 1, "failed");

    let result = mgr.get_step_failure_heatmap("heat-wf", 10).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].step_name, "step-always-fails");
    assert_eq!(result[1].step_name, "step-never-fails");
}

// ── get_step_retry_analytics ──────────────────────────────────────────────────

// Helper: insert a step with an explicit retry_count in addition to status.
fn insert_step_with_retries(
    conn: &rusqlite::Connection,
    step_id: &str,
    run_id: &str,
    step_name: &str,
    position: i64,
    status: &str,
    retry_count: i64,
) {
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, retry_count) \
         VALUES (?1, ?2, ?3, 'actor', ?4, ?5, ?6)",
        rusqlite::params![step_id, run_id, step_name, position, status, retry_count],
    )
    .unwrap();
}

#[test]
fn test_step_retry_analytics_empty_when_no_runs() {
    let conn = setup_db();
    let result = WorkflowManager::new(&conn)
        .get_step_retry_analytics("no-such-wf", 10)
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_step_retry_analytics_no_retries() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_named_worktree_run(&conn, "w1", "retry-wf");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    insert_step_with_retries(&conn, "s1", &run.id, "step-a", 0, "completed", 0);

    let result = mgr.get_step_retry_analytics("retry-wf", 10).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].step_name, "step-a");
    assert_eq!(result[0].total_executions, 1);
    assert_eq!(result[0].executions_with_retries, 0);
    assert!((result[0].retry_rate - 0.0).abs() < 0.01);
    assert!((result[0].avg_retry_count - 0.0).abs() < 0.01);
    assert!((result[0].retry_success_rate - 0.0).abs() < 0.01);
}

#[test]
fn test_step_retry_analytics_basic() {
    // One step that was retried (retry_count=2) and completed → 100% retry rate, 100% success
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_named_worktree_run(&conn, "w1", "retry-wf");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    insert_step_with_retries(&conn, "s1", &run.id, "step-a", 0, "completed", 2);

    let result = mgr.get_step_retry_analytics("retry-wf", 10).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].total_executions, 1);
    assert_eq!(result[0].executions_with_retries, 1);
    assert!((result[0].retry_rate - 100.0).abs() < 0.01);
    assert!((result[0].avg_retry_count - 2.0).abs() < 0.01);
    assert!((result[0].retry_success_rate - 100.0).abs() < 0.01);
}

#[test]
fn test_step_retry_analytics_mixed() {
    // 4 executions: 1 no-retry-completed, 1 retry-completed, 1 retry-failed, 1 no-retry-failed
    // retry_rate = 2/4 = 50%; retry_success_rate = 1/2 = 50%; avg_retry_count = avg(1,3)/2 = 2.0
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    for (idx, (wt, rc, status)) in [
        ("w1", 0_i64, "completed"),
        ("w2", 1_i64, "completed"),
        ("w3", 3_i64, "failed"),
        ("w1", 0_i64, "failed"),
    ]
    .iter()
    .enumerate()
    {
        let run = create_named_worktree_run(&conn, wt, "retry-wf");
        let wf_status = if *status == "completed" {
            WorkflowRunStatus::Completed
        } else {
            WorkflowRunStatus::Failed
        };
        mgr.update_workflow_status(&run.id, wf_status, None, None)
            .unwrap();
        insert_step_with_retries(&conn, &format!("s{idx}"), &run.id, "step-a", 0, status, *rc);
    }

    let result = mgr.get_step_retry_analytics("retry-wf", 10).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].total_executions, 4);
    assert_eq!(result[0].executions_with_retries, 2);
    assert!((result[0].retry_rate - 50.0).abs() < 0.01);
    assert!((result[0].avg_retry_count - 2.0).abs() < 0.01); // (1+3)/2
    assert!((result[0].retry_success_rate - 50.0).abs() < 0.01); // 1 of 2 retried completed
}

#[test]
fn test_step_retry_analytics_limit_runs() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // 3 runs; oldest (run1) has a retried step — should be excluded by limit_runs=2
    let run1 = create_named_worktree_run(&conn, "w1", "retry-wf");
    let run2 = create_named_worktree_run(&conn, "w1", "retry-wf");
    let run3 = create_named_worktree_run(&conn, "w1", "retry-wf");
    for r in [&run1, &run2, &run3] {
        mgr.update_workflow_status(&r.id, WorkflowRunStatus::Completed, None, None)
            .unwrap();
    }

    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-01-01T00:00:00Z' WHERE id = ?1",
        rusqlite::params![run1.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-01-02T00:00:00Z' WHERE id = ?1",
        rusqlite::params![run2.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET started_at = '2024-01-03T00:00:00Z' WHERE id = ?1",
        rusqlite::params![run3.id],
    )
    .unwrap();

    // oldest run: retried — must be excluded
    insert_step_with_retries(&conn, "s-old", &run1.id, "step-a", 0, "completed", 3);
    // two recent runs: no retries
    insert_step_with_retries(&conn, "s-r2", &run2.id, "step-a", 0, "completed", 0);
    insert_step_with_retries(&conn, "s-r3", &run3.id, "step-a", 0, "completed", 0);

    let result = mgr.get_step_retry_analytics("retry-wf", 2).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].total_executions, 2);
    assert_eq!(result[0].executions_with_retries, 0);
    assert!((result[0].retry_rate - 0.0).abs() < 0.01);
}

#[test]
fn test_get_workflow_percentiles_empty() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let result = mgr.get_workflow_percentiles("nonexistent-wf", 30).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_get_workflow_percentiles_no_duration() {
    // A completed run with no duration_ms should be excluded.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_named_worktree_run(&conn, "w1", "pct-wf");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    // persist_workflow_metrics with duration 0 — but do NOT set total_duration_ms directly
    // because persist_workflow_metrics sets it. Instead insert a run with NULL duration_ms
    // by completing without calling persist_workflow_metrics.
    let result = mgr.get_workflow_percentiles("pct-wf", 30).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_get_workflow_percentiles_single_run() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_named_worktree_run(&conn, "w1", "pct-wf");
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.persist_workflow_metrics(&run.id, 100, 200, 0, 0, 1, 0.05, 5000, None)
        .unwrap();

    let result = mgr.get_workflow_percentiles("pct-wf", 30).unwrap();
    assert!(result.is_some());
    let p = result.unwrap();
    assert_eq!(p.run_count, 1);
    // With a single run, all percentiles collapse to that run's value.
    assert!(p.p50_duration_ms.is_some());
    assert!((p.p50_duration_ms.unwrap() - 5000.0).abs() < 1.0);
    assert!(p.p95_duration_ms.is_some());
}

#[test]
fn test_get_workflow_percentiles_multi_run() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Insert 10 runs with durations 1000, 2000, …, 10000 ms
    for i in 1..=10u64 {
        let run = create_named_worktree_run(&conn, "w1", "pct-wf");
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
            .unwrap();
        mgr.persist_workflow_metrics(
            &run.id,
            (i * 100) as i64,
            (i * 50) as i64,
            0,
            0,
            1,
            (i as f64) * 0.01,
            (i * 1000) as i64,
            None,
        )
        .unwrap();
    }

    let result = mgr.get_workflow_percentiles("pct-wf", 30).unwrap();
    assert!(result.is_some());
    let p = result.unwrap();
    assert_eq!(p.run_count, 10);
    // P50 should be around the median (5000–6000 ms range)
    assert!(p.p50_duration_ms.is_some());
    let p50 = p.p50_duration_ms.unwrap();
    assert!((4000.0..=7000.0).contains(&p50), "p50={p50}");
    // P99 should be near the top
    assert!(p.p99_duration_ms.is_some());
    let p99 = p.p99_duration_ms.unwrap();
    assert!(p99 >= 8000.0, "p99={p99}");
    // Cost and token percentiles should also be present
    assert!(p.p50_cost_usd.is_some());
    assert!(p.p50_total_tokens.is_some());
}

#[test]
fn test_get_workflow_percentiles_excludes_other_workflows() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run_a = create_named_worktree_run(&conn, "w1", "wf-a");
    mgr.update_workflow_status(&run_a.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.persist_workflow_metrics(&run_a.id, 100, 200, 0, 0, 1, 0.10, 5000, None)
        .unwrap();

    let run_b = create_named_worktree_run(&conn, "w1", "wf-b");
    mgr.update_workflow_status(&run_b.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.persist_workflow_metrics(&run_b.id, 999, 999, 0, 0, 1, 9.99, 99000, None)
        .unwrap();

    let result = mgr.get_workflow_percentiles("wf-a", 30).unwrap();
    assert!(result.is_some());
    let p = result.unwrap();
    assert_eq!(p.run_count, 1);
    assert!((p.p50_duration_ms.unwrap() - 5000.0).abs() < 1.0);
}

// ─── get_gate_analytics ────────────────────────────────────────────────────

/// Helper: insert a terminal gate step with explicit started_at / ended_at so
/// the julianday wait calculation is deterministic.
#[allow(clippy::too_many_arguments)]
fn insert_terminal_gate_step(
    conn: &rusqlite::Connection,
    step_id: &str,
    run_id: &str,
    step_name: &str,
    status: &str, // "completed" or "failed"
    started_at: &str,
    ended_at: &str,
    feedback: Option<&str>,
) {
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, gate_type, status, started_at, ended_at, gate_feedback) \
         VALUES (?1, ?2, ?3, 'gate', 0, 'human_approval', ?4, ?5, ?6, ?7)",
        rusqlite::params![step_id, run_id, step_name, status, started_at, ended_at, feedback],
    )
    .unwrap();
}

#[test]
fn test_get_gate_analytics_empty() {
    let conn = setup_db();
    let result = WorkflowManager::new(&conn)
        .get_gate_analytics("no-such-wf", 30)
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_get_gate_analytics_single_approved() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_named_worktree_run(&conn, "w1", "gate-wf");

    // approved gate: 10 000 ms wait (started 10 s before ended)
    insert_terminal_gate_step(
        &conn,
        "gs1",
        &run.id,
        "approve",
        "completed",
        "2026-04-06T10:00:00Z",
        "2026-04-06T10:00:10Z", // 10 s = 10 000 ms
        Some("lgtm"),
    );

    let rows = mgr.get_gate_analytics("gate-wf", 30).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.step_name, "approve");
    assert_eq!(row.total_gate_hits, 1);
    assert_eq!(row.approved_count, 1);
    assert_eq!(row.rejected_count, 0);
    assert!(
        (row.approval_rate - 100.0).abs() < 0.001,
        "approval_rate={}",
        row.approval_rate
    );
    // avg_wait_ms should be ~10 000
    assert!(row.avg_wait_ms.is_some());
    let avg = row.avg_wait_ms.unwrap();
    assert!((avg - 10_000.0).abs() < 1.0, "avg_wait_ms={avg}");
    // p50 and p95 should also resolve to the single row's wait
    assert!(row.p50_wait_ms.is_some());
    assert!(row.p95_wait_ms.is_some());
    // feedback length should reflect "lgtm" (4 chars)
    assert!(row.avg_feedback_length.is_some());
    assert!((row.avg_feedback_length.unwrap() - 4.0).abs() < 0.001);
}

#[test]
fn test_get_gate_analytics_approved_and_rejected() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_named_worktree_run(&conn, "w1", "mixed-wf");

    // 2 approved + 1 rejected → approval_rate = 2/3 * 100 ≈ 66.67
    insert_terminal_gate_step(
        &conn,
        "g1",
        &run.id,
        "review",
        "completed",
        "2026-04-06T10:00:00Z",
        "2026-04-06T10:00:10Z",
        None,
    );
    insert_terminal_gate_step(
        &conn,
        "g2",
        &run.id,
        "review",
        "completed",
        "2026-04-06T10:00:00Z",
        "2026-04-06T10:00:20Z",
        None,
    );
    insert_terminal_gate_step(
        &conn,
        "g3",
        &run.id,
        "review",
        "failed",
        "2026-04-06T10:00:00Z",
        "2026-04-06T10:00:30Z",
        None,
    );

    let rows = mgr.get_gate_analytics("mixed-wf", 30).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.total_gate_hits, 3);
    assert_eq!(row.approved_count, 2);
    assert_eq!(row.rejected_count, 1);
    let expected_rate = 2.0 / 3.0 * 100.0;
    assert!(
        (row.approval_rate - expected_rate).abs() < 0.01,
        "approval_rate={} expected≈{expected_rate}",
        row.approval_rate
    );
}

#[test]
fn test_get_gate_analytics_multiple_step_names() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_named_worktree_run(&conn, "w1", "multi-step-wf");

    insert_terminal_gate_step(
        &conn,
        "g1",
        &run.id,
        "gate-a",
        "completed",
        "2026-04-06T10:00:00Z",
        "2026-04-06T10:00:05Z",
        None,
    );
    insert_terminal_gate_step(
        &conn,
        "g2",
        &run.id,
        "gate-b",
        "failed",
        "2026-04-06T10:00:00Z",
        "2026-04-06T10:00:15Z",
        None,
    );

    let rows = mgr.get_gate_analytics("multi-step-wf", 30).unwrap();
    assert_eq!(rows.len(), 2);
    let names: Vec<&str> = rows.iter().map(|r| r.step_name.as_str()).collect();
    assert!(names.contains(&"gate-a"), "gate-a missing from {names:?}");
    assert!(names.contains(&"gate-b"), "gate-b missing from {names:?}");
}

#[test]
fn test_get_gate_analytics_excludes_other_workflow() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run_a = create_named_worktree_run(&conn, "w1", "wf-a");
    let run_b = create_named_worktree_run(&conn, "w1", "wf-b");

    insert_terminal_gate_step(
        &conn,
        "g1",
        &run_a.id,
        "gate",
        "completed",
        "2026-04-06T10:00:00Z",
        "2026-04-06T10:00:10Z",
        None,
    );
    insert_terminal_gate_step(
        &conn,
        "g2",
        &run_b.id,
        "gate",
        "completed",
        "2026-04-06T10:00:00Z",
        "2026-04-06T10:00:10Z",
        None,
    );

    let rows = mgr.get_gate_analytics("wf-a", 30).unwrap();
    assert_eq!(rows.len(), 1, "must only return rows for wf-a");
}

#[test]
fn test_get_gate_analytics_excludes_waiting_steps() {
    // Steps still in 'waiting' status must not be counted (only terminal ones count).
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_named_worktree_run(&conn, "w1", "wait-wf");

    let step_id = mgr
        .insert_step(&run.id, "approval", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, None, "1h")
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    let rows = mgr.get_gate_analytics("wait-wf", 30).unwrap();
    assert!(
        rows.is_empty(),
        "waiting steps must not appear in gate analytics"
    );
}

// ─── get_all_pending_gates ─────────────────────────────────────────────────

#[test]
fn test_get_all_pending_gates_empty() {
    let conn = setup_db();
    let result = WorkflowManager::new(&conn).get_all_pending_gates().unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_get_all_pending_gates_returns_waiting_gate() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_named_worktree_run(&conn, "w1", "pending-wf");

    let step_id = mgr
        .insert_step(&run.id, "human-review", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(
        &step_id,
        GateType::HumanApproval,
        Some("Please review"),
        "1h",
    )
    .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    let rows = mgr.get_all_pending_gates().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.step_id, step_id);
    assert_eq!(row.step_name, "human-review");
    assert_eq!(row.workflow_name, "pending-wf");
    assert_eq!(row.workflow_run_id, run.id);
    assert_eq!(row.gate_type, "human_approval");
    assert_eq!(row.gate_prompt.as_deref(), Some("Please review"));
    // wait_ms_so_far is computed live; tolerate small negative values from clock skew
    assert!(
        row.wait_ms_so_far > -1000,
        "wait_ms_so_far={}",
        row.wait_ms_so_far
    );
}

#[test]
fn test_get_all_pending_gates_excludes_completed() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_named_worktree_run(&conn, "w1", "done-wf");

    let step_id = mgr
        .insert_step(&run.id, "gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, None, "1h")
        .unwrap();
    mgr.approve_gate(&step_id, "alice", None, None).unwrap();

    let rows = mgr.get_all_pending_gates().unwrap();
    assert!(rows.is_empty(), "completed gate must not appear");
}

#[test]
fn test_get_all_pending_gates_excludes_non_gate_steps() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_named_worktree_run(&conn, "w1", "actor-wf");

    // Regular actor step put into waiting — must not appear (gate_type IS NULL).
    let step_id = mgr
        .insert_step(&run.id, "build", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    let rows = mgr.get_all_pending_gates().unwrap();
    assert!(rows.is_empty(), "non-gate step must not appear");
}

#[test]
fn test_get_all_pending_gates_cross_workflow() {
    // Pending gates from multiple different workflows should all appear.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run_a = create_named_worktree_run(&conn, "w1", "wf-alpha");
    let step_a = mgr
        .insert_step(&run_a.id, "gate-a", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_a, GateType::HumanApproval, None, "1h")
        .unwrap();
    set_step_status(&mgr, &step_a, WorkflowStepStatus::Waiting);

    let run_b = create_named_worktree_run(&conn, "w1", "wf-beta");
    let step_b = mgr
        .insert_step(&run_b.id, "gate-b", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_b, GateType::HumanApproval, None, "1h")
        .unwrap();
    set_step_status(&mgr, &step_b, WorkflowStepStatus::Waiting);

    let rows = mgr.get_all_pending_gates().unwrap();
    assert_eq!(rows.len(), 2);
    let wf_names: Vec<&str> = rows.iter().map(|r| r.workflow_name.as_str()).collect();
    assert!(wf_names.contains(&"wf-alpha"));
    assert!(wf_names.contains(&"wf-beta"));
}

#[test]
fn test_get_all_pending_gates_null_started_at() {
    // Simulate a legacy row where started_at is NULL (written before the fix).
    // The COALESCE in the query must prevent a rusqlite mapping error.
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let run = create_named_worktree_run(&conn, "w1", "legacy-wf");

    let step_id = mgr
        .insert_step(&run.id, "legacy-gate", "gate", false, 0, 0)
        .unwrap();
    mgr.set_step_gate_info(&step_id, GateType::HumanApproval, None, "1h")
        .unwrap();
    // Force status to 'waiting' without going through update_step_status_full
    // so that started_at stays NULL (simulating pre-fix rows).
    conn.execute(
        "UPDATE workflow_run_steps SET status = 'waiting' WHERE id = ?1",
        rusqlite::params![step_id],
    )
    .unwrap();

    let rows = mgr.get_all_pending_gates().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.step_id, step_id);
    // With COALESCE(started_at, now), wait_ms_so_far should be ~0 for a NULL row.
    assert!(
        row.wait_ms_so_far >= 0,
        "wait_ms_so_far={} must be non-negative for NULL started_at",
        row.wait_ms_so_far
    );
}

// ── get_workflow_regression_signals ──────────────────────────────────────────

/// Backdate a run's `started_at` to N days ago (required for time-window filtering).
fn backdate_run(conn: &rusqlite::Connection, run_id: &str, days_ago: i64) {
    conn.execute(
        "UPDATE workflow_runs SET started_at = datetime('now', '-' || ?1 || ' days') WHERE id = ?2",
        rusqlite::params![days_ago, run_id],
    )
    .unwrap();
}

/// Complete a run with specific `duration_ms` and `cost_usd`.
fn complete_with_duration_cost(
    conn: &rusqlite::Connection,
    run_id: &str,
    duration_ms: i64,
    cost_usd: f64,
) {
    let mgr = WorkflowManager::new(conn);
    mgr.update_workflow_status(run_id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.persist_workflow_metrics(run_id, 0, 0, 0, 0, 1, cost_usd, duration_ms, None)
        .unwrap();
}

#[test]
fn test_regression_signals_empty_when_no_runs() {
    let conn = setup_db();
    let result = WorkflowManager::new(&conn)
        .get_workflow_regression_signals(1, 7, 30)
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_regression_signals_excluded_below_min_recent_runs() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // 2 recent runs — below min_recent_runs=3, so HAVING clause excludes the workflow.
    for _ in 0..2 {
        let run = create_named_worktree_run(&conn, "w1", "sparse-wf");
        complete_with_duration_cost(&conn, &run.id, 1000, 0.01);
        // stays at "now" — inside the recent window
    }
    // Baseline runs present so INNER JOIN would succeed if HAVING didn't block it.
    for _ in 0..5 {
        let run = create_named_worktree_run(&conn, "w1", "sparse-wf");
        complete_with_duration_cost(&conn, &run.id, 1000, 0.01);
        backdate_run(&conn, &run.id, 14);
    }

    let result = mgr.get_workflow_regression_signals(3, 7, 30).unwrap();
    assert!(
        result.is_empty(),
        "workflow with only 2 recent runs should be excluded by HAVING min_recent_runs=3"
    );
}

#[test]
fn test_regression_signals_excluded_when_no_baseline() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // One recent run but zero baseline runs → INNER JOIN produces no match.
    let run = create_named_worktree_run(&conn, "w1", "no-baseline-wf");
    complete_with_duration_cost(&conn, &run.id, 1000, 0.01);

    let result = mgr.get_workflow_regression_signals(1, 7, 30).unwrap();
    assert!(
        result.is_empty(),
        "workflow with no baseline runs should be excluded by INNER JOIN"
    );
}

#[test]
fn test_regression_signals_no_regression_when_stable() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // 5 baseline runs: durations [1000..5000] → P75 row 4 (1-indexed) = 4000 ms.
    // (cnt=5: (5*75+99)/100 = 4 in integer division)
    let durations = [1000i64, 2000, 3000, 4000, 5000];
    let costs = [0.01f64, 0.02, 0.03, 0.04, 0.05];

    for (&d, &c) in durations.iter().zip(costs.iter()) {
        let run = create_named_worktree_run(&conn, "w1", "stable-wf");
        complete_with_duration_cost(&conn, &run.id, d, c);
        backdate_run(&conn, &run.id, 14);
    }
    // 5 recent runs with identical metrics → 0% change on all signals.
    for (&d, &c) in durations.iter().zip(costs.iter()) {
        let run = create_named_worktree_run(&conn, "w1", "stable-wf");
        complete_with_duration_cost(&conn, &run.id, d, c);
    }

    let result = mgr.get_workflow_regression_signals(1, 7, 30).unwrap();
    assert_eq!(result.len(), 1);
    let s = &result[0];
    assert_eq!(s.workflow_name, "stable-wf");
    assert!(
        !s.duration_regressed,
        "identical durations should not flag duration regression"
    );
    assert!(
        !s.cost_regressed,
        "identical costs should not flag cost regression"
    );
    assert!(
        !s.failure_rate_regressed,
        "0% failure rate should not flag failure-rate regression"
    );
}

#[test]
fn test_regression_signals_duration_regressed() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Baseline P75 duration = 4000 ms: sorted [1000, 2000, 3000, 4000, 5000], row 4 = 4000.
    for &d in &[1000i64, 2000, 3000, 4000, 5000] {
        let run = create_named_worktree_run(&conn, "w1", "slow-wf");
        complete_with_duration_cost(&conn, &run.id, d, 0.01);
        backdate_run(&conn, &run.id, 14);
    }
    // Recent P75 duration = 6000 ms: sorted [1000, 2000, 3000, 6000, 7000], row 4 = 6000.
    // pct_change = (6000 - 4000) / 4000 * 100 = 50% > REGRESSION_DURATION_THRESHOLD_PCT (25%).
    for &d in &[1000i64, 2000, 3000, 6000, 7000] {
        let run = create_named_worktree_run(&conn, "w1", "slow-wf");
        complete_with_duration_cost(&conn, &run.id, d, 0.01);
    }

    let result = mgr.get_workflow_regression_signals(1, 7, 30).unwrap();
    assert_eq!(result.len(), 1);
    let s = &result[0];
    assert!(
        s.duration_regressed,
        "50% P75 duration increase should exceed the 25% threshold"
    );
    assert!(!s.cost_regressed);
    assert!(!s.failure_rate_regressed);
}

#[test]
fn test_regression_signals_cost_regressed() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Baseline P75 cost = $0.04: sorted [0.01, 0.02, 0.03, 0.04, 0.05], row 4 = 0.04.
    for &c in &[0.01f64, 0.02, 0.03, 0.04, 0.05] {
        let run = create_named_worktree_run(&conn, "w1", "costly-wf");
        complete_with_duration_cost(&conn, &run.id, 1000, c);
        backdate_run(&conn, &run.id, 14);
    }
    // Recent P75 cost = $0.06: sorted [0.01, 0.02, 0.03, 0.06, 0.07], row 4 = 0.06.
    // pct_change = (0.06 - 0.04) / 0.04 * 100 = 50% > REGRESSION_COST_THRESHOLD_PCT (20%).
    for &c in &[0.01f64, 0.02, 0.03, 0.06, 0.07] {
        let run = create_named_worktree_run(&conn, "w1", "costly-wf");
        complete_with_duration_cost(&conn, &run.id, 1000, c);
    }

    let result = mgr.get_workflow_regression_signals(1, 7, 30).unwrap();
    assert_eq!(result.len(), 1);
    let s = &result[0];
    assert!(!s.duration_regressed);
    assert!(
        s.cost_regressed,
        "50% P75 cost increase should exceed the 20% threshold"
    );
    assert!(!s.failure_rate_regressed);
}

#[test]
fn test_regression_signals_failure_rate_regressed() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Baseline: 5 completed runs → failure rate = 0%.
    for _ in 0..5 {
        let run = create_named_worktree_run(&conn, "w1", "flaky-wf");
        complete_with_duration_cost(&conn, &run.id, 1000, 0.01);
        backdate_run(&conn, &run.id, 14);
    }
    // Recent: 5 failed runs → failure rate = 100%.
    // change_pp = 100 - 0 = 100 > REGRESSION_FAILURE_RATE_THRESHOLD_PP (5.0).
    for _ in 0..5 {
        let run = create_named_worktree_run(&conn, "w1", "flaky-wf");
        WorkflowManager::new(&conn)
            .update_workflow_status(&run.id, WorkflowRunStatus::Failed, None, None)
            .unwrap();
    }

    let result = mgr.get_workflow_regression_signals(1, 7, 30).unwrap();
    assert_eq!(result.len(), 1);
    let s = &result[0];
    assert!(!s.duration_regressed);
    assert!(!s.cost_regressed);
    assert!(
        s.failure_rate_regressed,
        "100pp failure-rate increase should exceed the 5pp threshold"
    );
    assert!((s.recent_failure_rate - 100.0).abs() < 0.01);
    assert!((s.baseline_failure_rate - 0.0).abs() < 0.01);
}

#[test]
fn test_regression_signals_excludes_runs_outside_both_windows() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Runs backdated 40 days → outside both recent (7 days) and baseline (7–37 days) windows.
    for _ in 0..5 {
        let run = create_named_worktree_run(&conn, "w1", "old-wf");
        complete_with_duration_cost(&conn, &run.id, 1000, 0.01);
        backdate_run(&conn, &run.id, 40);
    }

    let result = mgr.get_workflow_regression_signals(1, 7, 30).unwrap();
    assert!(
        result.is_empty(),
        "runs older than both windows (40 days > 7+30=37 days) should be excluded"
    );
}

#[test]
fn test_regression_signals_multiple_workflows_independent() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    // Workflow "wf-alpha": stable — no regression expected.
    for &d in &[1000i64, 2000, 3000, 4000, 5000] {
        let run = create_named_worktree_run(&conn, "w1", "wf-alpha");
        complete_with_duration_cost(&conn, &run.id, d, 0.01);
        backdate_run(&conn, &run.id, 14);
    }
    for &d in &[1000i64, 2000, 3000, 4000, 5000] {
        let run = create_named_worktree_run(&conn, "w1", "wf-alpha");
        complete_with_duration_cost(&conn, &run.id, d, 0.01);
    }

    // Workflow "wf-beta": duration regressed (baseline P75=4000, recent P75=6000 → +50%).
    for &d in &[1000i64, 2000, 3000, 4000, 5000] {
        let run = create_named_worktree_run(&conn, "w1", "wf-beta");
        complete_with_duration_cost(&conn, &run.id, d, 0.01);
        backdate_run(&conn, &run.id, 14);
    }
    for &d in &[1000i64, 2000, 3000, 6000, 7000] {
        let run = create_named_worktree_run(&conn, "w1", "wf-beta");
        complete_with_duration_cost(&conn, &run.id, d, 0.01);
    }

    let result = mgr.get_workflow_regression_signals(1, 7, 30).unwrap();
    assert_eq!(result.len(), 2, "both workflows should appear");
    // ORDER BY workflow_name → "wf-alpha" before "wf-beta".
    assert_eq!(result[0].workflow_name, "wf-alpha");
    assert_eq!(result[1].workflow_name, "wf-beta");
    assert!(!result[0].duration_regressed, "wf-alpha should be stable");
    assert!(
        result[1].duration_regressed,
        "wf-beta should have duration regression"
    );
}

// ── get_fan_out_items_checked ownership tests ────────────────────────────────

#[test]
fn test_get_fan_out_items_checked_returns_items_for_correct_run() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");
    let step_id = mgr
        .insert_step(&run.id, "fan-step", "actor", false, 0, 0)
        .unwrap();
    mgr.insert_fan_out_item(&step_id, "ticket", "t1", "TICKET-1")
        .unwrap();

    let items = mgr
        .get_fan_out_items_checked(&run.id, &step_id, None)
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].item_id, "t1");
}

#[test]
fn test_get_fan_out_items_checked_rejects_step_from_different_run() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run_a = create_worktree_run(&conn, "w1");
    let run_b = create_worktree_run(&conn, "w2");

    // Step belongs to run_a.
    let step_id = mgr
        .insert_step(&run_a.id, "fan-step", "actor", false, 0, 0)
        .unwrap();

    // Querying with run_b's ID must return WorkflowStepNotInRun.
    let err = mgr
        .get_fan_out_items_checked(&run_b.id, &step_id, None)
        .unwrap_err();
    assert!(
        matches!(
            err,
            crate::error::ConductorError::WorkflowStepNotInRun { .. }
        ),
        "expected WorkflowStepNotInRun, got: {err:?}"
    );
}

#[test]
fn test_get_fan_out_items_checked_rejects_nonexistent_step() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");

    let err = mgr
        .get_fan_out_items_checked(&run.id, "nonexistent-step-id", None)
        .unwrap_err();
    assert!(
        matches!(
            err,
            crate::error::ConductorError::WorkflowStepNotFound { .. }
        ),
        "expected WorkflowStepNotFound, got: {err:?}"
    );
}

// ── skip_fan_out_items_by_item_ids tests ─────────────────────────────────────

#[test]
fn test_skip_fan_out_items_by_item_ids_marks_subset_skipped() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");
    let step_id = mgr
        .insert_step(&run.id, "fan-step", "actor", false, 0, 0)
        .unwrap();

    // Insert three pending items.
    mgr.insert_fan_out_item(&step_id, "ticket", "t1", "TICKET-1")
        .unwrap();
    mgr.insert_fan_out_item(&step_id, "ticket", "t2", "TICKET-2")
        .unwrap();
    mgr.insert_fan_out_item(&step_id, "ticket", "t3", "TICKET-3")
        .unwrap();

    // Skip only t1 and t3.
    mgr.skip_fan_out_items_by_item_ids(
        &step_id,
        &["t1".to_string(), "t3".to_string()],
    )
    .unwrap();

    let items = mgr.get_fan_out_items(&step_id, None).unwrap();
    let status_map: std::collections::HashMap<_, _> =
        items.iter().map(|it| (it.item_id.as_str(), it.status.as_str())).collect();

    assert_eq!(status_map["t1"], "skipped");
    assert_eq!(status_map["t2"], "pending");
    assert_eq!(status_map["t3"], "skipped");

    // skipped items must have completed_at set.
    let t1 = items.iter().find(|it| it.item_id == "t1").unwrap();
    assert!(t1.completed_at.is_some(), "completed_at should be set for skipped item");
}

#[test]
fn test_skip_fan_out_items_by_item_ids_ignores_non_pending() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");
    let step_id = mgr
        .insert_step(&run.id, "fan-step", "actor", false, 0, 0)
        .unwrap();

    // Insert one item then mark it running (simulates an in-flight dispatch).
    let fan_item_db_id = mgr
        .insert_fan_out_item(&step_id, "ticket", "t1", "TICKET-1")
        .unwrap();
    mgr.update_fan_out_item_running(&fan_item_db_id, "child-run-1")
        .unwrap();

    // Attempting to skip t1 by item_id should not change a running item.
    mgr.skip_fan_out_items_by_item_ids(&step_id, &["t1".to_string()])
        .unwrap();

    let items = mgr.get_fan_out_items(&step_id, None).unwrap();
    assert_eq!(items[0].status, "running", "running item must not be overwritten by skip");
}

#[test]
fn test_skip_fan_out_items_by_item_ids_empty_list_is_noop() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let run = create_worktree_run(&conn, "w1");
    let step_id = mgr
        .insert_step(&run.id, "fan-step", "actor", false, 0, 0)
        .unwrap();
    mgr.insert_fan_out_item(&step_id, "ticket", "t1", "TICKET-1")
        .unwrap();

    // Empty slice must be a no-op (not a panic or SQL error).
    mgr.skip_fan_out_items_by_item_ids(&step_id, &[]).unwrap();

    let items = mgr.get_fan_out_items(&step_id, None).unwrap();
    assert_eq!(items[0].status, "pending");
}
