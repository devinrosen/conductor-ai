use std::collections::{HashMap, HashSet};

use conductor_core::worktree::Worktree;

use super::BranchPickerItem;

#[derive(Debug, Default, Clone)]
pub struct FilterState {
    pub active: bool,
    pub text: String,
}

impl FilterState {
    pub fn enter(&mut self) {
        self.active = true;
        self.text.clear();
    }
    pub fn exit(&mut self) {
        self.active = false;
    }
    pub fn push(&mut self, c: char) {
        self.text.push(c);
    }
    pub fn backspace(&mut self) {
        self.text.pop();
    }
    pub fn as_query(&self) -> Option<String> {
        if self.active || !self.text.is_empty() {
            Some(self.text.to_lowercase())
        } else {
            None
        }
    }
}

/// Position metadata for tree-indent rendering of worktrees.
#[derive(Debug, Clone, Default)]
pub struct TreePosition {
    pub depth: usize,
    pub is_last_sibling: bool,
    pub ancestors_are_last: Vec<bool>,
}

impl TreePosition {
    /// Build the tree-drawing prefix string (e.g. "│ └ ") for this position.
    pub fn to_prefix(&self) -> String {
        if self.depth == 0 {
            return String::new();
        }
        let mut p = String::new();
        for &ancestor_is_last in &self.ancestors_are_last {
            if ancestor_is_last {
                p.push_str("  ");
            } else {
                p.push_str("│ ");
            }
        }
        if self.is_last_sibling {
            p.push_str("└ ");
        } else {
            p.push_str("├ ");
        }
        p
    }
}

/// Core DFS tree ordering used by both `build_worktree_tree_indices` and
/// `build_branch_picker_tree`.
///
/// `get_branch(i)` → branch name for item `i`
/// `get_parent(i)` → already-resolved parent branch (empty string = root)
/// `default_branch` → treat this value as "root parent"
///
/// Returns `(indices, positions)` in DFS order with cycle-fallback appended.
fn dfs_tree_order<'a>(
    n: usize,
    get_branch: impl Fn(usize) -> &'a str,
    get_parent: impl Fn(usize) -> &'a str,
    default_branch: &str,
) -> (Vec<usize>, Vec<TreePosition>) {
    if n == 0 {
        return (Vec::new(), Vec::new());
    }

    let branch_set: HashSet<&str> = (0..n).map(&get_branch).collect();
    let mut children_of: HashMap<&str, Vec<usize>> = HashMap::new();

    for i in 0..n {
        let parent = get_parent(i);
        children_of.entry(parent).or_default().push(i);
    }

    let mut roots: Vec<usize> = Vec::new();
    for i in 0..n {
        let parent = get_parent(i);
        if parent == default_branch || !branch_set.contains(parent) {
            roots.push(i);
        }
    }
    roots.sort_by(|a, b| get_branch(*a).cmp(get_branch(*b)));

    for children in children_of.values_mut() {
        children.sort_by(|a, b| get_branch(*a).cmp(get_branch(*b)));
    }

    let mut indices: Vec<usize> = Vec::with_capacity(n);
    let mut positions: Vec<TreePosition> = Vec::with_capacity(n);
    let mut visited: HashSet<usize> = HashSet::new();

    // DFS via explicit stack: (index, depth, is_last_sibling, ancestors_are_last)
    let mut stack: Vec<(usize, usize, bool, Vec<bool>)> = Vec::new();

    let root_count = roots.len();
    for (ri, &root_idx) in roots.iter().enumerate().rev() {
        stack.push((root_idx, 0, ri == root_count - 1, Vec::new()));
    }

    while let Some((idx, depth, is_last, ancestors_are_last)) = stack.pop() {
        if !visited.insert(idx) {
            continue;
        }
        positions.push(TreePosition {
            depth,
            is_last_sibling: is_last,
            ancestors_are_last: ancestors_are_last.clone(),
        });
        indices.push(idx);

        let branch = get_branch(idx);
        if let Some(children) = children_of.get(branch) {
            let len = children.len();
            let mut child_ancestors = ancestors_are_last;
            child_ancestors.push(is_last);
            // Push children in reverse so they come out in sorted order.
            for (ci, &child_idx) in children.iter().enumerate().rev() {
                stack.push((child_idx, depth + 1, ci == len - 1, child_ancestors.clone()));
            }
        }
    }

    // Append any unvisited items (cycle members) as depth-0 roots.
    for i in 0..n {
        if !visited.contains(&i) {
            positions.push(TreePosition {
                depth: 0,
                is_last_sibling: true,
                ancestors_are_last: Vec::new(),
            });
            indices.push(i);
            visited.insert(i);
        }
    }

    (indices, positions)
}

/// Tree-order worktrees by `base_branch` parent-child relationships, returning
/// indices into the input and parallel `TreePosition`s — no cloning.
///
/// Accepts anything deref-able to `Worktree` so callers with `&[Worktree]` or
/// `&[&Worktree]` can both use it.
pub fn build_worktree_tree_indices<W: std::borrow::Borrow<Worktree>>(
    worktrees: &[W],
    default_branch: &str,
) -> (Vec<usize>, Vec<TreePosition>) {
    let get_branch = |i: usize| worktrees[i].borrow().branch.as_str();
    let get_parent = |i: usize| {
        worktrees[i]
            .borrow()
            .base_branch
            .as_deref()
            .unwrap_or(default_branch)
    };
    dfs_tree_order(worktrees.len(), get_branch, get_parent, default_branch)
}

/// Reorder worktrees into tree order based on `base_branch` parent-child relationships.
///
/// A worktree is a child of another worktree when its `base_branch` matches the other's `branch`.
/// Returns `(tree_ordered_worktrees, parallel_tree_positions)`.
pub fn build_worktree_tree(
    worktrees: &[Worktree],
    default_branch: &str,
) -> (Vec<Worktree>, Vec<TreePosition>) {
    let (indices, positions) = build_worktree_tree_indices(worktrees, default_branch);
    let ordered = indices.into_iter().map(|i| worktrees[i].clone()).collect();
    (ordered, positions)
}

/// Reorder branch picker items into tree order based on `base_branch` parent-child relationships.
///
/// The first item (default branch, `branch: None`) is always excluded from tree-building
/// and stays at index 0 with depth 0. The remaining items are ordered via DFS using the
/// same logic as `build_worktree_tree()`.
pub fn build_branch_picker_tree(
    items: &[BranchPickerItem],
) -> (Vec<BranchPickerItem>, Vec<TreePosition>) {
    if items.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // Separate the default-branch sentinel (index 0, branch: None) from the rest.
    let mut result: Vec<BranchPickerItem> = Vec::with_capacity(items.len());
    let mut positions: Vec<TreePosition> = Vec::with_capacity(items.len());

    // Always keep the default branch entry at index 0.
    result.push(items[0].clone());
    positions.push(TreePosition::default());

    let rest = &items[1..];
    if rest.is_empty() {
        return (result, positions);
    }

    let get_branch = |i: usize| rest[i].branch.as_deref().unwrap_or("");
    let get_parent = |i: usize| rest[i].base_branch.as_deref().unwrap_or("");
    let (rest_indices, rest_positions) = dfs_tree_order(rest.len(), get_branch, get_parent, "");

    for (idx, pos) in rest_indices.into_iter().zip(rest_positions.into_iter()) {
        result.push(rest[idx].clone());
        positions.push(pos);
    }

    (result, positions)
}
