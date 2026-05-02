#![allow(unused_imports)]

use super::*;
use crate::agent::AgentManager;
use rusqlite::{named_params, Connection};
use std::collections::HashMap;

#[test]
fn test_create_workflow_run() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let run = crate::workflow::create_workflow_run(
        &conn,
        "test-coverage",
        Some("w1"),
        &parent.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    assert_eq!(run.workflow_name, "test-coverage");
    assert_eq!(run.status, WorkflowRunStatus::Pending);
    assert!(!run.dry_run);
}

#[test]
fn test_create_workflow_run_with_snapshot() {
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
        Some(r#"{"name":"test"}"#),
    )
    .unwrap();

    let fetched = crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        fetched.definition_snapshot.as_deref(),
        Some(r#"{"name":"test"}"#)
    );
}

#[test]
fn test_create_workflow_run_with_repo_id_round_trip() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let run = crate::workflow::create_workflow_run_with_targets(
        &conn,
        "test-wf",
        Some("w1"),
        None,
        Some("r1"),
        &parent.id,
        false,
        "manual",
        None,
        None,
        None,
    )
    .unwrap();

    // Verify the struct returned by create reflects the inputs.
    assert_eq!(run.repo_id.as_deref(), Some("r1"));
    assert_eq!(run.ticket_id, None);

    // Read back from DB and assert columns are persisted correctly.
    let fetched = crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .unwrap();
    assert_eq!(fetched.repo_id.as_deref(), Some("r1"));
    assert_eq!(fetched.ticket_id, None);
}

#[test]
fn test_active_run_counts_by_repo_empty() {
    let conn = setup_db();
    let counts = crate::workflow::active_run_counts_by_repo(&conn).unwrap();
    assert!(
        counts.is_empty(),
        "expected no counts with no workflow runs"
    );
}

#[test]
fn test_active_run_counts_by_repo_with_runs() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

    // Create one pending and one running run for repo r1.
    let run1 = crate::workflow::create_workflow_run_with_targets(
        &conn,
        "wf-a",
        Some("w1"),
        None,
        Some("r1"),
        &parent.id,
        false,
        "manual",
        None,
        None,
        None,
    )
    .unwrap();
    // Advance run1 to running.
    conn.execute(
        "UPDATE workflow_runs SET status = 'running' WHERE id = ?1",
        [&run1.id],
    )
    .unwrap();
    let _run2 = crate::workflow::create_workflow_run_with_targets(
        &conn,
        "wf-b",
        Some("w1"),
        None,
        Some("r1"),
        &parent.id,
        false,
        "manual",
        None,
        None,
        None,
    )
    .unwrap();
    // run2 stays at pending (default).

    let counts = crate::workflow::active_run_counts_by_repo(&conn).unwrap();
    let c = counts.get("r1").expect("r1 should be in map");
    assert_eq!(c.running, 1, "expected 1 running");
    assert_eq!(c.pending, 1, "expected 1 pending");
    assert_eq!(c.waiting, 0, "expected 0 waiting");
}

#[test]
fn test_active_run_counts_by_repo_excludes_completed() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

    let run = crate::workflow::create_workflow_run_with_targets(
        &conn,
        "wf-done",
        Some("w1"),
        None,
        Some("r1"),
        &parent.id,
        false,
        "manual",
        None,
        None,
        None,
    )
    .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET status = 'completed' WHERE id = ?1",
        [&run.id],
    )
    .unwrap();

    let counts = crate::workflow::active_run_counts_by_repo(&conn).unwrap();
    assert!(
        !counts.contains_key("r1"),
        "completed runs must not appear in active counts"
    );
}

#[test]
fn test_create_workflow_run_with_ticket_id_round_trip() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

    insert_test_ticket(&conn, "tkt-rt-1", "r1");
    let run = crate::workflow::create_workflow_run_with_targets(
        &conn,
        "test-wf",
        None,
        Some("tkt-rt-1"),
        None,
        &parent.id,
        false,
        "manual",
        None,
        None,
        None,
    )
    .unwrap();

    // Verify the struct returned by create reflects the inputs.
    assert_eq!(run.ticket_id.as_deref(), Some("tkt-rt-1"));
    assert_eq!(run.repo_id, None);

    // Read back from DB and assert columns are persisted correctly.
    let fetched = crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .unwrap();
    assert_eq!(fetched.ticket_id.as_deref(), Some("tkt-rt-1"));
    assert_eq!(fetched.repo_id, None);
}

#[test]
fn test_insert_step_with_iteration() {
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
        crate::workflow::insert_step(&conn, &run.id, "review", "reviewer", false, 0, 2).unwrap();

    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].id, step_id);
    assert_eq!(steps[0].step_name, "review");
    assert_eq!(steps[0].iteration, 2);
}

#[test]
fn test_insert_step_running_is_atomic() {
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
        crate::workflow::insert_step_running(&conn, &run.id, "build", "script", false, 0, 0, 2)
            .unwrap();

    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].id, step_id);
    assert_eq!(steps[0].step_name, "build");
    // The row was inserted directly as 'running' — no intermediate 'pending' state
    assert_eq!(steps[0].status.to_string(), "running");
    // started_at must be set (was part of the single INSERT)
    assert!(
        steps[0].started_at.is_some(),
        "started_at should be set by insert_step_running"
    );
    // retry_count must reflect what was passed (2)
    assert_eq!(steps[0].retry_count, 2);
}

#[test]
fn test_update_step_with_markers() {
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
        crate::workflow::insert_step(&conn, &run.id, "review", "reviewer", false, 0, 0).unwrap();

    crate::workflow::update_step_status(
        &conn,
        &step_id,
        WorkflowStepStatus::Completed,
        None,
        Some("Found issues"),
        Some("2 issues in lib.rs"),
        Some(r#"["has_review_issues"]"#),
        Some(0),
    )
    .unwrap();

    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert_eq!(steps[0].context_out.as_deref(), Some("2 issues in lib.rs"));
    assert_eq!(
        steps[0].markers_out.as_deref(),
        Some(r#"["has_review_issues"]"#)
    );
}

#[test]
fn test_update_step_status_full_with_structured_output() {
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
        crate::workflow::insert_step(&conn, &run.id, "review", "reviewer", false, 0, 0).unwrap();

    let structured_json = r#"{"approved":true,"summary":"All good"}"#;
    crate::workflow::update_step_status_full(
        &conn,
        &step_id,
        WorkflowStepStatus::Completed,
        None,
        Some("result text"),
        Some("All good"),
        Some(r#"[]"#),
        Some(0),
        Some(structured_json),
        None,
    )
    .unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.structured_output.as_deref(), Some(structured_json));
    assert_eq!(step.context_out.as_deref(), Some("All good"));
    assert_eq!(step.result_text.as_deref(), Some("result text"));
}

#[test]
fn test_update_step_status_full_without_structured_output() {
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
        crate::workflow::insert_step(&conn, &run.id, "review", "reviewer", false, 0, 0).unwrap();

    crate::workflow::update_step_status_full(
        &conn,
        &step_id,
        WorkflowStepStatus::Completed,
        None,
        Some("result text"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert!(step.structured_output.is_none());
}

#[test]
fn test_update_step_status_full_with_step_error() {
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
        crate::workflow::insert_step(&conn, &run.id, "call-step", "reviewer", false, 0, 0).unwrap();

    let validation_error = "expected field 'approved' but output was missing required keys";
    crate::workflow::update_step_status_full(
        &conn,
        &step_id,
        WorkflowStepStatus::Failed,
        None,
        Some("raw agent output"),
        None,
        None,
        Some(0),
        None,
        Some(validation_error),
    )
    .unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.step_error.as_deref(), Some(validation_error));
    assert_eq!(step.result_text.as_deref(), Some("raw agent output"));
    assert!(step.structured_output.is_none());
}

#[test]
fn test_list_workflow_runs() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None).unwrap();
    let p2 = agent_mgr.create_run(Some("w1"), "wf2", None).unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "test-a",
        Some("w1"),
        &p1.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    crate::workflow::create_workflow_run(&conn, "test-b", Some("w1"), &p2.id, true, "pr", None)
        .unwrap();

    let runs = crate::workflow::list_workflow_runs(&conn, "w1").unwrap();
    assert_eq!(runs.len(), 2);
}

#[test]
fn test_list_all_workflow_runs_cross_worktree() {
    let conn = setup_db();
    // Insert a second worktree so we can test cross-worktree aggregation.
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let agent_mgr = AgentManager::new(&conn);
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None).unwrap();
    let p2 = agent_mgr.create_run(Some("w2"), "wf2", None).unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "flow-a",
        Some("w1"),
        &p1.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "flow-b",
        Some("w2"),
        &p2.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    // list_all returns both runs regardless of worktree
    let all = crate::workflow::list_all_workflow_runs(&conn, 100).unwrap();
    assert_eq!(all.len(), 2);
    let names: Vec<&str> = all.iter().map(|r| r.workflow_name.as_str()).collect();
    assert!(names.contains(&"flow-a"));
    assert!(names.contains(&"flow-b"));
}

#[test]
fn test_list_all_workflow_runs_respects_limit() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    for i in 0..5 {
        let p = agent_mgr
            .create_run(Some("w1"), &format!("wf{i}"), None)
            .unwrap();
        crate::workflow::create_workflow_run(
            &conn,
            &format!("flow-{i}"),
            Some("w1"),
            &p.id,
            false,
            "manual",
            None,
        )
        .unwrap();
    }

    let limited = crate::workflow::list_all_workflow_runs(&conn, 3).unwrap();
    assert_eq!(limited.len(), 3);
}

#[test]
fn test_list_all_workflow_runs_empty() {
    let conn = setup_db();
    let runs = crate::workflow::list_all_workflow_runs(&conn, 50).unwrap();
    assert!(runs.is_empty());
}

#[test]
fn test_list_all_workflow_runs_includes_ephemeral() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    // Create a normal run (with worktree)
    let parent1 = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "normal-wf",
        Some("w1"),
        &parent1.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    // Create an ephemeral run (no worktree)
    let parent2 = agent_mgr
        .create_run(None, "ephemeral workflow", None)
        .unwrap();
    let ephemeral = crate::workflow::create_workflow_run(
        &conn,
        "ephemeral-wf",
        None,
        &parent2.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    let all = crate::workflow::list_all_workflow_runs(&conn, 100).unwrap();
    assert_eq!(all.len(), 2);

    // Verify the ephemeral run has None worktree_id
    let found = all.iter().find(|r| r.id == ephemeral.id).unwrap();
    assert!(found.worktree_id.is_none());
}

#[test]
fn test_list_all_workflow_runs_excludes_merged_worktree() {
    let conn = setup_db();
    // Insert a second worktree with merged status
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r1', 'feat-merged', 'feat/merged', '/tmp/ws/merged', 'merged', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let agent_mgr = AgentManager::new(&conn);
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None).unwrap();
    let p2 = agent_mgr.create_run(Some("w2"), "wf2", None).unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "active-run",
        Some("w1"),
        &p1.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "merged-run",
        Some("w2"),
        &p2.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    let all = crate::workflow::list_all_workflow_runs(&conn, 100).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].workflow_name, "active-run");
}

#[test]
fn test_list_all_workflow_runs_excludes_abandoned_worktree() {
    let conn = setup_db();
    // Insert a second worktree with abandoned status
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r1', 'feat-abandoned', 'feat/abandoned', '/tmp/ws/abandoned', 'abandoned', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let agent_mgr = AgentManager::new(&conn);
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None).unwrap();
    let p2 = agent_mgr.create_run(Some("w2"), "wf2", None).unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "active-run",
        Some("w1"),
        &p1.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "abandoned-run",
        Some("w2"),
        &p2.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    let all = crate::workflow::list_all_workflow_runs(&conn, 100).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].workflow_name, "active-run");
}

#[test]
fn test_list_all_workflow_runs_includes_ephemeral_and_active() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    // Active worktree run
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None).unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "active-run",
        Some("w1"),
        &p1.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    // Ephemeral run (no worktree)
    let p2 = agent_mgr.create_run(None, "wf2", None).unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "ephemeral-run",
        None,
        &p2.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    let all = crate::workflow::list_all_workflow_runs(&conn, 100).unwrap();
    assert_eq!(all.len(), 2);
    let names: Vec<&str> = all.iter().map(|r| r.workflow_name.as_str()).collect();
    assert!(names.contains(&"active-run"));
    assert!(names.contains(&"ephemeral-run"));
}

#[test]
fn test_list_all_workflow_runs_filtered_paginated_status_filter() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    // Create one run and leave it in Pending state.
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None).unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "pending-run",
        Some("w1"),
        &p1.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    // Create a second run and advance it to Completed.
    let p2 = agent_mgr.create_run(Some("w1"), "wf2", None).unwrap();
    let r2 = crate::workflow::create_workflow_run(
        &conn,
        "done-run",
        Some("w1"),
        &p2.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &r2.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();

    let completed = crate::workflow::list_all_workflow_runs_filtered_paginated(
        &conn,
        Some(WorkflowRunStatus::Completed),
        100,
        0,
    )
    .unwrap();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].workflow_name, "done-run");

    let pending = crate::workflow::list_all_workflow_runs_filtered_paginated(
        &conn,
        Some(WorkflowRunStatus::Pending),
        100,
        0,
    )
    .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].workflow_name, "pending-run");
}

#[test]
fn test_list_all_workflow_runs_filtered_paginated_offset() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    for i in 0..4 {
        let p = agent_mgr
            .create_run(Some("w1"), &format!("wf{i}"), None)
            .unwrap();
        crate::workflow::create_workflow_run(
            &conn,
            &format!("flow-{i}"),
            Some("w1"),
            &p.id,
            false,
            "manual",
            None,
        )
        .unwrap();
    }

    let page1 =
        crate::workflow::list_all_workflow_runs_filtered_paginated(&conn, None, 2, 0).unwrap();
    assert_eq!(page1.len(), 2);

    let page2 =
        crate::workflow::list_all_workflow_runs_filtered_paginated(&conn, None, 2, 2).unwrap();
    assert_eq!(page2.len(), 2);

    // All 4 unique
    let all_ids: std::collections::HashSet<_> = page1
        .iter()
        .chain(page2.iter())
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(all_ids.len(), 4);
}

#[test]
fn test_list_workflow_runs_by_repo_id_excludes_merged_worktree() {
    let conn = setup_db();
    // Insert a second worktree with merged status (same repo)
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r1', 'feat-merged', 'feat/merged', '/tmp/ws/merged', 'merged', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let agent_mgr = AgentManager::new(&conn);
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None).unwrap();
    let p2 = agent_mgr.create_run(Some("w2"), "wf2", None).unwrap();
    // Use create_workflow_run_with_targets to set repo_id so the query can filter by it
    crate::workflow::create_workflow_run_with_targets(
        &conn,
        "active-run",
        Some("w1"),
        None,
        Some("r1"),
        &p1.id,
        false,
        "manual",
        None,
        None,
        None,
    )
    .unwrap();
    crate::workflow::create_workflow_run_with_targets(
        &conn,
        "merged-run",
        Some("w2"),
        None,
        Some("r1"),
        &p2.id,
        false,
        "manual",
        None,
        None,
        None,
    )
    .unwrap();

    let runs = crate::workflow::list_workflow_runs_by_repo_id(&conn, "r1", 100, 0).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].workflow_name, "active-run");
}

#[test]
fn test_list_workflow_runs_for_scope_scoped() {
    let conn = setup_db();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let agent_mgr = AgentManager::new(&conn);
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None).unwrap();
    let p2 = agent_mgr.create_run(Some("w2"), "wf2", None).unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "only-w1",
        Some("w1"),
        &p1.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "only-w2",
        Some("w2"),
        &p2.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    // Scoped: only w1's run
    let scoped = crate::workflow::list_workflow_runs_for_scope(&conn, Some("w1"), 50).unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].workflow_name, "only-w1");

    // Global: both runs
    let global = crate::workflow::list_workflow_runs_for_scope(&conn, None, 50).unwrap();
    assert_eq!(global.len(), 2);
}

#[test]
fn test_list_workflow_runs_for_scope_global_limit() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    for i in 0..5 {
        let p = agent_mgr
            .create_run(Some("w1"), &format!("wf{i}"), None)
            .unwrap();
        crate::workflow::create_workflow_run(
            &conn,
            &format!("flow-{i}"),
            Some("w1"),
            &p.id,
            false,
            "manual",
            None,
        )
        .unwrap();
    }
    let limited = crate::workflow::list_workflow_runs_for_scope(&conn, None, 2).unwrap();
    assert_eq!(limited.len(), 2);
}

#[test]
fn test_get_workflow_run_not_found() {
    let conn = setup_db();
    let result = crate::workflow::get_workflow_run(&conn, "nonexistent").unwrap();
    assert!(result.is_none());
}

#[test]
fn test_get_step_by_id() {
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
        crate::workflow::insert_step(&conn, &run.id, "build", "actor", false, 0, 0).unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id).unwrap();
    assert!(step.is_some());
    let step = step.unwrap();
    assert_eq!(step.id, step_id);
    assert_eq!(step.step_name, "build");
    assert_eq!(step.role, "actor");

    let missing = crate::workflow::get_step_by_id(&conn, "nonexistent").unwrap();
    assert!(missing.is_none());
}

#[test]
fn test_purge_all_terminal_statuses() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let a2 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let a3 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let a4 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let r_completed =
        crate::workflow::create_workflow_run(&conn, "t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
    let r_failed =
        crate::workflow::create_workflow_run(&conn, "t", Some("w1"), &a2.id, false, "manual", None)
            .unwrap();
    let r_cancelled =
        crate::workflow::create_workflow_run(&conn, "t", Some("w1"), &a3.id, false, "manual", None)
            .unwrap();
    let r_running =
        crate::workflow::create_workflow_run(&conn, "t", Some("w1"), &a4.id, false, "manual", None)
            .unwrap();

    crate::workflow::update_workflow_status(
        &conn,
        &r_completed.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &r_failed.id,
        WorkflowRunStatus::Failed,
        None,
        None,
    )
    .unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &r_cancelled.id,
        WorkflowRunStatus::Cancelled,
        None,
        None,
    )
    .unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &r_running.id,
        WorkflowRunStatus::Running,
        None,
        None,
    )
    .unwrap();

    let deleted =
        crate::workflow::purge(&conn, None, &["completed", "failed", "cancelled"]).unwrap();
    assert_eq!(deleted, 3);

    // running run must still exist
    assert!(crate::workflow::get_workflow_run(&conn, &r_running.id)
        .unwrap()
        .is_some());
    // terminal runs must be gone
    assert!(crate::workflow::get_workflow_run(&conn, &r_completed.id)
        .unwrap()
        .is_none());
    assert!(crate::workflow::get_workflow_run(&conn, &r_failed.id)
        .unwrap()
        .is_none());
    assert!(crate::workflow::get_workflow_run(&conn, &r_cancelled.id)
        .unwrap()
        .is_none());
}

#[test]
fn test_purge_single_status_filter() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let a2 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let r_completed =
        crate::workflow::create_workflow_run(&conn, "t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
    let r_failed =
        crate::workflow::create_workflow_run(&conn, "t", Some("w1"), &a2.id, false, "manual", None)
            .unwrap();

    crate::workflow::update_workflow_status(
        &conn,
        &r_completed.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &r_failed.id,
        WorkflowRunStatus::Failed,
        None,
        None,
    )
    .unwrap();

    // only purge completed
    let deleted = crate::workflow::purge(&conn, None, &["completed"]).unwrap();
    assert_eq!(deleted, 1);

    assert!(crate::workflow::get_workflow_run(&conn, &r_completed.id)
        .unwrap()
        .is_none());
    assert!(crate::workflow::get_workflow_run(&conn, &r_failed.id)
        .unwrap()
        .is_some());
}

#[test]
fn test_purge_repo_scoped() {
    let conn = setup_db();
    // Add a second repo + worktree
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
         VALUES ('r2', 'other-repo', '/tmp/r2', '', '/tmp/ws2', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r2', 'feat-other', 'feat/other', '/tmp/ws2/feat-other', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let agent_mgr = AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let a2 = agent_mgr.create_run(Some("w2"), "wf", None).unwrap();
    let run_r1 =
        crate::workflow::create_workflow_run(&conn, "t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
    let run_r2 =
        crate::workflow::create_workflow_run(&conn, "t", Some("w2"), &a2.id, false, "manual", None)
            .unwrap();

    crate::workflow::update_workflow_status(
        &conn,
        &run_r1.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &run_r2.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();

    // scope to r1 only
    let deleted = crate::workflow::purge(&conn, Some("r1"), &["completed"]).unwrap();
    assert_eq!(deleted, 1);

    assert!(crate::workflow::get_workflow_run(&conn, &run_r1.id)
        .unwrap()
        .is_none());
    assert!(crate::workflow::get_workflow_run(&conn, &run_r2.id)
        .unwrap()
        .is_some());
}

#[test]
fn test_purge_cascade_deletes_steps() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let run =
        crate::workflow::create_workflow_run(&conn, "t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
    crate::workflow::insert_step(&conn, &run.id, "step1", "actor", true, 0, 0).unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &run.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();

    let deleted = crate::workflow::purge(&conn, None, &["completed"]).unwrap();
    assert_eq!(deleted, 1);

    // steps must be gone (cascade)
    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert!(steps.is_empty());
}

#[test]
fn test_purge_count_matches_purge() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let a2 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let r1 =
        crate::workflow::create_workflow_run(&conn, "t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
    let r2 =
        crate::workflow::create_workflow_run(&conn, "t", Some("w1"), &a2.id, false, "manual", None)
            .unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &r1.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();
    crate::workflow::update_workflow_status(&conn, &r2.id, WorkflowRunStatus::Failed, None, None)
        .unwrap();

    let statuses = &["completed", "failed", "cancelled"];
    let count = crate::workflow::purge_count(&conn, None, statuses).unwrap();
    assert_eq!(count, 2);

    let deleted = crate::workflow::purge(&conn, None, statuses).unwrap();
    assert_eq!(deleted, count);
}

#[test]
fn test_purge_noop_when_no_matches() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let run =
        crate::workflow::create_workflow_run(&conn, "t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
    crate::workflow::update_workflow_status(&conn, &run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let count =
        crate::workflow::purge_count(&conn, None, &["completed", "failed", "cancelled"]).unwrap();
    assert_eq!(count, 0);

    let deleted =
        crate::workflow::purge(&conn, None, &["completed", "failed", "cancelled"]).unwrap();
    assert_eq!(deleted, 0);

    assert!(crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .is_some());
}

#[test]
fn test_purge_empty_statuses_is_noop() {
    let conn = setup_db();
    assert_eq!(crate::workflow::purge(&conn, None, &[]).unwrap(), 0);
    assert_eq!(crate::workflow::purge_count(&conn, None, &[]).unwrap(), 0);
}

/// Repo-scoped purge must NOT delete global workflow runs (worktree_id IS NULL).
#[test]
fn test_purge_repo_scoped_does_not_delete_global_runs() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    // Create a global run (no worktree) and a run scoped to w1.
    let a_global = agent_mgr.create_run(None, "wf", None).unwrap();
    let a_w1 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();

    let run_global =
        crate::workflow::create_workflow_run(&conn, "t", None, &a_global.id, false, "manual", None)
            .unwrap();
    let run_w1 = crate::workflow::create_workflow_run(
        &conn,
        "t",
        Some("w1"),
        &a_w1.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    crate::workflow::update_workflow_status(
        &conn,
        &run_global.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &run_w1.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();

    // Scope purge to r1 — must only delete the worktree-bound run.
    assert_eq!(
        crate::workflow::purge_count(&conn, Some("r1"), &["completed"]).unwrap(),
        1
    );
    let deleted = crate::workflow::purge(&conn, Some("r1"), &["completed"]).unwrap();
    assert_eq!(deleted, 1);

    // Global run must survive.
    assert!(crate::workflow::get_workflow_run(&conn, &run_global.id)
        .unwrap()
        .is_some());
    // w1 run must be gone.
    assert!(crate::workflow::get_workflow_run(&conn, &run_w1.id)
        .unwrap()
        .is_none());
}

// ---------- delete_run tests ----------

#[test]
fn test_delete_run_removes_completed_run() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    crate::workflow::update_workflow_status(
        &conn,
        &run.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();

    crate::workflow::delete_run(&conn, &run.id).unwrap();

    assert!(crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .is_none());
}

#[test]
fn test_delete_run_removes_failed_run() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    crate::workflow::update_workflow_status(&conn, &run.id, WorkflowRunStatus::Failed, None, None)
        .unwrap();

    crate::workflow::delete_run(&conn, &run.id).unwrap();

    assert!(crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .is_none());
}

#[test]
fn test_delete_run_removes_cancelled_run() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    crate::workflow::update_workflow_status(
        &conn,
        &run.id,
        WorkflowRunStatus::Cancelled,
        None,
        None,
    )
    .unwrap();

    crate::workflow::delete_run(&conn, &run.id).unwrap();

    assert!(crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .is_none());
}

#[test]
fn test_delete_run_cascade_deletes_steps() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    crate::workflow::insert_step(&conn, &run.id, "step1", "actor", false, 0, 0).unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &run.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();

    crate::workflow::delete_run(&conn, &run.id).unwrap();

    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert!(
        steps.is_empty(),
        "steps should be cascade-deleted with the run"
    );
}

#[test]
fn test_delete_run_not_found_returns_error() {
    let conn = setup_db();
    let result = crate::workflow::delete_run(&conn, "nonexistent-id");
    assert!(
        result.is_err(),
        "deleting a nonexistent run should return an error"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("nonexistent-id"),
        "error should mention the missing run ID"
    );
}

#[test]
fn test_delete_run_rejects_running_run() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    crate::workflow::update_workflow_status(&conn, &run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let result = crate::workflow::delete_run(&conn, &run.id);
    assert!(
        result.is_err(),
        "deleting a running run should return an error"
    );
    // Run must still exist
    assert!(crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .is_some());
}

#[test]
fn test_delete_run_rejects_pending_run() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    // run starts as Pending

    let result = crate::workflow::delete_run(&conn, &run.id);
    assert!(
        result.is_err(),
        "deleting a pending run should return an error"
    );
    assert!(crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .is_some());
}

#[test]
fn test_delete_run_recursive_removes_child_runs() {
    let conn = setup_db();
    // Create parent run
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent_agent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let parent_run = crate::workflow::create_workflow_run(
        &conn,
        "parent-wf",
        Some("w1"),
        &parent_agent.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    // Create a child run (parent_workflow_run_id points to parent_run)
    let child_agent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let child_run = crate::workflow::create_workflow_run_with_targets(
        &conn,
        "child-wf",
        Some("w1"),
        None,
        None,
        &child_agent.id,
        false,
        "manual",
        None,
        Some(&parent_run.id),
        None,
    )
    .unwrap();

    // Mark both terminal
    crate::workflow::update_workflow_status(
        &conn,
        &child_run.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &parent_run.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();

    crate::workflow::delete_run(&conn, &parent_run.id).unwrap();

    // Both parent and child should be gone
    assert!(crate::workflow::get_workflow_run(&conn, &parent_run.id)
        .unwrap()
        .is_none());
    assert!(crate::workflow::get_workflow_run(&conn, &child_run.id)
        .unwrap()
        .is_none());
}

#[test]
fn test_delete_run_does_not_affect_sibling_runs() {
    let conn = setup_db();
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();
    let a2 = agent_mgr.create_run(Some("w1"), "wf", None).unwrap();

    let run1 = crate::workflow::create_workflow_run(
        &conn,
        "wf",
        Some("w1"),
        &a1.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    let run2 = crate::workflow::create_workflow_run(
        &conn,
        "wf",
        Some("w1"),
        &a2.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    crate::workflow::update_workflow_status(
        &conn,
        &run1.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &run2.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();

    crate::workflow::delete_run(&conn, &run1.id).unwrap();

    assert!(crate::workflow::get_workflow_run(&conn, &run1.id)
        .unwrap()
        .is_none());
    assert!(
        crate::workflow::get_workflow_run(&conn, &run2.id)
            .unwrap()
            .is_some(),
        "sibling run should not be deleted"
    );
}

#[test]
fn test_cancel_run_pending() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    assert_eq!(run.status, WorkflowRunStatus::Pending);

    crate::workflow::cancel_run(&conn, &run.id, "user requested").unwrap();

    let updated = crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .unwrap();
    assert_eq!(updated.status, WorkflowRunStatus::Cancelled);
}

#[test]
fn test_cancel_run_running_with_active_steps() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    // Advance run to Running
    crate::workflow::update_workflow_status(&conn, &run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    // Insert a Running step with a child agent run
    let child_agent_mgr = AgentManager::new(&conn);
    let child = child_agent_mgr
        .create_run(Some("w1"), "child-step", None)
        .unwrap();

    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "do-work", "actor", false, 0, 0).unwrap();
    crate::workflow::update_step_status(
        &conn,
        &step_id,
        WorkflowStepStatus::Running,
        Some(&child.id),
        None,
        None,
        None,
        None,
    )
    .unwrap();

    // Cancel the run — should cancel step and child agent run
    crate::workflow::cancel_run(&conn, &run.id, "abort").unwrap();

    let updated_run = crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .unwrap();
    assert_eq!(updated_run.status, WorkflowRunStatus::Cancelled);

    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Failed);

    let agent_run: String = conn
        .query_row(
            "SELECT status FROM agent_runs WHERE id = :id",
            named_params! { ":id": child.id },
            |r| r.get("status"),
        )
        .unwrap();
    assert_eq!(agent_run, "cancelled");
}

#[test]
fn test_cancel_run_waiting_status() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    // Advance run to Waiting (e.g. at a gate)
    crate::workflow::set_waiting_blocked_on(
        &conn,
        &run.id,
        &BlockedOn::HumanApproval {
            gate_name: "human-gate".to_string(),
            prompt: None,
            options: vec![],
        },
    )
    .unwrap();

    // Insert a Waiting step (no child run)
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "human-gate", "gate", false, 0, 0).unwrap();
    set_step_status(&conn, &step_id, WorkflowStepStatus::Waiting);

    crate::workflow::cancel_run(&conn, &run.id, "timed out").unwrap();

    let updated = crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .unwrap();
    assert_eq!(updated.status, WorkflowRunStatus::Cancelled);

    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
}

#[test]
fn test_cancel_run_skips_terminal_steps() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    crate::workflow::update_workflow_status(&conn, &run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    // A completed step — must not be touched
    let done_step =
        crate::workflow::insert_step(&conn, &run.id, "already-done", "actor", false, 0, 0).unwrap();
    crate::workflow::update_step_status(
        &conn,
        &done_step,
        WorkflowStepStatus::Completed,
        None,
        Some("done"),
        None,
        None,
        None,
    )
    .unwrap();

    // An active step — must be cancelled
    let active_step =
        crate::workflow::insert_step(&conn, &run.id, "in-progress", "actor", false, 1, 0).unwrap();
    set_step_status(&conn, &active_step, WorkflowStepStatus::Running);

    crate::workflow::cancel_run(&conn, &run.id, "stop").unwrap();

    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    let done = steps.iter().find(|s| s.id == done_step).unwrap();
    let active = steps.iter().find(|s| s.id == active_step).unwrap();

    assert_eq!(
        done.status,
        WorkflowStepStatus::Completed,
        "completed step must not be modified"
    );
    assert_eq!(
        active.status,
        WorkflowStepStatus::Failed,
        "active step must be marked failed"
    );
}

#[test]
fn test_cancel_run_already_terminal_returns_error() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    crate::workflow::update_workflow_status(
        &conn,
        &run.id,
        WorkflowRunStatus::Completed,
        None,
        None,
    )
    .unwrap();

    let err = crate::workflow::cancel_run(&conn, &run.id, "too late").unwrap_err();
    assert!(
        err.to_string().contains("terminal state"),
        "expected terminal state error, got: {err}"
    );
}

#[test]
fn test_cancel_run_not_found_returns_error() {
    let conn = setup_db();

    let err = crate::workflow::cancel_run(&conn, "nonexistent-id", "reason").unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "expected not-found error, got: {err}"
    );
}

#[test]
fn test_find_resumable_child_run_returns_failed() {
    let conn = setup_db();
    insert_workflow_run(&conn, "parent1", "parent-wf", "failed", None);
    insert_workflow_run(&conn, "child1", "child-wf", "failed", Some("parent1"));
    let result = crate::workflow::find_resumable_child_run(&conn, "parent1", "child-wf").unwrap();
    assert!(result.is_some(), "failed child run should be found");
    assert_eq!(result.unwrap().id, "child1");
}

#[test]
fn test_find_resumable_child_run_ignores_completed() {
    let conn = setup_db();
    insert_workflow_run(&conn, "parent1", "parent-wf", "failed", None);
    insert_workflow_run(&conn, "child1", "child-wf", "completed", Some("parent1"));
    let result = crate::workflow::find_resumable_child_run(&conn, "parent1", "child-wf").unwrap();
    assert!(result.is_none(), "completed child run must not be returned");
}

#[test]
fn test_find_resumable_child_run_ignores_running() {
    let conn = setup_db();
    insert_workflow_run(&conn, "parent1", "parent-wf", "running", None);
    insert_workflow_run(&conn, "child1", "child-wf", "running", Some("parent1"));
    let result = crate::workflow::find_resumable_child_run(&conn, "parent1", "child-wf").unwrap();
    assert!(result.is_none(), "running child run must not be returned");
}

#[test]
fn test_find_resumable_child_run_ignores_cancelled() {
    let conn = setup_db();
    insert_workflow_run(&conn, "parent1", "parent-wf", "failed", None);
    insert_workflow_run(&conn, "child1", "child-wf", "cancelled", Some("parent1"));
    let result = crate::workflow::find_resumable_child_run(&conn, "parent1", "child-wf").unwrap();
    assert!(result.is_none(), "cancelled child run must not be returned");
}

#[test]
fn test_find_resumable_child_run_picks_most_recent() {
    let conn = setup_db();
    insert_workflow_run(&conn, "parent1", "parent-wf", "failed", None);

    // Insert two failed child runs with distinct timestamps
    let agent_mgr = AgentManager::new(&conn);
    let p1 = agent_mgr.create_run(None, "workflow", None).unwrap();
    let p2 = agent_mgr.create_run(None, "workflow", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at, \
          parent_workflow_run_id) \
         VALUES ('older-child', 'child-wf', NULL, :parent_run_id, 'failed', 0, 'manual', \
                 '2025-01-01T00:00:00Z', 'parent1')",
        named_params! { ":parent_run_id": p1.id },
    )
    .unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at, \
          parent_workflow_run_id) \
         VALUES ('newer-child', 'child-wf', NULL, :parent_run_id, 'failed', 0, 'manual', \
                 '2025-06-01T00:00:00Z', 'parent1')",
        named_params! { ":parent_run_id": p2.id },
    )
    .unwrap();
    let result = crate::workflow::find_resumable_child_run(&conn, "parent1", "child-wf").unwrap();
    assert!(result.is_some());
    assert_eq!(
        result.unwrap().id,
        "newer-child",
        "most recently started child must be returned"
    );
}

#[test]
fn test_reap_orphaned_workflow_runs_dead_parent() {
    let conn = setup_db();
    let run_id = "run-dead-parent";
    let step_id = insert_waiting_run_with_gate(&conn, run_id, "failed", Some("86400s"), None);
    let reaped = crate::workflow::reap_orphaned_workflow_runs(&conn).unwrap();
    assert_eq!(reaped, 1);

    // Run should be cancelled.
    assert_eq!(get_run_status(&conn, run_id), "cancelled");

    // Gate step should be timed_out.
    assert_eq!(get_step_status(&conn, &step_id), "timed_out");
}

#[test]
fn test_reap_orphaned_workflow_runs_gate_timeout_elapsed() {
    let conn = setup_db();
    let run_id = "run-gate-timeout";
    // Parent is still running but gate started long ago with a 1s timeout.
    insert_waiting_run_with_gate(
        &conn,
        run_id,
        "running",
        Some("1s"),
        Some("2020-01-01T00:00:00Z"), // well in the past
    );
    let reaped = crate::workflow::reap_orphaned_workflow_runs(&conn).unwrap();
    assert_eq!(reaped, 1);

    assert_eq!(get_run_status(&conn, run_id), "cancelled");
}

#[test]
fn test_reap_orphaned_workflow_runs_skips_active_parent() {
    let conn = setup_db();
    let run_id = "run-active-parent";
    // Parent is running, gate timeout is huge — not orphaned.
    // Use a future started_at to ensure the timeout check also passes.
    insert_waiting_run_with_gate(
        &conn,
        run_id,
        "running",
        Some("999999999s"),
        Some("2099-01-01T00:00:00Z"),
    );
    let reaped = crate::workflow::reap_orphaned_workflow_runs(&conn).unwrap();
    assert_eq!(reaped, 0);

    assert_eq!(
        get_run_status(&conn, run_id),
        "waiting",
        "run must remain waiting"
    );
}

#[test]
fn test_reap_orphaned_workflow_runs_skips_terminal() {
    let conn = setup_db();
    // Insert a completed run — must not be touched.
    insert_workflow_run(&conn, "run-completed", "test-wf", "completed", None);
    // Insert a cancelled run — must not be touched.
    insert_workflow_run(&conn, "run-cancelled", "test-wf", "cancelled", None);
    let reaped = crate::workflow::reap_orphaned_workflow_runs(&conn).unwrap();
    assert_eq!(reaped, 0);
}

#[test]
fn test_reap_orphaned_workflow_runs_purged_parent() {
    // A workflow run whose parent agent run no longer exists in the DB
    // must still be reaped (parent_status == None → treat as dead).
    // We insert the workflow run with FK checks disabled so we can
    // reference a non-existent agent_run ID, simulating a purged parent.
    let conn = setup_db();
    let run_id = "run-purged-parent";
    let ghost_parent_id = "ghost-agent-run-does-not-exist";

    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES (:run_id, 'test-wf', NULL, :ghost_parent_id, 'waiting', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        named_params! { ":run_id": run_id, ":ghost_parent_id": ghost_parent_id },
    )
    .unwrap();

    let step_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          gate_type, gate_timeout, started_at) \
         VALUES (:step_id, :run_id, 'approval-gate', 'gate', 0, 'waiting', 1, \
                 'human_approval', '999999999s', '2099-01-01T00:00:00Z')",
        named_params! { ":step_id": step_id, ":run_id": run_id },
    )
    .unwrap();

    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    let reaped = crate::workflow::reap_orphaned_workflow_runs(&conn).unwrap();
    assert_eq!(
        reaped, 1,
        "purged parent should cause the workflow run to be reaped"
    );

    assert_eq!(get_run_status(&conn, run_id), "cancelled");
}

#[test]
fn test_reap_orphaned_workflow_runs_multiple_dead_parents() {
    // 3 waiting runs with dead (failed) parents + 1 with an active parent.
    // Only the 3 dead-parent runs should be reaped.
    let conn = setup_db();

    insert_waiting_run_with_gate(&conn, "run-dead-1", "failed", Some("86400s"), None);
    insert_waiting_run_with_gate(&conn, "run-dead-2", "failed", Some("86400s"), None);
    insert_waiting_run_with_gate(&conn, "run-dead-3", "cancelled", Some("86400s"), None);
    insert_waiting_run_with_gate(
        &conn,
        "run-active",
        "running",
        Some("999999999s"),
        Some("2099-01-01T00:00:00Z"),
    );
    let reaped = crate::workflow::reap_orphaned_workflow_runs(&conn).unwrap();
    assert_eq!(reaped, 3, "exactly the 3 dead-parent runs should be reaped");

    for dead_id in &["run-dead-1", "run-dead-2", "run-dead-3"] {
        assert_eq!(
            get_run_status(&conn, dead_id),
            "cancelled",
            "{dead_id} should be cancelled"
        );
    }

    assert_eq!(
        get_run_status(&conn, "run-active"),
        "waiting",
        "active-parent run must remain waiting"
    );
}

// ---------------------------------------------------------------------------
// reap_orphaned_script_steps tests
// ---------------------------------------------------------------------------

/// Helper: insert a script-role step in 'running' status with a specific subprocess_pid.
/// Returns the step_id.
fn insert_running_script_step_with_pid(
    conn: &Connection,
    run_id: &str,
    step_name: &str,
    pid: Option<i64>,
    started_at: Option<&str>,
) -> String {
    let step_id = crate::new_id();
    let started = started_at.unwrap_or("2025-01-01T00:00:00Z");
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          subprocess_pid, started_at) \
         VALUES (:step_id, :run_id, :step_name, 'script', 0, 'running', 0, :pid, :started)",
        named_params! { ":step_id": step_id, ":run_id": run_id, ":step_name": step_name, ":pid": pid, ":started": started },
    )
    .unwrap();
    step_id
}

/// Helper: create a workflow_run and return its id.
fn make_workflow_run_id(conn: &Connection) -> String {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let run = crate::workflow::create_workflow_run(
        conn,
        "test-wf",
        Some("w1"),
        &parent.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    run.id
}

/// A step with a dead PID (subprocess has exited) must be reaped.
#[cfg(unix)]
#[test]
fn test_reap_orphaned_script_steps_dead_pid() {
    let conn = setup_db();

    // Spawn a short-lived process and wait for it to exit.
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = child.id();
    child.wait().unwrap();
    // Brief pause so the OS fully reaps the child.
    std::thread::sleep(std::time::Duration::from_millis(50));

    let run_id = make_workflow_run_id(&conn);
    let step_id = insert_running_script_step_with_pid(
        &conn,
        &run_id,
        "script-step",
        Some(dead_pid as i64),
        None,
    );
    let reaped = crate::workflow::reap_orphaned_script_steps(&conn).unwrap();
    assert_eq!(reaped, 1);

    assert_eq!(get_step_status(&conn, &step_id), "failed");

    let result: String = conn
        .query_row(
            "SELECT result_text FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id },
            |r| r.get("result_text"),
        )
        .unwrap();
    assert!(
        result.contains("subprocess lost"),
        "result_text should mention subprocess lost; got: {result}"
    );
}

/// A step with NULL subprocess_pid must NOT be reaped.
#[test]
fn test_reap_orphaned_script_steps_no_pid() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);
    insert_running_script_step_with_pid(&conn, &run_id, "script-step", None, None);
    let reaped = crate::workflow::reap_orphaned_script_steps(&conn).unwrap();
    assert_eq!(reaped, 0);
}

/// A completed script step must NOT be reaped even if subprocess_pid is set.
#[test]
fn test_reap_orphaned_script_steps_skips_completed() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    // Insert a completed step with a bogus PID.
    let step_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, subprocess_pid) \
         VALUES (:step_id, :run_id, 'script-done', 'script', 0, 'completed', 0, 99999)",
        named_params! { ":step_id": step_id, ":run_id": run_id },
    )
    .unwrap();
    let reaped = crate::workflow::reap_orphaned_script_steps(&conn).unwrap();
    assert_eq!(reaped, 0);

    assert_eq!(get_step_status(&conn, &step_id), "completed");
}

/// A running step with child_run_id set (agent step) must NOT be reaped.
#[test]
fn test_reap_orphaned_script_steps_skips_agent_step() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    // Insert an actor step with child_run_id set — simulates an agent step.
    let step_id = crate::new_id();
    let agent_mgr = AgentManager::new(&conn);
    let child_run = agent_mgr.create_run(Some("w1"), "agent", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          child_run_id, subprocess_pid) \
         VALUES (:step_id, :run_id, 'agent-step', 'actor', 0, 'running', 0, :child_run_id, 99999)",
        named_params! { ":step_id": step_id, ":run_id": run_id, ":child_run_id": child_run.id },
    )
    .unwrap();
    let reaped = crate::workflow::reap_orphaned_script_steps(&conn).unwrap();
    assert_eq!(reaped, 0);
}

/// Multiple orphaned script steps with dead PIDs must all be reaped.
#[cfg(unix)]
#[test]
fn test_reap_orphaned_script_steps_multiple() {
    let conn = setup_db();

    // Spawn and wait for two short-lived children.
    let mut c1 = std::process::Command::new("true").spawn().unwrap();
    let pid1 = c1.id();
    c1.wait().unwrap();

    let mut c2 = std::process::Command::new("true").spawn().unwrap();
    let pid2 = c2.id();
    c2.wait().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(50));

    let run_id = make_workflow_run_id(&conn);
    let s1 = insert_running_script_step_with_pid(&conn, &run_id, "step-1", Some(pid1 as i64), None);
    let s2 = insert_running_script_step_with_pid(&conn, &run_id, "step-2", Some(pid2 as i64), None);

    // A live step (current process PID) — must NOT be reaped.
    // Use the OS-reported process start time so pid_was_recycled returns false.
    let live_pid = std::process::id();
    #[cfg(target_os = "macos")]
    let live_started_at = crate::process_utils::process_started_at(live_pid)
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
    #[cfg(not(target_os = "macos"))]
    let live_started_at: Option<String> = Some(chrono::Utc::now().to_rfc3339());
    let s3 = insert_running_script_step_with_pid(
        &conn,
        &run_id,
        "step-3",
        Some(live_pid as i64),
        live_started_at.as_deref(),
    );
    let reaped = crate::workflow::reap_orphaned_script_steps(&conn).unwrap();
    assert_eq!(reaped, 2, "only the 2 dead-PID steps should be reaped");

    for dead_step in &[s1, s2] {
        assert_eq!(
            get_step_status(&conn, dead_step),
            "failed",
            "{dead_step} should be failed"
        );
    }

    assert_eq!(
        get_step_status(&conn, &s3),
        "running",
        "live step must remain running"
    );
}

#[test]
fn test_list_workflow_runs_paginated_limit_and_offset() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    // Create 5 runs for worktree w1
    for i in 0..5 {
        let p = agent_mgr
            .create_run(Some("w1"), &format!("wf-paginated-{i}"), None)
            .unwrap();
        crate::workflow::create_workflow_run(
            &conn,
            &format!("paginated-flow-{i}"),
            Some("w1"),
            &p.id,
            false,
            "manual",
            None,
        )
        .unwrap();
    }

    // First page: limit=2, offset=0
    let page1 = crate::workflow::list_workflow_runs_paginated(&conn, "w1", 2, 0).unwrap();
    assert_eq!(page1.len(), 2);

    // Second page: limit=2, offset=2
    let page2 = crate::workflow::list_workflow_runs_paginated(&conn, "w1", 2, 2).unwrap();
    assert_eq!(page2.len(), 2);

    // Third page: limit=2, offset=4 — only 1 remaining
    let page3 = crate::workflow::list_workflow_runs_paginated(&conn, "w1", 2, 4).unwrap();
    assert_eq!(page3.len(), 1);

    // Pages must not overlap
    let ids1: Vec<_> = page1.iter().map(|r| r.id.clone()).collect();
    let ids2: Vec<_> = page2.iter().map(|r| r.id.clone()).collect();
    assert!(
        ids1.iter().all(|id| !ids2.contains(id)),
        "page1 and page2 must not share runs"
    );

    // All 5 runs returned when limit exceeds count
    let all = crate::workflow::list_workflow_runs_paginated(&conn, "w1", 100, 0).unwrap();
    assert_eq!(all.len(), 5);
}

#[test]
fn test_list_workflow_runs_paginated_filters_by_worktree() {
    let conn = setup_db();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let agent_mgr = AgentManager::new(&conn);

    let p1 = agent_mgr.create_run(Some("w1"), "wf-w1", None).unwrap();
    let p2 = agent_mgr.create_run(Some("w2"), "wf-w2", None).unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "run-w1",
        Some("w1"),
        &p1.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    crate::workflow::create_workflow_run(
        &conn,
        "run-w2",
        Some("w2"),
        &p2.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    let w1_runs = crate::workflow::list_workflow_runs_paginated(&conn, "w1", 100, 0).unwrap();
    assert_eq!(w1_runs.len(), 1);
    assert_eq!(w1_runs[0].workflow_name, "run-w1");

    let w2_runs = crate::workflow::list_workflow_runs_paginated(&conn, "w2", 100, 0).unwrap();
    assert_eq!(w2_runs.len(), 1);
    assert_eq!(w2_runs[0].workflow_name, "run-w2");
}

#[test]
fn test_list_workflow_runs_by_repo_id_offset_pagination() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    // Create 4 runs for repo r1 (all on active worktree w1)
    for i in 0..4 {
        let p = agent_mgr
            .create_run(Some("w1"), &format!("wf-repo-{i}"), None)
            .unwrap();
        crate::workflow::create_workflow_run_with_targets(
            &conn,
            &format!("repo-flow-{i}"),
            Some("w1"),
            None,
            Some("r1"),
            &p.id,
            false,
            "manual",
            None,
            None,
            None,
        )
        .unwrap();
    }

    // First page
    let page1 = crate::workflow::list_workflow_runs_by_repo_id(&conn, "r1", 2, 0).unwrap();
    assert_eq!(page1.len(), 2);

    // Second page
    let page2 = crate::workflow::list_workflow_runs_by_repo_id(&conn, "r1", 2, 2).unwrap();
    assert_eq!(page2.len(), 2);

    // Pages must not overlap
    let ids1: Vec<_> = page1.iter().map(|r| r.id.clone()).collect();
    let ids2: Vec<_> = page2.iter().map(|r| r.id.clone()).collect();
    assert!(
        ids1.iter().all(|id| !ids2.contains(id)),
        "page1 and page2 must not share runs"
    );

    // Beyond end returns empty
    let beyond = crate::workflow::list_workflow_runs_by_repo_id(&conn, "r1", 2, 10).unwrap();
    assert!(beyond.is_empty());
}

#[test]
fn test_list_root_workflow_runs_excludes_children() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));
    let roots = crate::workflow::list_root_workflow_runs(&conn, 100).unwrap();
    let ids: Vec<&str> = roots.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&"root1"), "root run should appear");
    assert!(!ids.contains(&"child1"), "child run must not appear");
}

#[test]
fn test_list_root_workflow_runs_empty() {
    let conn = setup_db();
    let roots = crate::workflow::list_root_workflow_runs(&conn, 100).unwrap();
    assert!(roots.is_empty());
}

#[test]
fn test_get_active_chain_no_children() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    let chain = crate::workflow::get_active_chain_for_run(&conn, "root1").unwrap();
    assert!(chain.is_empty(), "no children → empty chain");
}

#[test]
fn test_get_active_chain_single_child() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));
    let chain = crate::workflow::get_active_chain_for_run(&conn, "root1").unwrap();
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0].0, "child1");
    assert_eq!(chain[0].1, "child-wf");
}

#[test]
fn test_get_active_chain_two_deep() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));
    insert_workflow_run(&conn, "grand1", "grand-wf", "running", Some("child1"));
    let chain = crate::workflow::get_active_chain_for_run(&conn, "root1").unwrap();
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0], ("child1".to_string(), "child-wf".to_string()));
    assert_eq!(chain[1], ("grand1".to_string(), "grand-wf".to_string()));
}

#[test]
fn test_get_active_chain_ignores_terminal_children() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    // completed child — must not appear in active chain
    insert_workflow_run(&conn, "child1", "child-wf", "completed", Some("root1"));
    let chain = crate::workflow::get_active_chain_for_run(&conn, "root1").unwrap();
    assert!(chain.is_empty(), "completed child must not appear in chain");
}

#[test]
fn test_get_step_summaries_no_children() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    insert_running_step(&conn, "step1", "root1", "my-step");
    let summaries = crate::workflow::get_step_summaries_for_runs(&conn, &["root1"]).unwrap();
    let s = summaries.get("root1").expect("summary should exist");
    assert_eq!(s.step_name, "my-step");
    assert_eq!(s.iteration, 1);
    // single-level: chain is empty
    assert!(s.workflow_chain.is_empty());
}

#[test]
fn test_get_step_summaries_with_child_chain() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));
    // running step is on the child (leaf)
    insert_running_step(&conn, "step1", "child1", "leaf-step");
    let summaries = crate::workflow::get_step_summaries_for_runs(&conn, &["root1"]).unwrap();
    let s = summaries.get("root1").expect("summary should exist");
    assert_eq!(s.step_name, "leaf-step");
    // workflow_chain is [root_name] because child is the leaf (excluded)
    assert_eq!(s.workflow_chain, vec!["root-wf"]);
}

#[test]
fn test_get_step_summaries_two_deep_chain() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));
    insert_workflow_run(&conn, "grand1", "grand-wf", "running", Some("child1"));
    insert_running_step(&conn, "step1", "grand1", "grand-step");
    let summaries = crate::workflow::get_step_summaries_for_runs(&conn, &["root1"]).unwrap();
    let s = summaries.get("root1").expect("summary should exist");
    assert_eq!(s.step_name, "grand-step");
    // root + first child (grand is leaf, excluded)
    assert_eq!(s.workflow_chain, vec!["root-wf", "child-wf"]);
}

#[test]
fn test_get_step_summaries_empty_run_ids() {
    let conn = setup_db();
    let summaries = crate::workflow::get_step_summaries_for_runs(&conn, &[]).unwrap();
    assert!(summaries.is_empty());
}

#[test]
fn test_get_step_summaries_no_running_step() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    // no steps inserted
    let summaries = crate::workflow::get_step_summaries_for_runs(&conn, &["root1"]).unwrap();
    assert!(
        !summaries.contains_key("root1"),
        "no running step → no entry in map"
    );
}

#[test]
fn test_resolve_run_context_run_not_found() {
    let conn = setup_db();
    let config = crate::config::Config::default();
    let err = crate::workflow::resolve_run_context(&conn, "nonexistent-id", &config).unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "expected 'not found' error, got: {err}"
    );
}

#[test]
fn test_resolve_run_context_worktree_path_exists() {
    let conn = setup_db();
    let config = crate::config::Config::default();

    // Create a real temp directory so the disk-existence guard passes.
    let tmp = std::env::temp_dir().join("conductor_test_wt_path_exists");
    std::fs::create_dir_all(&tmp).unwrap();
    let wt_path = tmp.to_string_lossy().to_string();

    // Insert a worktree pointing at the real temp dir.
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt-exists', 'r1', 'test-wt', 'feat/test', :wt_path, 'active', '2024-01-01T00:00:00Z')",
        named_params! { ":wt_path": wt_path },
    )
    .unwrap();

    let run_id = insert_workflow_run_with_targets(&conn, Some("wt-exists"), None);
    let ctx = crate::workflow::resolve_run_context(&conn, &run_id, &config).unwrap();

    assert_eq!(ctx.working_dir, wt_path);
    assert_eq!(ctx.repo_path, "/tmp/repo"); // repo r1 from setup_db
    assert_eq!(ctx.worktree_id.as_deref(), Some("wt-exists"));
    assert_eq!(ctx.repo_id.as_deref(), Some("r1"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_resolve_run_context_worktree_path_missing() {
    let conn = setup_db();
    let config = crate::config::Config::default();

    // setup_db inserts worktree w1 at /tmp/ws/feat-test which does not exist.
    // Verify the guard rejects it.
    let run_id = insert_workflow_run_with_targets(&conn, Some("w1"), None);
    let err = crate::workflow::resolve_run_context(&conn, &run_id, &config).unwrap_err();
    assert!(
        err.to_string().contains("no longer exists on disk"),
        "expected disk-existence error, got: {err}"
    );
}

#[test]
fn test_resolve_run_context_repo_only() {
    let conn = setup_db();
    let config = crate::config::Config::default();

    // Run with only repo_id (no worktree).
    let run_id = insert_workflow_run_with_targets(&conn, None, Some("r1"));
    let ctx = crate::workflow::resolve_run_context(&conn, &run_id, &config).unwrap();

    assert_eq!(ctx.working_dir, "/tmp/repo");
    assert_eq!(ctx.repo_path, "/tmp/repo");
    assert!(ctx.worktree_id.is_none());
    assert_eq!(ctx.repo_id.as_deref(), Some("r1"));
}

#[test]
fn test_resolve_run_context_no_worktree_no_repo() {
    let conn = setup_db();
    let config = crate::config::Config::default();

    // Run with neither worktree nor repo.
    let run_id = insert_workflow_run_with_targets(&conn, None, None);
    let err = crate::workflow::resolve_run_context(&conn, &run_id, &config).unwrap_err();
    assert!(
        err.to_string()
            .contains("has no associated worktree or repo"),
        "expected missing-targets error, got: {err}"
    );
}

#[test]
fn test_set_waiting_blocked_on_atomically_sets_status_and_blocked_on() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    // Start from Running
    crate::workflow::update_workflow_status(&conn, &run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let blocked = BlockedOn::HumanApproval {
        gate_name: "deploy-gate".to_string(),
        prompt: Some("Approve deploy?".to_string()),
        options: vec![],
    };

    crate::workflow::set_waiting_blocked_on(&conn, &run.id, &blocked).unwrap();

    let updated = crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .unwrap();
    assert_eq!(updated.status, WorkflowRunStatus::Waiting);
    assert!(updated.blocked_on.is_some());
    match updated.blocked_on.unwrap() {
        BlockedOn::HumanApproval {
            gate_name, prompt, ..
        } => {
            assert_eq!(gate_name, "deploy-gate");
            assert_eq!(prompt.as_deref(), Some("Approve deploy?"));
        }
        other => panic!("expected HumanApproval, got {other:?}"),
    }
}

#[test]
fn test_blocked_on_cleared_when_transitioning_away_from_waiting() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    // Set waiting with blocked_on
    let blocked = BlockedOn::PrChecks {
        gate_name: "ci-gate".to_string(),
    };
    crate::workflow::set_waiting_blocked_on(&conn, &run.id, &blocked).unwrap();

    let waiting = crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .unwrap();
    assert_eq!(waiting.status, WorkflowRunStatus::Waiting);
    assert!(waiting.blocked_on.is_some());

    // Transition to Running — blocked_on must be auto-cleared
    crate::workflow::update_workflow_status(&conn, &run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let running = crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .unwrap();
    assert_eq!(running.status, WorkflowRunStatus::Running);
    assert!(
        running.blocked_on.is_none(),
        "blocked_on should be cleared when leaving Waiting"
    );
}

#[test]
fn test_malformed_blocked_on_json_is_silently_dropped() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    // Directly inject malformed JSON into the blocked_on column
    conn.execute(
        "UPDATE workflow_runs SET blocked_on = :blocked_on WHERE id = :id",
        named_params! { ":blocked_on": "not-valid-json{{{", ":id": run.id },
    )
    .unwrap();

    // Reading the run should succeed with blocked_on = None
    let loaded = crate::workflow::get_workflow_run(&conn, &run.id)
        .unwrap()
        .unwrap();
    assert!(
        loaded.blocked_on.is_none(),
        "malformed blocked_on should deserialize as None"
    );
}

#[test]
fn test_update_workflow_status_rejects_waiting() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    // Calling update_workflow_status with Waiting must return an error — callers
    // should use set_waiting_blocked_on() to enforce the blocked_on invariant.
    let err = crate::workflow::update_workflow_status(
        &conn,
        &run.id,
        WorkflowRunStatus::Waiting,
        None,
        None,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("set_waiting_blocked_on()"),
        "Expected InvalidInput error, got: {err}"
    );
}

#[test]
fn test_backfill_migration_sets_repo_id_on_historical_runs() {
    // setup_db() provides repo r1 and worktree w1 (repo_id=r1).
    let conn = setup_db();

    // Create a workflow run with worktree_id but NULL repo_id (historical data).
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at) \
         VALUES ('run-hist', 'test-wf', 'w1', :parent_run_id, 'completed', 0, 'manual', '2025-01-01T00:00:00Z')",
        named_params! { ":parent_run_id": parent.id },
    )
    .unwrap();

    // Verify repo_id is NULL before backfill.
    let repo_id_before: Option<String> = conn
        .query_row(
            "SELECT repo_id FROM workflow_runs WHERE id = 'run-hist'",
            [],
            |row| row.get("repo_id"),
        )
        .unwrap();
    assert!(
        repo_id_before.is_none(),
        "repo_id should be NULL before backfill"
    );

    // Run the backfill SQL.
    conn.execute_batch(include_str!(
        "../../db/migrations/048_backfill_workflow_run_repo_id.sql"
    ))
    .unwrap();

    // Verify repo_id is now set.
    let repo_id_after: Option<String> = conn
        .query_row(
            "SELECT repo_id FROM workflow_runs WHERE id = 'run-hist'",
            [],
            |row| row.get("repo_id"),
        )
        .unwrap();
    assert_eq!(repo_id_after.as_deref(), Some("r1"));
}

#[test]
fn test_backfill_migration_skips_runs_with_existing_repo_id() {
    // setup_db() provides repo r1 and worktree w1 (repo_id=r1).
    let conn = setup_db();

    // Add a second repo.
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
         VALUES ('r2', 'other-repo', '/tmp/repo2', 'https://github.com/test/repo2', '/tmp/ws2', '2025-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    // Create a run that already has repo_id set (to r2, different from worktree w1's r1).
    let run_id = insert_workflow_run_with_targets(&conn, Some("w1"), Some("r2"));

    // Run the backfill — should not overwrite the existing repo_id.
    conn.execute_batch(include_str!(
        "../../db/migrations/048_backfill_workflow_run_repo_id.sql"
    ))
    .unwrap();

    let repo_id: Option<String> = conn
        .query_row(
            "SELECT repo_id FROM workflow_runs WHERE id = :id",
            named_params! { ":id": run_id },
            |row| row.get("repo_id"),
        )
        .unwrap();
    assert_eq!(
        repo_id.as_deref(),
        Some("r2"),
        "existing repo_id should not be overwritten"
    );
}

#[test]
fn test_backfill_migration_leaves_null_when_worktree_deleted() {
    // The primary bug scenario: worktree row was already deleted from DB before
    // the migration runs, so the subquery cannot resolve repo_id.
    let conn = setup_db();

    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

    // Insert a run referencing worktree w1, then orphan it by pointing
    // worktree_id at a non-existent ID (simulating a deleted worktree row).
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at) \
         VALUES ('run-orphan', 'test-wf', 'w1', :parent_run_id, 'completed', 0, 'manual', '2025-01-01T00:00:00Z')",
        named_params! { ":parent_run_id": parent.id },
    )
    .unwrap();

    // Temporarily disable FK checks so we can orphan the worktree_id reference,
    // simulating the real-world scenario where the worktree row was deleted.
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    conn.execute(
        "UPDATE workflow_runs SET worktree_id = 'deleted-wt' WHERE id = 'run-orphan'",
        [],
    )
    .unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

    // Run the backfill — should leave repo_id as NULL since worktree row is gone.
    conn.execute_batch(include_str!(
        "../../db/migrations/048_backfill_workflow_run_repo_id.sql"
    ))
    .unwrap();

    let repo_id: Option<String> = conn
        .query_row(
            "SELECT repo_id FROM workflow_runs WHERE id = 'run-orphan'",
            [],
            |row| row.get("repo_id"),
        )
        .unwrap();
    assert!(
        repo_id.is_none(),
        "repo_id should remain NULL when worktree row is deleted"
    );
}

// ---------------------------------------------------------------------------
// set_step_output_file
// ---------------------------------------------------------------------------

#[test]
fn test_set_step_output_file() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "script-step", "actor", false, 0, 0).unwrap();

    crate::workflow::set_step_output_file(&conn, &step_id, "/tmp/output.txt").unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.output_file.as_deref(), Some("/tmp/output.txt"));
}

// ---------------------------------------------------------------------------
// set_step_gate_info
// ---------------------------------------------------------------------------

#[test]
fn test_set_step_gate_info_with_prompt() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "gate-step", "gate", false, 0, 0).unwrap();

    crate::workflow::set_step_gate_info(
        &conn,
        &step_id,
        GateType::PrApproval,
        Some("Need 2 approvals"),
        "24h",
    )
    .unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.gate_type, Some(GateType::PrApproval));
    assert_eq!(step.gate_prompt.as_deref(), Some("Need 2 approvals"));
    assert_eq!(step.gate_timeout.as_deref(), Some("24h"));
}

#[test]
fn test_set_step_gate_info_no_prompt() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "gate-step", "gate", false, 0, 0).unwrap();

    crate::workflow::set_step_gate_info(&conn, &step_id, GateType::PrChecks, None, "1h").unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.gate_type, Some(GateType::PrChecks));
    assert!(step.gate_prompt.is_none());
    assert_eq!(step.gate_timeout.as_deref(), Some("1h"));
}

// ---------------------------------------------------------------------------
// set_step_parallel_group
// ---------------------------------------------------------------------------

#[test]
fn test_set_step_parallel_group() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "parallel-step", "actor", false, 0, 0)
            .unwrap();

    crate::workflow::set_step_parallel_group(&conn, &step_id, "group-abc").unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .unwrap();
    assert_eq!(step.parallel_group_id.as_deref(), Some("group-abc"));
}

// ---------------------------------------------------------------------------
// get_steps_for_runs
// ---------------------------------------------------------------------------

#[test]
fn test_get_steps_for_runs_empty_ids() {
    let conn = setup_db();
    let result = crate::workflow::get_steps_for_runs(&conn, &[]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_get_steps_for_runs_multiple_runs() {
    let conn = setup_db();
    let (_p1, run1) = make_workflow_run(&conn);

    let agent_mgr = AgentManager::new(&conn);
    let p2 = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let run2 = crate::workflow::create_workflow_run(
        &conn,
        "wf2",
        Some("w1"),
        &p2.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    // Add steps to each run
    crate::workflow::insert_step(&conn, &run1.id, "s1", "actor", false, 0, 0).unwrap();
    crate::workflow::insert_step(&conn, &run1.id, "s2", "actor", false, 1, 0).unwrap();
    crate::workflow::insert_step(&conn, &run2.id, "s3", "actor", false, 0, 0).unwrap();

    let result = crate::workflow::get_steps_for_runs(&conn, &[&run1.id, &run2.id]).unwrap();
    assert_eq!(result.get(&run1.id).unwrap().len(), 2);
    assert_eq!(result.get(&run2.id).unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// get_active_steps_for_runs
// ---------------------------------------------------------------------------

#[test]
fn test_get_active_steps_for_runs_filters_by_status() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    let s1 = crate::workflow::insert_step(&conn, &run.id, "completed-step", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&conn, &s1, WorkflowStepStatus::Completed);

    let s2 =
        crate::workflow::insert_step(&conn, &run.id, "running-step", "actor", false, 1, 0).unwrap();
    set_step_status(&conn, &s2, WorkflowStepStatus::Running);

    let s3 =
        crate::workflow::insert_step(&conn, &run.id, "waiting-step", "gate", false, 2, 0).unwrap();
    set_step_status(&conn, &s3, WorkflowStepStatus::Waiting);

    let s4 =
        crate::workflow::insert_step(&conn, &run.id, "failed-step", "actor", false, 3, 0).unwrap();
    set_step_status(&conn, &s4, WorkflowStepStatus::Failed);

    let result = crate::workflow::get_active_steps_for_runs(&conn, &[&run.id]).unwrap();
    let steps = result.get(&run.id).unwrap();
    // Only running and waiting should be returned
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].step_name, "running-step");
    assert_eq!(steps[1].step_name, "waiting-step");
}

#[test]
fn test_get_active_steps_for_runs_empty_ids() {
    let conn = setup_db();
    let result = crate::workflow::get_active_steps_for_runs(&conn, &[]).unwrap();
    assert!(result.is_empty());
}

// ---------------------------------------------------------------------------
// detect_stuck_workflow_run_ids — detection logic tests
// ---------------------------------------------------------------------------
/// Insert a workflow run in 'running' status with no parent_workflow_run_id.
fn insert_running_root_run(conn: &Connection, run_id: &str) {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES (:run_id, 'test-wf', NULL, :parent_run_id, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        named_params! { ":run_id": run_id, ":parent_run_id": parent.id },
    )
    .unwrap();
}

/// Insert a non-terminal step (pending/running/waiting) with no ended_at.
fn insert_non_terminal_step(conn: &Connection, step_id: &str, run_id: &str, status: &str) {
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration) \
         VALUES (:step_id, :run_id, 'step-a', 'actor', 0, :status, 0)",
        named_params! { ":step_id": step_id, ":run_id": run_id, ":status": status },
    )
    .unwrap();
}

#[test]
#[allow(deprecated)]
fn test_reap_stuck_workflow_runs_detects_stale_run() {
    let conn = setup_db();
    insert_running_root_run(&conn, "stuck-run");
    // Step completed with an old ended_at — well past any reasonable threshold.
    insert_terminal_step_with_id(
        &conn,
        "s1",
        "stuck-run",
        "completed",
        "2020-01-01T00:00:00Z",
    );
    // threshold_secs = 60: elapsed >> 60 → detected
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 60).unwrap();
    assert_eq!(ids.len(), 1, "stale run should be detected");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_fresh_run() {
    let conn = setup_db();
    insert_running_root_run(&conn, "fresh-run");
    // Update heartbeat to now so the run appears fresh.
    conn.execute(
        "UPDATE workflow_runs SET last_heartbeat = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
         WHERE id = 'fresh-run'",
        [],
    )
    .unwrap();
    // Step completed just now — store ended_at as the current UTC time.
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, ended_at) \
         VALUES ('s1', 'fresh-run', 'step-a', 'actor', 0, 'completed', 0, \
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
        [],
    )
    .unwrap();
    // Very large threshold — a run with recent heartbeat should not be detected.
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 999_999).unwrap();
    assert_eq!(ids.len(), 0, "fresh run must not be detected");
}

#[test]
fn test_detect_stuck_workflow_run_ids_detects_stale_heartbeat() {
    let conn = setup_db();
    insert_running_root_run(&conn, "stale-heartbeat-run");
    insert_terminal_step_with_id(
        &conn,
        "s1",
        "stale-heartbeat-run",
        "completed",
        "2020-01-01T00:00:00Z",
    );
    // Set heartbeat to 200 seconds ago — stale with threshold=60.
    let stale_time = chrono::Utc::now() - chrono::Duration::seconds(200);
    let stale_str = stale_time.to_rfc3339();
    conn.execute(
        "UPDATE workflow_runs SET last_heartbeat = :ts WHERE id = 'stale-heartbeat-run'",
        named_params! { ":ts": stale_str },
    )
    .unwrap();
    // threshold_secs = 60: heartbeat 200s ago >> 60 → detected
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 60).unwrap();
    assert_eq!(ids.len(), 1, "stale heartbeat run should be detected");
    assert_eq!(ids[0], "stale-heartbeat-run");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_pending_step() {
    let conn = setup_db();
    insert_running_root_run(&conn, "pending-run");
    insert_non_terminal_step(&conn, "s1", "pending-run", "pending");
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 0).unwrap();
    assert_eq!(ids.len(), 0, "run with pending step must not be detected");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_running_step() {
    let conn = setup_db();
    insert_running_root_run(&conn, "running-step-run");
    insert_non_terminal_step(&conn, "s1", "running-step-run", "running");
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 0).unwrap();
    assert_eq!(ids.len(), 0, "run with running step must not be detected");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_waiting_step() {
    let conn = setup_db();
    insert_running_root_run(&conn, "waiting-step-run");
    insert_non_terminal_step(&conn, "s1", "waiting-step-run", "waiting");
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 0).unwrap();
    assert_eq!(ids.len(), 0, "run with waiting step must not be detected");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_sub_workflow() {
    let conn = setup_db();
    // Insert a root run with a running step so it is NOT detected as stuck.
    insert_running_root_run(&conn, "root-run");
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, started_at) \
         VALUES ('root-step', 'root-run', 'step-a', 'actor', 0, 'running', 0, \
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
        [],
    )
    .unwrap();
    // Insert a sub-workflow with parent_workflow_run_id set.
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES ('sub-run', 'child-wf', NULL, :parent_run_id, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', 'root-run')",
        named_params! { ":parent_run_id": parent.id },
    )
    .unwrap();
    insert_terminal_step_with_id(&conn, "s1", "sub-run", "completed", "2020-01-01T00:00:00Z");
    // Sub-workflows (parent_workflow_run_id IS NOT NULL) are excluded from
    // stuck detection — only root runs are checked.
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 0).unwrap();
    assert_eq!(ids.len(), 0, "sub-workflow must not be detected as stuck");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_non_running_status() {
    let conn = setup_db();
    insert_workflow_run(&conn, "completed-run", "test-wf", "completed", None);
    insert_workflow_run(&conn, "failed-run", "test-wf", "failed", None);
    insert_workflow_run(&conn, "waiting-run", "test-wf", "waiting", None);
    insert_terminal_step_with_id(
        &conn,
        "s1",
        "completed-run",
        "completed",
        "2020-01-01T00:00:00Z",
    );
    insert_terminal_step_with_id(
        &conn,
        "s2",
        "failed-run",
        "completed",
        "2020-01-01T00:00:00Z",
    );
    insert_terminal_step_with_id(
        &conn,
        "s3",
        "waiting-run",
        "completed",
        "2020-01-01T00:00:00Z",
    );
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 0).unwrap();
    assert_eq!(ids.len(), 0, "non-running status runs must not be detected");
}

#[test]
fn test_reap_stuck_workflow_runs_detects_zero_step_runs() {
    let conn = setup_db();
    insert_running_root_run(&conn, "no-steps-run");
    // No steps inserted — the executor may have died before creating any steps.
    // detect_stuck_workflow_run_ids now matches reap_heartbeat_stuck_runs behavior:
    // zero-step runs ARE detected as stuck.
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 0).unwrap();
    assert_eq!(
        ids.len(),
        1,
        "run with no steps should be detected as stuck"
    );
    assert_eq!(ids[0], "no-steps-run");
}

#[test]
fn test_reap_stuck_workflow_runs_multiple_stuck_runs() {
    let conn = setup_db();
    insert_running_root_run(&conn, "stuck-1");
    insert_running_root_run(&conn, "stuck-2");
    insert_running_root_run(&conn, "stuck-3");
    insert_terminal_step_with_id(&conn, "s1", "stuck-1", "completed", "2020-01-01T00:00:00Z");
    insert_terminal_step_with_id(&conn, "s2", "stuck-2", "failed", "2020-01-01T00:00:00Z");
    insert_terminal_step_with_id(&conn, "s3", "stuck-3", "completed", "2020-01-01T00:00:00Z");
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 60).unwrap();
    assert_eq!(ids.len(), 3, "all 3 stuck runs should be detected");
}

// ---------------------------------------------------------------------------
// detect_stale_workflow_runs — stale watchdog tests
// ---------------------------------------------------------------------------

/// Insert a running root run with target_label for stale tests.
fn insert_running_root_run_with_label(
    conn: &Connection,
    run_id: &str,
    workflow_name: &str,
    target_label: Option<&str>,
) {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id, target_label) \
         VALUES (:run_id, :workflow_name, NULL, :parent_run_id, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL, :target_label)",
        named_params! { ":run_id": run_id, ":workflow_name": workflow_name, ":parent_run_id": parent.id, ":target_label": target_label },
    )
    .unwrap();
}

/// Insert a step in 'running' status with a specific started_at.
fn insert_running_step_with_started_at(
    conn: &Connection,
    step_id: &str,
    run_id: &str,
    step_name: &str,
    started_at: &str,
) {
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, started_at) \
         VALUES (:step_id, :run_id, :step_name, 'actor', 0, 'running', 0, :started_at)",
        named_params! { ":step_id": step_id, ":run_id": run_id, ":step_name": step_name, ":started_at": started_at },
    )
    .unwrap();
}

#[test]
fn test_detect_stale_workflow_runs_finds_old_running_step() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "stale-run", "deploy", Some("repo/wt"));
    // Step started 2 hours ago
    insert_running_step_with_started_at(
        &conn,
        "s1",
        "stale-run",
        "code-review",
        "2020-01-01T00:00:00Z",
    );
    let stale = crate::workflow::detect_stale_workflow_runs(&conn, 60).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].run_id, "stale-run");
    assert_eq!(stale[0].workflow_name, "deploy");
    assert_eq!(stale[0].target_label.as_deref(), Some("repo/wt"));
    assert_eq!(stale[0].step_name, "code-review");
    assert!(stale[0].running_minutes > 60);
}

#[test]
fn test_detect_stale_workflow_runs_skips_fresh_step() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "fresh-run", "deploy", None);
    // Step started just now — use SQL now() to set started_at.
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, started_at) \
         VALUES ('s1', 'fresh-run', 'code-review', 'actor', 0, 'running', 0, \
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
        [],
    )
    .unwrap();
    let stale = crate::workflow::detect_stale_workflow_runs(&conn, 60).unwrap();
    assert!(stale.is_empty(), "fresh running step should not be stale");
}

#[test]
fn test_detect_stale_workflow_runs_skips_completed_step() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "done-run", "deploy", None);
    // Step is completed, not running — should not be detected.
    insert_terminal_step(&conn, "done-run", WorkflowStepStatus::Completed, 0);
    let stale = crate::workflow::detect_stale_workflow_runs(&conn, 60).unwrap();
    assert!(stale.is_empty(), "completed step should not trigger stale");
}

#[test]
fn test_detect_stale_workflow_runs_skips_sub_workflows() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "root-run", "parent-wf", None);
    // Insert a sub-workflow with old running step.
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES ('sub-run', 'child-wf', NULL, :parent_run_id, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', 'root-run')",
        named_params! { ":parent_run_id": parent.id },
    )
    .unwrap();
    insert_running_step_with_started_at(&conn, "s1", "sub-run", "step-a", "2020-01-01T00:00:00Z");
    let stale = crate::workflow::detect_stale_workflow_runs(&conn, 60).unwrap();
    assert!(
        stale.is_empty(),
        "sub-workflow steps should not trigger stale"
    );
}

#[test]
fn test_detect_stale_workflow_runs_disabled_when_zero() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "stale-run", "deploy", None);
    insert_running_step_with_started_at(&conn, "s1", "stale-run", "step-a", "2020-01-01T00:00:00Z");
    let stale = crate::workflow::detect_stale_workflow_runs(&conn, 0).unwrap();
    assert!(stale.is_empty(), "threshold 0 should disable detection");
}

/// Insert a child workflow_run row with a given status and parent_workflow_run_id.
fn insert_child_workflow_run(
    conn: &Connection,
    child_run_id: &str,
    parent_workflow_run_id: &str,
    status: &str,
) {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES (:child_run_id, 'child-wf', NULL, :parent_run_id, :status, 0, 'manual', \
                 '2025-01-01T00:00:00Z', :parent_workflow_run_id)",
        named_params! {
            ":child_run_id": child_run_id,
            ":parent_run_id": parent.id,
            ":status": status,
            ":parent_workflow_run_id": parent_workflow_run_id,
        },
    )
    .unwrap();
}

#[test]
fn test_detect_stale_skips_parent_with_running_child() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "parent-run", "for-each-wf", None);
    insert_running_step_with_started_at(
        &conn,
        "s1",
        "parent-run",
        "foreach-step",
        "2020-01-01T00:00:00Z",
    );
    insert_child_workflow_run(&conn, "child-run-1", "parent-run", "running");
    let stale = crate::workflow::detect_stale_workflow_runs(&conn, 60).unwrap();
    assert!(
        stale.is_empty(),
        "parent with a running child should not be detected as stale"
    );
}

#[test]
fn test_detect_stale_skips_parent_with_pending_child() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "parent-run", "for-each-wf", None);
    insert_running_step_with_started_at(
        &conn,
        "s1",
        "parent-run",
        "foreach-step",
        "2020-01-01T00:00:00Z",
    );
    insert_child_workflow_run(&conn, "child-run-1", "parent-run", "pending");
    let stale = crate::workflow::detect_stale_workflow_runs(&conn, 60).unwrap();
    assert!(
        stale.is_empty(),
        "parent with a pending child should not be detected as stale"
    );
}

#[test]
fn test_detect_stale_skips_parent_with_waiting_child() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "parent-run", "for-each-wf", None);
    insert_running_step_with_started_at(
        &conn,
        "s1",
        "parent-run",
        "foreach-step",
        "2020-01-01T00:00:00Z",
    );
    insert_child_workflow_run(&conn, "child-run-1", "parent-run", "waiting");
    let stale = crate::workflow::detect_stale_workflow_runs(&conn, 60).unwrap();
    assert!(
        stale.is_empty(),
        "parent with a waiting child should not be detected as stale"
    );
}

#[test]
fn test_detect_stale_skips_parent_with_mixed_children() {
    // One child completed, one child still running — parent must not be detected as stale.
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "parent-run", "for-each-wf", None);
    insert_running_step_with_started_at(
        &conn,
        "s1",
        "parent-run",
        "foreach-step",
        "2020-01-01T00:00:00Z",
    );
    insert_child_workflow_run(&conn, "child-run-done", "parent-run", "completed");
    insert_child_workflow_run(&conn, "child-run-active", "parent-run", "running");
    let stale = crate::workflow::detect_stale_workflow_runs(&conn, 60).unwrap();
    assert!(
        stale.is_empty(),
        "parent with one completed and one running child should not be detected as stale"
    );
}

#[test]
fn test_detect_stale_includes_parent_when_children_completed() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "parent-run", "for-each-wf", None);
    insert_running_step_with_started_at(
        &conn,
        "s1",
        "parent-run",
        "foreach-step",
        "2020-01-01T00:00:00Z",
    );
    insert_child_workflow_run(&conn, "child-run-1", "parent-run", "completed");
    let stale = crate::workflow::detect_stale_workflow_runs(&conn, 60).unwrap();
    assert_eq!(
        stale.len(),
        1,
        "parent with only completed children should be detected as stale"
    );
    assert_eq!(stale[0].run_id, "parent-run");
}

#[test]
fn test_detect_stale_includes_parent_when_children_failed() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "parent-run", "for-each-wf", None);
    insert_running_step_with_started_at(
        &conn,
        "s1",
        "parent-run",
        "foreach-step",
        "2020-01-01T00:00:00Z",
    );
    insert_child_workflow_run(&conn, "child-run-1", "parent-run", "failed");
    let stale = crate::workflow::detect_stale_workflow_runs(&conn, 60).unwrap();
    assert_eq!(
        stale.len(),
        1,
        "parent with only failed children should be detected as stale"
    );
    assert_eq!(stale[0].run_id, "parent-run");
}

// ---------------------------------------------------------------------------
// reap_stale_workflow_runs — PID liveness check + mark-as-failed tests
// ---------------------------------------------------------------------------

#[test]
fn test_reap_stale_reaps_dead_agent() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "stale-run", "deploy", Some("repo/wt"));
    // Create a child agent run — no live subprocess.
    let agent_mgr = AgentManager::new(&conn);
    let child = agent_mgr.create_run(None, "step prompt", None).unwrap();
    // Insert step referencing that child agent run (no subprocess_pid → treated as dead).
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          started_at, child_run_id) \
         VALUES ('s1', 'stale-run', 'code-review', 'actor', 0, 'running', 0, \
                 '2020-01-01T00:00:00Z', :child_run_id)",
        named_params! { ":child_run_id": child.id },
    )
    .unwrap();
    let reaped = crate::workflow::reap_stale_workflow_runs(&conn, 60).unwrap();
    assert_eq!(reaped.len(), 1);
    assert_eq!(reaped[0].run_id, "stale-run");
    assert_eq!(reaped[0].step_name, "code-review");

    // Verify the workflow run is now failed.
    let run = crate::workflow::get_workflow_run(&conn, "stale-run")
        .unwrap()
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Failed);

    // Verify the child agent run is now failed.
    let agent_run = agent_mgr.get_run(&child.id).unwrap().unwrap();
    assert_eq!(agent_run.status, crate::agent::AgentRunStatus::Failed);
}

#[cfg(unix)]
#[test]
fn test_reap_stale_skips_live_agent() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "alive-run", "deploy", None);
    let agent_mgr = AgentManager::new(&conn);
    let child = agent_mgr.create_run(None, "step prompt", None).unwrap();
    // Set subprocess_pid to current process PID (alive).
    let live_pid = std::process::id() as i64;
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          started_at, child_run_id, subprocess_pid) \
         VALUES ('s1', 'alive-run', 'code-review', 'actor', 0, 'running', 0, \
                 '2020-01-01T00:00:00Z', :child_run_id, :live_pid)",
        named_params! { ":child_run_id": child.id, ":live_pid": live_pid },
    )
    .unwrap();
    let reaped = crate::workflow::reap_stale_workflow_runs(&conn, 60).unwrap();
    assert!(reaped.is_empty(), "live agent should not be reaped");

    // Verify the workflow run is still running.
    let run = crate::workflow::get_workflow_run(&conn, "alive-run")
        .unwrap()
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Running);
}

#[test]
fn test_reap_stale_reaps_step_with_no_pid() {
    let conn = setup_db();
    insert_running_root_run_with_label(&conn, "no-pid-run", "deploy", None);
    // Child agent run with no subprocess PID → treated as dead.
    let agent_mgr = AgentManager::new(&conn);
    let child = agent_mgr.create_run(None, "step prompt", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          started_at, child_run_id) \
         VALUES ('s1', 'no-pid-run', 'step-a', 'actor', 0, 'running', 0, \
                 '2020-01-01T00:00:00Z', :child_run_id)",
        named_params! { ":child_run_id": child.id },
    )
    .unwrap();
    let reaped = crate::workflow::reap_stale_workflow_runs(&conn, 60).unwrap();
    assert_eq!(
        reaped.len(),
        1,
        "no subprocess PID should be treated as dead"
    );
}

// ---------------------------------------------------------------------------
// detect_stuck_workflow_run_ids — stuck run detection tests
// (Tests the refactored API that replaced detect_stale_workflow_runs)
// ---------------------------------------------------------------------------

#[test]
fn test_detect_stuck_finds_run_with_only_terminal_steps() {
    let conn = setup_db();
    // Insert a running root run whose only step is completed (old ended_at).
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES ('stuck-run', 'deploy', NULL, :parent_run_id, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        named_params! { ":parent_run_id": parent.id },
    )
    .unwrap();
    insert_terminal_step_with_id(
        &conn,
        "s1",
        "stuck-run",
        "completed",
        "2020-01-01T00:00:00Z",
    );
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 60).unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0], "stuck-run");
}

#[test]
fn test_detect_stuck_skips_run_with_active_steps() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES ('active-run', 'deploy', NULL, :parent_run_id, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        named_params! { ":parent_run_id": parent.id },
    )
    .unwrap();
    // Step is still running — run is not stuck.
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, started_at) \
         VALUES ('s1', 'active-run', 'code-review', 'actor', 0, 'running', 0, '2020-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    let ids = crate::workflow::detect_stuck_workflow_run_ids(&conn, 60).unwrap();
    assert!(ids.is_empty(), "run with active steps should not be stuck");
}

// ---------------------------------------------------------------------------
// recover_stuck_steps — step recovery tests
// ---------------------------------------------------------------------------

#[test]
fn test_recover_stuck_steps_fixes_step_with_terminal_child() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES ('recover-run', 'deploy', NULL, :parent_run_id, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        named_params! { ":parent_run_id": parent.id },
    )
    .unwrap();

    // Create a child agent run and mark it completed via SQL.
    let child = agent_mgr.create_run(None, "step prompt", None).unwrap();
    conn.execute(
        "UPDATE agent_runs SET status = 'completed' WHERE id = :id",
        named_params! { ":id": child.id },
    )
    .unwrap();

    // Insert a step still marked 'running' but whose child is terminal.
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          started_at, child_run_id) \
         VALUES ('s1', 'recover-run', 'code-review', 'actor', 0, 'running', 0, \
                 '2020-01-01T00:00:00Z', :child_run_id)",
        named_params! { ":child_run_id": child.id },
    )
    .unwrap();
    let recovered = crate::workflow::recover_stuck_steps(&conn).unwrap();
    assert_eq!(recovered, 1, "should recover the stuck step");
}

// ---------------------------------------------------------------------------
// subprocess_pid cleared on reset tests
// ---------------------------------------------------------------------------

/// reset_failed_steps must clear subprocess_pid so the orphan reaper doesn't
/// see a stale PID on the freshly-reset pending step.
#[test]
fn test_reset_failed_steps_clears_subprocess_pid() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    // Insert a failed step that has a stale subprocess_pid.
    let step_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, subprocess_pid) \
         VALUES (:step_id, :run_id, 'step-failed', 'script', 0, 'failed', 0, 12345)",
        named_params! { ":step_id": step_id, ":run_id": run_id },
    )
    .unwrap();
    crate::workflow::reset_failed_steps(&conn, &run_id).unwrap();

    let pid: Option<i64> = conn
        .query_row(
            "SELECT subprocess_pid FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id },
            |r| r.get("subprocess_pid"),
        )
        .unwrap();
    assert!(
        pid.is_none(),
        "subprocess_pid must be NULL after reset_failed_steps"
    );
}

/// reset_completed_steps must clear subprocess_pid.
#[test]
fn test_reset_completed_steps_clears_subprocess_pid() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    let step_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, subprocess_pid) \
         VALUES (:step_id, :run_id, 'step-done', 'script', 0, 'completed', 0, 99999)",
        named_params! { ":step_id": step_id, ":run_id": run_id },
    )
    .unwrap();
    crate::workflow::reset_completed_steps(&conn, &run_id).unwrap();

    let pid: Option<i64> = conn
        .query_row(
            "SELECT subprocess_pid FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id },
            |r| r.get("subprocess_pid"),
        )
        .unwrap();
    assert!(
        pid.is_none(),
        "subprocess_pid must be NULL after reset_completed_steps"
    );
}

/// reset_steps_from_position must clear subprocess_pid.
#[test]
fn test_reset_steps_from_position_clears_subprocess_pid() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    let step_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, subprocess_pid) \
         VALUES (:step_id, :run_id, 'step-pos', 'script', 2, 'failed', 0, 55555)",
        named_params! { ":step_id": step_id, ":run_id": run_id },
    )
    .unwrap();
    crate::workflow::reset_steps_from_position(&conn, &run_id, 2).unwrap();

    let pid: Option<i64> = conn
        .query_row(
            "SELECT subprocess_pid FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id },
            |r| r.get("subprocess_pid"),
        )
        .unwrap();
    assert!(
        pid.is_none(),
        "subprocess_pid must be NULL after reset_steps_from_position"
    );
}

/// reset_failed_steps must attempt to signal running subprocesses before
/// nulling subprocess_pid, so orphaned child processes are cleaned up.
/// Uses non-existent PIDs (u32::MAX - N) — cancel_subprocess tolerates ESRCH.
/// Tests multiple running subprocesses to verify all PIDs are signalled.
#[test]
fn test_reset_failed_steps_kills_running_subprocesses() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    let step_id_a = crate::new_id();
    let step_id_b = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, subprocess_pid) \
         VALUES (:step_id, :run_id, 'step-running-a', 'script', 0, 'running', 0, 4294967295)",
        named_params! { ":step_id": step_id_a, ":run_id": run_id },
    )
    .unwrap();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, subprocess_pid) \
         VALUES (:step_id, :run_id, 'step-running-b', 'script', 1, 'running', 0, 4294967294)",
        named_params! { ":step_id": step_id_b, ":run_id": run_id },
    )
    .unwrap();
    // Must not error even though the PIDs do not exist.
    crate::workflow::reset_failed_steps(&conn, &run_id).unwrap();

    for (id, label) in [
        (&step_id_a, "step-running-a"),
        (&step_id_b, "step-running-b"),
    ] {
        let pid: Option<i64> = conn
            .query_row(
                "SELECT subprocess_pid FROM workflow_run_steps WHERE id = :id",
                named_params! { ":id": id },
                |r| r.get("subprocess_pid"),
            )
            .unwrap();
        assert!(
            pid.is_none(),
            "subprocess_pid must be NULL after reset_failed_steps for {label}"
        );
    }
}

/// reset_steps_from_position must attempt to signal running subprocesses at or
/// after `position` before nulling subprocess_pid, and must NOT signal steps
/// before the boundary.
#[test]
fn test_reset_steps_from_position_kills_running_subprocesses() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    // Step at position 1 — before the reset boundary; must be left untouched.
    let step_id_before = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, subprocess_pid) \
         VALUES (:step_id, :run_id, 'step-before', 'script', 1, 'running', 0, 4294967294)",
        named_params! { ":step_id": step_id_before, ":run_id": run_id },
    )
    .unwrap();

    // Step at position 2 — at the reset boundary; must be reset.
    let step_id_at = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, subprocess_pid) \
         VALUES (:step_id, :run_id, 'step-at', 'script', 2, 'running', 0, 4294967295)",
        named_params! { ":step_id": step_id_at, ":run_id": run_id },
    )
    .unwrap();

    // Step at position 3 — after the reset boundary; must also be reset.
    let step_id_after = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, subprocess_pid) \
         VALUES (:step_id, :run_id, 'step-after', 'script', 3, 'running', 0, 4294967293)",
        named_params! { ":step_id": step_id_after, ":run_id": run_id },
    )
    .unwrap();
    crate::workflow::reset_steps_from_position(&conn, &run_id, 2).unwrap();

    // Step at boundary must have subprocess_pid nulled.
    let pid_at: Option<i64> = conn
        .query_row(
            "SELECT subprocess_pid FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id_at },
            |r| r.get("subprocess_pid"),
        )
        .unwrap();
    assert!(
        pid_at.is_none(),
        "subprocess_pid must be NULL after reset_steps_from_position for step at boundary"
    );

    // Step after boundary must also have subprocess_pid nulled and status reset.
    let pid_after: Option<i64> = conn
        .query_row(
            "SELECT subprocess_pid FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id_after },
            |r| r.get("subprocess_pid"),
        )
        .unwrap();
    assert!(
        pid_after.is_none(),
        "subprocess_pid must be NULL after reset_steps_from_position for step after boundary"
    );
    let status_after: String = conn
        .query_row(
            "SELECT status FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id_after },
            |r| r.get("status"),
        )
        .unwrap();
    assert_eq!(
        status_after, "pending",
        "status of step after boundary must be reset to pending"
    );

    // Step before boundary must retain its status and subprocess_pid.
    let pid_before: Option<i64> = conn
        .query_row(
            "SELECT subprocess_pid FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id_before },
            |r| r.get("subprocess_pid"),
        )
        .unwrap();
    assert_eq!(
        pid_before,
        Some(4294967294_i64),
        "subprocess_pid of step before boundary must not be changed"
    );
    let status_before: String = conn
        .query_row(
            "SELECT status FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id_before },
            |r| r.get("status"),
        )
        .unwrap();
    assert_eq!(
        status_before, "running",
        "status of step before boundary must not be changed"
    );
}

/// Insert a running agent step into a workflow run, returning (agent_run_id, step_id).
fn create_agent_step(
    conn: &Connection,
    run_id: &str,
    step_name: &str,
    position: i64,
    subprocess_pid: i64,
) -> (String, String) {
    let agent_mgr = AgentManager::new(conn);
    let agent = agent_mgr.create_run(Some("w1"), step_name, None).unwrap();
    conn.execute(
        "UPDATE agent_runs SET subprocess_pid = :pid WHERE id = :id",
        named_params! { ":pid": subprocess_pid, ":id": agent.id },
    )
    .unwrap();
    let step_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, child_run_id) \
         VALUES (:step_id, :run_id, :step_name, 'actor', :position, 'running', 0, :agent_run_id)",
        named_params! { ":step_id": step_id, ":run_id": run_id, ":step_name": step_name, ":position": position, ":agent_run_id": agent.id },
    )
    .unwrap();
    (agent.id, step_id)
}

/// reset_steps_from_position with `from_position=Some(pos)` must also signal
/// agent-step subprocesses (tracked via child_run_id) at or after the boundary,
/// and must NOT signal agent steps before the boundary.
#[test]
fn test_reset_steps_from_position_kills_agent_subprocesses() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    // Agent run before the boundary — its subprocess must NOT be touched.
    let (_, step_before) = create_agent_step(&conn, &run_id, "agent-before", 1, 4294967290);

    // Agent run at the boundary — its subprocess must be signalled.
    let (_, step_at) = create_agent_step(&conn, &run_id, "agent-at", 2, 4294967291);

    // Agent run after the boundary — its subprocess must also be signalled.
    let (_, step_after) = create_agent_step(&conn, &run_id, "agent-after", 3, 4294967292);
    // Must not error even if the PIDs are not real processes.
    crate::workflow::reset_steps_from_position(&conn, &run_id, 2).unwrap();

    // Steps at and after the boundary must be reset to pending.
    let status_at: String = conn
        .query_row(
            "SELECT status FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_at },
            |r| r.get("status"),
        )
        .unwrap();
    assert_eq!(status_at, "pending", "agent step at boundary must be reset");

    let status_after: String = conn
        .query_row(
            "SELECT status FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_after },
            |r| r.get("status"),
        )
        .unwrap();
    assert_eq!(
        status_after, "pending",
        "agent step after boundary must be reset"
    );

    // Step before boundary must remain running.
    let status_before: String = conn
        .query_row(
            "SELECT status FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_before },
            |r| r.get("status"),
        )
        .unwrap();
    assert_eq!(
        status_before, "running",
        "agent step before boundary must not be reset"
    );
}

/// reset_failed_steps with `from_position=None` must signal agent-step subprocesses
/// (tracked via child_run_id) across the entire run.
#[test]
fn test_reset_failed_steps_kills_agent_subprocesses() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    // Agent run in running state — its subprocess must be signalled by reset_failed_steps.
    let (_, step_id) = create_agent_step(&conn, &run_id, "agent-step", 1, 4294967290);
    // Must not error even if the PID is not a real process.
    crate::workflow::reset_failed_steps(&conn, &run_id).unwrap();

    // Step must be reset to pending.
    let status: String = conn
        .query_row(
            "SELECT status FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id },
            |r| r.get("status"),
        )
        .unwrap();
    assert_eq!(status, "pending", "agent step must be reset to pending");
}

// ---------------------------------------------------------------------------
// step_error cleared on reset tests
// ---------------------------------------------------------------------------

/// reset_failed_steps must clear step_error so stale error text doesn't persist
/// after a successful resume.
#[test]
fn test_reset_failed_steps_clears_step_error() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    let step_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, step_error) \
         VALUES (:step_id, :run_id, 'step-failed', 'script', 0, 'failed', 0, 'some error')",
        named_params! { ":step_id": step_id, ":run_id": run_id },
    )
    .unwrap();
    crate::workflow::reset_failed_steps(&conn, &run_id).unwrap();

    let err: Option<String> = conn
        .query_row(
            "SELECT step_error FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id },
            |r| r.get("step_error"),
        )
        .unwrap();
    assert!(
        err.is_none(),
        "step_error must be NULL after reset_failed_steps"
    );
}

/// reset_completed_steps must clear step_error.
#[test]
fn test_reset_completed_steps_clears_step_error() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    let step_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, step_error) \
         VALUES (:step_id, :run_id, 'step-done', 'script', 0, 'completed', 0, 'some error')",
        named_params! { ":step_id": step_id, ":run_id": run_id },
    )
    .unwrap();
    crate::workflow::reset_completed_steps(&conn, &run_id).unwrap();

    let err: Option<String> = conn
        .query_row(
            "SELECT step_error FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id },
            |r| r.get("step_error"),
        )
        .unwrap();
    assert!(
        err.is_none(),
        "step_error must be NULL after reset_completed_steps"
    );
}

/// reset_steps_from_position must clear step_error.
#[test]
fn test_reset_steps_from_position_clears_step_error() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    let step_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, step_error) \
         VALUES (:step_id, :run_id, 'step-pos', 'script', 2, 'failed', 0, 'some error')",
        named_params! { ":step_id": step_id, ":run_id": run_id },
    )
    .unwrap();
    crate::workflow::reset_steps_from_position(&conn, &run_id, 2).unwrap();

    let err: Option<String> = conn
        .query_row(
            "SELECT step_error FROM workflow_run_steps WHERE id = :id",
            named_params! { ":id": step_id },
            |r| r.get("step_error"),
        )
        .unwrap();
    assert!(
        err.is_none(),
        "step_error must be NULL after reset_steps_from_position"
    );
}

// ---------------------------------------------------------------------------
// claim_expired_lease_runs tests
// ---------------------------------------------------------------------------

/// Helper: insert a minimal running root workflow_run with explicit started_at
/// and optional lease_until. Returns the run's id.
fn insert_orphaned_root_run(
    conn: &Connection,
    started_at: &str,
    lease_until: Option<&str>,
) -> String {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
    let id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id, lease_until) \
         VALUES (:id, 'test-wf', NULL, :parent_run_id, 'running', 0, 'manual', :started_at, NULL, :lease_until)",
        named_params! { ":id": id, ":parent_run_id": parent.id, ":started_at": started_at, ":lease_until": lease_until },
    )
    .unwrap();
    id
}

fn get_run_status(conn: &Connection, run_id: &str) -> String {
    conn.query_row(
        "SELECT status FROM workflow_runs WHERE id = :id",
        named_params! { ":id": run_id },
        |r| r.get("status"),
    )
    .unwrap()
}

fn get_step_status(conn: &Connection, step_id: &str) -> String {
    conn.query_row(
        "SELECT status FROM workflow_run_steps WHERE id = :id",
        named_params! { ":id": step_id },
        |r| r.get("status"),
    )
    .unwrap()
}

/// Expired lease_until (in the past) must be reaped by the watchdog.
#[test]
fn test_claim_expired_lease_runs_expired_lease() {
    let conn = setup_db();
    let expired = chrono::Utc::now() - chrono::Duration::seconds(300);
    // Use SQLite datetime format — production acquire_lease also uses datetime('now', '+N seconds').
    let expired_sqlite = expired.format("%Y-%m-%d %H:%M:%S").to_string();
    let run_id = insert_orphaned_root_run(&conn, &expired_sqlite, Some(&expired_sqlite));
    let config = crate::config::Config::default();
    let claimed = crate::workflow::claim_expired_lease_runs(&conn, &config).unwrap();

    assert_eq!(claimed.len(), 1, "expected 1 run reaped");
    assert_eq!(
        get_run_status(&conn, &run_id),
        "failed",
        "run status must be failed after CAS flip"
    );
}

/// A lease_until in the future must NOT be reaped.
#[test]
fn test_claim_expired_lease_runs_fresh_lease() {
    let conn = setup_db();
    let now = chrono::Utc::now();
    let fresh_lease = now + chrono::Duration::seconds(300);
    // Use SQLite datetime format — production acquire_lease also uses datetime('now', '+N seconds').
    let now_sqlite = now.format("%Y-%m-%d %H:%M:%S").to_string();
    let fresh_sqlite = fresh_lease.format("%Y-%m-%d %H:%M:%S").to_string();
    let run_id = insert_orphaned_root_run(&conn, &now_sqlite, Some(&fresh_sqlite));
    let config = crate::config::Config::default();
    let claimed = crate::workflow::claim_expired_lease_runs(&conn, &config).unwrap();

    assert_eq!(claimed.len(), 0, "run with future lease must not be reaped");
    assert_eq!(
        get_run_status(&conn, &run_id),
        "running",
        "run status must still be running"
    );
}

/// NULL lease_until (pre-migration row or unset) must be reaped — treated as expired.
#[test]
fn test_claim_expired_lease_runs_null_lease_until() {
    let conn = setup_db();
    let now = chrono::Utc::now();
    let run_id = insert_orphaned_root_run(&conn, &now.to_rfc3339(), None);
    let config = crate::config::Config::default();
    let claimed = crate::workflow::claim_expired_lease_runs(&conn, &config).unwrap();

    assert_eq!(claimed.len(), 1, "run with NULL lease_until must be reaped");
    assert_eq!(get_run_status(&conn, &run_id), "failed");
}

/// A run with an active child step (status='pending') must NOT be reaped, even
/// when the lease is expired — the NOT EXISTS guard blocks it.
#[test]
fn test_claim_expired_lease_runs_active_step_blocks_reap() {
    let conn = setup_db();
    let expired = chrono::Utc::now() - chrono::Duration::seconds(300);
    let expired_sqlite = expired.format("%Y-%m-%d %H:%M:%S").to_string();
    let run_id = insert_orphaned_root_run(&conn, &expired_sqlite, Some(&expired_sqlite));

    // Insert a pending step — makes the NOT EXISTS guard fire.
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration) \
         VALUES ('step-1', :run_id, 'step-a', 'actor', 0, 'pending', 0)",
        named_params! { ":run_id": run_id },
    )
    .unwrap();
    let config = crate::config::Config::default();
    let claimed = crate::workflow::claim_expired_lease_runs(&conn, &config).unwrap();

    assert_eq!(claimed.len(), 0, "run with active step must not be reaped");
    assert_eq!(get_run_status(&conn, &run_id), "running");
}

/// Two sequential calls on the same orphan: first wins the CAS (count=1),
/// second sees changes()=0 (count=0).
#[test]
fn test_claim_expired_lease_runs_cas_race() {
    let conn = setup_db();
    let expired = chrono::Utc::now() - chrono::Duration::seconds(300);
    let expired_sqlite = expired.format("%Y-%m-%d %H:%M:%S").to_string();
    let _run_id = insert_orphaned_root_run(&conn, &expired_sqlite, Some(&expired_sqlite));
    let config = crate::config::Config::default();

    // First call wins the CAS.
    let claimed1 = crate::workflow::claim_expired_lease_runs(&conn, &config).unwrap();
    assert_eq!(claimed1.len(), 1, "first call should win the CAS");

    // Second call sees status='failed' — detection query excludes it, count=0.
    let claimed2 = crate::workflow::claim_expired_lease_runs(&conn, &config).unwrap();
    assert_eq!(claimed2.len(), 0, "second call must see no orphaned runs");
}

/// Sub-workflow runs (parent_workflow_run_id IS NOT NULL) must never be reaped.
#[test]
fn test_claim_expired_lease_runs_sub_workflow_excluded() {
    let conn = setup_db();
    let expired = chrono::Utc::now() - chrono::Duration::seconds(300);
    // Use SQLite datetime format for lease_until comparison.
    let expired_sqlite = expired.format("%Y-%m-%d %H:%M:%S").to_string();

    // First create a parent run (to satisfy FK).
    let parent_agent = AgentManager::new(&conn)
        .create_run(None, "workflow", None)
        .unwrap();
    let parent_run_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id, lease_until) \
         VALUES (:parent_run_id, 'parent-wf', NULL, :parent_agent_id, 'running', 0, 'manual', :ts, NULL, :expired)",
        named_params! {
            ":parent_run_id": parent_run_id,
            ":parent_agent_id": parent_agent.id,
            ":ts": expired_sqlite,
            ":expired": expired_sqlite,
        },
    )
    .unwrap();

    // Now create a sub-workflow run with parent_workflow_run_id set.
    let child_agent = AgentManager::new(&conn)
        .create_run(None, "workflow", None)
        .unwrap();
    let child_run_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id, lease_until) \
         VALUES (:child_run_id, 'child-wf', NULL, :child_agent_id, 'running', 0, 'manual', :ts, :parent_run_id, :expired)",
        named_params! {
            ":child_run_id": child_run_id,
            ":child_agent_id": child_agent.id,
            ":ts": expired_sqlite,
            ":parent_run_id": parent_run_id,
            ":expired": expired_sqlite,
        },
    )
    .unwrap();
    let config = crate::config::Config::default();
    // Only the parent root run should be reaped; the child is excluded.
    let claimed = crate::workflow::claim_expired_lease_runs(&conn, &config).unwrap();
    assert_eq!(claimed.len(), 1, "only root run should be reaped");

    assert_eq!(
        get_run_status(&conn, &child_run_id),
        "running",
        "sub-workflow run must not be reaped"
    );
}

/// A completed run must not be reclaimed by the watchdog, even if its lease is expired.
#[test]
fn test_claim_expired_lease_runs_completed_run_not_reclaimed() {
    let conn = setup_db();
    let expired = chrono::Utc::now() - chrono::Duration::seconds(300);
    let expired_sqlite = expired.format("%Y-%m-%d %H:%M:%S").to_string();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
    let run_id = crate::new_id();
    // Insert with status='completed' — the query only picks up status='running' rows.
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id, lease_until) \
         VALUES (:id, 'test-wf', NULL, :parent_run_id, 'completed', 0, 'manual', :ts, NULL, :expired)",
        named_params! {
            ":id": run_id,
            ":parent_run_id": parent.id,
            ":ts": expired_sqlite,
            ":expired": expired_sqlite,
        },
    )
    .unwrap();
    let config = crate::config::Config::default();
    let claimed = crate::workflow::claim_expired_lease_runs(&conn, &config).unwrap();
    assert_eq!(claimed.len(), 0, "completed run must not be reclaimed");
    assert_eq!(get_run_status(&conn, &run_id), "completed");
}

/// terminate_subprocesses tolerates a nonexistent PID (ESRCH) without panicking.
/// This is the same ESRCH-tolerance pattern verified by the recovery.rs unit test —
/// exercise it here against the watchdog claim path.
#[cfg(unix)]
#[test]
fn test_claim_expired_lease_runs_terminate_subprocess_esrch_tolerated() {
    let conn = setup_db();
    let expired = chrono::Utc::now() - chrono::Duration::seconds(300);
    let expired_sqlite = expired.format("%Y-%m-%d %H:%M:%S").to_string();

    // Insert a running root workflow run with expired lease and no active steps.
    let run_id = insert_orphaned_root_run(&conn, &expired_sqlite, Some(&expired_sqlite));

    // Insert a running step with a nonexistent subprocess PID — terminate_subprocesses
    // spawns a cancel thread for PID 99999 and must not panic on ESRCH.
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, subprocess_pid) \
         VALUES ('step-esrch', :run_id, 'script', 'script', 0, 'running', 0, 99999)",
        named_params! { ":run_id": run_id },
    )
    .unwrap();

    // Call terminate_subprocesses directly — must return Ok even with a dead PID.
    let result = crate::workflow::terminate_subprocesses(&conn, &run_id, None);
    assert!(
        result.is_ok(),
        "terminate_subprocesses must tolerate ESRCH: {result:?}"
    );
}

/// Regression test for #2038: step_error must be persisted when post-execution
/// schema validation fails on a call step.
///
/// This mirrors exactly what `execute_call_with_schema` does in call.rs lines
/// 321-338: on a validation error it calls `update_step_status_full` with
/// `status = Failed` and `step_error = Some(&validation_err)`.  We verify here
/// that the value round-trips through SQLite and is readable via
/// `get_workflow_steps`.
#[test]
fn test_step_error_persisted_on_schema_validation_failure() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let run = crate::workflow::create_workflow_run(
        &conn,
        "test-wf",
        Some("w1"),
        &parent.id,
        false,
        "manual",
        None,
    )
    .unwrap();

    // Insert a step that simulates being mid-execution.
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "call-step", "reviewer", false, 0, 0).unwrap();

    // Simulate what call.rs does when `interpret_agent_output` returns an Err:
    // mark the step Failed and record the validation error message.
    let validation_err = "structured output validation failed: missing required field 'approved'";
    crate::workflow::update_step_status_full(
        &conn,
        &step_id,
        WorkflowStepStatus::Failed,
        Some("child-run-id"),
        Some("raw agent output text"),
        None, // no context_out on validation failure
        None, // no markers_out on validation failure
        Some(0),
        None, // no structured_output (validation failed)
        Some(validation_err),
    )
    .unwrap();

    // Read the step back and assert step_error is set correctly.
    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert_eq!(steps.len(), 1, "expected exactly one step");
    let step = &steps[0];
    assert_eq!(step.status, WorkflowStepStatus::Failed);
    assert_eq!(
        step.step_error.as_deref(),
        Some(validation_err),
        "step_error must be persisted when schema validation fails"
    );
    // Sanity: structured_output must NOT be set when validation failed.
    assert!(
        step.structured_output.is_none(),
        "structured_output must be None when validation failed"
    );
    // Sanity: raw result text must be preserved.
    assert_eq!(
        step.result_text.as_deref(),
        Some("raw agent output text"),
        "raw result_text must still be stored even on validation failure"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// claim_stuck_workflows
// ────────────────────────────────────────────────────────────────────────────

/// A stuck run (running, all steps terminal, old ended_at) should be detected,
/// CAS-flipped to failed, and counted as resumed.
#[test]
fn test_auto_resume_stuck_workflows_detects_and_flips() {
    let conn = setup_db();
    insert_running_root_run(&conn, "stuck-auto");
    insert_terminal_step_with_id(
        &conn,
        "s1",
        "stuck-auto",
        "completed",
        "2020-01-01T00:00:00Z",
    );
    let config = Config::default();
    let claimed = crate::workflow::claim_stuck_workflows(&conn, &config, None).unwrap();
    assert_eq!(
        claimed.len(),
        1,
        "one stuck run should be detected and flipped"
    );

    // After CAS flip the run must be in 'failed' status.
    assert_eq!(
        get_run_status(&conn, "stuck-auto"),
        "failed",
        "CAS flip should transition run from running to failed"
    );
}

/// A second concurrent call should see 0 runs because the first call already
/// CAS-flipped the status to failed (detection query only finds running runs).
#[test]
fn test_auto_resume_stuck_workflows_concurrent_race() {
    let conn = setup_db();
    insert_running_root_run(&conn, "race-auto");
    insert_terminal_step_with_id(
        &conn,
        "s1",
        "race-auto",
        "completed",
        "2020-01-01T00:00:00Z",
    );
    let config = Config::default();

    let claimed1 = crate::workflow::claim_stuck_workflows(&conn, &config, None).unwrap();
    assert_eq!(claimed1.len(), 1, "first call should win the CAS");

    let claimed2 = crate::workflow::claim_stuck_workflows(&conn, &config, None).unwrap();
    assert_eq!(claimed2.len(), 0, "second call must see no stuck runs");
}

/// Fresh runs (recent heartbeat) must not be detected.
#[test]
fn test_auto_resume_stuck_workflows_skips_fresh_run() {
    let conn = setup_db();
    // Use insert_orphaned_root_run with a fresh heartbeat so the detection
    // query (COALESCE(last_heartbeat, started_at)) sees a recent timestamp.
    let now = chrono::Utc::now().to_rfc3339();
    let run_id = insert_orphaned_root_run(&conn, &now, Some(&now));
    let config = Config::default();
    let claimed = crate::workflow::claim_stuck_workflows(&conn, &config, None).unwrap();
    assert_eq!(claimed.len(), 0, "fresh run must not be resumed");

    // Status must remain running.
    assert_eq!(get_run_status(&conn, &run_id), "running");
}

/// When a configurable threshold is supplied, the function should use the
/// minimum of 60s and the configurable value.
#[test]
fn test_auto_resume_stuck_workflows_uses_min_threshold() {
    let conn = setup_db();
    insert_running_root_run(&conn, "thresh-auto");
    insert_terminal_step_with_id(
        &conn,
        "s1",
        "thresh-auto",
        "completed",
        "2020-01-01T00:00:00Z",
    );

    let config = Config::default();

    // Even with a very large configurable threshold, min(60, 9999) = 60 and
    // the 2020 ended_at is well past 60s.
    let claimed = crate::workflow::claim_stuck_workflows(&conn, &config, Some(9999)).unwrap();
    assert_eq!(claimed.len(), 1);

    assert_eq!(get_run_status(&conn, "thresh-auto"), "failed");
}

// ---------------------------------------------------------------------------
// delete_orphaned_pending_steps
// ---------------------------------------------------------------------------

/// A pending step with started_at IS NULL (never started) must be deleted.
/// A completed step in the same run must be preserved.
#[test]
fn test_delete_orphaned_pending_steps_removes_never_started() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    // Insert an orphaned pending step (no started_at).
    let orphan_id =
        crate::workflow::insert_step(&conn, &run.id, "orphan-step", "actor", false, 0, 0).unwrap();
    // Confirm insert_step leaves started_at NULL and status = 'pending'.
    let steps = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].status, WorkflowStepStatus::Pending);
    assert!(steps[0].started_at.is_none());

    // Insert a completed step that must survive.
    let completed_id =
        crate::workflow::insert_step(&conn, &run.id, "done-step", "actor", false, 1, 0).unwrap();
    crate::workflow::update_step_status(
        &conn,
        &completed_id,
        WorkflowStepStatus::Completed,
        None,
        Some("ok"),
        None,
        None,
        Some(0),
    )
    .unwrap();

    let deleted = crate::workflow::delete_orphaned_pending_steps(&conn, &run.id).unwrap();
    assert_eq!(deleted, 1, "exactly one orphaned row should be deleted");

    let remaining = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, completed_id, "completed step must survive");
    assert!(
        remaining.iter().all(|s| s.id != orphan_id),
        "orphaned pending step must be gone"
    );
}

/// A pending step that HAS a started_at value (was started, then reset) must NOT be deleted.
#[test]
fn test_delete_orphaned_pending_steps_ignores_started_pending() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    // Insert a step and then manually set it to pending WITH a started_at value
    // (simulates a step that was reset after having started once).
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "reset-step", "actor", false, 0, 0).unwrap();
    conn.execute(
        "UPDATE workflow_run_steps \
         SET status = 'pending', started_at = '2025-01-01T00:00:00Z' \
         WHERE id = :id",
        named_params! { ":id": step_id },
    )
    .unwrap();

    let deleted = crate::workflow::delete_orphaned_pending_steps(&conn, &run.id).unwrap();
    assert_eq!(
        deleted, 0,
        "pending step with started_at must not be deleted"
    );

    let remaining = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, step_id);
}

/// Completed, failed, and running rows with started_at IS NULL are NOT deleted.
#[test]
fn test_delete_orphaned_pending_steps_only_targets_pending() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    // Insert steps with various non-pending statuses and no started_at.
    for (name, status) in &[
        ("comp", "completed"),
        ("failed", "failed"),
        ("running", "running"),
    ] {
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status, iteration) \
             VALUES (:id, :run_id, :name, 'actor', 0, :status, 0)",
            named_params! { ":id": crate::new_id(), ":run_id": run.id, ":name": name, ":status": status },
        )
        .unwrap();
    }

    let deleted = crate::workflow::delete_orphaned_pending_steps(&conn, &run.id).unwrap();
    assert_eq!(deleted, 0, "non-pending rows must not be deleted");

    let remaining = crate::workflow::get_workflow_steps(&conn, &run.id).unwrap();
    assert_eq!(remaining.len(), 3, "all three rows must survive");
}

/// When there are no orphaned rows, the function is a no-op and returns 0.
#[test]
fn test_delete_orphaned_pending_steps_noop_when_none() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    let deleted = crate::workflow::delete_orphaned_pending_steps(&conn, &run.id).unwrap();
    assert_eq!(deleted, 0);
}

// ---------------------------------------------------------------------------
// update_step_child_run_id
// ---------------------------------------------------------------------------

/// `update_step_child_run_id` writes the child run ID to the step row so the
/// TUI can drill into a running child workflow.
#[test]
fn test_update_step_child_run_id() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "call-child", "actor", false, 0, 0).unwrap();

    let child_run_id = crate::new_id();
    crate::workflow::update_step_child_run_id(&conn, &step_id, &child_run_id).unwrap();

    let step = crate::workflow::get_step_by_id(&conn, &step_id)
        .unwrap()
        .expect("step must exist");
    assert_eq!(
        step.child_run_id.as_deref(),
        Some(child_run_id.as_str()),
        "child_run_id must be written to the step row"
    );
}

/// Calling `update_step_child_run_id` on a non-existent step is a no-op (no error).
#[test]
fn test_update_step_child_run_id_nonexistent_step() {
    let conn = setup_db();
    // SQLite UPDATE on a missing row succeeds with 0 rows affected — no error expected.
    crate::workflow::update_step_child_run_id(&conn, "nonexistent-step-id", "any-child-run-id")
        .unwrap();
}

// ---------------------------------------------------------------------------
// predecessor_completed
// ---------------------------------------------------------------------------

#[test]
fn test_predecessor_completed_pos_zero_always_true() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    // No rows at all — pos 0 should always return true.
    assert!(crate::workflow::predecessor_completed(&conn, &run.id, 0).unwrap());
}

#[test]
fn test_predecessor_completed_true_when_prev_completed() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "step-a", "actor", false, 0, 0).unwrap();
    crate::workflow::update_step_status(
        &conn,
        &step_id,
        WorkflowStepStatus::Completed,
        None,
        None,
        None,
        None,
        Some(0),
    )
    .unwrap();
    // Predecessor at pos 0 is completed → pos 1 check should return true.
    assert!(crate::workflow::predecessor_completed(&conn, &run.id, 1).unwrap());
}

#[test]
fn test_predecessor_completed_false_when_prev_running() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "step-a", "actor", false, 0, 0).unwrap();
    crate::workflow::update_step_status(
        &conn,
        &step_id,
        WorkflowStepStatus::Running,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    assert!(!crate::workflow::predecessor_completed(&conn, &run.id, 1).unwrap());
}

#[test]
fn test_predecessor_completed_false_when_no_prev_row() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    // No step at position 0, iteration 0 → pos 1 predecessor check returns false.
    assert!(!crate::workflow::predecessor_completed(&conn, &run.id, 1).unwrap());
}

#[test]
fn test_regression_2448_predecessor_completed_cross_iteration() {
    // Regression: predecessor_completed must find steps stored in iteration 0
    // when the next step is at a higher position that belongs to iteration 1.
    // Before the fix, the query included `AND iteration = :iter` which caused
    // position 5 (iteration=0) to be invisible when checking from position 6
    // (iteration=1), silently skipping every sub-workflow step in the loop.
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    // Simulate do-while iteration 0: insert and complete steps at positions 0–5
    // all with iteration=0.
    for pos in 0..6i64 {
        let step_id =
            crate::workflow::insert_step(&conn, &run.id, "step-a", "actor", false, pos, 0).unwrap();
        crate::workflow::update_step_status(
            &conn,
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            None,
            None,
            None,
            Some(0),
        )
        .unwrap();
    }
    // Iteration 1 starts at position 6. Guard A checks whether position 5
    // (stored with iteration=0) is completed. Must return true.
    assert!(
        crate::workflow::predecessor_completed(&conn, &run.id, 6).unwrap(),
        "predecessor_completed must find a step from iteration 0 when checking position 6 in iteration 1"
    );
}

// ---------------------------------------------------------------------------
// active_step_exists
// ---------------------------------------------------------------------------

#[test]
fn test_active_step_exists_true_for_running() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "workflow:lint-fix", "workflow", false, 2, 0)
            .unwrap();
    crate::workflow::update_step_status(
        &conn,
        &step_id,
        WorkflowStepStatus::Running,
        None,
        None,
        None,
        None,
        Some(0),
    )
    .unwrap();
    assert!(
        crate::workflow::active_step_exists(&conn, &run.id, 2, 0, "workflow:lint-fix").unwrap()
    );
}

#[test]
fn test_active_step_exists_false_for_failed() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "workflow:lint-fix", "workflow", false, 2, 0)
            .unwrap();
    crate::workflow::update_step_status(
        &conn,
        &step_id,
        WorkflowStepStatus::Failed,
        None,
        None,
        None,
        None,
        Some(0),
    )
    .unwrap();
    // Failed is terminal — retries are allowed, so active_step_exists returns false.
    assert!(
        !crate::workflow::active_step_exists(&conn, &run.id, 2, 0, "workflow:lint-fix").unwrap()
    );
}

#[test]
fn test_active_step_exists_false_different_step_name() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    // An active row exists at pos 2 but for a different step name (parallel step).
    let step_id = crate::workflow::insert_step(
        &conn,
        &run.id,
        "workflow:other-step",
        "workflow",
        false,
        2,
        0,
    )
    .unwrap();
    crate::workflow::update_step_status(
        &conn,
        &step_id,
        WorkflowStepStatus::Running,
        None,
        None,
        None,
        None,
        Some(0),
    )
    .unwrap();
    assert!(
        !crate::workflow::active_step_exists(&conn, &run.id, 2, 0, "workflow:lint-fix").unwrap()
    );
}

#[test]
fn test_active_step_exists_true_for_pending() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    // insert_step creates a row in pending status by default.
    crate::workflow::insert_step(&conn, &run.id, "workflow:lint-fix", "workflow", false, 2, 0)
        .unwrap();
    assert!(
        crate::workflow::active_step_exists(&conn, &run.id, 2, 0, "workflow:lint-fix").unwrap()
    );
}

#[test]
fn test_active_step_exists_true_for_waiting() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "workflow:lint-fix", "workflow", false, 2, 0)
            .unwrap();
    crate::workflow::update_step_status(
        &conn,
        &step_id,
        WorkflowStepStatus::Waiting,
        None,
        None,
        None,
        None,
        Some(0),
    )
    .unwrap();
    assert!(
        crate::workflow::active_step_exists(&conn, &run.id, 2, 0, "workflow:lint-fix").unwrap()
    );
}

#[test]
fn test_active_step_exists_true_for_completed() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "workflow:lint-fix", "workflow", false, 2, 0)
            .unwrap();
    crate::workflow::update_step_status(
        &conn,
        &step_id,
        WorkflowStepStatus::Completed,
        None,
        None,
        None,
        None,
        Some(0),
    )
    .unwrap();
    // completed rows block re-insertion so retries don't re-run a succeeded step.
    assert!(
        crate::workflow::active_step_exists(&conn, &run.id, 2, 0, "workflow:lint-fix").unwrap()
    );
}

// ---------------------------------------------------------------------------
// Regression: #2406 — Guard A + Guard B prevent duplicate call_workflow steps
// ---------------------------------------------------------------------------

/// Regression test for bug #2406: a recovery poll fired while the predecessor
/// step (implement, pos 1) was still `running`, which previously caused the
/// engine to advance and insert a duplicate `workflow:lint-fix` row at pos 2.
///
/// Guard A: predecessor_completed(run_id, 2) must return false while pos 1
///          is still running → the engine should bail out before inserting.
/// Guard B: if a premature row was somehow inserted, active_step_exists must
///          return true → the engine bails out before inserting a second row.
#[test]
fn test_regression_2406_guards_prevent_duplicate_call_workflow_step() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);

    // Simulate the "implement" step at position 1 still running (not completed).
    let implement_id =
        crate::workflow::insert_step(&conn, &run.id, "implement", "actor", false, 1, 0).unwrap();
    crate::workflow::update_step_status(
        &conn,
        &implement_id,
        WorkflowStepStatus::Running,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    // Guard A: predecessor at pos 1 is running → pos 2 predecessor check is false.
    assert!(
        !crate::workflow::predecessor_completed(&conn, &run.id, 2).unwrap(),
        "Guard A: predecessor_completed must be false while implement is running"
    );

    // Now simulate the premature duplicate row that the bug caused.
    let premature_id =
        crate::workflow::insert_step(&conn, &run.id, "workflow:lint-fix", "workflow", false, 2, 0)
            .unwrap();
    crate::workflow::update_step_status(
        &conn,
        &premature_id,
        WorkflowStepStatus::Running,
        None,
        None,
        None,
        None,
        Some(0),
    )
    .unwrap();

    // Guard B: lint-fix row already exists → active_step_exists must be true,
    // preventing the legitimate poll from inserting a second row.
    assert!(
        crate::workflow::active_step_exists(&conn, &run.id, 2, 0, "workflow:lint-fix").unwrap(),
        "Guard B: active_step_exists must be true for the already-inserted duplicate"
    );

    // After implement completes, Guard A is satisfied but Guard B still blocks
    // re-insertion because a row is already present (running).
    crate::workflow::update_step_status(
        &conn,
        &implement_id,
        WorkflowStepStatus::Completed,
        None,
        None,
        None,
        None,
        Some(0),
    )
    .unwrap();
    assert!(
        crate::workflow::predecessor_completed(&conn, &run.id, 2).unwrap(),
        "predecessor_completed must be true once implement is completed"
    );
    assert!(
        crate::workflow::active_step_exists(&conn, &run.id, 2, 0, "workflow:lint-fix").unwrap(),
        "Guard B still blocks a second insertion while the first row is running"
    );
}

#[test]
fn test_active_step_exists_false_for_skipped() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "workflow:lint-fix", "workflow", false, 2, 0)
            .unwrap();
    crate::workflow::update_step_status(
        &conn,
        &step_id,
        WorkflowStepStatus::Skipped,
        None,
        None,
        None,
        None,
        Some(0),
    )
    .unwrap();
    // Skipped is terminal — retries are allowed, so active_step_exists returns false.
    assert!(
        !crate::workflow::active_step_exists(&conn, &run.id, 2, 0, "workflow:lint-fix").unwrap()
    );
}

#[test]
fn test_active_step_exists_false_for_timed_out() {
    let conn = setup_db();
    let (_parent, run) = make_workflow_run(&conn);
    let step_id =
        crate::workflow::insert_step(&conn, &run.id, "workflow:lint-fix", "workflow", false, 2, 0)
            .unwrap();
    crate::workflow::update_step_status(
        &conn,
        &step_id,
        WorkflowStepStatus::TimedOut,
        None,
        None,
        None,
        None,
        Some(0),
    )
    .unwrap();
    // TimedOut is terminal — retries are allowed, so active_step_exists returns false.
    assert!(
        !crate::workflow::active_step_exists(&conn, &run.id, 2, 0, "workflow:lint-fix").unwrap()
    );
}

// ---------------------------------------------------------------------------
// reap_finalization_stuck_workflow_runs — false-positive guard for in-flight
// actor cleanup (issue #2787)
// ---------------------------------------------------------------------------

/// Insert one terminal step + its linked agent_run into an existing workflow run.
/// Use this to build up multi-step scenarios without re-creating the parent run.
fn insert_step_with_agent(
    conn: &Connection,
    run_id: &str,
    step_id: &str,
    step_name: &str,
    position: i64,
    step_status: &str,
    agent_status: &str,
    ended_at: &str,
) {
    let agent_mgr = AgentManager::new(conn);
    let agent_run = agent_mgr.create_run(None, "actor", None).unwrap();

    conn.execute(
        "UPDATE agent_runs SET status = :status WHERE id = :id",
        named_params! { ":status": agent_status, ":id": agent_run.id },
    )
    .unwrap();

    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          ended_at, child_run_id) \
         VALUES (:step_id, :run_id, :step_name, 'actor', :position, :step_status, 0, \
                 :ended_at, :child_run_id)",
        named_params! {
            ":step_id": step_id,
            ":run_id": run_id,
            ":step_name": step_name,
            ":position": position,
            ":step_status": step_status,
            ":ended_at": ended_at,
            ":child_run_id": agent_run.id,
        },
    )
    .unwrap();
}

/// Insert a workflow run in 'running' status with an actor step whose record
/// is already terminal but whose linked agent_run is in `agent_status`. The
/// step's ended_at is stamped at `step_ended_at` so callers can put it well
/// past the reaper threshold.
fn insert_root_run_with_actor_step(
    conn: &Connection,
    run_id: &str,
    step_id: &str,
    agent_status: &str,
    step_ended_at: &str,
) {
    insert_running_root_run(conn, run_id);
    insert_step_with_agent(
        conn,
        run_id,
        step_id,
        "implement",
        0,
        "completed",
        agent_status,
        step_ended_at,
    );
}

#[test]
fn test_reap_finalization_skips_run_with_in_flight_actor_cleanup() {
    // The scenario from issue #2787: an actor step's record is already
    // terminal but the agent subprocess is still cleaning up. The reaper
    // must not finalize the parent in this window.
    let conn = setup_db();
    insert_root_run_with_actor_step(
        &conn,
        "run-actor-cleanup",
        "step-impl",
        "running",              // agent process not yet exited
        "2020-01-01T00:00:00Z", // step "ended" well past any threshold
    );

    let reaped = crate::workflow::reap_finalization_stuck_workflow_runs(&conn, 60).unwrap();
    assert_eq!(
        reaped, 0,
        "must not finalize a run whose actor agent is still running"
    );
    assert_eq!(
        get_run_status(&conn, "run-actor-cleanup"),
        "running",
        "parent must remain running while actor cleanup is in flight"
    );
}

#[test]
fn test_reap_finalization_flags_orphan_run_for_resume() {
    // Regression for the legitimate case from #1777: all steps did finish
    // (agent_run flipped to 'completed') but the parent's status update
    // failed. The reaper must act on this run — but now flags it needs_resume
    // instead of completed, so the resume pipeline determines true outcome.
    let conn = setup_db();
    insert_root_run_with_actor_step(
        &conn,
        "run-finalization-failed",
        "step-impl",
        "completed",            // agent fully done
        "2020-01-01T00:00:00Z", // step "ended" well past any threshold
    );

    let reaped = crate::workflow::reap_finalization_stuck_workflow_runs(&conn, 60).unwrap();
    assert_eq!(
        reaped, 1,
        "must act on a run whose actor agent is fully done"
    );
    assert_eq!(
        get_run_status(&conn, "run-finalization-failed"),
        "needs_resume",
        "parent must transition to needs_resume, never completed"
    );
}

#[test]
fn test_reap_finalization_keeps_failed_status_when_step_failed() {
    // When at least one step failed, the reaper must still transition the
    // stuck run to 'failed' (not needs_resume).
    let conn = setup_db();
    insert_running_root_run(&conn, "run-step-failed");
    insert_step_with_agent(
        &conn,
        "run-step-failed",
        "step-failed-1",
        "implement",
        0,
        "failed",
        "completed",
        "2020-01-01T00:00:00Z",
    );

    let reaped = crate::workflow::reap_finalization_stuck_workflow_runs(&conn, 60).unwrap();
    assert_eq!(reaped, 1, "must finalize a run with a failed step");
    assert_eq!(
        get_run_status(&conn, "run-step-failed"),
        "failed",
        "run with a failed step must transition to failed"
    );
}

#[test]
fn test_reap_finalization_orphan_mid_body_flagged_for_resume() {
    // Acceptance-criterion test for issue #2819: a workflow run crashed
    // mid-body (only 2 of N steps written, no pending/running/waiting steps,
    // all linked agent_runs terminal). The reaper must flag it needs_resume,
    // never completed.
    let conn = setup_db();
    insert_running_root_run(&conn, "run-orphan-mid-body");

    // Step 0 — plan — completed
    insert_step_with_agent(
        &conn,
        "run-orphan-mid-body",
        "step-plan",
        "plan",
        0,
        "completed",
        "completed",
        "2020-01-01T00:00:00Z",
    );

    // Step 1 — implement — completed; engine died before scheduling step 2.
    // Steps 2-4 (lint-fix, fmt-fix, push-and-pr) were never scheduled —
    // no rows exist for them. This is the orphan-mid-body scenario.
    insert_step_with_agent(
        &conn,
        "run-orphan-mid-body",
        "step-impl",
        "implement",
        1,
        "completed",
        "completed",
        "2020-01-01T00:00:00Z",
    );

    let reaped = crate::workflow::reap_finalization_stuck_workflow_runs(&conn, 60).unwrap();
    assert_eq!(reaped, 1, "reaper must act on the orphaned mid-body run");
    let status = get_run_status(&conn, "run-orphan-mid-body");
    assert_eq!(
        status, "needs_resume",
        "orphaned mid-body run must land in needs_resume, not completed (got: {status})"
    );
}
