#![allow(unused_imports)]

use super::*;
use crate::agent::AgentManager;
use rusqlite::{params, Connection};
use std::collections::HashMap;

#[test]
fn test_create_workflow_run() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run(
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
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run(
            "test",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some(r#"{"name":"test"}"#),
        )
        .unwrap();

    let fetched = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(
        fetched.definition_snapshot.as_deref(),
        Some(r#"{"name":"test"}"#)
    );
}

#[test]
fn test_create_workflow_run_with_repo_id_round_trip() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run_with_targets(
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
            None,
        )
        .unwrap();

    // Verify the struct returned by create reflects the inputs.
    assert_eq!(run.repo_id.as_deref(), Some("r1"));
    assert_eq!(run.ticket_id, None);

    // Read back from DB and assert columns are persisted correctly.
    let fetched = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(fetched.repo_id.as_deref(), Some("r1"));
    assert_eq!(fetched.ticket_id, None);
}

#[test]
fn test_active_run_counts_by_repo_empty() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let counts = mgr.active_run_counts_by_repo().unwrap();
    assert!(
        counts.is_empty(),
        "expected no counts with no workflow runs"
    );
}

#[test]
fn test_active_run_counts_by_repo_with_runs() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let mgr = WorkflowManager::new(&conn);

    // Create one pending and one running run for repo r1.
    let run1 = mgr
        .create_workflow_run_with_targets(
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
            None,
        )
        .unwrap();
    // Advance run1 to running.
    conn.execute(
        "UPDATE workflow_runs SET status = 'running' WHERE id = ?1",
        [&run1.id],
    )
    .unwrap();
    let _run2 = mgr
        .create_workflow_run_with_targets(
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
            None,
        )
        .unwrap();
    // run2 stays at pending (default).

    let counts = mgr.active_run_counts_by_repo().unwrap();
    let c = counts.get("r1").expect("r1 should be in map");
    assert_eq!(c.running, 1, "expected 1 running");
    assert_eq!(c.pending, 1, "expected 1 pending");
    assert_eq!(c.waiting, 0, "expected 0 waiting");
}

#[test]
fn test_active_run_counts_by_repo_excludes_completed() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let mgr = WorkflowManager::new(&conn);

    let run = mgr
        .create_workflow_run_with_targets(
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
            None,
        )
        .unwrap();
    conn.execute(
        "UPDATE workflow_runs SET status = 'completed' WHERE id = ?1",
        [&run.id],
    )
    .unwrap();

    let counts = mgr.active_run_counts_by_repo().unwrap();
    assert!(
        !counts.contains_key("r1"),
        "completed runs must not appear in active counts"
    );
}

#[test]
fn test_create_workflow_run_with_ticket_id_round_trip() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    insert_test_ticket(&conn, "tkt-rt-1", "r1");

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run_with_targets(
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
            None,
        )
        .unwrap();

    // Verify the struct returned by create reflects the inputs.
    assert_eq!(run.ticket_id.as_deref(), Some("tkt-rt-1"));
    assert_eq!(run.repo_id, None);

    // Read back from DB and assert columns are persisted correctly.
    let fetched = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(fetched.ticket_id.as_deref(), Some("tkt-rt-1"));
    assert_eq!(fetched.repo_id, None);
}

#[test]
fn test_insert_step_with_iteration() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let step_id = mgr
        .insert_step(&run.id, "review", "reviewer", false, 0, 2)
        .unwrap();

    let steps = mgr.get_workflow_steps(&run.id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].id, step_id);
    assert_eq!(steps[0].step_name, "review");
    assert_eq!(steps[0].iteration, 2);
}

#[test]
fn test_update_step_with_markers() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "review", "reviewer", false, 0, 0)
        .unwrap();

    mgr.update_step_status(
        &step_id,
        WorkflowStepStatus::Completed,
        None,
        Some("Found issues"),
        Some("2 issues in lib.rs"),
        Some(r#"["has_review_issues"]"#),
        Some(0),
    )
    .unwrap();

    let steps = mgr.get_workflow_steps(&run.id).unwrap();
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
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "review", "reviewer", false, 0, 0)
        .unwrap();

    let structured_json = r#"{"approved":true,"summary":"All good"}"#;
    mgr.update_step_status_full(
        &step_id,
        WorkflowStepStatus::Completed,
        None,
        Some("result text"),
        Some("All good"),
        Some(r#"[]"#),
        Some(0),
        Some(structured_json),
    )
    .unwrap();

    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
    assert_eq!(step.structured_output.as_deref(), Some(structured_json));
    assert_eq!(step.context_out.as_deref(), Some("All good"));
    assert_eq!(step.result_text.as_deref(), Some("result text"));
}

#[test]
fn test_update_step_status_full_without_structured_output() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = mgr
        .insert_step(&run.id, "review", "reviewer", false, 0, 0)
        .unwrap();

    mgr.update_step_status_full(
        &step_id,
        WorkflowStepStatus::Completed,
        None,
        Some("result text"),
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
    assert!(step.structured_output.is_none());
}

#[test]
fn test_list_workflow_runs() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
    let p2 = agent_mgr.create_run(Some("w1"), "wf2", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    mgr.create_workflow_run("test-a", Some("w1"), &p1.id, false, "manual", None)
        .unwrap();
    mgr.create_workflow_run("test-b", Some("w1"), &p2.id, true, "pr", None)
        .unwrap();

    let runs = mgr.list_workflow_runs("w1").unwrap();
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
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
    let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    mgr.create_workflow_run("flow-a", Some("w1"), &p1.id, false, "manual", None)
        .unwrap();
    mgr.create_workflow_run("flow-b", Some("w2"), &p2.id, false, "manual", None)
        .unwrap();

    // list_all returns both runs regardless of worktree
    let all = mgr.list_all_workflow_runs(100).unwrap();
    assert_eq!(all.len(), 2);
    let names: Vec<&str> = all.iter().map(|r| r.workflow_name.as_str()).collect();
    assert!(names.contains(&"flow-a"));
    assert!(names.contains(&"flow-b"));
}

#[test]
fn test_list_all_workflow_runs_respects_limit() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    let mgr = WorkflowManager::new(&conn);
    for i in 0..5 {
        let p = agent_mgr
            .create_run(Some("w1"), &format!("wf{i}"), None, None)
            .unwrap();
        mgr.create_workflow_run(
            &format!("flow-{i}"),
            Some("w1"),
            &p.id,
            false,
            "manual",
            None,
        )
        .unwrap();
    }

    let limited = mgr.list_all_workflow_runs(3).unwrap();
    assert_eq!(limited.len(), 3);
}

#[test]
fn test_list_all_workflow_runs_empty() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let runs = mgr.list_all_workflow_runs(50).unwrap();
    assert!(runs.is_empty());
}

#[test]
fn test_list_all_workflow_runs_includes_ephemeral() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let mgr = WorkflowManager::new(&conn);

    // Create a normal run (with worktree)
    let parent1 = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    mgr.create_workflow_run("normal-wf", Some("w1"), &parent1.id, false, "manual", None)
        .unwrap();

    // Create an ephemeral run (no worktree)
    let parent2 = agent_mgr
        .create_run(None, "ephemeral workflow", None, None)
        .unwrap();
    let ephemeral = mgr
        .create_workflow_run("ephemeral-wf", None, &parent2.id, false, "manual", None)
        .unwrap();

    let all = mgr.list_all_workflow_runs(100).unwrap();
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
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
    let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    mgr.create_workflow_run("active-run", Some("w1"), &p1.id, false, "manual", None)
        .unwrap();
    mgr.create_workflow_run("merged-run", Some("w2"), &p2.id, false, "manual", None)
        .unwrap();

    let all = mgr.list_all_workflow_runs(100).unwrap();
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
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
    let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    mgr.create_workflow_run("active-run", Some("w1"), &p1.id, false, "manual", None)
        .unwrap();
    mgr.create_workflow_run("abandoned-run", Some("w2"), &p2.id, false, "manual", None)
        .unwrap();

    let all = mgr.list_all_workflow_runs(100).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].workflow_name, "active-run");
}

#[test]
fn test_list_all_workflow_runs_includes_ephemeral_and_active() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let mgr = WorkflowManager::new(&conn);

    // Active worktree run
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
    mgr.create_workflow_run("active-run", Some("w1"), &p1.id, false, "manual", None)
        .unwrap();

    // Ephemeral run (no worktree)
    let p2 = agent_mgr.create_run(None, "wf2", None, None).unwrap();
    mgr.create_workflow_run("ephemeral-run", None, &p2.id, false, "manual", None)
        .unwrap();

    let all = mgr.list_all_workflow_runs(100).unwrap();
    assert_eq!(all.len(), 2);
    let names: Vec<&str> = all.iter().map(|r| r.workflow_name.as_str()).collect();
    assert!(names.contains(&"active-run"));
    assert!(names.contains(&"ephemeral-run"));
}

#[test]
fn test_list_all_workflow_runs_filtered_paginated_status_filter() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let mgr = WorkflowManager::new(&conn);

    // Create one run and leave it in Pending state.
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
    mgr.create_workflow_run("pending-run", Some("w1"), &p1.id, false, "manual", None)
        .unwrap();

    // Create a second run and advance it to Completed.
    let p2 = agent_mgr.create_run(Some("w1"), "wf2", None, None).unwrap();
    let r2 = mgr
        .create_workflow_run("done-run", Some("w1"), &p2.id, false, "manual", None)
        .unwrap();
    mgr.update_workflow_status(&r2.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    let completed = mgr
        .list_all_workflow_runs_filtered_paginated(Some(WorkflowRunStatus::Completed), 100, 0)
        .unwrap();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].workflow_name, "done-run");

    let pending = mgr
        .list_all_workflow_runs_filtered_paginated(Some(WorkflowRunStatus::Pending), 100, 0)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].workflow_name, "pending-run");
}

#[test]
fn test_list_all_workflow_runs_filtered_paginated_offset() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let mgr = WorkflowManager::new(&conn);

    for i in 0..4 {
        let p = agent_mgr
            .create_run(Some("w1"), &format!("wf{i}"), None, None)
            .unwrap();
        mgr.create_workflow_run(
            &format!("flow-{i}"),
            Some("w1"),
            &p.id,
            false,
            "manual",
            None,
        )
        .unwrap();
    }

    let page1 = mgr
        .list_all_workflow_runs_filtered_paginated(None, 2, 0)
        .unwrap();
    assert_eq!(page1.len(), 2);

    let page2 = mgr
        .list_all_workflow_runs_filtered_paginated(None, 2, 2)
        .unwrap();
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
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
    let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    // Use create_workflow_run_with_targets to set repo_id so the query can filter by it
    mgr.create_workflow_run_with_targets(
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
        None,
    )
    .unwrap();
    mgr.create_workflow_run_with_targets(
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
        None,
    )
    .unwrap();

    let runs = mgr.list_workflow_runs_by_repo_id("r1", 100, 0).unwrap();
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
    let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
    let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    mgr.create_workflow_run("only-w1", Some("w1"), &p1.id, false, "manual", None)
        .unwrap();
    mgr.create_workflow_run("only-w2", Some("w2"), &p2.id, false, "manual", None)
        .unwrap();

    // Scoped: only w1's run
    let scoped = mgr.list_workflow_runs_for_scope(Some("w1"), 50).unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].workflow_name, "only-w1");

    // Global: both runs
    let global = mgr.list_workflow_runs_for_scope(None, 50).unwrap();
    assert_eq!(global.len(), 2);
}

#[test]
fn test_list_workflow_runs_for_scope_global_limit() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let mgr = WorkflowManager::new(&conn);
    for i in 0..5 {
        let p = agent_mgr
            .create_run(Some("w1"), &format!("wf{i}"), None, None)
            .unwrap();
        mgr.create_workflow_run(
            &format!("flow-{i}"),
            Some("w1"),
            &p.id,
            false,
            "manual",
            None,
        )
        .unwrap();
    }
    let limited = mgr.list_workflow_runs_for_scope(None, 2).unwrap();
    assert_eq!(limited.len(), 2);
}

#[test]
fn test_get_workflow_run_not_found() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let result = mgr.get_workflow_run("nonexistent").unwrap();
    assert!(result.is_none());
}

#[test]
fn test_get_step_by_id() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let step_id = mgr
        .insert_step(&run.id, "build", "actor", false, 0, 0)
        .unwrap();

    let step = mgr.get_step_by_id(&step_id).unwrap();
    assert!(step.is_some());
    let step = step.unwrap();
    assert_eq!(step.id, step_id);
    assert_eq!(step.step_name, "build");
    assert_eq!(step.role, "actor");

    let missing = mgr.get_step_by_id("nonexistent").unwrap();
    assert!(missing.is_none());
}

#[test]
fn test_purge_all_terminal_statuses() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let a2 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let a3 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let a4 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let r_completed = mgr
        .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
        .unwrap();
    let r_failed = mgr
        .create_workflow_run("t", Some("w1"), &a2.id, false, "manual", None)
        .unwrap();
    let r_cancelled = mgr
        .create_workflow_run("t", Some("w1"), &a3.id, false, "manual", None)
        .unwrap();
    let r_running = mgr
        .create_workflow_run("t", Some("w1"), &a4.id, false, "manual", None)
        .unwrap();

    mgr.update_workflow_status(&r_completed.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.update_workflow_status(&r_failed.id, WorkflowRunStatus::Failed, None, None)
        .unwrap();
    mgr.update_workflow_status(&r_cancelled.id, WorkflowRunStatus::Cancelled, None, None)
        .unwrap();
    mgr.update_workflow_status(&r_running.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let deleted = mgr
        .purge(None, &["completed", "failed", "cancelled"])
        .unwrap();
    assert_eq!(deleted, 3);

    // running run must still exist
    assert!(mgr.get_workflow_run(&r_running.id).unwrap().is_some());
    // terminal runs must be gone
    assert!(mgr.get_workflow_run(&r_completed.id).unwrap().is_none());
    assert!(mgr.get_workflow_run(&r_failed.id).unwrap().is_none());
    assert!(mgr.get_workflow_run(&r_cancelled.id).unwrap().is_none());
}

#[test]
fn test_purge_single_status_filter() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let a2 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let r_completed = mgr
        .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
        .unwrap();
    let r_failed = mgr
        .create_workflow_run("t", Some("w1"), &a2.id, false, "manual", None)
        .unwrap();

    mgr.update_workflow_status(&r_completed.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.update_workflow_status(&r_failed.id, WorkflowRunStatus::Failed, None, None)
        .unwrap();

    // only purge completed
    let deleted = mgr.purge(None, &["completed"]).unwrap();
    assert_eq!(deleted, 1);

    assert!(mgr.get_workflow_run(&r_completed.id).unwrap().is_none());
    assert!(mgr.get_workflow_run(&r_failed.id).unwrap().is_some());
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
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let a2 = agent_mgr.create_run(Some("w2"), "wf", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run_r1 = mgr
        .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
        .unwrap();
    let run_r2 = mgr
        .create_workflow_run("t", Some("w2"), &a2.id, false, "manual", None)
        .unwrap();

    mgr.update_workflow_status(&run_r1.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.update_workflow_status(&run_r2.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    // scope to r1 only
    let deleted = mgr.purge(Some("r1"), &["completed"]).unwrap();
    assert_eq!(deleted, 1);

    assert!(mgr.get_workflow_run(&run_r1.id).unwrap().is_none());
    assert!(mgr.get_workflow_run(&run_r2.id).unwrap().is_some());
}

#[test]
fn test_purge_cascade_deletes_steps() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
        .unwrap();
    mgr.insert_step(&run.id, "step1", "actor", true, 0, 0)
        .unwrap();
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    let deleted = mgr.purge(None, &["completed"]).unwrap();
    assert_eq!(deleted, 1);

    // steps must be gone (cascade)
    let steps = mgr.get_workflow_steps(&run.id).unwrap();
    assert!(steps.is_empty());
}

#[test]
fn test_purge_count_matches_purge() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let a2 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let r1 = mgr
        .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
        .unwrap();
    let r2 = mgr
        .create_workflow_run("t", Some("w1"), &a2.id, false, "manual", None)
        .unwrap();
    mgr.update_workflow_status(&r1.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.update_workflow_status(&r2.id, WorkflowRunStatus::Failed, None, None)
        .unwrap();

    let statuses = &["completed", "failed", "cancelled"];
    let count = mgr.purge_count(None, statuses).unwrap();
    assert_eq!(count, 2);

    let deleted = mgr.purge(None, statuses).unwrap();
    assert_eq!(deleted, count);
}

#[test]
fn test_purge_noop_when_no_matches() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
        .unwrap();
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let count = mgr
        .purge_count(None, &["completed", "failed", "cancelled"])
        .unwrap();
    assert_eq!(count, 0);

    let deleted = mgr
        .purge(None, &["completed", "failed", "cancelled"])
        .unwrap();
    assert_eq!(deleted, 0);

    assert!(mgr.get_workflow_run(&run.id).unwrap().is_some());
}

#[test]
fn test_purge_empty_statuses_is_noop() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    assert_eq!(mgr.purge(None, &[]).unwrap(), 0);
    assert_eq!(mgr.purge_count(None, &[]).unwrap(), 0);
}

/// Repo-scoped purge must NOT delete global workflow runs (worktree_id IS NULL).
#[test]
fn test_purge_repo_scoped_does_not_delete_global_runs() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    // Create a global run (no worktree) and a run scoped to w1.
    let a_global = agent_mgr.create_run(None, "wf", None, None).unwrap();
    let a_w1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

    let mgr = WorkflowManager::new(&conn);
    let run_global = mgr
        .create_workflow_run("t", None, &a_global.id, false, "manual", None)
        .unwrap();
    let run_w1 = mgr
        .create_workflow_run("t", Some("w1"), &a_w1.id, false, "manual", None)
        .unwrap();

    mgr.update_workflow_status(&run_global.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();
    mgr.update_workflow_status(&run_w1.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    // Scope purge to r1 — must only delete the worktree-bound run.
    assert_eq!(mgr.purge_count(Some("r1"), &["completed"]).unwrap(), 1);
    let deleted = mgr.purge(Some("r1"), &["completed"]).unwrap();
    assert_eq!(deleted, 1);

    // Global run must survive.
    assert!(mgr.get_workflow_run(&run_global.id).unwrap().is_some());
    // w1 run must be gone.
    assert!(mgr.get_workflow_run(&run_w1.id).unwrap().is_none());
}

#[test]
fn test_cancel_run_pending() {
    let conn = setup_db();
    let (mgr, _parent, run) = make_workflow_run(&conn);
    assert_eq!(run.status, WorkflowRunStatus::Pending);

    mgr.cancel_run(&run.id, "user requested").unwrap();

    let updated = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(updated.status, WorkflowRunStatus::Cancelled);
}

#[test]
fn test_cancel_run_running_with_active_steps() {
    let conn = setup_db();
    let (mgr, _parent, run) = make_workflow_run(&conn);

    // Advance run to Running
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    // Insert a Running step with a child agent run
    let child_agent_mgr = AgentManager::new(&conn);
    let child = child_agent_mgr
        .create_run(Some("w1"), "child-step", None, None)
        .unwrap();

    let step_id = mgr
        .insert_step(&run.id, "do-work", "actor", false, 0, 0)
        .unwrap();
    mgr.update_step_status(
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
    mgr.cancel_run(&run.id, "abort").unwrap();

    let updated_run = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(updated_run.status, WorkflowRunStatus::Cancelled);

    let steps = mgr.get_workflow_steps(&run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Failed);

    let agent_run: String = conn
        .query_row(
            "SELECT status FROM agent_runs WHERE id = ?1",
            params![child.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(agent_run, "cancelled");
}

#[test]
fn test_cancel_run_waiting_status() {
    let conn = setup_db();
    let (mgr, _parent, run) = make_workflow_run(&conn);

    // Advance run to Waiting (e.g. at a gate)
    mgr.set_waiting_blocked_on(
        &run.id,
        &BlockedOn::HumanApproval {
            gate_name: "human-gate".to_string(),
            prompt: None,
            options: vec![],
        },
    )
    .unwrap();

    // Insert a Waiting step (no child run)
    let step_id = mgr
        .insert_step(&run.id, "human-gate", "gate", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

    mgr.cancel_run(&run.id, "timed out").unwrap();

    let updated = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(updated.status, WorkflowRunStatus::Cancelled);

    let steps = mgr.get_workflow_steps(&run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
}

#[test]
fn test_cancel_run_skips_terminal_steps() {
    let conn = setup_db();
    let (mgr, _parent, run) = make_workflow_run(&conn);

    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    // A completed step — must not be touched
    let done_step = mgr
        .insert_step(&run.id, "already-done", "actor", false, 0, 0)
        .unwrap();
    mgr.update_step_status(
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
    let active_step = mgr
        .insert_step(&run.id, "in-progress", "actor", false, 1, 0)
        .unwrap();
    set_step_status(&mgr, &active_step, WorkflowStepStatus::Running);

    mgr.cancel_run(&run.id, "stop").unwrap();

    let steps = mgr.get_workflow_steps(&run.id).unwrap();
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
    let (mgr, _parent, run) = make_workflow_run(&conn);

    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None, None)
        .unwrap();

    let err = mgr.cancel_run(&run.id, "too late").unwrap_err();
    assert!(
        err.to_string().contains("terminal state"),
        "expected terminal state error, got: {err}"
    );
}

#[test]
fn test_cancel_run_not_found_returns_error() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);

    let err = mgr.cancel_run("nonexistent-id", "reason").unwrap_err();
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

    let mgr = WorkflowManager::new(&conn);
    let result = mgr.find_resumable_child_run("parent1", "child-wf").unwrap();
    assert!(result.is_some(), "failed child run should be found");
    assert_eq!(result.unwrap().id, "child1");
}

#[test]
fn test_find_resumable_child_run_ignores_completed() {
    let conn = setup_db();
    insert_workflow_run(&conn, "parent1", "parent-wf", "failed", None);
    insert_workflow_run(&conn, "child1", "child-wf", "completed", Some("parent1"));

    let mgr = WorkflowManager::new(&conn);
    let result = mgr.find_resumable_child_run("parent1", "child-wf").unwrap();
    assert!(result.is_none(), "completed child run must not be returned");
}

#[test]
fn test_find_resumable_child_run_ignores_running() {
    let conn = setup_db();
    insert_workflow_run(&conn, "parent1", "parent-wf", "running", None);
    insert_workflow_run(&conn, "child1", "child-wf", "running", Some("parent1"));

    let mgr = WorkflowManager::new(&conn);
    let result = mgr.find_resumable_child_run("parent1", "child-wf").unwrap();
    assert!(result.is_none(), "running child run must not be returned");
}

#[test]
fn test_find_resumable_child_run_ignores_cancelled() {
    let conn = setup_db();
    insert_workflow_run(&conn, "parent1", "parent-wf", "failed", None);
    insert_workflow_run(&conn, "child1", "child-wf", "cancelled", Some("parent1"));

    let mgr = WorkflowManager::new(&conn);
    let result = mgr.find_resumable_child_run("parent1", "child-wf").unwrap();
    assert!(result.is_none(), "cancelled child run must not be returned");
}

#[test]
fn test_find_resumable_child_run_picks_most_recent() {
    let conn = setup_db();
    insert_workflow_run(&conn, "parent1", "parent-wf", "failed", None);

    // Insert two failed child runs with distinct timestamps
    let agent_mgr = AgentManager::new(&conn);
    let p1 = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    let p2 = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at, \
          parent_workflow_run_id) \
         VALUES ('older-child', 'child-wf', NULL, ?1, 'failed', 0, 'manual', \
                 '2025-01-01T00:00:00Z', 'parent1')",
        params![p1.id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at, \
          parent_workflow_run_id) \
         VALUES ('newer-child', 'child-wf', NULL, ?1, 'failed', 0, 'manual', \
                 '2025-06-01T00:00:00Z', 'parent1')",
        params![p2.id],
    )
    .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let result = mgr.find_resumable_child_run("parent1", "child-wf").unwrap();
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
    insert_waiting_run_with_gate(&conn, run_id, "failed", Some("86400s"), None);

    let mgr = WorkflowManager::new(&conn);
    let reaped = mgr.reap_orphaned_workflow_runs().unwrap();
    assert_eq!(reaped, 1);

    // Run should be cancelled.
    let status: String = conn
        .query_row(
            "SELECT status FROM workflow_runs WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "cancelled");

    // Gate step should be timed_out.
    let step_status: String = conn
        .query_row(
            "SELECT status FROM workflow_run_steps WHERE workflow_run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(step_status, "timed_out");
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

    let mgr = WorkflowManager::new(&conn);
    let reaped = mgr.reap_orphaned_workflow_runs().unwrap();
    assert_eq!(reaped, 1);

    let status: String = conn
        .query_row(
            "SELECT status FROM workflow_runs WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "cancelled");
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

    let mgr = WorkflowManager::new(&conn);
    let reaped = mgr.reap_orphaned_workflow_runs().unwrap();
    assert_eq!(reaped, 0);

    let status: String = conn
        .query_row(
            "SELECT status FROM workflow_runs WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "waiting", "run must remain waiting");
}

#[test]
fn test_reap_orphaned_workflow_runs_skips_terminal() {
    let conn = setup_db();
    // Insert a completed run — must not be touched.
    insert_workflow_run(&conn, "run-completed", "test-wf", "completed", None);
    // Insert a cancelled run — must not be touched.
    insert_workflow_run(&conn, "run-cancelled", "test-wf", "cancelled", None);

    let mgr = WorkflowManager::new(&conn);
    let reaped = mgr.reap_orphaned_workflow_runs().unwrap();
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
         VALUES (?1, 'test-wf', NULL, ?2, 'waiting', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        params![run_id, ghost_parent_id],
    )
    .unwrap();

    let step_id = crate::new_id();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          gate_type, gate_timeout, started_at) \
         VALUES (?1, ?2, 'approval-gate', 'gate', 0, 'waiting', 1, \
                 'human_approval', '999999999s', '2099-01-01T00:00:00Z')",
        params![step_id, run_id],
    )
    .unwrap();

    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

    let mgr = WorkflowManager::new(&conn);
    let reaped = mgr.reap_orphaned_workflow_runs().unwrap();
    assert_eq!(
        reaped, 1,
        "purged parent should cause the workflow run to be reaped"
    );

    let status: String = conn
        .query_row(
            "SELECT status FROM workflow_runs WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "cancelled");
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

    let mgr = WorkflowManager::new(&conn);
    let reaped = mgr.reap_orphaned_workflow_runs().unwrap();
    assert_eq!(reaped, 3, "exactly the 3 dead-parent runs should be reaped");

    for dead_id in &["run-dead-1", "run-dead-2", "run-dead-3"] {
        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                params![dead_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "cancelled", "{dead_id} should be cancelled");
    }

    let active_status: String = conn
        .query_row(
            "SELECT status FROM workflow_runs WHERE id = ?1",
            params!["run-active"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        active_status, "waiting",
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
         VALUES (?1, ?2, ?3, 'script', 0, 'running', 0, ?4, ?5)",
        params![step_id, run_id, step_name, pid, started],
    )
    .unwrap();
    step_id
}

/// Helper: create a workflow_run and return its id.
fn make_workflow_run_id(conn: &Connection) -> String {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let mgr = WorkflowManager::new(conn);
    let run = mgr
        .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
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

    let mgr = WorkflowManager::new(&conn);
    let reaped = mgr.reap_orphaned_script_steps().unwrap();
    assert_eq!(reaped, 1);

    let status: String = conn
        .query_row(
            "SELECT status FROM workflow_run_steps WHERE id = ?1",
            params![step_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "failed");

    let result: String = conn
        .query_row(
            "SELECT result_text FROM workflow_run_steps WHERE id = ?1",
            params![step_id],
            |r| r.get(0),
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

    let mgr = WorkflowManager::new(&conn);
    let reaped = mgr.reap_orphaned_script_steps().unwrap();
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
         VALUES (?1, ?2, 'script-done', 'script', 0, 'completed', 0, 99999)",
        params![step_id, run_id],
    )
    .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let reaped = mgr.reap_orphaned_script_steps().unwrap();
    assert_eq!(reaped, 0);

    let status: String = conn
        .query_row(
            "SELECT status FROM workflow_run_steps WHERE id = ?1",
            params![step_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "completed");
}

/// A running step with child_run_id set (agent step) must NOT be reaped.
#[test]
fn test_reap_orphaned_script_steps_skips_agent_step() {
    let conn = setup_db();
    let run_id = make_workflow_run_id(&conn);

    // Insert an actor step with child_run_id set — simulates an agent step.
    let step_id = crate::new_id();
    let agent_mgr = AgentManager::new(&conn);
    let child_run = agent_mgr
        .create_run(Some("w1"), "agent", None, None)
        .unwrap();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          child_run_id, subprocess_pid) \
         VALUES (?1, ?2, 'agent-step', 'actor', 0, 'running', 0, ?3, 99999)",
        params![step_id, run_id, child_run.id],
    )
    .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let reaped = mgr.reap_orphaned_script_steps().unwrap();
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
    let live_pid = std::process::id();
    let s3 = insert_running_script_step_with_pid(
        &conn,
        &run_id,
        "step-3",
        Some(live_pid as i64),
        Some(&chrono::Utc::now().to_rfc3339()),
    );

    let mgr = WorkflowManager::new(&conn);
    let reaped = mgr.reap_orphaned_script_steps().unwrap();
    assert_eq!(reaped, 2, "only the 2 dead-PID steps should be reaped");

    for dead_step in &[s1, s2] {
        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_run_steps WHERE id = ?1",
                params![dead_step],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "failed", "{dead_step} should be failed");
    }

    let live_status: String = conn
        .query_row(
            "SELECT status FROM workflow_run_steps WHERE id = ?1",
            params![s3],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(live_status, "running", "live step must remain running");
}

#[test]
fn test_list_workflow_runs_paginated_limit_and_offset() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let mgr = WorkflowManager::new(&conn);

    // Create 5 runs for worktree w1
    for i in 0..5 {
        let p = agent_mgr
            .create_run(Some("w1"), &format!("wf-paginated-{i}"), None, None)
            .unwrap();
        mgr.create_workflow_run(
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
    let page1 = mgr.list_workflow_runs_paginated("w1", 2, 0).unwrap();
    assert_eq!(page1.len(), 2);

    // Second page: limit=2, offset=2
    let page2 = mgr.list_workflow_runs_paginated("w1", 2, 2).unwrap();
    assert_eq!(page2.len(), 2);

    // Third page: limit=2, offset=4 — only 1 remaining
    let page3 = mgr.list_workflow_runs_paginated("w1", 2, 4).unwrap();
    assert_eq!(page3.len(), 1);

    // Pages must not overlap
    let ids1: Vec<_> = page1.iter().map(|r| r.id.clone()).collect();
    let ids2: Vec<_> = page2.iter().map(|r| r.id.clone()).collect();
    assert!(
        ids1.iter().all(|id| !ids2.contains(id)),
        "page1 and page2 must not share runs"
    );

    // All 5 runs returned when limit exceeds count
    let all = mgr.list_workflow_runs_paginated("w1", 100, 0).unwrap();
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
    let mgr = WorkflowManager::new(&conn);

    let p1 = agent_mgr
        .create_run(Some("w1"), "wf-w1", None, None)
        .unwrap();
    let p2 = agent_mgr
        .create_run(Some("w2"), "wf-w2", None, None)
        .unwrap();
    mgr.create_workflow_run("run-w1", Some("w1"), &p1.id, false, "manual", None)
        .unwrap();
    mgr.create_workflow_run("run-w2", Some("w2"), &p2.id, false, "manual", None)
        .unwrap();

    let w1_runs = mgr.list_workflow_runs_paginated("w1", 100, 0).unwrap();
    assert_eq!(w1_runs.len(), 1);
    assert_eq!(w1_runs[0].workflow_name, "run-w1");

    let w2_runs = mgr.list_workflow_runs_paginated("w2", 100, 0).unwrap();
    assert_eq!(w2_runs.len(), 1);
    assert_eq!(w2_runs[0].workflow_name, "run-w2");
}

#[test]
fn test_list_workflow_runs_by_repo_id_offset_pagination() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let mgr = WorkflowManager::new(&conn);

    // Create 4 runs for repo r1 (all on active worktree w1)
    for i in 0..4 {
        let p = agent_mgr
            .create_run(Some("w1"), &format!("wf-repo-{i}"), None, None)
            .unwrap();
        mgr.create_workflow_run_with_targets(
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
            None,
        )
        .unwrap();
    }

    // First page
    let page1 = mgr.list_workflow_runs_by_repo_id("r1", 2, 0).unwrap();
    assert_eq!(page1.len(), 2);

    // Second page
    let page2 = mgr.list_workflow_runs_by_repo_id("r1", 2, 2).unwrap();
    assert_eq!(page2.len(), 2);

    // Pages must not overlap
    let ids1: Vec<_> = page1.iter().map(|r| r.id.clone()).collect();
    let ids2: Vec<_> = page2.iter().map(|r| r.id.clone()).collect();
    assert!(
        ids1.iter().all(|id| !ids2.contains(id)),
        "page1 and page2 must not share runs"
    );

    // Beyond end returns empty
    let beyond = mgr.list_workflow_runs_by_repo_id("r1", 2, 10).unwrap();
    assert!(beyond.is_empty());
}

#[test]
fn test_list_root_workflow_runs_excludes_children() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));

    let mgr = WorkflowManager::new(&conn);
    let roots = mgr.list_root_workflow_runs(100).unwrap();
    let ids: Vec<&str> = roots.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&"root1"), "root run should appear");
    assert!(!ids.contains(&"child1"), "child run must not appear");
}

#[test]
fn test_list_root_workflow_runs_empty() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let roots = mgr.list_root_workflow_runs(100).unwrap();
    assert!(roots.is_empty());
}

#[test]
fn test_get_active_chain_no_children() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);

    let mgr = WorkflowManager::new(&conn);
    let chain = mgr.get_active_chain_for_run("root1").unwrap();
    assert!(chain.is_empty(), "no children → empty chain");
}

#[test]
fn test_get_active_chain_single_child() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));

    let mgr = WorkflowManager::new(&conn);
    let chain = mgr.get_active_chain_for_run("root1").unwrap();
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

    let mgr = WorkflowManager::new(&conn);
    let chain = mgr.get_active_chain_for_run("root1").unwrap();
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

    let mgr = WorkflowManager::new(&conn);
    let chain = mgr.get_active_chain_for_run("root1").unwrap();
    assert!(chain.is_empty(), "completed child must not appear in chain");
}

#[test]
fn test_get_step_summaries_no_children() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    insert_running_step(&conn, "step1", "root1", "my-step");

    let mgr = WorkflowManager::new(&conn);
    let summaries = mgr.get_step_summaries_for_runs(&["root1"]).unwrap();
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

    let mgr = WorkflowManager::new(&conn);
    let summaries = mgr.get_step_summaries_for_runs(&["root1"]).unwrap();
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

    let mgr = WorkflowManager::new(&conn);
    let summaries = mgr.get_step_summaries_for_runs(&["root1"]).unwrap();
    let s = summaries.get("root1").expect("summary should exist");
    assert_eq!(s.step_name, "grand-step");
    // root + first child (grand is leaf, excluded)
    assert_eq!(s.workflow_chain, vec!["root-wf", "child-wf"]);
}

#[test]
fn test_get_step_summaries_empty_run_ids() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let summaries = mgr.get_step_summaries_for_runs(&[]).unwrap();
    assert!(summaries.is_empty());
}

#[test]
fn test_get_step_summaries_no_running_step() {
    let conn = setup_db();
    insert_workflow_run(&conn, "root1", "root-wf", "running", None);
    // no steps inserted

    let mgr = WorkflowManager::new(&conn);
    let summaries = mgr.get_step_summaries_for_runs(&["root1"]).unwrap();
    assert!(
        !summaries.contains_key("root1"),
        "no running step → no entry in map"
    );
}

#[test]
fn test_resolve_run_context_run_not_found() {
    let conn = setup_db();
    let config = crate::config::Config::default();
    let mgr = WorkflowManager::new(&conn);
    let err = mgr
        .resolve_run_context("nonexistent-id", &config)
        .unwrap_err();
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
         VALUES ('wt-exists', 'r1', 'test-wt', 'feat/test', ?1, 'active', '2024-01-01T00:00:00Z')",
        params![wt_path],
    )
    .unwrap();

    let run_id = insert_workflow_run_with_targets(&conn, Some("wt-exists"), None);
    let mgr = WorkflowManager::new(&conn);
    let ctx = mgr.resolve_run_context(&run_id, &config).unwrap();

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
    let mgr = WorkflowManager::new(&conn);
    let err = mgr.resolve_run_context(&run_id, &config).unwrap_err();
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
    let mgr = WorkflowManager::new(&conn);
    let ctx = mgr.resolve_run_context(&run_id, &config).unwrap();

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
    let mgr = WorkflowManager::new(&conn);
    let err = mgr.resolve_run_context(&run_id, &config).unwrap_err();
    assert!(
        err.to_string()
            .contains("has no associated worktree or repo"),
        "expected missing-targets error, got: {err}"
    );
}

#[test]
fn test_set_waiting_blocked_on_atomically_sets_status_and_blocked_on() {
    let conn = setup_db();
    let (mgr, _parent, run) = make_workflow_run(&conn);

    // Start from Running
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let blocked = BlockedOn::HumanApproval {
        gate_name: "deploy-gate".to_string(),
        prompt: Some("Approve deploy?".to_string()),
        options: vec![],
    };

    mgr.set_waiting_blocked_on(&run.id, &blocked).unwrap();

    let updated = mgr.get_workflow_run(&run.id).unwrap().unwrap();
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
    let (mgr, _parent, run) = make_workflow_run(&conn);

    // Set waiting with blocked_on
    let blocked = BlockedOn::PrChecks {
        gate_name: "ci-gate".to_string(),
    };
    mgr.set_waiting_blocked_on(&run.id, &blocked).unwrap();

    let waiting = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(waiting.status, WorkflowRunStatus::Waiting);
    assert!(waiting.blocked_on.is_some());

    // Transition to Running — blocked_on must be auto-cleared
    mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let running = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(running.status, WorkflowRunStatus::Running);
    assert!(
        running.blocked_on.is_none(),
        "blocked_on should be cleared when leaving Waiting"
    );
}

#[test]
fn test_malformed_blocked_on_json_is_silently_dropped() {
    let conn = setup_db();
    let (mgr, _parent, run) = make_workflow_run(&conn);

    // Directly inject malformed JSON into the blocked_on column
    conn.execute(
        "UPDATE workflow_runs SET blocked_on = ? WHERE id = ?",
        params!["not-valid-json{{{", run.id],
    )
    .unwrap();

    // Reading the run should succeed with blocked_on = None
    let loaded = mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert!(
        loaded.blocked_on.is_none(),
        "malformed blocked_on should deserialize as None"
    );
}

#[test]
fn test_update_workflow_status_rejects_waiting() {
    let conn = setup_db();
    let (mgr, _parent, run) = make_workflow_run(&conn);

    // Calling update_workflow_status with Waiting must return an error — callers
    // should use set_waiting_blocked_on() to enforce the blocked_on invariant.
    let err = mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Waiting, None, None)
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
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at) \
         VALUES ('run-hist', 'test-wf', 'w1', ?1, 'completed', 0, 'manual', '2025-01-01T00:00:00Z')",
        params![parent.id],
    )
    .unwrap();

    // Verify repo_id is NULL before backfill.
    let repo_id_before: Option<String> = conn
        .query_row(
            "SELECT repo_id FROM workflow_runs WHERE id = 'run-hist'",
            [],
            |row| row.get(0),
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
            |row| row.get(0),
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
            "SELECT repo_id FROM workflow_runs WHERE id = ?1",
            params![run_id],
            |row| row.get(0),
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
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();

    // Insert a run referencing worktree w1, then orphan it by pointing
    // worktree_id at a non-existent ID (simulating a deleted worktree row).
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at) \
         VALUES ('run-orphan', 'test-wf', 'w1', ?1, 'completed', 0, 'manual', '2025-01-01T00:00:00Z')",
        params![parent.id],
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
            |row| row.get(0),
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
    let (mgr, _parent, run) = make_workflow_run(&conn);
    let step_id = mgr
        .insert_step(&run.id, "script-step", "actor", false, 0, 0)
        .unwrap();

    mgr.set_step_output_file(&step_id, "/tmp/output.txt")
        .unwrap();

    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
    assert_eq!(step.output_file.as_deref(), Some("/tmp/output.txt"));
}

// ---------------------------------------------------------------------------
// set_step_gate_info
// ---------------------------------------------------------------------------

#[test]
fn test_set_step_gate_info_with_prompt() {
    let conn = setup_db();
    let (mgr, _parent, run) = make_workflow_run(&conn);
    let step_id = mgr
        .insert_step(&run.id, "gate-step", "gate", false, 0, 0)
        .unwrap();

    mgr.set_step_gate_info(
        &step_id,
        GateType::PrApproval,
        Some("Need 2 approvals"),
        "24h",
    )
    .unwrap();

    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
    assert_eq!(step.gate_type, Some(GateType::PrApproval));
    assert_eq!(step.gate_prompt.as_deref(), Some("Need 2 approvals"));
    assert_eq!(step.gate_timeout.as_deref(), Some("24h"));
}

#[test]
fn test_set_step_gate_info_no_prompt() {
    let conn = setup_db();
    let (mgr, _parent, run) = make_workflow_run(&conn);
    let step_id = mgr
        .insert_step(&run.id, "gate-step", "gate", false, 0, 0)
        .unwrap();

    mgr.set_step_gate_info(&step_id, GateType::PrChecks, None, "1h")
        .unwrap();

    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
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
    let (mgr, _parent, run) = make_workflow_run(&conn);
    let step_id = mgr
        .insert_step(&run.id, "parallel-step", "actor", false, 0, 0)
        .unwrap();

    mgr.set_step_parallel_group(&step_id, "group-abc").unwrap();

    let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
    assert_eq!(step.parallel_group_id.as_deref(), Some("group-abc"));
}

// ---------------------------------------------------------------------------
// get_steps_for_runs
// ---------------------------------------------------------------------------

#[test]
fn test_get_steps_for_runs_empty_ids() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let result = mgr.get_steps_for_runs(&[]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_get_steps_for_runs_multiple_runs() {
    let conn = setup_db();
    let (mgr, _p1, run1) = make_workflow_run(&conn);

    let agent_mgr = AgentManager::new(&conn);
    let p2 = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let run2 = mgr
        .create_workflow_run("wf2", Some("w1"), &p2.id, false, "manual", None)
        .unwrap();

    // Add steps to each run
    mgr.insert_step(&run1.id, "s1", "actor", false, 0, 0)
        .unwrap();
    mgr.insert_step(&run1.id, "s2", "actor", false, 1, 0)
        .unwrap();
    mgr.insert_step(&run2.id, "s3", "actor", false, 0, 0)
        .unwrap();

    let result = mgr.get_steps_for_runs(&[&run1.id, &run2.id]).unwrap();
    assert_eq!(result.get(&run1.id).unwrap().len(), 2);
    assert_eq!(result.get(&run2.id).unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// get_active_steps_for_runs
// ---------------------------------------------------------------------------

#[test]
fn test_get_active_steps_for_runs_filters_by_status() {
    let conn = setup_db();
    let (mgr, _parent, run) = make_workflow_run(&conn);

    let s1 = mgr
        .insert_step(&run.id, "completed-step", "actor", false, 0, 0)
        .unwrap();
    set_step_status(&mgr, &s1, WorkflowStepStatus::Completed);

    let s2 = mgr
        .insert_step(&run.id, "running-step", "actor", false, 1, 0)
        .unwrap();
    set_step_status(&mgr, &s2, WorkflowStepStatus::Running);

    let s3 = mgr
        .insert_step(&run.id, "waiting-step", "gate", false, 2, 0)
        .unwrap();
    set_step_status(&mgr, &s3, WorkflowStepStatus::Waiting);

    let s4 = mgr
        .insert_step(&run.id, "failed-step", "actor", false, 3, 0)
        .unwrap();
    set_step_status(&mgr, &s4, WorkflowStepStatus::Failed);

    let result = mgr.get_active_steps_for_runs(&[&run.id]).unwrap();
    let steps = result.get(&run.id).unwrap();
    // Only running and waiting should be returned
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].step_name, "running-step");
    assert_eq!(steps[1].step_name, "waiting-step");
}

#[test]
fn test_get_active_steps_for_runs_empty_ids() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let result = mgr.get_active_steps_for_runs(&[]).unwrap();
    assert!(result.is_empty());
}

// ---------------------------------------------------------------------------
// detect_stuck_workflow_run_ids — detection logic tests
// ---------------------------------------------------------------------------

/// Insert a workflow run in 'running' status with no parent_workflow_run_id.
fn insert_running_root_run(conn: &Connection, run_id: &str) {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES (?1, 'test-wf', NULL, ?2, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        params![run_id, parent.id],
    )
    .unwrap();
}

/// Insert a non-terminal step (pending/running/waiting) with no ended_at.
fn insert_non_terminal_step(conn: &Connection, step_id: &str, run_id: &str, status: &str) {
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration) \
         VALUES (?1, ?2, 'step-a', 'actor', 0, ?3, 0)",
        params![step_id, run_id, status],
    )
    .unwrap();
}

#[test]
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

    let mgr = WorkflowManager::new(&conn);
    // threshold_secs = 60: elapsed >> 60 → detected
    let ids = mgr.detect_stuck_workflow_run_ids(60).unwrap();
    assert_eq!(ids.len(), 1, "stale run should be detected");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_fresh_run() {
    let conn = setup_db();
    insert_running_root_run(&conn, "fresh-run");
    // Step completed just now — store ended_at as the current UTC time.
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, ended_at) \
         VALUES ('s1', 'fresh-run', 'step-a', 'actor', 0, 'completed', 0, \
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
        [],
    )
    .unwrap();

    let mgr = WorkflowManager::new(&conn);
    // Very large threshold — a just-completed step should not be detected.
    let ids = mgr.detect_stuck_workflow_run_ids(999_999).unwrap();
    assert_eq!(ids.len(), 0, "fresh run must not be detected");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_pending_step() {
    let conn = setup_db();
    insert_running_root_run(&conn, "pending-run");
    insert_non_terminal_step(&conn, "s1", "pending-run", "pending");

    let mgr = WorkflowManager::new(&conn);
    let ids = mgr.detect_stuck_workflow_run_ids(0).unwrap();
    assert_eq!(ids.len(), 0, "run with pending step must not be detected");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_running_step() {
    let conn = setup_db();
    insert_running_root_run(&conn, "running-step-run");
    insert_non_terminal_step(&conn, "s1", "running-step-run", "running");

    let mgr = WorkflowManager::new(&conn);
    let ids = mgr.detect_stuck_workflow_run_ids(0).unwrap();
    assert_eq!(ids.len(), 0, "run with running step must not be detected");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_waiting_step() {
    let conn = setup_db();
    insert_running_root_run(&conn, "waiting-step-run");
    insert_non_terminal_step(&conn, "s1", "waiting-step-run", "waiting");

    let mgr = WorkflowManager::new(&conn);
    let ids = mgr.detect_stuck_workflow_run_ids(0).unwrap();
    assert_eq!(ids.len(), 0, "run with waiting step must not be detected");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_sub_workflow() {
    let conn = setup_db();
    // Insert a root run first to satisfy the FK for parent_workflow_run_id.
    insert_running_root_run(&conn, "root-run");
    // Insert a sub-workflow with parent_workflow_run_id set.
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES ('sub-run', 'child-wf', NULL, ?1, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', 'root-run')",
        params![parent.id],
    )
    .unwrap();
    insert_terminal_step_with_id(&conn, "s1", "sub-run", "completed", "2020-01-01T00:00:00Z");

    let mgr = WorkflowManager::new(&conn);
    let ids = mgr.detect_stuck_workflow_run_ids(0).unwrap();
    assert_eq!(ids.len(), 1, "sub-workflow must also be detected as stuck");
    assert_eq!(ids[0], "sub-run");
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

    let mgr = WorkflowManager::new(&conn);
    let ids = mgr.detect_stuck_workflow_run_ids(0).unwrap();
    assert_eq!(ids.len(), 0, "non-running status runs must not be detected");
}

#[test]
fn test_reap_stuck_workflow_runs_skips_no_steps() {
    let conn = setup_db();
    insert_running_root_run(&conn, "no-steps-run");
    // No steps inserted → last_step_ended IS NULL → skipped by SQL guard.

    let mgr = WorkflowManager::new(&conn);
    let ids = mgr.detect_stuck_workflow_run_ids(0).unwrap();
    assert_eq!(ids.len(), 0, "run with no steps must not be detected");
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

    let mgr = WorkflowManager::new(&conn);
    let ids = mgr.detect_stuck_workflow_run_ids(60).unwrap();
    assert_eq!(ids.len(), 3, "all 3 stuck runs should be detected");
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
    let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES ('stuck-run', 'deploy', NULL, ?1, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        params![parent.id],
    )
    .unwrap();
    insert_terminal_step_with_id(
        &conn,
        "s1",
        "stuck-run",
        "completed",
        "2020-01-01T00:00:00Z",
    );

    let mgr = WorkflowManager::new(&conn);
    let ids = mgr.detect_stuck_workflow_run_ids(60).unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0], "stuck-run");
}

#[test]
fn test_detect_stuck_skips_run_with_active_steps() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES ('active-run', 'deploy', NULL, ?1, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        params![parent.id],
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

    let mgr = WorkflowManager::new(&conn);
    let ids = mgr.detect_stuck_workflow_run_ids(60).unwrap();
    assert!(ids.is_empty(), "run with active steps should not be stuck");
}

// ---------------------------------------------------------------------------
// recover_stuck_steps — step recovery tests
// ---------------------------------------------------------------------------

#[test]
fn test_recover_stuck_steps_fixes_step_with_terminal_child() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES ('recover-run', 'deploy', NULL, ?1, 'running', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        params![parent.id],
    )
    .unwrap();

    // Create a child agent run and mark it completed via SQL.
    let child = agent_mgr
        .create_run(None, "step prompt", None, None)
        .unwrap();
    conn.execute(
        "UPDATE agent_runs SET status = 'completed' WHERE id = ?1",
        params![child.id],
    )
    .unwrap();

    // Insert a step still marked 'running' but whose child is terminal.
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          started_at, child_run_id) \
         VALUES ('s1', 'recover-run', 'code-review', 'actor', 0, 'running', 0, \
                 '2020-01-01T00:00:00Z', ?1)",
        params![child.id],
    )
    .unwrap();

    let mgr = WorkflowManager::new(&conn);
    let recovered = mgr.recover_stuck_steps().unwrap();
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
         VALUES (?1, ?2, 'step-failed', 'script', 0, 'failed', 0, 12345)",
        params![step_id, run_id],
    )
    .unwrap();

    let mgr = WorkflowManager::new(&conn);
    mgr.reset_failed_steps(&run_id).unwrap();

    let pid: Option<i64> = conn
        .query_row(
            "SELECT subprocess_pid FROM workflow_run_steps WHERE id = ?1",
            params![step_id],
            |r| r.get(0),
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
         VALUES (?1, ?2, 'step-done', 'script', 0, 'completed', 0, 99999)",
        params![step_id, run_id],
    )
    .unwrap();

    let mgr = WorkflowManager::new(&conn);
    mgr.reset_completed_steps(&run_id).unwrap();

    let pid: Option<i64> = conn
        .query_row(
            "SELECT subprocess_pid FROM workflow_run_steps WHERE id = ?1",
            params![step_id],
            |r| r.get(0),
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
         VALUES (?1, ?2, 'step-pos', 'script', 2, 'failed', 0, 55555)",
        params![step_id, run_id],
    )
    .unwrap();

    let mgr = WorkflowManager::new(&conn);
    mgr.reset_steps_from_position(&run_id, 2).unwrap();

    let pid: Option<i64> = conn
        .query_row(
            "SELECT subprocess_pid FROM workflow_run_steps WHERE id = ?1",
            params![step_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        pid.is_none(),
        "subprocess_pid must be NULL after reset_steps_from_position"
    );
}
