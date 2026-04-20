use std::sync::Arc;

use crate::error::Result;

use crate::workflow::executors::gate_resolver::{
    GateContext, GateParams, GatePoll, GateResolver, GitHubTokenCache,
};

use super::run_gh_json;

pub(in crate::workflow::executors) struct PrChecksGateResolver {
    #[allow(dead_code)]
    token_cache: Arc<GitHubTokenCache>,
}

impl PrChecksGateResolver {
    pub(in crate::workflow::executors) fn new(token_cache: Arc<GitHubTokenCache>) -> Self {
        Self { token_cache }
    }
}

/// Parse a `gh pr checks --json state` response and return true if all checks
/// are `SUCCESS` or `SKIPPED` (and at least one check exists).
fn parse_pr_checks(val: &serde_json::Value) -> bool {
    if let Some(checks) = val.as_array() {
        !checks.is_empty()
            && checks.iter().all(|c| {
                c["state"].as_str() == Some("SUCCESS") || c["state"].as_str() == Some("SKIPPED")
            })
    } else {
        false
    }
}

impl GateResolver for PrChecksGateResolver {
    fn gate_type(&self) -> &str {
        "pr_checks"
    }

    fn poll(&self, _run_id: &str, params: &GateParams, ctx: &GateContext<'_>) -> Result<GatePoll> {
        let gate_bot_token = ctx.resolve_token(params);
        let token_ref = gate_bot_token.as_deref();

        if let Some(val) = run_gh_json(
            &["pr", "checks", "--json", "state"],
            ctx.working_dir,
            token_ref,
        ) {
            if parse_pr_checks(&val) {
                tracing::info!("Gate '{}': all checks passing", params.gate_name);
                return Ok(GatePoll::Approved(None));
            }
        }
        Ok(GatePoll::Pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_pr_checks_all_success() {
        let val = json!([
            { "state": "SUCCESS" },
            { "state": "SUCCESS" }
        ]);
        assert!(parse_pr_checks(&val), "all SUCCESS should pass");
    }

    #[test]
    fn test_parse_pr_checks_mixed_success_and_skipped() {
        let val = json!([
            { "state": "SUCCESS" },
            { "state": "SKIPPED" }
        ]);
        assert!(parse_pr_checks(&val), "SUCCESS + SKIPPED should pass");
    }

    #[test]
    fn test_parse_pr_checks_all_skipped() {
        let val = json!([{ "state": "SKIPPED" }]);
        assert!(parse_pr_checks(&val), "all SKIPPED should pass");
    }

    #[test]
    fn test_parse_pr_checks_with_failure() {
        let val = json!([
            { "state": "SUCCESS" },
            { "state": "FAILURE" }
        ]);
        assert!(!parse_pr_checks(&val), "any FAILURE should not pass");
    }

    #[test]
    fn test_parse_pr_checks_with_pending() {
        let val = json!([{ "state": "PENDING" }]);
        assert!(!parse_pr_checks(&val), "PENDING should not pass");
    }

    #[test]
    fn test_parse_pr_checks_empty() {
        let val = json!([]);
        assert!(!parse_pr_checks(&val), "empty checks array should not pass");
    }

    #[test]
    fn test_parse_pr_checks_not_array() {
        let val = json!({ "state": "SUCCESS" });
        assert!(!parse_pr_checks(&val), "non-array JSON should not pass");
    }
}
