use std::process::Command;

use super::AgentManager;

/// Best-effort capture of tmux scrollback to `~/.conductor/agent-logs/<run_id>.log`.
///
/// Returns the log file path on success, or `None` if capture failed.
/// This is a free function (no `&AgentManager` / `&Connection` needed) so it
/// can be called outside a DB lock on a blocking thread.
pub fn capture_tmux_scrollback(run_id: &str, tmux_window: &str) -> Option<String> {
    let log_dir = crate::config::agent_log_dir();

    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        tracing::warn!("could not create agent-logs dir: {e}");
        return None;
    }

    let log_path = crate::config::agent_log_path(run_id);

    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-t",
            &format!(":{tmux_window}"),
            "-p",
            "-S",
            "-",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            if let Err(e) = std::fs::write(&log_path, &o.stdout) {
                tracing::warn!("could not write agent log: {e}");
                return None;
            }
            Some(log_path.to_string_lossy().to_string())
        }
        Ok(o) => {
            tracing::warn!(
                "tmux capture-pane failed for run {run_id} window {tmux_window}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            None
        }
        Err(e) => {
            tracing::warn!("could not execute tmux capture-pane for run {run_id}: {e}");
            None
        }
    }
}

/// Kill a tmux window by name. Best-effort — failures are logged but ignored.
pub fn kill_tmux_window(tmux_window: &str) {
    match Command::new("tmux")
        .args(["kill-window", "-t", &format!(":{tmux_window}")])
        .output()
    {
        Ok(o) if !o.status.success() => {
            tracing::warn!(
                "tmux kill-window failed for {tmux_window}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
        Err(e) => {
            tracing::warn!("could not execute tmux kill-window for {tmux_window}: {e}");
        }
        _ => {}
    }
}

/// Capture tmux scrollback and then kill the window. Returns the log file path
/// on success, or `None` if capture failed. This is a free function designed to
/// run on a blocking thread without needing DB access.
pub fn capture_and_kill_tmux_window(run_id: &str, tmux_window: &str) -> Option<String> {
    let path = capture_tmux_scrollback(run_id, tmux_window);
    kill_tmux_window(tmux_window);
    path
}

impl<'a> AgentManager<'a> {
    /// Best-effort capture of tmux scrollback to `~/.conductor/agent-logs/<run_id>.log`.
    pub fn capture_agent_log(&self, run_id: &str, tmux_window: &str) {
        if let Some(path_str) = capture_tmux_scrollback(run_id, tmux_window) {
            if let Err(e) = self.update_run_log_file(run_id, &path_str) {
                tracing::warn!(
                    "captured agent log but failed to record path in DB for run {run_id}: {e}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::*;

    #[test]
    fn test_capture_agent_log_no_tmux_does_not_panic() {
        // capture_agent_log is best-effort (returns void); it must not panic
        // when tmux is unavailable or the window does not exist.
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        // Should complete without panicking even when tmux is not available.
        mgr.capture_agent_log("test-run-id-no-tmux", "nonexistent-window-xyz");
    }

    #[test]
    fn test_capture_agent_log_no_log_file_without_tmux() {
        // After capture_agent_log with no tmux, no log file is written.
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let run_id = "test-run-no-log-file";
        mgr.capture_agent_log(run_id, "window-does-not-exist");
        // No log file should be created when tmux capture-pane fails.
        let log_path = crate::config::agent_log_path(run_id);
        assert!(
            !log_path.exists(),
            "expected no log file when tmux is unavailable"
        );
    }

    #[test]
    fn test_capture_tmux_scrollback_returns_none_without_tmux() {
        // Free function returns None when tmux is not available or window doesn't exist.
        let result = capture_tmux_scrollback("test-scrollback-none", "nonexistent-window-xyz");
        assert!(
            result.is_none(),
            "expected None when tmux window does not exist"
        );
    }

    #[test]
    fn test_capture_tmux_scrollback_no_log_file_on_failure() {
        // No log file should be written when capture-pane fails.
        let run_id = "test-scrollback-no-file";
        let _ = capture_tmux_scrollback(run_id, "nonexistent-window-xyz");
        let log_path = crate::config::agent_log_path(run_id);
        assert!(
            !log_path.exists(),
            "expected no log file when tmux capture-pane fails"
        );
    }

    #[test]
    fn test_kill_tmux_window_does_not_panic_without_tmux() {
        // kill_tmux_window is best-effort and must not panic when tmux is unavailable.
        kill_tmux_window("nonexistent-window-xyz");
    }

    #[test]
    fn test_capture_and_kill_does_not_panic_without_tmux() {
        // Combined capture+kill must not panic; returns None without tmux.
        let result = capture_and_kill_tmux_window("test-capture-kill", "nonexistent-window-xyz");
        assert!(result.is_none(), "expected None when tmux is unavailable");
    }
}
