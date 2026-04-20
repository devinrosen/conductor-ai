use std::process::Command;
use std::sync::Arc;

use crate::error::Result;

use crate::workflow::executors::gate_resolver::{
    GateContext, GateParams, GatePoll, GateResolver, GitHubTokenCache,
};

pub(in crate::workflow::executors) struct PrChecksGateResolver {
    token_cache: Arc<GitHubTokenCache>,
}

impl PrChecksGateResolver {
    pub(in crate::workflow::executors) fn new(token_cache: Arc<GitHubTokenCache>) -> Self {
        Self { token_cache }
    }
}

impl GateResolver for PrChecksGateResolver {
    fn gate_type(&self) -> &str {
        "pr_checks"
    }

    fn poll(&self, _run_id: &str, params: &GateParams, ctx: &GateContext<'_>) -> Result<GatePoll> {
        let effective_bot = params.bot_name.as_deref().or(ctx.default_bot_name);
        let gate_bot_token = self.token_cache.get(ctx.config, effective_bot);

        let mut cmd = Command::new("gh");
        cmd.args(["pr", "checks", "--json", "state"])
            .current_dir(ctx.working_dir);
        if let Some(ref token) = gate_bot_token {
            cmd.env("GH_TOKEN", token);
        }
        let output = cmd.output();

        if let Ok(out) = output {
            if out.status.success() {
                let json_str = String::from_utf8_lossy(&out.stdout);
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                    if let Some(checks) = val.as_array() {
                        let all_pass = !checks.is_empty()
                            && checks.iter().all(|c| {
                                c["state"].as_str() == Some("SUCCESS")
                                    || c["state"].as_str() == Some("SKIPPED")
                            });
                        if all_pass {
                            tracing::info!("Gate '{}': all checks passing", params.gate_name);
                            return Ok(GatePoll::Approved(None));
                        }
                    }
                }
            }
        }
        Ok(GatePoll::Pending)
    }
}
