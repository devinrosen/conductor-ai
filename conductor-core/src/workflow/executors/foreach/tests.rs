use super::*;
use crate::tickets::{TicketInput, TicketLabelInput, TicketSyncer};
use crate::workflow_dsl::{ForeachScope, OnChildFail, OnCycle, TicketScope};

fn setup_db() -> rusqlite::Connection {
    crate::test_helpers::setup_db()
}

fn make_ticket(source_id: &str, title: &str) -> TicketInput {
    crate::test_helpers::make_ticket(source_id, title)
}

fn make_foreach_node_unlabeled() -> ForEachNode {
    ForEachNode {
        name: "test-foreach".to_string(),
        over: "tickets".to_string(),
        scope: Some(ForeachScope::Ticket(TicketScope::Unlabeled)),
        filter: std::collections::HashMap::new(),
        ordered: false,
        on_cycle: OnCycle::Fail,
        max_parallel: 3,
        workflow: "label-ticket".to_string(),
        inputs: std::collections::HashMap::new(),
        on_child_fail: OnChildFail::Continue,
    }
}

#[test]
fn test_collect_ticket_items_unlabeled_scope() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    // setup_db() inserts repo "r1" / worktree "w1" — reuse them.
    let syncer = TicketSyncer::new(&conn);

    // t1 is labeled, t2 and t3 are unlabeled.
    let mut t1 = make_ticket("1", "Labeled issue");
    t1.label_details = vec![TicketLabelInput {
        name: "bug".to_string(),
        color: None,
    }];
    let t2 = make_ticket("2", "Unlabeled A");
    let t3 = make_ticket("3", "Unlabeled B");
    syncer.upsert_tickets("r1", &[t1, t2, t3]).unwrap();

    // Build a minimal ExecutionState.
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let node = make_foreach_node_unlabeled();
    let existing_set = HashSet::new();
    let items = collect_ticket_items(&mut state, &node, &existing_set).unwrap();

    // Should only return t2 and t3 (unlabeled).
    assert_eq!(items.len(), 2);
    let source_refs: Vec<&str> = items.iter().map(|(_, _, r)| r.as_str()).collect();
    assert!(source_refs.contains(&"2"), "source_id 2 should be included");
    assert!(source_refs.contains(&"3"), "source_id 3 should be included");
    // item_type should be "ticket"
    for (item_type, _, _) in &items {
        assert_eq!(item_type, "ticket");
    }
}

// Regression tests for #2094: worktree_id must be cleared for Tickets/Repos fan-outs
// so that child dispatches do not inherit the parent's active-workflow guard.

#[test]
fn test_resolve_child_context_ids_tickets_clears_worktree_id() {
    let (ticket_id, repo_id, worktree_id) = resolve_child_context_ids(
        "tickets",
        "ticket-abc",
        &None,
        &Some("repo-1".to_string()),
        &Some("worktree-parent".to_string()),
    );
    assert_eq!(ticket_id, Some("ticket-abc".to_string()));
    assert_eq!(repo_id, Some("repo-1".to_string()));
    assert!(
        worktree_id.is_none(),
        "Tickets fan-out must clear worktree_id"
    );
}

#[test]
fn test_resolve_child_context_ids_repos_clears_worktree_id() {
    let (ticket_id, repo_id, worktree_id) = resolve_child_context_ids(
        "repos",
        "repo-xyz",
        &Some("ticket-parent".to_string()),
        &None,
        &Some("worktree-parent".to_string()),
    );
    assert!(ticket_id.is_none(), "Repos fan-out must clear ticket_id");
    assert_eq!(repo_id, Some("repo-xyz".to_string()));
    assert!(
        worktree_id.is_none(),
        "Repos fan-out must clear worktree_id"
    );
}

#[test]
fn test_resolve_child_context_ids_workflow_runs_passes_through_worktree_id() {
    let (ticket_id, repo_id, worktree_id) = resolve_child_context_ids(
        "workflow_runs",
        "run-999",
        &Some("ticket-parent".to_string()),
        &Some("repo-parent".to_string()),
        &Some("worktree-parent".to_string()),
    );
    assert_eq!(ticket_id, Some("ticket-parent".to_string()));
    assert_eq!(repo_id, Some("repo-parent".to_string()));
    assert_eq!(
        worktree_id,
        Some("worktree-parent".to_string()),
        "WorkflowRuns fan-out must pass worktree_id through"
    );
}

#[test]
fn test_resolve_child_context_ids_workflow_runs_none_worktree_passthrough() {
    let (_, _, worktree_id) =
        resolve_child_context_ids("workflow_runs", "run-000", &None, &None, &None);
    assert!(worktree_id.is_none());
}

// Regression tests for #2097: verify that build_child_dispatch_params wires
// worktree_id correctly — the clearing logic in resolve_child_context_ids must
// actually reach the WorkflowExecStandalone struct.

fn make_minimal_item(
    item_id: &str,
    item_ref: &str,
    item_type: &str,
) -> crate::workflow::manager::FanOutItemRow {
    crate::workflow::manager::FanOutItemRow {
        id: "item-id".to_string(),
        step_run_id: "step-run-id".to_string(),
        item_type: item_type.to_string(),
        item_id: item_id.to_string(),
        item_ref: item_ref.to_string(),
        child_run_id: None,
        status: "pending".to_string(),
        dispatched_at: None,
        completed_at: None,
    }
}

fn make_minimal_child_def() -> crate::workflow_dsl::WorkflowDef {
    crate::workflow_dsl::WorkflowDef {
        name: "child-workflow".to_string(),
        title: None,
        description: String::new(),
        trigger: crate::workflow_dsl::WorkflowTrigger::Manual,
        targets: vec![],
        group: None,
        inputs: vec![],
        body: vec![],
        always: vec![],
        source_path: String::new(),
    }
}

fn make_foreach_node_for(over: String) -> ForEachNode {
    ForEachNode {
        name: "test-foreach".to_string(),
        over,
        scope: None,
        filter: std::collections::HashMap::new(),
        ordered: false,
        on_cycle: crate::workflow_dsl::OnCycle::Fail,
        max_parallel: 3,
        workflow: "child-workflow".to_string(),
        inputs: std::collections::HashMap::new(),
        on_child_fail: crate::workflow_dsl::OnChildFail::Continue,
    }
}

fn make_execution_state_with_worktree<'a>(
    conn: &'a rusqlite::Connection,
    config: &'static crate::config::Config,
    workflow_run_id: String,
    parent_run_id: String,
    worktree_id: Option<String>,
    repo_id: Option<String>,
    ticket_id: Option<String>,
) -> ExecutionState<'a> {
    ExecutionState {
        worktree_ctx: crate::workflow::engine::WorktreeContext {
            worktree_id,
            working_dir: "/tmp/test".to_string(),
            worktree_slug: "test".to_string(),
            repo_path: "/tmp/repo".to_string(),
            ticket_id,
            repo_id,
            conductor_bin_dir: None,
            extra_plugin_dirs: vec![],
        },
        ..crate::workflow::tests::common::base_execution_state(
            conn,
            config,
            workflow_run_id,
            parent_run_id,
        )
    }
}

#[test]
fn test_dispatch_params_tickets_clears_worktree_id() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let node = make_foreach_node_for("tickets".to_string());
    let item = make_minimal_item("ticket-abc", "42", "ticket");
    let child_def = make_minimal_child_def();

    let params = build_child_dispatch_params(&mut state, &node, &item, &child_def).unwrap();
    assert!(
        params.worktree_id.is_none(),
        "Tickets fan-out must clear worktree_id in dispatch params"
    );
}

#[test]
fn test_dispatch_params_repos_clears_worktree_id() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        None,
        None,
    );

    let node = make_foreach_node_for("repos".to_string());
    let item = make_minimal_item("repo-xyz", "my-repo", "repo");
    let child_def = make_minimal_child_def();

    let params = build_child_dispatch_params(&mut state, &node, &item, &child_def).unwrap();
    assert!(
        params.worktree_id.is_none(),
        "Repos fan-out must clear worktree_id in dispatch params"
    );
}

#[test]
fn test_dispatch_params_workflow_runs_passes_worktree_id_through() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let node = make_foreach_node_for("workflow_runs".to_string());
    let item = make_minimal_item("run-999", "some-workflow", "workflow_run");
    let child_def = make_minimal_child_def();

    let params = build_child_dispatch_params(&mut state, &node, &item, &child_def).unwrap();
    assert_eq!(
        params.worktree_id,
        Some("w1".to_string()),
        "WorkflowRuns fan-out must pass worktree_id through in dispatch params"
    );
}

/// Regression test for fix(workflow): when a Worktrees fan-out item cannot be
/// found by get_by_id(), build_child_dispatch_params must fall back to the
/// parent's working_dir rather than propagating the error.
#[test]
fn test_dispatch_params_worktrees_missing_worktree_falls_back_to_parent_dir() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    // state.working_dir is "/tmp/test" (set by make_execution_state_with_worktree).
    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let node = make_foreach_node_for("worktrees".to_string());
    // item_id "wt-nonexistent" has no row in the DB — get_by_id() will error.
    let item = make_minimal_item("wt-nonexistent", "feat-missing", "worktree");
    let child_def = make_minimal_child_def();

    let params = build_child_dispatch_params(&mut state, &node, &item, &child_def).unwrap();
    assert_eq!(
        params.working_dir, "/tmp/test",
        "Worktrees fan-out must fall back to parent working_dir when worktree lookup fails"
    );
}

/// When the worktree exists in the DB, build_child_dispatch_params must use the
/// stored path as the child's working_dir, not the parent's CWD.
#[test]
fn test_dispatch_params_worktrees_uses_child_worktree_path() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-child', 'r1', 'feat-child', 'feat/child', '/tmp/child-wt', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let node = make_foreach_node_for("worktrees".to_string());
    let item = make_minimal_item("wt-child", "feat-child", "worktree");
    let child_def = make_minimal_child_def();

    let params = build_child_dispatch_params(&mut state, &node, &item, &child_def).unwrap();
    assert_eq!(
        params.working_dir, "/tmp/child-wt",
        "Worktrees fan-out must use the child worktree's stored path as working_dir"
    );
}

#[test]
fn test_resolve_child_context_ids_worktrees_sets_worktree_id() {
    let (ticket_id, repo_id, worktree_id) = resolve_child_context_ids(
        "worktrees",
        "worktree-abc",
        &Some("ticket-parent".to_string()),
        &Some("repo-1".to_string()),
        &Some("old-worktree".to_string()),
    );
    assert!(
        ticket_id.is_none(),
        "Worktrees fan-out must clear ticket_id"
    );
    assert_eq!(repo_id, Some("repo-1".to_string()));
    assert_eq!(
        worktree_id,
        Some("worktree-abc".to_string()),
        "Worktrees fan-out must set worktree_id to the item_id"
    );
}

#[test]
fn test_collect_worktree_items_matching_base_branch() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    // Insert two active worktrees with matching base_branch.
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-a', 'r1', 'feat-a', 'feat/a', '/tmp/a', 'active', '2024-01-01T00:00:00Z', 'release/1.0')",
            [],
        ).unwrap();
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-b', 'r1', 'feat-b', 'feat/b', '/tmp/b', 'active', '2024-01-02T00:00:00Z', 'release/1.0')",
            [],
        ).unwrap();
    // Insert one with different base_branch — should be excluded.
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-c', 'r1', 'feat-c', 'feat/c', '/tmp/c', 'active', '2024-01-03T00:00:00Z', 'main')",
            [],
        ).unwrap();
    // Insert one with matching base_branch but non-active status — should be excluded.
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-d', 'r1', 'feat-d', 'feat/d', '/tmp/d', 'abandoned', '2024-01-04T00:00:00Z', 'release/1.0')",
            [],
        ).unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let node = ForEachNode {
        name: "test-wt-foreach".to_string(),
        over: "worktrees".to_string(),
        scope: Some(ForeachScope::Worktree(crate::workflow_dsl::WorktreeScope {
            base_branch: Some("release/1.0".to_string()),
            has_open_pr: None,
        })),
        filter: std::collections::HashMap::new(),
        ordered: false,
        on_cycle: OnCycle::Fail,
        max_parallel: 2,
        workflow: "child".to_string(),
        inputs: std::collections::HashMap::new(),
        on_child_fail: OnChildFail::Continue,
    };

    let existing_set = HashSet::new();
    let items = collect_worktree_items(&mut state, &node, &existing_set).unwrap();

    assert_eq!(
            items.len(),
            2,
            "should find only the 2 active worktrees on release/1.0 (wt-c excluded by base_branch, wt-d excluded by non-active status)"
        );
    let ids: Vec<&str> = items.iter().map(|(_, id, _)| id.as_str()).collect();
    assert!(ids.contains(&"wt-a"));
    assert!(ids.contains(&"wt-b"));
    assert!(!ids.contains(&"wt-d"), "completed worktree must not appear");
    for (item_type, _, _) in &items {
        assert_eq!(item_type, "worktree");
    }
}

/// Regression test: when no base_branch is in scope, collect_worktree_items must use the
/// context worktree's own branch to find children — not its parent (which would select
/// siblings instead).
#[test]
fn test_collect_worktree_items_inferred_base_uses_worktree_branch_not_parent() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    // Context worktree: release/1.0 branched from main.
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-release', 'r1', 'release-1.0', 'release/1.0', '/tmp/release', 'active', '2024-01-01T00:00:00Z', 'main')",
            [],
        ).unwrap();
    // Children: branched from release/1.0 — should be selected.
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-child-a', 'r1', 'feat-child-a', 'feat/child-a', '/tmp/ca', 'active', '2024-01-02T00:00:00Z', 'release/1.0')",
            [],
        ).unwrap();
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-child-b', 'r1', 'feat-child-b', 'feat/child-b', '/tmp/cb', 'active', '2024-01-03T00:00:00Z', 'release/1.0')",
            [],
        ).unwrap();
    // Sibling: also branched from main — must NOT be selected.
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-sibling', 'r1', 'feat-sibling', 'feat/sibling', '/tmp/sib', 'active', '2024-01-04T00:00:00Z', 'main')",
            [],
        ).unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("wt-release"), "workflow", None)
        .unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run(
            "test",
            Some("wt-release"),
            &parent.id,
            false,
            "manual",
            None,
        )
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("wt-release".to_string()),
        Some("r1".to_string()),
        None,
    );

    // No base_branch in scope — engine must infer from context worktree's branch.
    let node = ForEachNode {
        name: "test-children".to_string(),
        over: "worktrees".to_string(),
        scope: None,
        filter: std::collections::HashMap::new(),
        ordered: false,
        on_cycle: OnCycle::Fail,
        max_parallel: 1,
        workflow: "ticket-to-pr".to_string(),
        inputs: std::collections::HashMap::new(),
        on_child_fail: OnChildFail::Continue,
    };

    let items = collect_worktree_items(&mut state, &node, &HashSet::new()).unwrap();

    let ids: Vec<&str> = items.iter().map(|(_, id, _)| id.as_str()).collect();
    assert!(ids.contains(&"wt-child-a"), "child-a should be selected");
    assert!(ids.contains(&"wt-child-b"), "child-b should be selected");
    assert!(
        !ids.contains(&"wt-sibling"),
        "sibling (same parent as context worktree) must not be selected"
    );
    assert!(
        !ids.contains(&"wt-release"),
        "context worktree itself must not be selected"
    );
}

#[test]
fn test_collect_worktree_items_has_open_pr_filter() {
    // Covers the degraded/no-auth fallback path only: list_open_prs returns [] in CI
    // (no gh auth), so this test does NOT exercise the actual branch-matching filter.
    // See test_open_pr_filter_* tests below for coverage of the filtering logic itself.
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-p1', 'r1', 'feat-p1', 'feat/p1', '/tmp/p1', 'active', '2024-01-01T00:00:00Z', 'release/1.0')",
            [],
        ).unwrap();
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-p2', 'r1', 'feat-p2', 'feat/p2', '/tmp/p2', 'active', '2024-01-02T00:00:00Z', 'release/1.0')",
            [],
        ).unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id.clone(),
        parent.id.clone(),
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    // has_open_pr = Some(false): list_open_prs returns [] → all worktrees pass.
    let node_no_pr = ForEachNode {
        name: "wt-no-pr".to_string(),
        over: "worktrees".to_string(),
        scope: Some(ForeachScope::Worktree(crate::workflow_dsl::WorktreeScope {
            base_branch: Some("release/1.0".to_string()),
            has_open_pr: Some(false),
        })),
        filter: std::collections::HashMap::new(),
        ordered: false,
        on_cycle: OnCycle::Fail,
        max_parallel: 2,
        workflow: "child".to_string(),
        inputs: std::collections::HashMap::new(),
        on_child_fail: OnChildFail::Continue,
    };

    let existing_set = HashSet::new();
    let items = collect_worktree_items(&mut state, &node_no_pr, &existing_set).unwrap();
    assert_eq!(
        items.len(),
        2,
        "has_open_pr=false with empty PR list: both worktrees should pass (no PRs found)"
    );

    // has_open_pr = Some(true): list_open_prs returns [] → no worktrees pass.
    let node_has_pr = ForEachNode {
        name: "wt-has-pr".to_string(),
        over: "worktrees".to_string(),
        scope: Some(ForeachScope::Worktree(crate::workflow_dsl::WorktreeScope {
            base_branch: Some("release/1.0".to_string()),
            has_open_pr: Some(true),
        })),
        filter: std::collections::HashMap::new(),
        ordered: false,
        on_cycle: OnCycle::Fail,
        max_parallel: 2,
        workflow: "child".to_string(),
        inputs: std::collections::HashMap::new(),
        on_child_fail: OnChildFail::Continue,
    };

    let items = collect_worktree_items(&mut state, &node_has_pr, &existing_set).unwrap();
    assert_eq!(
        items.len(),
        0,
        "has_open_pr=true with empty PR list: no worktrees should pass"
    );
}

fn make_pr(branch: &str) -> crate::github::GithubPr {
    crate::github::GithubPr {
        number: 1,
        title: "test PR".to_string(),
        url: "https://github.com/test/repo/pull/1".to_string(),
        author: "user".to_string(),
        state: "OPEN".to_string(),
        head_ref_name: branch.to_string(),
        is_draft: false,
        review_decision: None,
        ci_status: "SUCCESS".to_string(),
    }
}

fn make_wt(id: &str, branch: &str) -> crate::worktree::Worktree {
    crate::worktree::Worktree {
        id: id.to_string(),
        repo_id: "r1".to_string(),
        slug: id.to_string(),
        branch: branch.to_string(),
        path: format!("/tmp/{id}"),
        ticket_id: None,
        status: crate::worktree::WorktreeStatus::Active,
        created_at: "2024-01-01T00:00:00Z".to_string(),
        completed_at: None,
        model: None,
        base_branch: Some("release/1.0".to_string()),
    }
}

#[test]
fn test_has_open_pr_filter_true_with_matching_pr() {
    let candidates = vec![make_wt("wt-p1", "feat/p1"), make_wt("wt-p2", "feat/p2")];
    let open_prs = vec![make_pr("feat/p1")];
    let result = filter_worktrees_by_open_pr(candidates, true, open_prs);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, "wt-p1");
}

#[test]
fn test_has_open_pr_filter_false_with_matching_pr() {
    let candidates = vec![make_wt("wt-p1", "feat/p1"), make_wt("wt-p2", "feat/p2")];
    let open_prs = vec![make_pr("feat/p1")];
    let result = filter_worktrees_by_open_pr(candidates, false, open_prs);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, "wt-p2");
}

#[test]
fn test_has_open_pr_filter_true_all_have_prs() {
    let candidates = vec![make_wt("wt-p1", "feat/p1"), make_wt("wt-p2", "feat/p2")];
    let open_prs = vec![make_pr("feat/p1"), make_pr("feat/p2")];
    let result = filter_worktrees_by_open_pr(candidates, true, open_prs);
    assert_eq!(result.len(), 2);
}

#[test]
fn test_has_open_pr_filter_false_no_prs() {
    let candidates = vec![make_wt("wt-p1", "feat/p1"), make_wt("wt-p2", "feat/p2")];
    let result = filter_worktrees_by_open_pr(candidates, false, vec![]);
    assert_eq!(
        result.len(),
        2,
        "no open PRs means all worktrees have no PR"
    );
}

// -----------------------------------------------------------------------
// Two-cycle failure confirmation tests (#2269)
// -----------------------------------------------------------------------

fn make_foreach_node_continue() -> ForEachNode {
    ForEachNode {
        name: "test-foreach".to_string(),
        over: "tickets".to_string(),
        scope: None,
        filter: std::collections::HashMap::new(),
        ordered: false,
        on_cycle: crate::workflow_dsl::OnCycle::Fail,
        max_parallel: 1,
        workflow: "child-wf".to_string(),
        inputs: std::collections::HashMap::new(),
        on_child_fail: OnChildFail::Continue,
    }
}

/// Regression test for #2269: a child run observed as 'failed' only once must NOT
/// immediately mark the fan-out item failed.  If the child run recovers to 'completed'
/// before the second polling cycle, the item must end as 'completed'.
///
/// Uses a file-based WAL DB so a background thread can update the child run status
/// between the two dispatch loop cycles. Channel-based synchronization via
/// `between_cycle_hook` replaces wall-clock timing to avoid flaky-test failures.
#[test]
fn test_foreach_does_not_fail_item_on_first_failed_observation() {
    use crate::workflow::status::WorkflowRunStatus;
    use std::sync::mpsc;
    use std::time::Duration;

    let tmp_dir = tempfile::tempdir().unwrap();
    let db_path = tmp_dir.path().join("test.db");
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    let conn = crate::db::open_database(&db_path).unwrap();
    crate::test_helpers::insert_test_repo(&conn, "r1", "test-repo", "/tmp/repo");
    crate::test_helpers::insert_test_worktree(&conn, "w1", "r1", "feat-test", "/tmp/ws");

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = wf_mgr
        .insert_step(&run.id, "foreach-step", "foreach", false, 0, 1)
        .unwrap();

    let child_agent = agent_mgr.create_run(Some("w1"), "child", None).unwrap();
    let child_run = wf_mgr
        .create_workflow_run(
            "child-wf",
            Some("w1"),
            &child_agent.id,
            false,
            "manual",
            None,
        )
        .unwrap();
    let child_run_id = child_run.id.clone();
    // Child run starts as 'failed' — simulating a transient DB state.
    wf_mgr
        .update_workflow_status(&child_run_id, WorkflowRunStatus::Failed, None, None)
        .unwrap();

    let item_id = wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "ticket".into(),
                item_id: "ticket-1".into(),
                item_ref: "ticket-1".into(),
            },
        )
        .unwrap();
    wf_mgr
        .update_fan_out_item_running(&item_id, &child_run_id)
        .unwrap();

    // Channels synchronize the between-cycle hook with the background update thread.
    // trigger: hook → BG ("cycle 1 done, update now")
    // done:    BG → hook ("update complete")
    let (trigger_tx, trigger_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<()>();

    // Background thread: waits for the between-cycle hook to fire, then updates the
    // child run to 'completed' so cycle 2 sees the recovered state.
    let db_path_bg = db_path.clone();
    let child_run_id_bg = child_run_id.clone();
    let bg = std::thread::spawn(move || {
        trigger_rx.recv().unwrap();
        let bg_conn = crate::db::open_database(&db_path_bg).unwrap();
        let bg_wf_mgr = crate::workflow::manager::WorkflowManager::new(&bg_conn);
        bg_wf_mgr
            .update_workflow_status(
                &child_run_id_bg,
                WorkflowRunStatus::Completed,
                Some("recovered"),
                None,
            )
            .unwrap();
        done_tx.send(()).unwrap();
    });

    let node = make_foreach_node_continue();
    let child_def = make_minimal_child_def();
    let run_id = run.id.clone();
    let parent_id = parent.id.clone();
    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run_id,
        parent_id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );
    // Zero poll interval: the between-cycle hook provides all needed synchronization.
    state.exec_config.poll_interval = Duration::from_millis(0);
    // Hook fires between cycles: signals BG to update DB and waits for confirmation.
    set_between_cycle_hook(move || {
        trigger_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    let result = run_dispatch_loop_for_test(&mut state, &node, &step_id, &child_def, 1);
    clear_between_cycle_hook();
    bg.join().unwrap();
    assert!(result.is_ok(), "dispatch loop should succeed: {:?}", result);

    let items = wf_mgr.get_fan_out_items(&step_id, None).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].status, "completed",
        "transient failure must not be committed: item must end as completed after recovery"
    );
}

/// Regression test for #2269: a child run that remains 'failed' across two consecutive
/// polling cycles must be committed as 'failed'.
#[test]
fn test_foreach_fails_item_after_two_consecutive_failed_observations() {
    use crate::workflow::status::WorkflowRunStatus;
    use std::time::Duration;

    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = wf_mgr
        .insert_step(&run.id, "foreach-step", "foreach", false, 0, 1)
        .unwrap();

    let child_agent = agent_mgr.create_run(Some("w1"), "child", None).unwrap();
    let child_run = wf_mgr
        .create_workflow_run(
            "child-wf",
            Some("w1"),
            &child_agent.id,
            false,
            "manual",
            None,
        )
        .unwrap();
    let child_run_id = child_run.id.clone();
    wf_mgr
        .update_workflow_status(&child_run_id, WorkflowRunStatus::Failed, None, None)
        .unwrap();

    let item_id = wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "ticket".into(),
                item_id: "ticket-1".into(),
                item_ref: "ticket-1".into(),
            },
        )
        .unwrap();
    wf_mgr
        .update_fan_out_item_running(&item_id, &child_run_id)
        .unwrap();

    let node = make_foreach_node_continue();
    let child_def = make_minimal_child_def();
    let run_id = run.id.clone();
    let parent_id = parent.id.clone();
    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run_id,
        parent_id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );
    // No sleep between cycles so the test runs fast.
    state.exec_config.poll_interval = Duration::ZERO;

    let result = run_dispatch_loop_for_test(&mut state, &node, &step_id, &child_def, 1);
    assert!(result.is_ok(), "dispatch loop should succeed: {:?}", result);

    let items = wf_mgr.get_fan_out_items(&step_id, None).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].status, "failed",
        "persistent failure must be committed after two consecutive failed observations"
    );
}

#[test]
fn test_build_item_vars_worktrees_all_fields() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    // Insert a worktree with all fields populated.
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-x', 'r1', 'feat-x', 'feat/x', '/tmp/x', 'active', '2024-01-01T00:00:00Z', 'release/2.0')",
            [],
        ).unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let node = make_foreach_node_for("worktrees".to_string());
    let item = make_minimal_item("wt-x", "feat-x", "worktree");

    let vars = build_item_vars(&mut state, &node, &item).unwrap();
    assert_eq!(vars.get("item.slug").map(|s| s.as_str()), Some("feat-x"));
    assert_eq!(vars.get("item.branch").map(|s| s.as_str()), Some("feat/x"));
    assert_eq!(vars.get("item.path").map(|s| s.as_str()), Some("/tmp/x"));
    assert_eq!(
        vars.get("item.base_branch").map(|s| s.as_str()),
        Some("release/2.0")
    );
    assert_eq!(vars.get("item.ticket_id").map(|s| s.as_str()), Some(""));
    assert_eq!(vars.get("item.id").map(|s| s.as_str()), Some("wt-x"));
}

// -----------------------------------------------------------------------
// load_worktree_dep_edges tests
// -----------------------------------------------------------------------

/// Seed two worktrees each linked to a ticket, where ticket-1 blocks ticket-2.
/// Verify that load_worktree_dep_edges returns a single edge (wt-blocker → wt-blocked).
#[test]
fn test_load_worktree_dep_edges_basic() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    // Insert two tickets where ticket "1" blocks ticket "2".
    let t1 = make_ticket("1", "Blocker");
    let mut t2 = make_ticket("2", "Blocked");
    t2.blocked_by = vec!["1".to_string()];
    let syncer = TicketSyncer::new(&conn);
    syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

    // Resolve ULIDs (tickets table uses server-generated ULIDs).
    let ticket1_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    let ticket2_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '2'", [], |row| {
            row.get("id")
        })
        .unwrap();

    // Insert two worktrees linked to the tickets.
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, ticket_id) \
             VALUES ('wt-1', 'r1', 'feat-1', 'feat/1', '/tmp/1', 'active', '2024-01-01T00:00:00Z', :ticket_id)",
            rusqlite::named_params![":ticket_id": ticket1_id],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, ticket_id) \
             VALUES ('wt-2', 'r1', 'feat-2', 'feat/2', '/tmp/2', 'active', '2024-01-02T00:00:00Z', :ticket_id)",
            rusqlite::named_params![":ticket_id": ticket2_id],
        )
        .unwrap();

    // Create a workflow run + step + fan_out_items for the two worktrees.
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = wf_mgr
        .insert_step(&run.id, "release-foreach", "foreach", false, 0, 0)
        .unwrap();

    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "worktree".into(),
                item_id: "wt-1".into(),
                item_ref: "feat-1".into(),
            },
        )
        .unwrap();
    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "worktree".into(),
                item_id: "wt-2".into(),
                item_ref: "feat-2".into(),
            },
        )
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let edges = load_worktree_dep_edges(&mut state, &step_id).unwrap();

    assert_eq!(edges.len(), 1, "expected exactly one dependency edge");
    assert_eq!(
        edges[0],
        ("wt-1".to_string(), "wt-2".to_string()),
        "wt-1 (blocker) should point to wt-2 (blocked)"
    );
}

/// When no worktrees have a linked ticket, load_worktree_dep_edges returns no edges.
#[test]
fn test_load_worktree_dep_edges_no_tickets() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    // Insert worktrees with no ticket_id.
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-a', 'r1', 'feat-a', 'feat/a', '/tmp/a', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-b', 'r1', 'feat-b', 'feat/b', '/tmp/b', 'active', '2024-01-02T00:00:00Z')",
        [],
    )
    .unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = wf_mgr
        .insert_step(&run.id, "release-foreach", "foreach", false, 0, 0)
        .unwrap();

    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "worktree".into(),
                item_id: "wt-a".into(),
                item_ref: "feat-a".into(),
            },
        )
        .unwrap();
    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "worktree".into(),
                item_id: "wt-b".into(),
                item_ref: "feat-b".into(),
            },
        )
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let edges = load_worktree_dep_edges(&mut state, &step_id).unwrap();
    assert!(
        edges.is_empty(),
        "expected no edges when worktrees have no linked tickets"
    );
}

/// Mixed-case: wt-1 linked to ticket (which has a blocker in the set), wt-2 linked to
/// ticket (the blocker), wt-3 has no ticket. Expect one edge (wt-2 → wt-1); wt-3 ignored.
#[test]
fn test_load_worktree_dep_edges_mixed_some_with_tickets() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    // ticket "blocker" blocks ticket "dep"
    let t_blocker = make_ticket("blocker", "Blocker ticket");
    let mut t_dep = make_ticket("dep", "Dependent ticket");
    t_dep.blocked_by = vec!["blocker".to_string()];
    let syncer = TicketSyncer::new(&conn);
    syncer.upsert_tickets("r1", &[t_blocker, t_dep]).unwrap();

    let blocker_id: String = conn
        .query_row(
            "SELECT id FROM tickets WHERE source_id = 'blocker'",
            [],
            |row| row.get("id"),
        )
        .unwrap();
    let dep_id: String = conn
        .query_row(
            "SELECT id FROM tickets WHERE source_id = 'dep'",
            [],
            |row| row.get("id"),
        )
        .unwrap();

    // wt-1 linked to "dep" ticket, wt-2 linked to "blocker" ticket, wt-3 no ticket
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, ticket_id) \
             VALUES ('wt-1', 'r1', 'feat-dep', 'feat/dep', '/tmp/dep', 'active', '2024-01-01T00:00:00Z', :ticket_id)",
            rusqlite::named_params![":ticket_id": dep_id],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, ticket_id) \
             VALUES ('wt-2', 'r1', 'feat-blocker', 'feat/blocker', '/tmp/blocker', 'active', '2024-01-02T00:00:00Z', :ticket_id)",
            rusqlite::named_params![":ticket_id": blocker_id],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-3', 'r1', 'feat-no-ticket', 'feat/no-ticket', '/tmp/no-ticket', 'active', '2024-01-03T00:00:00Z')",
            [],
        )
        .unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = wf_mgr
        .insert_step(&run.id, "mixed-foreach", "foreach", false, 0, 0)
        .unwrap();

    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "worktree".into(),
                item_id: "wt-1".into(),
                item_ref: "feat-dep".into(),
            },
        )
        .unwrap();
    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "worktree".into(),
                item_id: "wt-2".into(),
                item_ref: "feat-blocker".into(),
            },
        )
        .unwrap();
    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "worktree".into(),
                item_id: "wt-3".into(),
                item_ref: "feat-no-ticket".into(),
            },
        )
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let edges = load_worktree_dep_edges(&mut state, &step_id).unwrap();

    assert_eq!(edges.len(), 1, "expected one edge: wt-2 blocks wt-1");
    assert_eq!(
        edges[0],
        ("wt-2".to_string(), "wt-1".to_string()),
        "wt-2 (blocker) should point to wt-1 (dependent)"
    );
}

// -----------------------------------------------------------------------
// load_ticket_dep_edges tests
// -----------------------------------------------------------------------

/// Two tickets where ticket "1" blocks ticket "2". Fan-out both tickets.
/// Expect exactly one edge: (ticket1_id, ticket2_id).
#[test]
fn test_load_ticket_dep_edges_basic() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    let t1 = make_ticket("t1", "Blocker");
    let mut t2 = make_ticket("t2", "Blocked");
    t2.blocked_by = vec!["t1".to_string()];
    let syncer = TicketSyncer::new(&conn);
    syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

    let ticket1_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = 't1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    let ticket2_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = 't2'", [], |row| {
            row.get("id")
        })
        .unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = wf_mgr
        .insert_step(&run.id, "ticket-foreach", "foreach", false, 0, 0)
        .unwrap();

    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "ticket".into(),
                item_id: ticket1_id.clone(),
                item_ref: "t1".into(),
            },
        )
        .unwrap();
    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "ticket".into(),
                item_id: ticket2_id.clone(),
                item_ref: "t2".into(),
            },
        )
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let edges = load_ticket_dep_edges(&mut state, &step_id).unwrap();

    assert_eq!(edges.len(), 1, "expected exactly one dependency edge");
    assert_eq!(
        edges[0],
        (ticket1_id.clone(), ticket2_id.clone()),
        "ticket1 (blocker) should point to ticket2 (blocked)"
    );
}

/// Two tickets with no blocking relationship. Fan-out both. Expect empty edges.
#[test]
fn test_load_ticket_dep_edges_no_deps() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    let t1 = make_ticket("nd1", "Independent A");
    let t2 = make_ticket("nd2", "Independent B");
    let syncer = TicketSyncer::new(&conn);
    syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

    let ticket1_id: String = conn
        .query_row(
            "SELECT id FROM tickets WHERE source_id = 'nd1'",
            [],
            |row| row.get("id"),
        )
        .unwrap();
    let ticket2_id: String = conn
        .query_row(
            "SELECT id FROM tickets WHERE source_id = 'nd2'",
            [],
            |row| row.get("id"),
        )
        .unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = wf_mgr
        .insert_step(&run.id, "ticket-foreach", "foreach", false, 0, 0)
        .unwrap();

    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "ticket".into(),
                item_id: ticket1_id.clone(),
                item_ref: "nd1".into(),
            },
        )
        .unwrap();
    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "ticket".into(),
                item_id: ticket2_id.clone(),
                item_ref: "nd2".into(),
            },
        )
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let edges = load_ticket_dep_edges(&mut state, &step_id).unwrap();
    assert!(
        edges.is_empty(),
        "expected no edges for independent tickets"
    );
}

/// t-ext (external blocker, not in fan-out) blocks t-a. t-b is unrelated.
/// Fan-out only t-a and t-b. The raw edge (t-ext → t-a) must be filtered because
/// t-ext is not in the fan-out id_set. Expect empty edges.
#[test]
fn test_load_ticket_dep_edges_filters_external_blocker() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    let t_ext = make_ticket("ext", "External blocker");
    let mut t_a = make_ticket("ta", "Blocked by external");
    t_a.blocked_by = vec!["ext".to_string()];
    let t_b = make_ticket("tb", "Unrelated");
    let syncer = TicketSyncer::new(&conn);
    syncer.upsert_tickets("r1", &[t_ext, t_a, t_b]).unwrap();

    let ticket_a_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = 'ta'", [], |row| {
            row.get("id")
        })
        .unwrap();
    let ticket_b_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = 'tb'", [], |row| {
            row.get("id")
        })
        .unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let step_id = wf_mgr
        .insert_step(&run.id, "ticket-foreach", "foreach", false, 0, 0)
        .unwrap();

    // t-ext is intentionally NOT in the fan-out.
    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "ticket".into(),
                item_id: ticket_a_id.clone(),
                item_ref: "ta".into(),
            },
        )
        .unwrap();
    wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "ticket".into(),
                item_id: ticket_b_id.clone(),
                item_ref: "tb".into(),
            },
        )
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let edges = load_ticket_dep_edges(&mut state, &step_id).unwrap();
    assert!(
        edges.is_empty(),
        "external blocker not in fan-out must be filtered out"
    );
}

/// collect_worktree_items returns an error when repo_id is missing from the execution state.
#[test]
fn test_collect_worktree_items_no_repo_id_returns_error() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    // repo_id = None — collect_worktree_items must error
    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        None,
        None,
    );

    let node = ForEachNode {
        name: "wt-foreach".to_string(),
        over: "worktrees".to_string(),
        scope: Some(ForeachScope::Worktree(crate::workflow_dsl::WorktreeScope {
            base_branch: Some("release/1.0".to_string()),
            has_open_pr: None,
        })),
        filter: std::collections::HashMap::new(),
        ordered: false,
        on_cycle: OnCycle::Fail,
        max_parallel: 2,
        workflow: "child".to_string(),
        inputs: std::collections::HashMap::new(),
        on_child_fail: OnChildFail::Continue,
    };

    let result = collect_worktree_items(&mut state, &node, &HashSet::new());
    assert!(result.is_err(), "expected error when repo_id is missing");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("repo_id"),
        "error should mention repo_id, got: {msg}"
    );
}

/// collect_worktree_items errors when scope is None and worktree_id is also absent.
#[test]
fn test_collect_worktree_items_no_scope_no_worktree_id_errors() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    // worktree_id = None — no scope and no context worktree → must error
    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        None,
        Some("r1".to_string()),
        None,
    );

    let node = ForEachNode {
        name: "no-scope".to_string(),
        over: "worktrees".to_string(),
        scope: None,
        filter: std::collections::HashMap::new(),
        ordered: false,
        on_cycle: OnCycle::Fail,
        max_parallel: 2,
        workflow: "child".to_string(),
        inputs: std::collections::HashMap::new(),
        on_child_fail: OnChildFail::Continue,
    };

    let result = collect_worktree_items(&mut state, &node, &HashSet::new());
    assert!(
        result.is_err(),
        "expected error when neither scope nor worktree_id is set"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("worktree_id") || msg.contains("scope"),
        "error should mention scope or worktree_id, got: {msg}"
    );
}

/// collect_worktree_items infers base_branch from state.worktree_id when scope is omitted.
#[test]
fn test_collect_worktree_items_infers_base_branch_from_context() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    // "w1" has branch "feat/test"; the new inference uses wt.branch as the base_branch filter,
    // so we look for worktrees whose base_branch = "feat/test" (children of w1).
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-infer', 'r1', 'feat-infer', 'feat/infer', '/tmp/infer', 'active', '2024-01-05T00:00:00Z', 'feat/test')",
            [],
        ).unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    // worktree_id = "w1" (base_branch NULL → effective "main"), scope = None
    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let node = ForEachNode {
        name: "infer-foreach".to_string(),
        over: "worktrees".to_string(),
        scope: None,
        filter: std::collections::HashMap::new(),
        ordered: false,
        on_cycle: OnCycle::Fail,
        max_parallel: 3,
        workflow: "child".to_string(),
        inputs: std::collections::HashMap::new(),
        on_child_fail: OnChildFail::Continue,
    };

    let result = collect_worktree_items(&mut state, &node, &HashSet::new());
    assert!(
        result.is_ok(),
        "expected Ok when worktree_id is present, got: {:?}",
        result
    );
    let items = result.unwrap();
    // "wt-infer" has base_branch = "feat/test" (w1's branch); "w1" itself is excluded because
    // its base_branch is NULL, not "feat/test". Only "wt-infer" should match.
    let ids: Vec<&str> = items.iter().map(|(_, id, _)| id.as_str()).collect();
    assert!(
        ids.contains(&"wt-infer"),
        "should include wt-infer on main, got: {:?}",
        ids
    );
}

/// build_item_vars for a worktree with NULL base_branch and NULL ticket_id should
/// populate those fields with empty strings rather than panicking or erroring.
#[test]
fn test_build_item_vars_worktrees_null_optional_fields() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    // Insert a worktree with no base_branch and no ticket_id (both NULL).
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-null', 'r1', 'feat-null', 'feat/null', '/tmp/null', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let node = make_foreach_node_for("worktrees".to_string());
    let item = make_minimal_item("wt-null", "feat-null", "worktree");

    let vars = build_item_vars(&mut state, &node, &item).unwrap();
    assert_eq!(vars.get("item.slug").map(|s| s.as_str()), Some("feat-null"));
    assert_eq!(
        vars.get("item.branch").map(|s| s.as_str()),
        Some("feat/null")
    );
    assert_eq!(vars.get("item.path").map(|s| s.as_str()), Some("/tmp/null"));
    assert_eq!(
        vars.get("item.base_branch").map(|s| s.as_str()),
        Some(""),
        "NULL base_branch should become empty string"
    );
    assert_eq!(
        vars.get("item.ticket_id").map(|s| s.as_str()),
        Some(""),
        "NULL ticket_id should become empty string"
    );
}

/// build_item_vars for a worktree with a linked ticket_id should populate
/// vars["item.ticket_id"] with the ticket ULID.
#[test]
fn test_build_item_vars_worktrees_with_ticket_id() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    // setup_db() inserts repo "r1" — upsert a ticket against it to get a valid ULID.
    let syncer = TicketSyncer::new(&conn);
    syncer
        .upsert_tickets("r1", &[make_ticket("t-linked-1", "Linked ticket")])
        .unwrap();
    let ticket_id: String = conn
        .query_row(
            "SELECT id FROM tickets WHERE source_id = 't-linked-1' AND repo_id = 'r1'",
            [],
            |row| row.get("id"),
        )
        .unwrap();

    // Insert a worktree linked to the real ticket.
    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch, ticket_id) \
             VALUES ('wt-linked', 'r1', 'feat-linked', 'feat/linked', '/tmp/linked', 'active', '2024-01-01T00:00:00Z', 'release/1.0', :ticket_id)",
            rusqlite::named_params![":ticket_id": ticket_id],
        )
        .unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        Some("w1".to_string()),
        Some("r1".to_string()),
        None,
    );

    let node = make_foreach_node_for("worktrees".to_string());
    let item = make_minimal_item("wt-linked", "feat-linked", "worktree");

    let vars = build_item_vars(&mut state, &node, &item).unwrap();
    assert_eq!(
        vars.get("item.ticket_id").map(|s| s.as_str()),
        Some(ticket_id.as_str()),
        "ticket_id should be populated from the linked ticket"
    );
    assert_eq!(
        vars.get("item.base_branch").map(|s| s.as_str()),
        Some("release/1.0")
    );
    assert_eq!(
        vars.get("item.slug").map(|s| s.as_str()),
        Some("feat-linked")
    );
}

/// build_item_vars for Worktrees with a non-existent worktree ID falls back to
/// minimal vars (item.id, item.slug) without hard-failing.
#[test]
fn test_build_item_vars_worktrees_missing_worktree_falls_back() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        None,
        Some("r1".to_string()),
        None,
    );

    let node = make_foreach_node_for("worktrees".to_string());
    let item = make_minimal_item("nonexistent-wt", "some-slug", "worktree");

    let vars = build_item_vars(&mut state, &node, &item).unwrap();
    assert_eq!(
        vars.get("item.id").map(|s| s.as_str()),
        Some("nonexistent-wt")
    );
    assert_eq!(vars.get("item.slug").map(|s| s.as_str()), Some("some-slug"));
    assert!(!vars.contains_key("item.branch"));
    assert!(!vars.contains_key("item.path"));
    assert!(!vars.contains_key("item.base_branch"));
    assert!(!vars.contains_key("item.ticket_id"));
}

/// build_item_vars for Tickets with a non-existent ticket ID falls back to
/// minimal vars (item.id, item.source_id) without hard-failing.
#[test]
fn test_build_item_vars_tickets_missing_ticket_falls_back() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = make_execution_state_with_worktree(
        &conn,
        config,
        run.id,
        parent.id,
        None,
        Some("r1".to_string()),
        None,
    );

    let node = make_foreach_node_for("tickets".to_string());
    let item = make_minimal_item("nonexistent-ticket", "ISSUE-99", "ticket");

    let vars = build_item_vars(&mut state, &node, &item).unwrap();
    assert_eq!(
        vars.get("item.id").map(|s| s.as_str()),
        Some("nonexistent-ticket")
    );
    assert_eq!(
        vars.get("item.source_id").map(|s| s.as_str()),
        Some("ISSUE-99")
    );
    assert!(!vars.contains_key("item.title"));
    assert!(!vars.contains_key("item.url"));
    assert!(!vars.contains_key("item.state"));
    assert!(!vars.contains_key("item.labels"));
}

// -----------------------------------------------------------------------
// Resume / find-or-reuse step tests (#2306)
// -----------------------------------------------------------------------

/// Regression test for #2306: on resume with an existing non-completed foreach step,
/// `find_step_by_name_and_iteration` must find the old step, and
/// `reset_running_items_without_child_run` must reset orphaned items back to pending.
/// No second step row must be created.
#[test]
fn test_foreach_resume_reuses_existing_step_and_resets_orphaned_items() {
    let conn = setup_db();
    let _config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let step_key = "foreach:my-foreach";
    let iteration: i64 = 0;

    // Simulate a prior interrupted run: insert a step in 'running' state.
    let old_step_id = wf_mgr
        .insert_step(&run.id, step_key, "foreach", false, 0, iteration)
        .unwrap();
    wf_mgr
        .update_step_status(
            &old_step_id,
            crate::workflow::status::WorkflowStepStatus::Running,
            None,
            None,
            None,
            None,
            Some(0),
        )
        .unwrap();

    // Insert an orphaned item: running, no child_run_id.
    let orphan_item_id = wf_mgr
        .insert_fan_out_item(
            &old_step_id,
            &NewFanOutItem {
                item_type: "ticket".into(),
                item_id: "ticket-orphan".into(),
                item_ref: "orphan".into(),
            },
        )
        .unwrap();
    conn.execute(
        "UPDATE workflow_run_step_fan_out_items SET status = 'running' WHERE id = ?1",
        rusqlite::params![orphan_item_id],
    )
    .unwrap();

    // Verify find_step_by_name_and_iteration returns the old step.
    let found = wf_mgr
        .find_step_by_name_and_iteration(&run.id, step_key, iteration)
        .unwrap();
    assert!(
        found.is_some(),
        "must find the existing non-completed step on resume"
    );
    let found_step = found.unwrap();
    assert_eq!(
        found_step.id, old_step_id,
        "must reuse old step_id, not create a new one"
    );

    // Verify reset_running_items_without_child_run resets the orphaned item.
    let reset_count = wf_mgr
        .reset_running_items_without_child_run(&old_step_id)
        .unwrap();
    assert_eq!(reset_count, 1, "exactly one orphaned item should be reset");

    let items = wf_mgr.get_fan_out_items(&old_step_id, None).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].status, "pending",
        "orphaned item must be reset to pending so it is re-dispatched"
    );

    // Verify no second step row was created for this (run_id, step_name, iteration).
    let all_steps: Vec<_> = conn
        .prepare(
            "SELECT id FROM workflow_run_steps \
             WHERE workflow_run_id = ?1 AND step_name = ?2 AND iteration = ?3",
        )
        .unwrap()
        .query_map(rusqlite::params![&run.id, step_key, iteration], |row| {
            row.get::<_, String>(0)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        all_steps.len(),
        1,
        "only one step row must exist — no duplicate created on resume"
    );
    assert_eq!(all_steps[0], old_step_id);
}

/// Regression test for #2306: a running item that HAS a child_run_id must NOT be reset.
/// Only orphaned items (running, no child_run_id) should be affected.
#[test]
fn test_foreach_resume_preserves_running_items_with_child_run_id() {
    let conn = setup_db();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let step_id = wf_mgr
        .insert_step(&run.id, "foreach:check", "foreach", false, 0, 0)
        .unwrap();

    // Item with child_run_id: must remain running after the call.
    let item_with_child = wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "ticket".into(),
                item_id: "ticket-live".into(),
                item_ref: "live".into(),
            },
        )
        .unwrap();
    conn.execute(
        "UPDATE workflow_run_step_fan_out_items \
         SET status = 'running', child_run_id = 'real-child-run' WHERE id = ?1",
        rusqlite::params![item_with_child],
    )
    .unwrap();

    // Orphaned item: no child_run_id — should be reset.
    let orphan = wf_mgr
        .insert_fan_out_item(
            &step_id,
            &NewFanOutItem {
                item_type: "ticket".into(),
                item_id: "ticket-orphan".into(),
                item_ref: "orphan".into(),
            },
        )
        .unwrap();
    conn.execute(
        "UPDATE workflow_run_step_fan_out_items SET status = 'running' WHERE id = ?1",
        rusqlite::params![orphan],
    )
    .unwrap();

    let reset_count = wf_mgr
        .reset_running_items_without_child_run(&step_id)
        .unwrap();
    assert_eq!(reset_count, 1, "only the orphan should be reset");

    let items = wf_mgr.get_fan_out_items(&step_id, None).unwrap();
    let status_of = |id: &str| {
        items
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.status.as_str())
            .unwrap_or("NOT FOUND")
    };
    assert_eq!(
        status_of(&item_with_child),
        "running",
        "item with child_run_id must remain running"
    );
    assert_eq!(
        status_of(&orphan),
        "pending",
        "orphan must be reset to pending"
    );
}

/// build_item_vars for Repos with a non-existent repo ID falls back to
/// minimal vars (item.id, item.slug) without hard-failing.
#[test]
fn test_build_item_vars_repos_missing_repo_falls_back() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state =
        make_execution_state_with_worktree(&conn, config, run.id, parent.id, None, None, None);

    let node = make_foreach_node_for("repos".to_string());
    let item = make_minimal_item("nonexistent-repo", "my-repo", "repo");

    let vars = build_item_vars(&mut state, &node, &item).unwrap();
    assert_eq!(
        vars.get("item.id").map(|s| s.as_str()),
        Some("nonexistent-repo")
    );
    assert_eq!(vars.get("item.slug").map(|s| s.as_str()), Some("my-repo"));
    assert!(!vars.contains_key("item.local_path"));
    assert!(!vars.contains_key("item.remote_url"));
}

#[test]
fn test_execute_foreach_unknown_provider_returns_error() {
    let conn = setup_db();
    let config: &'static crate::config::Config =
        Box::leak(Box::new(crate::config::Config::default()));
    let mut state = crate::workflow::tests::common::make_loop_test_state(&conn, config);

    let node = ForEachNode {
        name: "test-unknown".to_string(),
        over: "nonexistent_provider_xyz".to_string(),
        scope: None,
        filter: std::collections::HashMap::new(),
        ordered: false,
        on_cycle: crate::workflow_dsl::OnCycle::Fail,
        max_parallel: 1,
        workflow: "some-workflow".to_string(),
        inputs: std::collections::HashMap::new(),
        on_child_fail: OnChildFail::Continue,
    };

    let result = execute_foreach(&mut state, &node, 0);
    assert!(result.is_err(), "unknown provider should return Err");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("unknown provider") || err_msg.contains("no ItemProvider"),
        "error should mention unknown provider, got: {err_msg}"
    );
}
