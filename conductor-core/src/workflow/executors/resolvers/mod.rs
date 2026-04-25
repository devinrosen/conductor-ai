mod human_approval;
mod pr_approval;
mod pr_checks;

pub(in crate::workflow) use human_approval::{HumanApprovalGateResolver, HumanGateKind};
pub(in crate::workflow) use pr_approval::PrApprovalGateResolver;
pub(in crate::workflow) use pr_checks::PrChecksGateResolver;

use std::process::Command;
use std::sync::Arc;

use crate::workflow::executors::gate_resolver::{GateContext, GitHubTokenCache};

/// Shared state for gate resolvers that call `gh` on behalf of a bot account.
///
/// Centralises the 3-field constructor and token resolution so concrete
/// resolver types stay thin and a single-point change (e.g. adding a timeout)
/// applies to all of them.
pub(super) struct GhGateCommon {
    pub(super) working_dir: String,
    default_bot_name: Option<String>,
    token_cache: Arc<GitHubTokenCache>,
}

impl GhGateCommon {
    pub(super) fn new(
        working_dir: String,
        default_bot_name: Option<String>,
        token_cache: Arc<GitHubTokenCache>,
    ) -> Self {
        Self {
            working_dir,
            default_bot_name,
            token_cache,
        }
    }

    /// Resolve the effective bot token and run a `gh` JSON command.
    pub(super) fn run_gh(
        &self,
        args: &[&str],
        params_bot: Option<&str>,
        ctx: &GateContext<'_>,
    ) -> Option<serde_json::Value> {
        let effective_bot = params_bot.or(self.default_bot_name.as_deref());
        let token = self.token_cache.get(ctx.config, effective_bot);
        run_gh_json(args, &self.working_dir, token.as_deref())
    }
}

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
        // Truncate and redact stderr before logging: gh CLI can include auth tokens/scopes
        // in 401/403 error messages, so we emit only the first 200 characters.
        let stderr_str = String::from_utf8_lossy(stderr);
        let excerpt = stderr_str.trim();
        let truncated = if excerpt.len() > 200 {
            let end = excerpt.floor_char_boundary(200);
            &excerpt[..end]
        } else {
            excerpt
        };
        tracing::warn!(
            "gh command exited non-zero (stderr truncated): {}",
            truncated
        );
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

    #[test]
    fn test_process_gh_output_multibyte_stderr_does_not_panic() {
        // Each "é" is 2 bytes. 101 of them = 202 bytes but only 101 chars.
        // Without floor_char_boundary, &s[..200] would split the last é and panic.
        let stderr: Vec<u8> = "é".repeat(101).into_bytes();
        // Must not panic; result is None because success=false.
        let result = process_gh_output(false, b"", &stderr);
        assert!(result.is_none());
    }
}
