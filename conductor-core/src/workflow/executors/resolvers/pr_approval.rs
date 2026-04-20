use crate::error::Result;
use crate::workflow_dsl::ApprovalMode;

use crate::workflow::executors::gate_resolver::{GateContext, GateParams, GatePoll, GateResolver};

use super::run_gh_json;

pub(in crate::workflow::executors) struct PrApprovalGateResolver;

impl PrApprovalGateResolver {
    pub(in crate::workflow::executors) fn new() -> Self {
        Self
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

/// Convert parsed gh output into a `GatePoll` for a given approval mode.
///
/// Extracted so tests can exercise the approval logic without invoking a real
/// `gh` subprocess.
fn evaluate_approval(val: &serde_json::Value, params: &GateParams) -> GatePoll {
    match params.approval_mode {
        ApprovalMode::MinApprovals => {
            if parse_min_approvals(val, params.min_approvals) {
                tracing::info!(
                    "Gate '{}': sufficient approvals (required {})",
                    params.gate_name,
                    params.min_approvals
                );
                GatePoll::Approved(None)
            } else {
                GatePoll::Pending
            }
        }
        ApprovalMode::ReviewDecision => {
            let decision = val["reviewDecision"].as_str().unwrap_or("");
            tracing::info!("Gate '{}': reviewDecision = {}", params.gate_name, decision);
            if parse_review_decision(val) {
                GatePoll::Approved(None)
            } else {
                GatePoll::Pending
            }
        }
    }
}

impl GateResolver for PrApprovalGateResolver {
    fn gate_type(&self) -> &str {
        "pr_approval"
    }

    fn poll(&self, _run_id: &str, params: &GateParams, ctx: &GateContext<'_>) -> Result<GatePoll> {
        let gate_bot_token = ctx.resolve_token(params);
        let token_ref = gate_bot_token.as_deref();

        let args = match params.approval_mode {
            ApprovalMode::MinApprovals => ["pr", "view", "--json", "reviews,author"].as_slice(),
            ApprovalMode::ReviewDecision => ["pr", "view", "--json", "reviewDecision"].as_slice(),
        };
        if let Some(val) = run_gh_json(args, ctx.working_dir, token_ref) {
            return Ok(evaluate_approval(&val, params));
        }
        Ok(GatePoll::Pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_dsl::ApprovalMode;
    use serde_json::json;

    fn make_params(mode: ApprovalMode, min_approvals: u32) -> GateParams {
        GateParams {
            gate_name: "test-gate".into(),
            prompt: None,
            min_approvals,
            approval_mode: mode,
            options: vec![],
            timeout_secs: 60,
            bot_name: None,
            step_id: "step-1".into(),
        }
    }

    #[test]
    fn test_evaluate_approval_min_approvals_approved() {
        let val = json!({
            "author": { "login": "alice" },
            "reviews": [
                { "state": "APPROVED", "author": { "login": "bob" } },
                { "state": "APPROVED", "author": { "login": "carol" } }
            ]
        });
        let params = make_params(ApprovalMode::MinApprovals, 2);
        assert!(matches!(
            evaluate_approval(&val, &params),
            GatePoll::Approved(None)
        ));
    }

    #[test]
    fn test_evaluate_approval_min_approvals_pending() {
        let val = json!({
            "author": { "login": "alice" },
            "reviews": [
                { "state": "APPROVED", "author": { "login": "bob" } }
            ]
        });
        let params = make_params(ApprovalMode::MinApprovals, 2);
        assert!(matches!(
            evaluate_approval(&val, &params),
            GatePoll::Pending
        ));
    }

    #[test]
    fn test_evaluate_approval_review_decision_approved() {
        let val = json!({ "reviewDecision": "APPROVED" });
        let params = make_params(ApprovalMode::ReviewDecision, 1);
        assert!(matches!(
            evaluate_approval(&val, &params),
            GatePoll::Approved(None)
        ));
    }

    #[test]
    fn test_evaluate_approval_review_decision_pending() {
        let val = json!({ "reviewDecision": "REVIEW_REQUIRED" });
        let params = make_params(ApprovalMode::ReviewDecision, 1);
        assert!(matches!(
            evaluate_approval(&val, &params),
            GatePoll::Pending
        ));
    }

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
