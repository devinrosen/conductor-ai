/// Given a flat list sorted by group (get_group returns a comparable key),
/// return the visual row index (including interleaved header rows) for
/// the item at `logical_idx`.
pub fn visual_idx_with_headers<T, K: PartialEq + Default>(
    items: &[T],
    get_group: impl Fn(&T) -> K,
    logical_idx: usize,
) -> usize {
    let mut headers = 0usize;
    let mut prev = K::default();
    for item in items.iter().take(logical_idx + 1) {
        let group = get_group(item);
        if group != prev {
            headers += 1;
            prev = group;
        }
    }
    logical_idx + headers
}

/// Replace absolute worktree and home-directory paths with short placeholders.
pub fn shorten_paths(summary: &str, worktree_path: &str, home_dir: Option<&str>) -> String {
    // Replace worktree path first (more specific), then home dir (less specific)
    let s = if !worktree_path.is_empty() {
        summary.replacen(worktree_path, "{worktree}", 1)
    } else {
        summary.to_string()
    };
    match home_dir {
        Some(home) => s.replacen(home, "~", 1),
        None => s,
    }
}
