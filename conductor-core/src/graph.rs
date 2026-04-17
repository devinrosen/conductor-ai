use std::collections::{HashMap, HashSet, VecDeque};

fn build_adj<'a>(
    ids: &'a [String],
    edges: &'a [(String, String)],
) -> HashMap<&'a str, Vec<&'a str>> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in ids {
        adj.entry(id.as_str()).or_default();
    }
    for (from, to) in edges {
        adj.entry(from.as_str()).or_default().push(to.as_str());
    }
    adj
}

/// DFS cycle detection on a directed graph.
///
/// `ids` is the full node set; `edges` is `(from, to)` where `from` must precede `to`.
/// Returns `Some(cycle_path)` if a cycle is found, `None` otherwise.
pub fn detect_cycles(ids: &[String], edges: &[(String, String)]) -> Option<Vec<String>> {
    let adj = build_adj(ids, edges);

    let mut visited: HashSet<&str> = HashSet::new();
    let mut stack: HashSet<&str> = HashSet::new();
    let mut path: Vec<&str> = Vec::new();

    for id in ids {
        if !visited.contains(id.as_str()) {
            if let Some(cycle) = dfs_cycle(id.as_str(), &adj, &mut visited, &mut stack, &mut path) {
                return Some(cycle.into_iter().map(str::to_string).collect());
            }
        }
    }

    None
}

fn dfs_cycle<'a>(
    node: &'a str,
    adj: &HashMap<&'a str, Vec<&'a str>>,
    visited: &mut HashSet<&'a str>,
    stack: &mut HashSet<&'a str>,
    path: &mut Vec<&'a str>,
) -> Option<Vec<&'a str>> {
    visited.insert(node);
    stack.insert(node);
    path.push(node);

    if let Some(neighbors) = adj.get(node) {
        for &neighbor in neighbors {
            if !visited.contains(neighbor) {
                if let Some(cycle) = dfs_cycle(neighbor, adj, visited, stack, path) {
                    return Some(cycle);
                }
            } else if stack.contains(neighbor) {
                let cycle_start = path.iter().position(|&n| n == neighbor).unwrap_or(0);
                let mut cycle: Vec<&'a str> = path[cycle_start..].to_vec();
                cycle.push(neighbor);
                return Some(cycle);
            }
        }
    }

    stack.remove(node);
    path.pop();
    None
}

/// Kahn's BFS topological sort. Returns IDs in dependency-first order.
///
/// `edges` are `(from, to)` where `from` must precede `to`.
/// Assumes the graph is acyclic — call [`detect_cycles`] first if unsure.
/// Nodes at the same level are sorted lexicographically for deterministic output.
pub fn topological_sort(ids: &[String], edges: &[(String, String)]) -> Vec<String> {
    let id_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let filtered_edges: Vec<(String, String)> = edges
        .iter()
        .filter(|(f, t)| id_set.contains(f.as_str()) && id_set.contains(t.as_str()))
        .cloned()
        .collect();

    let adj = build_adj(ids, &filtered_edges);
    let mut in_degree: HashMap<&str, usize> = HashMap::new();

    for id in ids {
        in_degree.entry(id.as_str()).or_insert(0);
    }

    for (_, to) in &filtered_edges {
        *in_degree.entry(to.as_str()).or_insert(0) += 1;
    }

    let mut queue: VecDeque<&str> = {
        let mut roots: Vec<&str> = in_degree
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(&id, _)| id)
            .collect();
        roots.sort();
        roots.into_iter().collect()
    };

    let mut sorted: Vec<String> = Vec::with_capacity(ids.len());
    while let Some(node) = queue.pop_front() {
        sorted.push(node.to_string());
        if let Some(neighbors) = adj.get(node) {
            let mut ready: Vec<&str> = Vec::new();
            for &neighbor in neighbors {
                if let Some(deg) = in_degree.get_mut(neighbor) {
                    *deg -= 1;
                    if *deg == 0 {
                        ready.push(neighbor);
                    }
                }
            }
            ready.sort();
            for n in ready {
                queue.push_back(n);
            }
        }
    }

    sorted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn e(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn test_detect_cycles_no_cycle() {
        let ids = s(&["a", "b", "c"]);
        let edges = e(&[("a", "b"), ("b", "c")]);
        assert!(detect_cycles(&ids, &edges).is_none());
    }

    #[test]
    fn test_detect_cycles_direct_cycle() {
        let ids = s(&["a", "b"]);
        let edges = e(&[("a", "b"), ("b", "a")]);
        assert!(detect_cycles(&ids, &edges).is_some());
    }

    #[test]
    fn test_detect_cycles_self_loop() {
        let ids = s(&["a"]);
        let edges = e(&[("a", "a")]);
        assert!(detect_cycles(&ids, &edges).is_some());
    }

    #[test]
    fn test_detect_cycles_indirect_cycle() {
        let ids = s(&["a", "b", "c"]);
        let edges = e(&[("a", "b"), ("b", "c"), ("c", "a")]);
        assert!(detect_cycles(&ids, &edges).is_some());
    }

    #[test]
    fn test_topo_sort_linear_chain() {
        let ids = s(&["a", "b", "c"]);
        let edges = e(&[("a", "b"), ("b", "c")]);
        let sorted = topological_sort(&ids, &edges);
        assert_eq!(sorted, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_topo_sort_parallel_roots() {
        // Two independent chains: a→b and c→d
        let ids = s(&["a", "b", "c", "d"]);
        let edges = e(&[("a", "b"), ("c", "d")]);
        let sorted = topological_sort(&ids, &edges);
        // Roots a and c come before their dependents; within each level, sorted lex
        assert!(sorted.iter().position(|s| s == "a") < sorted.iter().position(|s| s == "b"));
        assert!(sorted.iter().position(|s| s == "c") < sorted.iter().position(|s| s == "d"));
    }

    #[test]
    fn test_topo_sort_no_edges() {
        let ids = s(&["b", "a", "c"]);
        let edges = e(&[]);
        let sorted = topological_sort(&ids, &edges);
        assert_eq!(sorted, vec!["a", "b", "c"]);
    }
}
