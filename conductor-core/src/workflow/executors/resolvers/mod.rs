mod human_approval;
mod pr_approval;
mod pr_checks;

pub(in crate::workflow::executors) use human_approval::{HumanApprovalGateResolver, HumanGateKind};
pub(in crate::workflow::executors) use pr_approval::PrApprovalGateResolver;
pub(in crate::workflow::executors) use pr_checks::PrChecksGateResolver;

use std::process::Command;

/// Run a `gh` command and parse stdout as JSON.
///
/// Logs a warning and returns `None` on subprocess failure or JSON parse error.
pub(super) fn run_gh_json(
    args: &[&str],
    working_dir: &str,
    token: Option<&str>,
) -> Option<serde_json::Value> {
    let mut cmd = Command::new("gh");
    cmd.args(args).current_dir(working_dir);
    if let Some(t) = token {
        cmd.env("GH_TOKEN", t);
    }
    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("gh command failed: {e}");
            return None;
        }
    };
    if !output.status.success() {
        let stderr_str = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("gh command exited {}: {}", output.status, stderr_str.trim());
        return None;
    }
    let json_str = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<serde_json::Value>(&json_str) {
        Ok(val) => Some(val),
        Err(e) => {
            tracing::warn!("gh command JSON parse error: {e}");
            None
        }
    }
}
