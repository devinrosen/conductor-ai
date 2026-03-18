use std::process::Command;

/// Fetch all live tmux window names across all sessions.
///
/// Calls `tmux list-windows -a` once and returns the set of window names.
/// Returns an empty set if tmux is not running or the command fails.
pub(crate) fn list_live_tmux_windows() -> std::collections::HashSet<String> {
    let Ok(output) = Command::new("tmux")
        .args(["list-windows", "-a", "-F", "#{window_name}"])
        .output()
    else {
        return std::collections::HashSet::new();
    };
    if !output.status.success() {
        return std::collections::HashSet::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().map(|line| line.trim().to_owned()).collect()
}
