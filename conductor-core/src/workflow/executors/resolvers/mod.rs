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
    process_gh_output(output.status.success(), &output.stdout, &output.stderr)
}

/// Parse `gh` subprocess output into a JSON value.
///
/// Separated from `run_gh_json` so the success/failure logic can be unit-tested
/// without spawning a real subprocess.
fn process_gh_output(success: bool, stdout: &[u8], stderr: &[u8]) -> Option<serde_json::Value> {
    if !success {
        let stderr_str = String::from_utf8_lossy(stderr);
        tracing::warn!("gh command exited non-zero: {}", stderr_str.trim());
        return None;
    }
    let json_str = String::from_utf8_lossy(stdout);
    match serde_json::from_str::<serde_json::Value>(&json_str) {
        Ok(val) => Some(val),
        Err(e) => {
            tracing::warn!("gh command JSON parse error: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_process_gh_output_success_valid_json() {
        let stdout = br#"{"state":"APPROVED"}"#;
        let result = process_gh_output(true, stdout, b"");
        assert_eq!(result, Some(json!({"state": "APPROVED"})));
    }

    #[test]
    fn test_process_gh_output_non_zero_exit_returns_none() {
        let result = process_gh_output(false, b"", b"some error");
        assert!(result.is_none());
    }

    #[test]
    fn test_process_gh_output_invalid_json_returns_none() {
        let result = process_gh_output(true, b"not valid json {{{", b"");
        assert!(result.is_none());
    }

    #[test]
    fn test_process_gh_output_empty_stdout_returns_none() {
        let result = process_gh_output(true, b"", b"");
        assert!(result.is_none());
    }

    #[test]
    fn test_run_gh_json_nonexistent_dir_returns_none() {
        // Subprocess launch fails when working_dir doesn't exist → None without panic.
        let result = run_gh_json(&["pr", "view"], "/nonexistent/conductor/test/dir", None);
        assert!(result.is_none());
    }
}
