/// Replace absolute worktree and home-directory paths with short placeholders.
pub fn shorten_paths(summary: &str, worktree_path: &str) -> String {
    // Replace worktree path first (more specific), then home dir (less specific)
    let s = if !worktree_path.is_empty() {
        summary.replacen(worktree_path, "{worktree}", 1)
    } else {
        summary.to_string()
    };
    match dirs::home_dir() {
        Some(home) => s.replacen(home.to_string_lossy().as_ref(), "~", 1),
        None => s,
    }
}
