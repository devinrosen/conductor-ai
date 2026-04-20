use std::sync::Arc;

use crate::error::Result;
use crate::workflow_dsl::ApprovalMode;

use crate::workflow::executors::gate_resolver::{
    GateContext, GateParams, GatePoll, GateResolver, GitHubTokenCache,
};

use super::run_gh_json;

pub(in crate::workflow::executors) struct PrApprovalGateResolver {
    token_cache: Arc<GitHubTokenCache>,
}

impl PrApprovalGateResolver {
    pub(in crate::workflow::executors) fn new(token_cache: Arc<GitHubTokenCache>) -> Self {
        Self { token_cache }
    }
}

/// Parse a `gh pr view --json reviews,author` response and return true if the
/// number of non-author APPROVED reviews meets or exceeds `min_approvals`.
fn parse_min_approvals(val: &serde_json::Value, min_approvals: u32) -> bool {
    let pr_author = val["author"]["login"].as_str().unwrap_or("").to_string();
    let approvals = val["reviews"]
        .as_array()
        .map(|reviews| {
            reviews
                .iter()
                .filter(|r| {
                    r["state"].as_str() == Some("APPROVED")
                        && r["author"]["login"].as_str().unwrap_or("") != pr_author
                })
                .count() as u32
        })
        .unwrap_or(0);
    approvals >= min_approvals
}

/// Parse a `gh pr view --json reviewDecision` response and return true if the
/// PR's review decision is `"APPROVED"`.
fn parse_review_decision(val: &serde_json::Value) -> bool {
    val["reviewDecision"].as_str() == Some("APPROVED")
}

impl GateResolver for PrApprovalGateResolver {
    fn gate_type(&self) -> &str {
        "pr_approval"
    }

    fn poll(&self, _run_id: &str, params: &GateParams, ctx: &GateContext<'_>) -> Result<GatePoll> {
        let effective_bot = params.bot_name.as_deref().or(ctx.default_bot_name);
        let gate_bot_token = self.token_cache.get(ctx.config, effective_bot);
        let token_ref = gate_bot_token.as_deref();

        match params.approval_mode {
            ApprovalMode::MinApprovals => {
                if let Some(val) = run_gh_json(
                    &["pr", "view", "--json", "reviews,author"],
                    ctx.working_dir,
                    token_ref,
                ) {
                    if parse_min_approvals(&val, params.min_approvals) {
                        tracing::info!(
                            "Gate '{}': sufficient approvals (required {})",
                            params.gate_name,
                            params.min_approvals
                        );
                        return Ok(GatePoll::Approved(None));
                    }
                }
                Ok(GatePoll::Pending)
            }
            ApprovalMode::ReviewDecision => {
                if let Some(val) = run_gh_json(
                    &["pr", "view", "--json", "reviewDecision"],
                    ctx.working_dir,
                    token_ref,
                ) {
                    let decision = val["reviewDecision"].as_str().unwrap_or("");
                    tracing::info!("Gate '{}': reviewDecision = {}", params.gate_name, decision);
                    if parse_review_decision(&val) {
                        return Ok(GatePoll::Approved(None));
                    }
                }
                Ok(GatePoll::Pending)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_min_approvals_approved() {
        let val = json!({
            "author": { "login": "alice" },
            "reviews": [
                { "state": "APPROVED", "author": { "login": "bob" } },
                { "state": "APPROVED", "author": { "login": "carol" } }
            ]
        });
        assert!(parse_min_approvals(&val, 2), "should pass with 2 approvals");
        assert!(
            parse_min_approvals(&val, 1),
            "should pass with 1 approval required"
        );
    }

    #[test]
    fn test_parse_min_approvals_not_enough() {
        let val = json!({
            "author": { "login": "alice" },
            "reviews": [
                { "state": "APPROVED", "author": { "login": "bob" } }
            ]
        });
        assert!(
            !parse_min_approvals(&val, 2),
            "should fail when only 1 approval but 2 required"
        );
    }

    #[test]
    fn test_parse_min_approvals_excludes_author_self_review() {
        let val = json!({
            "author": { "login": "alice" },
            "reviews": [
                { "state": "APPROVED", "author": { "login": "alice" } }
            ]
        });
        assert!(
            !parse_min_approvals(&val, 1),
            "author's own approval should not count"
        );
    }

    #[test]
    fn test_parse_min_approvals_empty_reviews() {
        let val = json!({ "author": { "login": "alice" }, "reviews": [] });
        assert!(
            !parse_min_approvals(&val, 1),
            "no reviews should not be approved"
        );
    }

    #[test]
    fn test_parse_review_decision_approved() {
        let val = json!({ "reviewDecision": "APPROVED" });
        assert!(parse_review_decision(&val));
    }

    #[test]
    fn test_parse_review_decision_not_approved() {
        let val = json!({ "reviewDecision": "REVIEW_REQUIRED" });
        assert!(!parse_review_decision(&val));
    }

    #[test]
    fn test_parse_review_decision_missing() {
        let val = json!({});
        assert!(!parse_review_decision(&val));
    }
}
