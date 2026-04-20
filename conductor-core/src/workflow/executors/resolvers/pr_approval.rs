use std::process::Command;
use std::sync::Arc;

use crate::error::Result;
use crate::workflow_dsl::ApprovalMode;

use crate::workflow::executors::gate_resolver::{
    GateContext, GateParams, GatePoll, GateResolver, GitHubTokenCache,
};

pub(in crate::workflow::executors) struct PrApprovalGateResolver {
    token_cache: Arc<GitHubTokenCache>,
}

impl PrApprovalGateResolver {
    pub(in crate::workflow::executors) fn new(token_cache: Arc<GitHubTokenCache>) -> Self {
        Self { token_cache }
    }
}

impl GateResolver for PrApprovalGateResolver {
    fn gate_type(&self) -> &str {
        "pr_approval"
    }

    fn poll(&self, _run_id: &str, params: &GateParams, ctx: &GateContext<'_>) -> Result<GatePoll> {
        let effective_bot = params.bot_name.as_deref().or(ctx.default_bot_name);
        let gate_bot_token = self.token_cache.get(ctx.config, effective_bot);

        match params.approval_mode {
            ApprovalMode::MinApprovals => {
                let mut cmd = Command::new("gh");
                cmd.args(["pr", "view", "--json", "reviews,author"])
                    .current_dir(ctx.working_dir);
                if let Some(ref token) = gate_bot_token {
                    cmd.env("GH_TOKEN", token);
                }
                let output = cmd.output();

                if let Ok(out) = output {
                    if out.status.success() {
                        let json_str = String::from_utf8_lossy(&out.stdout);
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            let pr_author =
                                val["author"]["login"].as_str().unwrap_or("").to_string();
                            let approvals = val["reviews"]
                                .as_array()
                                .map(|reviews| {
                                    reviews
                                        .iter()
                                        .filter(|r| {
                                            r["state"].as_str() == Some("APPROVED")
                                                && r["author"]["login"].as_str().unwrap_or("")
                                                    != pr_author
                                        })
                                        .count() as u32
                                })
                                .unwrap_or(0);
                            if approvals >= params.min_approvals {
                                tracing::info!(
                                    "Gate '{}': {} approvals (required {})",
                                    params.gate_name,
                                    approvals,
                                    params.min_approvals
                                );
                                return Ok(GatePoll::Approved(None));
                            }
                        }
                    }
                }
                Ok(GatePoll::Pending)
            }
            ApprovalMode::ReviewDecision => {
                let mut cmd = Command::new("gh");
                cmd.args(["pr", "view", "--json", "reviewDecision"])
                    .current_dir(ctx.working_dir);
                if let Some(ref token) = gate_bot_token {
                    cmd.env("GH_TOKEN", token);
                }
                let output = cmd.output();

                if let Ok(out) = output {
                    if out.status.success() {
                        let json_str = String::from_utf8_lossy(&out.stdout);
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            let decision = val["reviewDecision"].as_str().unwrap_or("");
                            tracing::info!(
                                "Gate '{}': reviewDecision = {}",
                                params.gate_name,
                                decision
                            );
                            if decision == "APPROVED" {
                                return Ok(GatePoll::Approved(None));
                            }
                        }
                    }
                }
                Ok(GatePoll::Pending)
            }
        }
    }
}
