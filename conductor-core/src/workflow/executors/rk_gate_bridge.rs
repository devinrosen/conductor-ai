use std::path::PathBuf;
use std::sync::Arc;

use crate::config::Config;
use crate::workflow::engine_error::EngineError;
use runkon_flow::traits::gate_resolver::{
    GateContext as RkGateContext, GateParams as RkGateParams, GatePoll as RkGatePoll,
    GateResolver as RkGateResolver,
};
use runkon_flow::traits::persistence::WorkflowPersistence as RkPersistence;
use runkon_flow::FlowEngineBuilder;

use super::gate_resolver::{
    GateContext as CoreGateContext, GateParams as CoreGateParams, GatePoll as CoreGatePoll,
    GateResolver as CoreGateResolver, GitHubTokenCache,
};
use super::resolvers::{PrApprovalGateResolver, PrChecksGateResolver};

// ---------------------------------------------------------------------------
// Human gates (human_approval + human_review)
// ---------------------------------------------------------------------------

pub(in crate::workflow) struct RkHumanApprovalGateResolver {
    persistence: Arc<dyn RkPersistence>,
    gate_type_str: &'static str,
}

impl RkGateResolver for RkHumanApprovalGateResolver {
    fn gate_type(&self) -> &str {
        self.gate_type_str
    }

    fn poll(
        &self,
        _run_id: &str,
        params: &RkGateParams,
        _ctx: &RkGateContext,
    ) -> Result<RkGatePoll, EngineError> {
        use runkon_flow::traits::persistence::GateApprovalState;
        let state = self.persistence.get_gate_approval(&params.step_id)?;
        Ok(match state {
            GateApprovalState::Approved { feedback, .. } => RkGatePoll::Approved(feedback),
            GateApprovalState::Rejected { feedback } => RkGatePoll::Rejected(
                feedback.unwrap_or_else(|| format!("Gate '{}' rejected", params.gate_name)),
            ),
            GateApprovalState::Pending => RkGatePoll::Pending,
        })
    }
}

// ---------------------------------------------------------------------------
// PR gate resolvers (approval + checks)
//
// Both resolvers share an identical struct layout and poll() body — only the
// gate_type() string and the inner resolver type differ.  A macro removes the
// duplication while keeping the two distinct public type names.
// ---------------------------------------------------------------------------

macro_rules! impl_pr_gate_resolver {
    ($name:ident, $inner_ty:ty, $gate_type_str:literal) => {
        pub(in crate::workflow) struct $name {
            inner: $inner_ty,
            config: Config,
            db_path: PathBuf,
        }

        impl RkGateResolver for $name {
            fn gate_type(&self) -> &str {
                $gate_type_str
            }

            fn poll(
                &self,
                run_id: &str,
                params: &RkGateParams,
                _ctx: &RkGateContext,
            ) -> Result<RkGatePoll, EngineError> {
                let core_params = rk_params_to_core(params);
                let ctx = CoreGateContext {
                    config: &self.config,
                    db_path: &self.db_path,
                };
                let result = CoreGateResolver::poll(&self.inner, run_id, &core_params, &ctx)
                    .map_err(|e| EngineError::Persistence(e.to_string()))?;
                Ok(gate_poll_to_rk(result))
            }
        }
    };
}

impl_pr_gate_resolver!(
    RkPrApprovalGateResolver,
    PrApprovalGateResolver,
    "pr_approval"
);
impl_pr_gate_resolver!(RkPrChecksGateResolver, PrChecksGateResolver, "pr_checks");

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn rk_params_to_core(params: &RkGateParams) -> CoreGateParams {
    CoreGateParams {
        gate_name: params.gate_name.clone(),
        prompt: params.prompt.clone(),
        min_approvals: params.min_approvals,
        approval_mode: rk_approval_mode_to_core(params.approval_mode.clone()),
        options: params.options.clone(),
        timeout_secs: params.timeout_secs,
        bot_name: params.bot_name.clone(),
        step_id: params.step_id.clone(),
    }
}

fn rk_approval_mode_to_core(
    m: runkon_flow::dsl::ApprovalMode,
) -> crate::workflow_dsl::ApprovalMode {
    match m {
        runkon_flow::dsl::ApprovalMode::MinApprovals => {
            crate::workflow_dsl::ApprovalMode::MinApprovals
        }
        runkon_flow::dsl::ApprovalMode::ReviewDecision => {
            crate::workflow_dsl::ApprovalMode::ReviewDecision
        }
    }
}

fn gate_poll_to_rk(p: CoreGatePoll) -> RkGatePoll {
    match p {
        CoreGatePoll::Approved(s) => RkGatePoll::Approved(s),
        CoreGatePoll::Rejected(s) => RkGatePoll::Rejected(s),
        CoreGatePoll::Pending => RkGatePoll::Pending,
    }
}

// ---------------------------------------------------------------------------
// Builder helper
// ---------------------------------------------------------------------------

/// Register all four gate resolver types on a `FlowEngineBuilder`.
///
/// - `human_approval` / `human_review` poll the persistence layer directly
/// - `pr_approval` / `pr_checks` delegate to the existing conductor-core resolvers,
///   which run `gh` CLI commands using `config` and `db_path` for context
pub(in crate::workflow) fn register_rk_gate_resolvers(
    builder: FlowEngineBuilder,
    persistence: Arc<dyn RkPersistence>,
    working_dir: String,
    default_bot_name: Option<String>,
    config: Config,
    db_path: PathBuf,
) -> FlowEngineBuilder {
    let token_cache = Arc::new(GitHubTokenCache::new(None));
    builder
        .gate_resolver(RkHumanApprovalGateResolver {
            persistence: Arc::clone(&persistence),
            gate_type_str: "human_approval",
        })
        .gate_resolver(RkHumanApprovalGateResolver {
            persistence: Arc::clone(&persistence),
            gate_type_str: "human_review",
        })
        .gate_resolver(RkPrApprovalGateResolver {
            inner: PrApprovalGateResolver::new(
                working_dir.clone(),
                default_bot_name.clone(),
                Arc::clone(&token_cache),
            ),
            config: config.clone(),
            db_path: db_path.clone(),
        })
        .gate_resolver(RkPrChecksGateResolver {
            inner: PrChecksGateResolver::new(working_dir, default_bot_name, token_cache),
            config,
            db_path,
        })
}
