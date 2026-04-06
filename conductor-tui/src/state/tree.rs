use std::collections::{HashMap, HashSet};

use conductor_core::tickets::{Ticket, TicketDependencies};
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
    /// True if this node has at least one child in the tree.
    pub is_parent: bool,
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
    reverse: bool,
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
    let sort_fn = |a: &usize, b: &usize| {
        let ord = get_branch(*a).cmp(get_branch(*b));
        if reverse {
            ord.reverse()
        } else {
            ord
        }
    };

    roots.sort_by(|a, b| sort_fn(a, b));

    for children in children_of.values_mut() {
        children.sort_by(|a, b| sort_fn(a, b));
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
        let branch = get_branch(idx);
        let has_children = children_of.get(branch).is_some_and(|c| !c.is_empty());
        positions.push(TreePosition {
            depth,
            is_last_sibling: is_last,
            ancestors_are_last: ancestors_are_last.clone(),
            is_parent: has_children,
        });
        indices.push(idx);

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
            let branch = get_branch(i);
            let has_children = children_of.get(branch).is_some_and(|c| !c.is_empty());
            positions.push(TreePosition {
                depth: 0,
                is_last_sibling: true,
                ancestors_are_last: Vec::new(),
                is_parent: has_children,
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
    dfs_tree_order(
        worktrees.len(),
        get_branch,
        get_parent,
        default_branch,
        false,
    )
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

/// Tree-order tickets by parent/child relationships from `ticket_dependencies`, returning
/// indices into the input slice, parallel `TreePosition`s, and the child→parent reverse map
/// (so callers can reuse it without rebuilding).
///
/// The `deps` map is keyed by ticket ID; each entry's `.children` field lists child tickets.
/// Tickets whose parent is not present in the input slice are treated as roots.
pub fn build_ticket_tree_indices<'a>(
    tickets: &'a [Ticket],
    deps: &'a HashMap<String, TicketDependencies>,
) -> (Vec<usize>, Vec<TreePosition>, HashMap<&'a str, &'a str>) {
    // Build a child_id → parent_id reverse map.
    let mut child_to_parent: HashMap<&'a str, &'a str> = HashMap::new();
    for (parent_id, dep) in deps {
        for child in &dep.children {
            child_to_parent.insert(child.id.as_str(), parent_id.as_str());
        }
    }

    let get_branch = |i: usize| tickets[i].id.as_str();
    let get_parent = |i: usize| {
        child_to_parent
            .get(tickets[i].id.as_str())
            .copied()
            .unwrap_or("")
    };
    let (indices, positions) = dfs_tree_order(tickets.len(), get_branch, get_parent, "", true);
    (indices, positions, child_to_parent)
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
    let (rest_indices, rest_positions) =
        dfs_tree_order(rest.len(), get_branch, get_parent, "", false);

    for (idx, pos) in rest_indices.into_iter().zip(rest_positions.into_iter()) {
        result.push(rest[idx].clone());
        positions.push(pos);
    }

    (result, positions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::tickets::{Ticket, TicketDependencies};

    fn make_ticket(id: &str) -> Ticket {
        Ticket {
            id: id.to_string(),
            repo_id: "repo1".to_string(),
            source_type: "github".to_string(),
            source_id: id.to_string(),
            title: format!("Ticket {id}"),
            state: "open".to_string(),
            body: String::new(),
            labels: String::new(),
            assignee: None,
            priority: None,
            url: String::new(),
            synced_at: "2026-01-01T00:00:00Z".to_string(),
            raw_json: String::new(),
            workflow: None,
            agent_map: None,
        }
    }

    fn make_child_dep(child_ids: &[&str]) -> TicketDependencies {
        TicketDependencies {
            children: child_ids.iter().map(|id| make_ticket(id)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn test_build_ticket_tree_indices_flat() {
        let tickets = vec![make_ticket("a"), make_ticket("b"), make_ticket("c")];
        let deps = HashMap::new();
        let (indices, positions, child_to_parent) = build_ticket_tree_indices(&tickets, &deps);

        assert_eq!(indices.len(), 3);
        assert_eq!(positions.len(), 3);
        assert!(child_to_parent.is_empty());
        // All root nodes: depth 0, not parents
        for pos in &positions {
            assert_eq!(pos.depth, 0);
            assert!(!pos.is_parent);
        }
    }

    #[test]
    fn test_build_ticket_tree_indices_parent_child() {
        // parent "a" has child "b"
        let tickets = vec![make_ticket("a"), make_ticket("b")];
        let mut deps = HashMap::new();
        deps.insert("a".to_string(), make_child_dep(&["b"]));

        let (indices, positions, child_to_parent) = build_ticket_tree_indices(&tickets, &deps);

        assert_eq!(indices.len(), 2);
        assert_eq!(child_to_parent.get("b"), Some(&"a"));

        // Find position for "a" (parent) and "b" (child)
        let pos_a = positions
            .iter()
            .zip(indices.iter())
            .find(|(_, &i)| tickets[i].id == "a")
            .map(|(p, _)| p)
            .unwrap();
        let pos_b = positions
            .iter()
            .zip(indices.iter())
            .find(|(_, &i)| tickets[i].id == "b")
            .map(|(p, _)| p)
            .unwrap();

        assert!(pos_a.is_parent, "a should be marked is_parent");
        assert_eq!(pos_a.depth, 0);
        assert!(!pos_b.is_parent, "b should not be marked is_parent");
        assert_eq!(pos_b.depth, 1);
    }

    #[test]
    fn test_build_ticket_tree_indices_dfs_order() {
        // Tree: root -> [a, b]; a -> [c]
        let tickets = vec![
            make_ticket("root"),
            make_ticket("a"),
            make_ticket("b"),
            make_ticket("c"),
        ];
        let mut deps = HashMap::new();
        deps.insert("root".to_string(), make_child_dep(&["a", "b"]));
        deps.insert("a".to_string(), make_child_dep(&["c"]));

        let (indices, positions, _) = build_ticket_tree_indices(&tickets, &deps);

        let id_order: Vec<&str> = indices.iter().map(|&i| tickets[i].id.as_str()).collect();
        // DFS descending: root, b, a, c (children sorted z→a, so b before a)
        assert_eq!(id_order, vec!["root", "b", "a", "c"]);

        let pos_root = &positions[0];
        let pos_b = &positions[1];
        let pos_a = &positions[2];
        let pos_c = &positions[3];

        assert!(pos_root.is_parent);
        assert_eq!(pos_root.depth, 0);
        assert!(pos_a.is_parent);
        assert_eq!(pos_a.depth, 1);
        assert!(!pos_c.is_parent);
        assert_eq!(pos_c.depth, 2);
        assert!(!pos_b.is_parent);
        assert_eq!(pos_b.depth, 1);
    }

    #[test]
    fn test_build_ticket_tree_indices_returns_child_to_parent_map() {
        let tickets = vec![
            make_ticket("parent"),
            make_ticket("child1"),
            make_ticket("child2"),
        ];
        let mut deps = HashMap::new();
        deps.insert("parent".to_string(), make_child_dep(&["child1", "child2"]));

        let (_, _, child_to_parent) = build_ticket_tree_indices(&tickets, &deps);

        assert_eq!(child_to_parent.get("child1"), Some(&"parent"));
        assert_eq!(child_to_parent.get("child2"), Some(&"parent"));
        assert_eq!(child_to_parent.get("parent"), None);
    }
}
