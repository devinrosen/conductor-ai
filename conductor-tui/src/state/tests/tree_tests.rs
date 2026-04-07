use super::*;

#[test]
fn build_worktree_tree_flat_list() {
    let wts = vec![make_wt("feat/b", None), make_wt("feat/a", None)];
    let (ordered, positions) = build_worktree_tree(&wts, "main");
    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].branch, "feat/a");
    assert_eq!(ordered[1].branch, "feat/b");
    assert_eq!(positions[0].depth, 0);
    assert_eq!(positions[1].depth, 0);
}

#[test]
fn build_worktree_tree_parent_child() {
    let wts = vec![
        make_wt("feat/parent", None),
        make_wt("feat/child", Some("feat/parent")),
    ];
    let (ordered, positions) = build_worktree_tree(&wts, "main");
    assert_eq!(ordered[0].branch, "feat/parent");
    assert_eq!(ordered[1].branch, "feat/child");
    assert_eq!(positions[0].depth, 0);
    assert_eq!(positions[1].depth, 1);
    assert!(positions[1].is_last_sibling);
}

#[test]
fn build_worktree_tree_nested_hierarchy() {
    let wts = vec![
        make_wt("feat/test", None),
        make_wt("feat/test-child-1", Some("feat/test")),
        make_wt("feat/test-child-2", Some("feat/test")),
        make_wt("feat/test-grandchild", Some("feat/test-child-1")),
        make_wt("feat/test-three", None),
    ];
    let (ordered, positions) = build_worktree_tree(&wts, "main");
    assert_eq!(ordered[0].branch, "feat/test");
    assert_eq!(ordered[1].branch, "feat/test-child-1");
    assert_eq!(ordered[2].branch, "feat/test-grandchild");
    assert_eq!(ordered[3].branch, "feat/test-child-2");
    assert_eq!(ordered[4].branch, "feat/test-three");

    assert_eq!(positions[0].depth, 0);
    assert!(!positions[0].is_last_sibling);
    assert_eq!(positions[1].depth, 1);
    assert!(!positions[1].is_last_sibling);
    assert_eq!(positions[2].depth, 2);
    assert!(positions[2].is_last_sibling);
    assert_eq!(positions[3].depth, 1);
    assert!(positions[3].is_last_sibling);
    assert_eq!(positions[4].depth, 0);
    assert!(positions[4].is_last_sibling);
}

#[test]
fn build_worktree_tree_orphan_base_branch() {
    let wts = vec![make_wt("feat/orphan", Some("feat/deleted-parent"))];
    let (ordered, positions) = build_worktree_tree(&wts, "main");
    assert_eq!(ordered[0].branch, "feat/orphan");
    assert_eq!(positions[0].depth, 0);
}

#[test]
fn build_worktree_tree_empty() {
    let (ordered, positions) = build_worktree_tree(&[], "main");
    assert!(ordered.is_empty());
    assert!(positions.is_empty());
}

#[test]
fn build_worktree_tree_cycle() {
    let wts = vec![
        make_wt("feat/a", Some("feat/c")),
        make_wt("feat/b", Some("feat/a")),
        make_wt("feat/c", Some("feat/b")),
    ];
    let (ordered, positions) = build_worktree_tree(&wts, "main");
    assert_eq!(ordered.len(), 3);
    assert_eq!(positions.len(), 3);
    for pos in &positions {
        assert_eq!(pos.depth, 0);
    }
    for pos in &positions {
        assert!(pos.is_last_sibling);
        assert!(pos.ancestors_are_last.is_empty());
    }
}

// --- build_branch_picker_tree tests ---

#[test]
fn build_branch_picker_tree_flat() {
    let items = vec![
        make_picker_item(None, None),
        make_picker_item(Some("feat/a"), Some("main")),
        make_picker_item(Some("feat/b"), Some("main")),
    ];
    let (ordered, positions) = build_branch_picker_tree(&items);
    assert_eq!(ordered.len(), 3);
    assert_eq!(positions.len(), 3);
    assert!(ordered[0].branch.is_none());
    assert_eq!(positions[0].depth, 0);
    assert_eq!(positions[1].depth, 0);
    assert_eq!(positions[2].depth, 0);
}

#[test]
fn build_branch_picker_tree_parent_child() {
    let items = vec![
        make_picker_item(None, None),
        make_picker_item(Some("feat/parent"), Some("main")),
        make_picker_item(Some("feat/child"), Some("feat/parent")),
    ];
    let (ordered, positions) = build_branch_picker_tree(&items);
    assert_eq!(ordered.len(), 3);
    assert!(ordered[0].branch.is_none());
    assert_eq!(positions[0].depth, 0);
    assert_eq!(ordered[1].branch.as_deref(), Some("feat/parent"));
    assert_eq!(positions[1].depth, 0);
    assert_eq!(ordered[2].branch.as_deref(), Some("feat/child"));
    assert_eq!(positions[2].depth, 1);
    assert!(positions[2].is_last_sibling);
}

#[test]
fn build_branch_picker_tree_nested() {
    let items = vec![
        make_picker_item(None, None),
        make_picker_item(Some("feat/root"), Some("main")),
        make_picker_item(Some("feat/mid"), Some("feat/root")),
        make_picker_item(Some("feat/leaf"), Some("feat/mid")),
    ];
    let (ordered, positions) = build_branch_picker_tree(&items);
    assert_eq!(ordered.len(), 4);
    assert_eq!(ordered[1].branch.as_deref(), Some("feat/root"));
    assert_eq!(positions[1].depth, 0);
    assert_eq!(ordered[2].branch.as_deref(), Some("feat/mid"));
    assert_eq!(positions[2].depth, 1);
    assert_eq!(ordered[3].branch.as_deref(), Some("feat/leaf"));
    assert_eq!(positions[3].depth, 2);
}

#[test]
fn build_branch_picker_tree_empty() {
    let (ordered, positions) = build_branch_picker_tree(&[]);
    assert!(ordered.is_empty());
    assert!(positions.is_empty());
}

#[test]
fn build_branch_picker_tree_only_default() {
    let items = vec![make_picker_item(None, None)];
    let (ordered, positions) = build_branch_picker_tree(&items);
    assert_eq!(ordered.len(), 1);
    assert_eq!(positions.len(), 1);
    assert!(ordered[0].branch.is_none());
    assert_eq!(positions[0].depth, 0);
}

// --- TreePosition::to_prefix tests ---

#[test]
fn tree_position_to_prefix_depth_zero_returns_empty() {
    let pos = TreePosition {
        depth: 0,
        is_last_sibling: false,
        ancestors_are_last: vec![],
        is_parent: false,
    };
    assert_eq!(pos.to_prefix(), "");
}

#[test]
fn tree_position_to_prefix_non_last_sibling() {
    let pos = TreePosition {
        depth: 1,
        is_last_sibling: false,
        ancestors_are_last: vec![],
        is_parent: false,
    };
    assert_eq!(pos.to_prefix(), "├ ");
}

#[test]
fn tree_position_to_prefix_last_sibling() {
    let pos = TreePosition {
        depth: 1,
        is_last_sibling: true,
        ancestors_are_last: vec![],
        is_parent: false,
    };
    assert_eq!(pos.to_prefix(), "└ ");
}

#[test]
fn tree_position_to_prefix_nested_with_non_last_ancestor() {
    let pos = TreePosition {
        depth: 2,
        is_last_sibling: false,
        ancestors_are_last: vec![false],
        is_parent: false,
    };
    assert_eq!(pos.to_prefix(), "│ ├ ");
}

#[test]
fn tree_position_to_prefix_nested_with_last_ancestor() {
    let pos = TreePosition {
        depth: 2,
        is_last_sibling: true,
        ancestors_are_last: vec![true],
        is_parent: false,
    };
    assert_eq!(pos.to_prefix(), "  └ ");
}

#[test]
fn tree_position_to_prefix_deep_mixed_ancestors() {
    let pos = TreePosition {
        depth: 3,
        is_last_sibling: false,
        ancestors_are_last: vec![false, true],
        is_parent: false,
    };
    assert_eq!(pos.to_prefix(), "│   ├ ");
}
