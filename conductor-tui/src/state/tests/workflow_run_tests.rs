use super::*;
use conductor_core::workflow::WorkflowRunStatus;

#[test]
fn visible_workflow_run_rows_empty() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    assert!(state.visible_workflow_run_rows().is_empty());
}

#[test]
fn visible_workflow_run_rows_single_parent_no_children() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Running, None)];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0], WorkflowRunRow::Parent { run_id, child_count: 0, collapsed: false, .. } if run_id == "p1")
    );
}

#[test]
fn visible_workflow_run_rows_parent_with_child_expanded() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 2);
    assert!(
        matches!(&rows[0], WorkflowRunRow::Parent { run_id, child_count: 1, collapsed: false, .. } if run_id == "p1")
    );
    assert!(matches!(&rows[1], WorkflowRunRow::Child { run_id, .. } if run_id == "c1"));
}

#[test]
fn visible_workflow_run_rows_parent_with_child_collapsed() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
    ];
    state.collapsed_workflow_run_ids.insert("p1".into());
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0], WorkflowRunRow::Parent { run_id, child_count: 1, collapsed: true, .. } if run_id == "p1")
    );
}

#[test]
fn visible_workflow_run_rows_orphaned_child_treated_as_root() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![make_wf_run_full(
        "c1",
        WorkflowRunStatus::Running,
        Some("nonexistent"),
    )];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(&rows[0], WorkflowRunRow::Parent { run_id, child_count: 0, .. } if run_id == "c1")
    );
}

// --- global mode grouping tests ---

fn make_wf_run_with_label(
    id: &str,
    target_label: Option<&str>,
    repo_id: Option<&str>,
) -> conductor_core::workflow::WorkflowRun {
    conductor_core::workflow::WorkflowRun {
        id: id.into(),
        workflow_name: "test-workflow".into(),
        worktree_id: None,
        parent_run_id: "run-1".into(),
        status: WorkflowRunStatus::Running,
        dry_run: false,
        trigger: "manual".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
        ended_at: None,
        result_summary: None,
        definition_snapshot: None,
        inputs: std::collections::HashMap::new(),
        ticket_id: None,
        repo_id: repo_id.map(|s| s.into()),
        parent_workflow_run_id: None,
        target_label: target_label.map(|s| s.into()),
        default_bot_name: None,
        iteration: 0,
        blocked_on: None,
        feature_id: None,
    }
}

#[test]
fn parse_target_label_worktree_format() {
    let (repo, key, ty) = parse_target_label("my-repo/feat-123");
    assert_eq!(repo, "my-repo");
    assert_eq!(key, "feat-123");
    assert_eq!(ty, TargetType::Worktree);
}

#[test]
fn parse_target_label_pr_format() {
    let (repo, key, ty) = parse_target_label("owner/repo#42");
    assert_eq!(repo, "unknown");
    assert_eq!(key, "owner/repo#42");
    assert_eq!(ty, TargetType::Pr);
}

#[test]
fn parse_target_label_no_slash() {
    let (repo, key, ty) = parse_target_label("standalone");
    assert_eq!(repo, "unknown");
    assert_eq!(key, "standalone");
    assert_eq!(ty, TargetType::Worktree);
}

#[test]
fn global_mode_groups_by_repo_then_target() {
    let mut state = AppState::new();
    state.data.workflow_runs = vec![
        make_wf_run_with_label("r1", Some("repo-a/feat-1"), None),
        make_wf_run_with_label("r2", Some("repo-a/feat-2"), None),
        make_wf_run_with_label("r3", Some("repo-b/feat-3"), None),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 8);
    assert!(
        matches!(&rows[0], WorkflowRunRow::RepoHeader { repo_slug, .. } if repo_slug == "repo-a")
    );
    assert!(matches!(&rows[1], WorkflowRunRow::TargetHeader { label, .. } if label == "feat-1"));
    assert!(matches!(&rows[2], WorkflowRunRow::Parent { run_id, .. } if run_id == "r1"));
    assert!(matches!(&rows[3], WorkflowRunRow::TargetHeader { label, .. } if label == "feat-2"));
    assert!(matches!(&rows[4], WorkflowRunRow::Parent { run_id, .. } if run_id == "r2"));
    assert!(
        matches!(&rows[5], WorkflowRunRow::RepoHeader { repo_slug, .. } if repo_slug == "repo-b")
    );
    assert!(matches!(&rows[6], WorkflowRunRow::TargetHeader { label, .. } if label == "feat-3"));
    assert!(matches!(&rows[7], WorkflowRunRow::Parent { run_id, .. } if run_id == "r3"));
}

#[test]
fn global_mode_collapsed_repo_hides_children() {
    let mut state = AppState::new();
    state.data.workflow_runs = vec![
        make_wf_run_with_label("r1", Some("repo-a/feat-1"), None),
        make_wf_run_with_label("r2", Some("repo-b/feat-2"), None),
    ];
    state.collapsed_repo_headers.insert("repo-a".into());
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 4);
    assert!(
        matches!(&rows[0], WorkflowRunRow::RepoHeader { repo_slug, collapsed: true, .. } if repo_slug == "repo-a")
    );
    assert!(
        matches!(&rows[1], WorkflowRunRow::RepoHeader { repo_slug, collapsed: false, .. } if repo_slug == "repo-b")
    );
}

#[test]
fn global_mode_collapsed_target_hides_runs() {
    let mut state = AppState::new();
    state.data.workflow_runs = vec![
        make_wf_run_with_label("r1", Some("repo-a/feat-1"), None),
        make_wf_run_with_label("r2", Some("repo-a/feat-2"), None),
    ];
    state
        .collapsed_target_headers
        .insert("repo-a/feat-1".into());
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 4);
    assert!(matches!(&rows[0], WorkflowRunRow::RepoHeader { .. }));
    assert!(
        matches!(&rows[1], WorkflowRunRow::TargetHeader { label, collapsed: true, .. } if label == "feat-1")
    );
    assert!(
        matches!(&rows[2], WorkflowRunRow::TargetHeader { label, collapsed: false, .. } if label == "feat-2")
    );
    assert!(matches!(&rows[3], WorkflowRunRow::Parent { run_id, .. } if run_id == "r2"));
}

#[test]
fn global_mode_pr_run_uses_repo_id_fallback() {
    use conductor_core::repo::Repo;
    let mut state = AppState::new();
    state.data.repos = vec![Repo {
        id: "repo-id-1".into(),
        slug: "my-repo".into(),
        remote_url: String::new(),
        local_path: String::new(),
        default_branch: String::new(),
        workspace_dir: String::new(),
        created_at: String::new(),
        model: None,
        allow_agent_issue_creation: false,
    }];
    state.data.workflow_runs = vec![make_wf_run_with_label(
        "pr1",
        Some("owner/repo#99"),
        Some("repo-id-1"),
    )];
    let rows = state.visible_workflow_run_rows();
    assert!(
        matches!(&rows[0], WorkflowRunRow::RepoHeader { repo_slug, .. } if repo_slug == "my-repo")
    );
}

#[test]
fn global_mode_run_without_label_buckets_under_unknown() {
    let mut state = AppState::new();
    state.data.workflow_runs = vec![make_wf_run_with_label("r1", None, None)];
    let rows = state.visible_workflow_run_rows();
    assert!(
        matches!(&rows[0], WorkflowRunRow::RepoHeader { repo_slug, .. } if repo_slug == "unknown")
    );
}

// --- multi-level expand/collapse tests ---

#[test]
fn visible_workflow_run_rows_grandchild_expanded() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
        make_wf_run_full("gc1", WorkflowRunStatus::Running, Some("c1")),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 3);
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
    assert!(
        matches!(&rows[1], WorkflowRunRow::Child { run_id, depth: 1, child_count: 1, collapsed: false, .. } if run_id == "c1")
    );
    assert!(
        matches!(&rows[2], WorkflowRunRow::Child { run_id, depth: 2, child_count: 0, collapsed: false, .. } if run_id == "gc1")
    );
}

#[test]
fn visible_workflow_run_rows_intermediate_child_collapsed() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
        make_wf_run_full("gc1", WorkflowRunStatus::Running, Some("c1")),
    ];
    state.collapsed_workflow_run_ids.insert("c1".into());
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 2);
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
    assert!(
        matches!(&rows[1], WorkflowRunRow::Child { run_id, depth: 1, child_count: 1, collapsed: true, .. } if run_id == "c1")
    );
}

#[test]
fn visible_workflow_run_rows_child_depth_values() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
        make_wf_run_full("c2", WorkflowRunStatus::Running, Some("c1")),
        make_wf_run_full("c3", WorkflowRunStatus::Running, Some("c2")),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 4);
    assert!(matches!(&rows[1], WorkflowRunRow::Child { run_id, depth: 1, .. } if run_id == "c1"));
    assert!(matches!(&rows[2], WorkflowRunRow::Child { run_id, depth: 2, .. } if run_id == "c2"));
    assert!(matches!(&rows[3], WorkflowRunRow::Child { run_id, depth: 3, .. } if run_id == "c3"));
}

#[test]
fn visible_workflow_run_rows_leaf_child_count_zero() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 2);
    assert!(
        matches!(&rows[1], WorkflowRunRow::Child { run_id, child_count: 0, collapsed: false, depth: 1, .. } if run_id == "c1")
    );
}

// --- Step row tests ---

#[test]
fn visible_workflow_run_rows_step_rows_appear_when_expanded() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.show_completed_workflow_runs = true;
    state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Completed, None)];
    state.data.workflow_run_steps.insert(
        "p1".into(),
        vec![
            make_wf_step("s1", "p1", "lint", 0),
            make_wf_step("s2", "p1", "test", 1),
        ],
    );
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));

    state.expanded_step_run_ids.insert("p1".into());
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 3);
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
    assert!(matches!(&rows[1], WorkflowRunRow::Step { step_name, .. } if step_name == "lint"));
    assert!(matches!(&rows[2], WorkflowRunRow::Step { step_name, .. } if step_name == "test"));
}

#[test]
fn visible_workflow_run_rows_steps_sorted_by_position() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.show_completed_workflow_runs = true;
    state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Completed, None)];
    state.data.workflow_run_steps.insert(
        "p1".into(),
        vec![
            make_wf_step("s3", "p1", "deploy", 2),
            make_wf_step("s1", "p1", "lint", 0),
            make_wf_step("s2", "p1", "test", 1),
        ],
    );
    state.expanded_step_run_ids.insert("p1".into());
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 4);
    assert!(
        matches!(&rows[1], WorkflowRunRow::Step { step_name, position: 0, .. } if step_name == "lint")
    );
    assert!(
        matches!(&rows[2], WorkflowRunRow::Step { step_name, position: 1, .. } if step_name == "test")
    );
    assert!(
        matches!(&rows[3], WorkflowRunRow::Step { step_name, position: 2, .. } if step_name == "deploy")
    );
}

#[test]
fn visible_workflow_run_rows_steps_for_leaf_child_run() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.show_completed_workflow_runs = true;
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Completed, None),
        make_wf_run_full("c1", WorkflowRunStatus::Completed, Some("p1")),
    ];
    state
        .data
        .workflow_run_steps
        .insert("c1".into(), vec![make_wf_step("s1", "c1", "review", 0)]);
    state.expanded_step_run_ids.insert("c1".into());
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 3);
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
    assert!(matches!(&rows[1], WorkflowRunRow::Child { run_id, .. } if run_id == "c1"));
    assert!(
        matches!(&rows[2], WorkflowRunRow::Step { run_id, step_name, depth: 2, .. } if run_id == "c1" && step_name == "review")
    );
}

#[test]
fn visible_workflow_run_rows_filters_completed_by_default() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("r1", WorkflowRunStatus::Completed, None),
        make_wf_run_full("r2", WorkflowRunStatus::Cancelled, None),
        make_wf_run_full("r3", WorkflowRunStatus::Failed, None),
        make_wf_run_full("r4", WorkflowRunStatus::Running, None),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 2);
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "r3"));
    assert!(matches!(&rows[1], WorkflowRunRow::Parent { run_id, .. } if run_id == "r4"));
    assert_eq!(state.hidden_workflow_run_count(), 2);

    state.show_completed_workflow_runs = true;
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 4);
    assert_eq!(state.hidden_workflow_run_count(), 0);
}

#[test]
fn visible_workflow_run_rows_no_steps_without_data() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.show_completed_workflow_runs = true;
    state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Completed, None)];
    state.expanded_step_run_ids.insert("p1".into());
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
}

#[test]
fn visible_workflow_run_rows_parallel_group_header_and_members() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.show_completed_workflow_runs = true;
    state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Completed, None)];

    let mut lint = make_wf_step("s1", "p1", "lint", 0);
    lint.parallel_group_id = Some("g1".into());
    let mut test = make_wf_step("s2", "p1", "test", 1);
    test.parallel_group_id = Some("g1".into());
    let deploy = make_wf_step("s3", "p1", "deploy", 2);

    state
        .data
        .workflow_run_steps
        .insert("p1".into(), vec![lint, test, deploy]);
    state.expanded_step_run_ids.insert("p1".into());

    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 5);
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
    assert!(matches!(
        &rows[1],
        WorkflowRunRow::ParallelGroup {
            count: 2,
            depth: 1,
            ..
        }
    ));
    assert!(
        matches!(&rows[2], WorkflowRunRow::Step { step_name, depth: 2, .. } if step_name == "lint")
    );
    assert!(
        matches!(&rows[3], WorkflowRunRow::Step { step_name, depth: 2, .. } if step_name == "test")
    );
    assert!(
        matches!(&rows[4], WorkflowRunRow::Step { step_name, depth: 1, .. } if step_name == "deploy")
    );
}

// --- repo-detail mode slug label tests ---

fn make_wf_run_with_target(
    id: &str,
    target_label: Option<&str>,
) -> conductor_core::workflow::WorkflowRun {
    let mut run = make_wf_run_full(id, WorkflowRunStatus::Running, None);
    run.target_label = target_label.map(|s| s.into());
    run
}

fn set_repo_detail_mode(state: &mut AppState, repo_id: &str) {
    state.selected_repo_id = Some(repo_id.into());
    state.selected_worktree_id = None;
}

#[test]
fn repo_detail_mode_emits_slug_labels() {
    let mut state = AppState::new();
    set_repo_detail_mode(&mut state, "repo-1");
    state.data.workflow_runs = vec![
        make_wf_run_with_target("r1", Some("my-repo/feat-123")),
        make_wf_run_with_target("r2", Some("my-repo/feat-456")),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 4);
    assert!(matches!(&rows[0], WorkflowRunRow::SlugLabel { label } if label == "feat-123"));
    assert!(matches!(&rows[1], WorkflowRunRow::Parent { run_id, .. } if run_id == "r1"));
    assert!(matches!(&rows[2], WorkflowRunRow::SlugLabel { label } if label == "feat-456"));
    assert!(matches!(&rows[3], WorkflowRunRow::Parent { run_id, .. } if run_id == "r2"));
}

#[test]
fn repo_detail_mode_consecutive_deduplication() {
    let mut state = AppState::new();
    set_repo_detail_mode(&mut state, "repo-1");
    state.data.workflow_runs = vec![
        make_wf_run_with_target("r1", Some("my-repo/feat-123")),
        make_wf_run_with_target("r2", Some("my-repo/feat-123")),
        make_wf_run_with_target("r3", Some("my-repo/feat-456")),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 5);
    assert!(matches!(&rows[0], WorkflowRunRow::SlugLabel { label } if label == "feat-123"));
    assert!(matches!(&rows[1], WorkflowRunRow::Parent { run_id, .. } if run_id == "r1"));
    assert!(matches!(&rows[2], WorkflowRunRow::Parent { run_id, .. } if run_id == "r2"));
    assert!(matches!(&rows[3], WorkflowRunRow::SlugLabel { label } if label == "feat-456"));
    assert!(matches!(&rows[4], WorkflowRunRow::Parent { run_id, .. } if run_id == "r3"));
}

#[test]
fn repo_detail_mode_no_slug_label_for_missing_target() {
    let mut state = AppState::new();
    set_repo_detail_mode(&mut state, "repo-1");
    state.data.workflow_runs = vec![make_wf_run_with_target("r1", None)];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "r1"));
}

#[test]
fn repo_detail_mode_no_slug_label_for_pr_format_target() {
    let mut state = AppState::new();
    set_repo_detail_mode(&mut state, "repo-1");
    state.data.workflow_runs = vec![make_wf_run_with_target("r1", Some("owner/repo#42"))];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "r1"));
}

#[test]
fn worktree_detail_mode_no_slug_labels() {
    let mut state = AppState::new();
    state.selected_worktree_id = Some("wt-id".into());
    state.selected_repo_id = Some("repo-1".into());
    state.data.workflow_runs = vec![
        make_wf_run_with_target("r1", Some("my-repo/feat-123")),
        make_wf_run_with_target("r2", Some("my-repo/feat-456")),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 2);
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "r1"));
    assert!(matches!(&rows[1], WorkflowRunRow::Parent { run_id, .. } if run_id == "r2"));
}

// --- iteration-related step & child tests ---

#[test]
fn push_steps_for_run_shows_only_latest_iteration() {
    use super::super::workflow_rows::push_steps_for_run;
    let steps = vec![
        make_iter_step("run1", "step-a", 0, 0),
        make_iter_step("run1", "step-b", 0, 1),
        make_iter_step("run1", "step-a", 1, 0),
        make_iter_step("run1", "step-b", 1, 1),
    ];
    let mut map = std::collections::HashMap::new();
    map.insert("run1".to_string(), steps);

    let mut expanded = std::collections::HashSet::new();
    expanded.insert("run1".to_string());

    let mut rows = vec![];
    push_steps_for_run("run1", 1, &mut rows, &expanded, &map);

    assert_eq!(
        rows.len(),
        2,
        "expected 2 step rows (one per step in iter 1)"
    );
    for row in &rows {
        match row {
            WorkflowRunRow::Step { step_id, .. } => {
                assert!(
                    step_id.ends_with("-1"),
                    "expected iter-1 step id, got {step_id}"
                );
            }
            other => panic!("unexpected row type: {other:?}"),
        }
    }
}

#[test]
fn push_steps_for_run_not_expanded_emits_no_rows() {
    use super::super::workflow_rows::push_steps_for_run;
    let steps = vec![make_iter_step("run1", "step-a", 0, 0)];
    let mut map = std::collections::HashMap::new();
    map.insert("run1".to_string(), steps);

    let expanded = std::collections::HashSet::new();
    let mut rows = vec![];
    push_steps_for_run("run1", 1, &mut rows, &expanded, &map);
    assert!(rows.is_empty());
}

#[test]
fn push_steps_for_run_single_iteration_emits_all_steps() {
    use super::super::workflow_rows::push_steps_for_run;
    let steps = vec![
        make_iter_step("run1", "step-a", 0, 0),
        make_iter_step("run1", "step-b", 0, 1),
        make_iter_step("run1", "step-c", 0, 2),
    ];
    let mut map = std::collections::HashMap::new();
    map.insert("run1".to_string(), steps);

    let mut expanded = std::collections::HashSet::new();
    expanded.insert("run1".to_string());

    let mut rows = vec![];
    push_steps_for_run("run1", 1, &mut rows, &expanded, &map);
    assert_eq!(rows.len(), 3);
}

#[test]
fn push_steps_for_run_partial_loop_uses_per_step_max() {
    use super::super::workflow_rows::push_steps_for_run;
    let steps = vec![
        make_iter_step("run1", "step-a", 0, 0),
        make_iter_step("run1", "step-b", 0, 1),
        make_iter_step("run1", "step-a", 1, 0),
    ];
    let mut map = std::collections::HashMap::new();
    map.insert("run1".to_string(), steps);

    let mut expanded = std::collections::HashSet::new();
    expanded.insert("run1".to_string());

    let mut rows = vec![];
    push_steps_for_run("run1", 1, &mut rows, &expanded, &map);

    assert_eq!(rows.len(), 2, "both steps should appear");
    let names: Vec<_> = rows
        .iter()
        .map(|r| match r {
            WorkflowRunRow::Step { step_name, .. } => step_name.clone(),
            other => panic!("unexpected row: {other:?}"),
        })
        .collect();
    assert!(names.contains(&"step-a".to_string()));
    assert!(names.contains(&"step-b".to_string()));
}

#[test]
fn max_iteration_for_run_returns_zero_for_unknown_run() {
    use super::super::workflow_rows::max_iteration_for_run;
    let map = std::collections::HashMap::new();
    assert_eq!(max_iteration_for_run("nonexistent", &map), 0);
}

#[test]
fn max_iteration_for_run_returns_highest() {
    use super::super::workflow_rows::max_iteration_for_run;
    let steps = vec![
        make_iter_step("run1", "step-a", 0, 0),
        make_iter_step("run1", "step-a", 3, 0),
        make_iter_step("run1", "step-a", 1, 0),
    ];
    let mut map = std::collections::HashMap::new();
    map.insert("run1".to_string(), steps);
    assert_eq!(max_iteration_for_run("run1", &map), 3);
}

#[test]
fn visible_workflow_run_rows_parent_max_iteration_populated() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Running, None)];
    state.data.workflow_run_steps.insert(
        "p1".to_string(),
        vec![
            make_iter_step("p1", "step-a", 0, 0),
            make_iter_step("p1", "step-a", 1, 0),
            make_iter_step("p1", "step-a", 2, 0),
        ],
    );
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 1);
    match &rows[0] {
        WorkflowRunRow::Parent {
            run_id,
            max_iteration,
            ..
        } => {
            assert_eq!(run_id, "p1");
            assert_eq!(
                *max_iteration, 2,
                "expected max_iteration=2 (3rd iteration, 0-indexed)"
            );
        }
        other => panic!("expected Parent row, got {other:?}"),
    }
}

#[test]
fn visible_workflow_run_rows_child_max_iteration_populated() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
    ];
    state.data.workflow_run_steps.insert(
        "c1".to_string(),
        vec![
            make_iter_step("c1", "step-x", 0, 0),
            make_iter_step("c1", "step-x", 1, 0),
        ],
    );
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 2);
    match &rows[1] {
        WorkflowRunRow::Child {
            run_id,
            max_iteration,
            ..
        } => {
            assert_eq!(run_id, "c1");
            assert_eq!(
                *max_iteration, 1,
                "expected max_iteration=1 for child with 2 iterations"
            );
        }
        other => panic!("expected Child row, got {other:?}"),
    }
}

// --- loop iteration child-filtering tests ---

#[test]
fn visible_workflow_run_rows_loop_shows_only_latest_iteration_children() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_with_iter(
            "c1",
            WorkflowRunStatus::Completed,
            Some("p1"),
            "review-pr",
            0,
        ),
        make_wf_run_with_iter("c2", WorkflowRunStatus::Completed, Some("p1"), "fix-pr", 0),
        make_wf_run_with_iter("c3", WorkflowRunStatus::Completed, Some("p1"), "test-pr", 0),
        make_wf_run_with_iter("d1", WorkflowRunStatus::Running, Some("p1"), "review-pr", 1),
        make_wf_run_with_iter("d2", WorkflowRunStatus::Running, Some("p1"), "fix-pr", 1),
        make_wf_run_with_iter("d3", WorkflowRunStatus::Running, Some("p1"), "test-pr", 1),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(
        rows.len(),
        4,
        "expected parent + 3 latest-iteration children"
    );
    let child_ids: Vec<_> = rows
        .iter()
        .filter_map(|r| {
            if let WorkflowRunRow::Child { run_id, .. } = r {
                Some(run_id.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(child_ids.contains(&"d1"), "d1 should be visible");
    assert!(child_ids.contains(&"d2"), "d2 should be visible");
    assert!(child_ids.contains(&"d3"), "d3 should be visible");
    assert!(!child_ids.contains(&"c1"), "c1 (iter 0) must be hidden");
    assert!(!child_ids.contains(&"c2"), "c2 (iter 0) must be hidden");
    assert!(!child_ids.contains(&"c3"), "c3 (iter 0) must be hidden");
}

#[test]
fn visible_workflow_run_rows_loop_all_iter_zero_shows_all_children() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_full("c1", WorkflowRunStatus::Completed, Some("p1")),
        make_wf_run_full("c2", WorkflowRunStatus::Running, Some("p1")),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(
        rows.len(),
        3,
        "parent + 2 children (all at iter 0, no filter)"
    );
}

#[test]
fn visible_workflow_run_rows_loop_partial_iteration_shows_latest() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_with_iter(
            "c1",
            WorkflowRunStatus::Completed,
            Some("p1"),
            "review-pr",
            0,
        ),
        make_wf_run_with_iter("c2", WorkflowRunStatus::Completed, Some("p1"), "fix-pr", 0),
        make_wf_run_with_iter("c3", WorkflowRunStatus::Completed, Some("p1"), "test-pr", 0),
        make_wf_run_with_iter("d1", WorkflowRunStatus::Running, Some("p1"), "review-pr", 1),
        make_wf_run_with_iter("d2", WorkflowRunStatus::Running, Some("p1"), "fix-pr", 1),
    ];
    let rows = state.visible_workflow_run_rows();
    assert_eq!(rows.len(), 4, "expected parent + 3 children (partial iter)");
    let child_ids: Vec<_> = rows
        .iter()
        .filter_map(|r| {
            if let WorkflowRunRow::Child { run_id, .. } = r {
                Some(run_id.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(
        child_ids.contains(&"d1"),
        "d1 (review-pr iter 1) should be visible"
    );
    assert!(
        child_ids.contains(&"d2"),
        "d2 (fix-pr iter 1) should be visible"
    );
    assert!(
        child_ids.contains(&"c3"),
        "c3 (test-pr iter 0, still latest) should be visible"
    );
    assert!(
        !child_ids.contains(&"c1"),
        "c1 (review-pr iter 0) must be hidden"
    );
    assert!(
        !child_ids.contains(&"c2"),
        "c2 (fix-pr iter 0) must be hidden"
    );
}

// --- direct-step interleaving tests ---

#[test]
fn push_children_interleaves_direct_steps_with_child_runs() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_with_iter("c1", WorkflowRunStatus::Running, Some("p1"), "sub-wf", 0),
    ];
    let mut agent_step = make_iter_step("p1", "agent-step", 0, 0);
    agent_step.child_run_id = Some("agent-run-1".to_string());
    agent_step.role = "actor".to_string();
    let mut wf_step = make_iter_step("p1", "workflow:sub-wf", 0, 1);
    wf_step.child_run_id = Some("c1".to_string());
    wf_step.role = "workflow".to_string();
    state
        .data
        .workflow_run_steps
        .insert("p1".to_string(), vec![agent_step, wf_step]);
    state.expanded_step_run_ids.insert("p1".to_string());
    let rows = state.visible_workflow_run_rows();
    assert_eq!(
        rows.len(),
        3,
        "expected parent + agent step + child run, got {:?}",
        rows
    );
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
    assert!(
        matches!(&rows[1], WorkflowRunRow::Step { step_name, .. } if step_name == "agent-step")
    );
    assert!(matches!(&rows[2], WorkflowRunRow::Child { run_id, .. } if run_id == "c1"));
}

#[test]
fn push_children_global_max_iter_filters_old_iteration_direct_steps() {
    let mut state = AppState::new();
    set_worktree_mode(&mut state);
    state.data.workflow_runs = vec![
        make_wf_run_full("p1", WorkflowRunStatus::Running, None),
        make_wf_run_with_iter("c1", WorkflowRunStatus::Running, Some("p1"), "sub-wf", 1),
    ];

    let step_a_iter0 = make_iter_step("p1", "step-a", 0, 0);
    let step_b_iter0 = make_iter_step("p1", "step-b", 0, 1);
    let mut wf_step_iter0 = make_iter_step("p1", "workflow:sub-wf", 0, 2);
    wf_step_iter0.child_run_id = Some("c0".to_string());
    let step_a_iter1 = make_iter_step("p1", "step-a", 1, 3);
    let mut wf_step_iter1 = make_iter_step("p1", "workflow:sub-wf", 1, 4);
    wf_step_iter1.child_run_id = Some("c1".to_string());

    state.data.workflow_run_steps.insert(
        "p1".to_string(),
        vec![
            step_a_iter0,
            step_b_iter0,
            wf_step_iter0,
            step_a_iter1,
            wf_step_iter1,
        ],
    );

    state.expanded_step_run_ids.insert("p1".to_string());

    let rows = state.visible_workflow_run_rows();

    assert_eq!(
        rows.len(),
        3,
        "expected parent + 1 iteration-1 step + 1 child run, got {:?}",
        rows
    );
    assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
    assert!(
        matches!(&rows[1], WorkflowRunRow::Step { step_name, position, .. }
            if step_name == "step-a" && *position == 3),
        "only iteration 1 direct step (position 3) should appear, got {:?}",
        rows[1]
    );
    assert!(
        matches!(&rows[2], WorkflowRunRow::Child { run_id, .. } if run_id == "c1"),
        "child run c1 should appear, got {:?}",
        rows[2]
    );
}
