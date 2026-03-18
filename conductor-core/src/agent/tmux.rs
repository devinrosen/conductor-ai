use std::process::Command;

use crate::error::{ConductorError, Result};

use super::manager::AgentManager;

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

impl<'a> AgentManager<'a> {
    /// Best-effort capture of tmux scrollback to `~/.conductor/agent-logs/<run_id>.log`.
    pub fn capture_agent_log(&self, run_id: &str, tmux_window: &str) {
        let log_dir = crate::config::agent_log_dir();

        if let Err(e) = std::fs::create_dir_all(&log_dir) {
            tracing::warn!("could not create agent-logs dir: {e}");
            return;
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
                    return;
                }
                let path_str = log_path.to_string_lossy().to_string();
                if let Err(e) = self.update_run_log_file(run_id, &path_str) {
                    tracing::warn!(
                        "captured agent log but failed to record path in DB for run {run_id}: {e}"
                    );
                }
            }
            Ok(o) => {
                tracing::warn!(
                    "tmux capture-pane failed for run {run_id} window {tmux_window}: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            Err(e) => {
                tracing::warn!("could not execute tmux capture-pane for run {run_id}: {e}");
            }
        }
    }

    /// Switch the current tmux client to the given agent window.
    ///
    /// Runs `tmux select-window -t :{window}` and returns an error (including
    /// tmux's stderr) if the command fails or tmux is unavailable.
    pub fn attach_agent_window(&self, window: &str) -> Result<()> {
        let output = Command::new("tmux")
            .args(["select-window", "-t", &format!(":{window}")])
            .output()
            .map_err(|e| ConductorError::Agent(format!("could not execute tmux: {e}")))?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();

        if stderr.is_empty() {
            Err(ConductorError::Agent(
                "tmux select-window failed".to_string(),
            ))
        } else if stderr.contains("No such file or directory")
            || stderr.contains("error connecting to")
            || stderr.contains("no server running")
        {
            Err(ConductorError::Agent(
                "tmux is not running — start a tmux session first, then relaunch conductor"
                    .to_string(),
            ))
        } else {
            Err(ConductorError::Agent(format!(
                "tmux select-window failed: {stderr}"
            )))
        }
    }
}
