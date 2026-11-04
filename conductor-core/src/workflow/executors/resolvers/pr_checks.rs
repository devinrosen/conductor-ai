use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use runkon_flow::engine_error::EngineError;
use runkon_flow::traits::gate_resolver::{GateParams, GatePoll, GateResolver};
use runkon_flow::traits::run_context::RunContext;

use super::{GhGateCommon, GitHubTokenCache, GH_GATE_POLL_INTERVAL};

pub(in crate::workflow) struct PrChecksGateResolver {
    common: GhGateCommon,
}

impl PrChecksGateResolver {
    pub(in crate::workflow) fn new(
        working_dir: String,
        default_bot_name: Option<String>,
        token_cache: Arc<GitHubTokenCache>,
        config: crate::config::Config,
        db_path: PathBuf,
        owner: String,
    ) -> Self {
        Self {
            common: GhGateCommon::new(
                working_dir,
                default_bot_name,
                token_cache,
                config,
                db_path,
                owner,
            ),
        }
    }
}

/// How `gh pr checks` classifies a check (its `bucket` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckBucket {
    Pass,
    Skipping,
    Fail,
    Cancel,
    Pending,
}

/// Classify one check from `gh pr checks --json name,state,bucket`.
///
/// Uses `bucket` when present; otherwise maps the raw `state` with the same
/// table gh uses. Anything unrecognised is `Pending` (the conservative choice).
fn check_bucket(c: &serde_json::Value) -> CheckBucket {
    if let Some(b) = c["bucket"].as_str() {
        return match b {
            "pass" => CheckBucket::Pass,
            "skipping" => CheckBucket::Skipping,
            "fail" => CheckBucket::Fail,
            "cancel" => CheckBucket::Cancel,
            _ => CheckBucket::Pending,
        };
    }
    match c["state"].as_str() {
        Some("SUCCESS") => CheckBucket::Pass,
        Some("SKIPPED") | Some("NEUTRAL") => CheckBucket::Skipping,
        Some("ERROR") | Some("FAILURE") | Some("TIMED_OUT") | Some("ACTION_REQUIRED") => {
            CheckBucket::Fail
        }
        Some("CANCELLED") => CheckBucket::Cancel,
        _ => CheckBucket::Pending,
    }
}

/// Convert parsed `gh pr checks` output into a `GatePoll`.
///
/// - Any failed or cancelled check → `Rejected`, even if others are still
///   running, so a red PR fails promptly instead of waiting out the timeout.
/// - Otherwise any pending check, no checks, or non-array output → `Pending`.
/// - Otherwise (all pass/skipping) → `Approved`.
///
/// Extracted so tests can exercise the check evaluation logic without invoking
/// a real `gh` subprocess.
fn evaluate_checks(val: &serde_json::Value, gate_name: &str) -> GatePoll {
    let Some(checks) = val.as_array() else {
        return GatePoll::Pending;
    };
    if checks.is_empty() {
        return GatePoll::Pending;
    }

    let mut failed = Vec::new();
    let mut any_pending = false;
    for c in checks {
        match check_bucket(c) {
            CheckBucket::Pass | CheckBucket::Skipping => {}
            CheckBucket::Pending => any_pending = true,
            bucket @ (CheckBucket::Fail | CheckBucket::Cancel) => {
                let name = c["name"].as_str().unwrap_or("<unnamed>");
                let state = c["state"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{bucket:?}").to_lowercase());
                failed.push(format!("{name} ({state})"));
            }
        }
    }

    if !failed.is_empty() {
        let msg = format!("{} PR check(s) failed: {}", failed.len(), failed.join(", "));
        tracing::warn!("Gate '{}': {}", gate_name, msg);
        GatePoll::Rejected(msg)
    } else if any_pending {
        GatePoll::Pending
    } else {
        tracing::info!("Gate '{}': all checks passing", gate_name);
        GatePoll::Approved(None)
    }
}

impl GateResolver for PrChecksGateResolver {
    fn gate_type(&self) -> &str {
        "pr_checks"
    }

    fn poll_interval(&self) -> Option<Duration> {
        Some(GH_GATE_POLL_INTERVAL)
    }

    fn poll(
        &self,
        _run_id: &str,
        params: &GateParams,
        _ctx: &dyn RunContext,
    ) -> Result<GatePoll, EngineError> {
        if let Some(val) = self.common.run_gh(
            &["pr", "checks", "--json", "name,state,bucket"],
            params.as_identity.as_deref(),
        ) {
            return Ok(evaluate_checks(&val, &params.gate_name));
        }
        Ok(GatePoll::Pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rejected_msg(val: &serde_json::Value) -> String {
        match evaluate_checks(val, "test-gate") {
            GatePoll::Rejected(m) => m,
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn test_poll_interval_is_sixty_seconds() {
        let resolver = PrChecksGateResolver::new(
            "/tmp".into(),
            None,
            Arc::new(GitHubTokenCache::new(Some("t".into()), String::new())),
            crate::config::Config::default(),
            PathBuf::from("/tmp/unused.db"),
            String::new(),
        );
        assert_eq!(resolver.poll_interval(), Some(Duration::from_secs(60)));
    }

    #[test]
    fn test_all_pass_approved() {
        let val = json!([
            { "name": "a", "state": "SUCCESS", "bucket": "pass" },
            { "name": "b", "state": "SUCCESS", "bucket": "pass" }
        ]);
        assert!(matches!(
            evaluate_checks(&val, "g"),
            GatePoll::Approved(None)
        ));
    }

    #[test]
    fn test_pass_and_skipping_approved() {
        let val = json!([
            { "name": "a", "state": "SUCCESS", "bucket": "pass" },
            { "name": "b", "state": "SKIPPED", "bucket": "skipping" }
        ]);
        assert!(matches!(
            evaluate_checks(&val, "g"),
            GatePoll::Approved(None)
        ));
        let val = json!([{ "name": "b", "state": "SKIPPED", "bucket": "skipping" }]);
        assert!(matches!(
            evaluate_checks(&val, "g"),
            GatePoll::Approved(None)
        ));
    }

    #[test]
    fn test_fail_rejected_names_check_and_state() {
        let val = json!([
            { "name": "Test", "state": "SUCCESS", "bucket": "pass" },
            { "name": "Clippy", "state": "FAILURE", "bucket": "fail" }
        ]);
        let msg = rejected_msg(&val);
        assert!(msg.contains("Clippy"), "{msg}");
        assert!(msg.contains("FAILURE"), "{msg}");
        assert!(!msg.contains("Test"), "{msg}");
    }

    #[test]
    fn test_cancel_rejected() {
        let val = json!([{ "name": "Build", "state": "CANCELLED", "bucket": "cancel" }]);
        assert!(rejected_msg(&val).contains("Build"));
    }

    #[test]
    fn test_fail_with_pending_rejected() {
        let val = json!([
            { "name": "Slow", "state": "IN_PROGRESS", "bucket": "pending" },
            { "name": "Clippy", "state": "FAILURE", "bucket": "fail" }
        ]);
        assert!(rejected_msg(&val).contains("Clippy"));
    }

    #[test]
    fn test_multiple_failures_all_named() {
        let val = json!([
            { "name": "Clippy", "state": "FAILURE", "bucket": "fail" },
            { "name": "Test", "state": "TIMED_OUT", "bucket": "fail" }
        ]);
        let msg = rejected_msg(&val);
        assert!(msg.starts_with("2 PR check(s) failed"), "{msg}");
        assert!(msg.contains("Clippy (FAILURE)") && msg.contains("Test (TIMED_OUT)"));
    }

    #[test]
    fn test_pending_with_pass_is_pending() {
        let val = json!([
            { "name": "a", "state": "SUCCESS", "bucket": "pass" },
            { "name": "b", "state": "QUEUED", "bucket": "pending" }
        ]);
        assert!(matches!(evaluate_checks(&val, "g"), GatePoll::Pending));
    }

    #[test]
    fn test_empty_and_non_array_pending() {
        assert!(matches!(evaluate_checks(&json!([]), "g"), GatePoll::Pending));
        assert!(matches!(
            evaluate_checks(&json!({ "state": "SUCCESS" }), "g"),
            GatePoll::Pending
        ));
    }

    #[test]
    fn test_state_only_fallback() {
        for (state, expected) in [
            ("SUCCESS", CheckBucket::Pass),
            ("SKIPPED", CheckBucket::Skipping),
            ("NEUTRAL", CheckBucket::Skipping),
            ("FAILURE", CheckBucket::Fail),
            ("ERROR", CheckBucket::Fail),
            ("TIMED_OUT", CheckBucket::Fail),
            ("ACTION_REQUIRED", CheckBucket::Fail),
            ("CANCELLED", CheckBucket::Cancel),
            ("PENDING", CheckBucket::Pending),
            ("IN_PROGRESS", CheckBucket::Pending),
        ] {
            assert_eq!(check_bucket(&json!({ "state": state })), expected, "{state}");
        }
        assert!(matches!(
            evaluate_checks(&json!([{ "name": "x", "state": "FAILURE" }]), "g"),
            GatePoll::Rejected(_)
        ));
    }

    #[test]
    fn test_unknown_bucket_pending() {
        let val = json!([{ "name": "x", "state": "SUCCESS", "bucket": "weird" }]);
        assert!(matches!(evaluate_checks(&val, "g"), GatePoll::Pending));
    }
}
