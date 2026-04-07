use super::*;

#[test]
fn dashboard_rows_repo_only() {
    let mut state = AppState::new();
    state.data.repos = vec![make_repo("r1", "repo-a")];
    let rows = state.dashboard_rows();
    assert_eq!(rows, vec![DashboardRow::Repo(0)]);
}

#[test]
fn dashboard_rows_flat_worktrees() {
    let mut state = AppState::new();
    state.data.repos = vec![make_repo("r1", "repo-a")];
    state.data.worktrees = vec![
        make_worktree(
            "wt1",
            "r1",
            None,
            conductor_core::worktree::WorktreeStatus::Active,
        ),
        make_worktree(
            "wt2",
            "r1",
            None,
            conductor_core::worktree::WorktreeStatus::Active,
        ),
    ];
    let rows = state.dashboard_rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], DashboardRow::Repo(0));
    match &rows[1] {
        DashboardRow::Worktree { prefix, .. } => assert_eq!(prefix, "  ├ "),
        other => panic!("expected Worktree, got {other:?}"),
    }
    match &rows[2] {
        DashboardRow::Worktree { prefix, .. } => assert_eq!(prefix, "  └ "),
        other => panic!("expected Worktree, got {other:?}"),
    }
}

#[test]
fn dashboard_rows_tree_ordered_parent_child() {
    let mut state = AppState::new();
    state.data.repos = vec![make_repo("r1", "repo-a")];
    state.data.worktrees = vec![
        make_worktree(
            "wt1",
            "r1",
            Some("feat/wt2"),
            conductor_core::worktree::WorktreeStatus::Active,
        ),
        make_worktree(
            "wt2",
            "r1",
            None,
            conductor_core::worktree::WorktreeStatus::Active,
        ),
    ];
    let rows = state.dashboard_rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], DashboardRow::Repo(0));
    match &rows[1] {
        DashboardRow::Worktree { idx, prefix } => {
            assert_eq!(*idx, 1, "wt2 should come first (parent)");
            assert_eq!(prefix, "  └ ", "sole root gets └ connector");
        }
        other => panic!("expected Worktree, got {other:?}"),
    }
    match &rows[2] {
        DashboardRow::Worktree { idx, prefix } => {
            assert_eq!(*idx, 0, "wt1 should come second (child)");
            assert_eq!(
                prefix, "    └ ",
                "child prefix: 2-space repo indent + to_prefix(depth=1, last)"
            );
        }
        other => panic!("expected Worktree, got {other:?}"),
    }
}

#[test]
fn current_dashboard_row_returns_correct_row() {
    let mut state = AppState::new();
    state.data.repos = vec![make_repo("r1", "repo-a")];
    state.data.worktrees = vec![make_worktree(
        "wt1",
        "r1",
        None,
        conductor_core::worktree::WorktreeStatus::Active,
    )];
    state.dashboard_index = 0;
    assert_eq!(state.current_dashboard_row(), Some(DashboardRow::Repo(0)));
    state.dashboard_index = 1;
    assert!(matches!(
        state.current_dashboard_row(),
        Some(DashboardRow::Worktree { idx: 0, .. })
    ));
}

#[test]
fn current_dashboard_row_out_of_bounds() {
    let mut state = AppState::new();
    state.data.repos = vec![make_repo("r1", "repo-a")];
    state.dashboard_index = 99;
    assert_eq!(state.current_dashboard_row(), None);
}

#[test]
fn current_dashboard_row_agrees_with_dashboard_rows() {
    let mut state = AppState::new();
    state.data.repos = vec![make_repo("r1", "repo-a"), make_repo("r2", "repo-b")];
    state.data.worktrees = vec![
        make_worktree(
            "wt1",
            "r1",
            None,
            conductor_core::worktree::WorktreeStatus::Active,
        ),
        make_worktree(
            "wt2",
            "r2",
            None,
            conductor_core::worktree::WorktreeStatus::Active,
        ),
    ];
    let rows = state.dashboard_rows();
    for (i, row) in rows.iter().enumerate() {
        state.dashboard_index = i;
        assert_eq!(state.current_dashboard_row().as_ref(), Some(row));
    }
    state.dashboard_index = rows.len();
    assert_eq!(state.current_dashboard_row(), None);
}

#[test]
fn dashboard_rows_always_reflects_current_data() {
    let mut state = AppState::new();
    state.data.repos = vec![make_repo("r1", "repo-a")];
    state.data.worktrees = vec![make_worktree(
        "wt1",
        "r1",
        None,
        conductor_core::worktree::WorktreeStatus::Active,
    )];

    let rows1 = state.dashboard_rows();
    assert_eq!(rows1.len(), 2);

    state.data.worktrees.push(make_worktree(
        "wt2",
        "r1",
        None,
        conductor_core::worktree::WorktreeStatus::Active,
    ));

    let rows2 = state.dashboard_rows();
    assert_eq!(rows2.len(), 3, "should immediately see Repo + 2 Worktrees");
}

#[test]
fn dashboard_rows_multi_level_tree() {
    let mut state = AppState::new();
    state.data.repos = vec![make_repo("r1", "repo-a")];
    state.data.worktrees = vec![
        make_worktree(
            "wt_leaf",
            "r1",
            Some("feat/wt_mid"),
            conductor_core::worktree::WorktreeStatus::Active,
        ),
        make_worktree(
            "wt_root",
            "r1",
            None,
            conductor_core::worktree::WorktreeStatus::Active,
        ),
        make_worktree(
            "wt_mid",
            "r1",
            Some("feat/wt_root"),
            conductor_core::worktree::WorktreeStatus::Active,
        ),
    ];
    let rows = state.dashboard_rows();
    assert_eq!(rows.len(), 4);
    match &rows[1] {
        DashboardRow::Worktree { idx, prefix } => {
            assert_eq!(*idx, 1, "wt_root first");
            assert_eq!(prefix, "  └ ", "sole root gets └ connector");
        }
        other => panic!("expected Worktree, got {other:?}"),
    }
    match &rows[2] {
        DashboardRow::Worktree { idx, prefix } => {
            assert_eq!(*idx, 2, "wt_mid second");
            assert_eq!(
                prefix, "    └ ",
                "child prefix: 2-space repo indent + to_prefix(depth=1, last)"
            );
        }
        other => panic!("expected Worktree, got {other:?}"),
    }
    match &rows[3] {
        DashboardRow::Worktree { idx, prefix } => {
            assert_eq!(*idx, 0, "wt_leaf third");
            assert_eq!(
                prefix, "      └ ",
                "grandchild prefix: 2-space repo indent + to_prefix(depth=2, last)"
            );
        }
        other => panic!("expected Worktree, got {other:?}"),
    }
}
