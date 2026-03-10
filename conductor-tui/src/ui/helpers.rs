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
