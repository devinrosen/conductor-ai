//! Bridge adapters: runkon-flow gate resolver traits → conductor-core gate resolvers.
//!
//! Lives at the `workflow` module level (not inside `executors/`) so that all
//! runkon-flow bridge code is co-located in the same module boundary.

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

use super::executors::gate_resolver::{
    GateContext as CoreGateContext, GateParams as CoreGateParams, GatePoll as CoreGatePoll,
    GateResolver as CoreGateResolver, GitHubTokenCache,
};
use super::executors::resolvers::{PrApprovalGateResolver, PrChecksGateResolver};

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
        approval_mode: params.approval_mode.clone(),
        options: params.options.clone(),
        timeout_secs: params.timeout_secs,
        bot_name: params.bot_name.clone(),
        step_id: params.step_id.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use runkon_flow::persistence_memory::InMemoryWorkflowPersistence;
    use runkon_flow::traits::gate_resolver::{
        GateContext as RkGateContext, GateParams as RkGateParams,
    };
    use runkon_flow::traits::persistence::{NewRun, NewStep, WorkflowPersistence};

    fn make_persistence_with_step() -> (Arc<InMemoryWorkflowPersistence>, String) {
        let p = Arc::new(InMemoryWorkflowPersistence::new());
        let run = p
            .create_run(NewRun {
                workflow_name: "wf".to_string(),
                worktree_id: None,
                ticket_id: None,
                repo_id: None,
                parent_run_id: String::new(),
                dry_run: false,
                trigger: "manual".to_string(),
                definition_snapshot: None,
                parent_workflow_run_id: None,
                target_label: None,
            })
            .unwrap();
        let step_id = p
            .insert_step(NewStep {
                workflow_run_id: run.id,
                step_name: "gate".to_string(),
                role: "gate".to_string(),
                can_commit: false,
                position: 0,
                iteration: 0,
                retry_count: None,
            })
            .unwrap();
        (p, step_id)
    }

    fn make_resolver(p: Arc<InMemoryWorkflowPersistence>) -> RkHumanApprovalGateResolver {
        RkHumanApprovalGateResolver {
            persistence: p,
            gate_type_str: "human_approval",
        }
    }

    fn make_params(step_id: &str) -> RkGateParams {
        RkGateParams {
            step_id: step_id.to_string(),
            gate_name: "gate".to_string(),
            prompt: None,
            min_approvals: 1,
            approval_mode: runkon_flow::dsl::ApprovalMode::MinApprovals,
            timeout_secs: 0,
            bot_name: None,
            options: vec![],
        }
    }

    fn make_ctx(step_id: &str) -> RkGateContext {
        RkGateContext {
            run_id: "run-1".to_string(),
            step_id: step_id.to_string(),
        }
    }

    #[test]
    fn poll_returns_pending_when_not_yet_approved() {
        let (p, step_id) = make_persistence_with_step();
        let resolver = make_resolver(p);
        let poll = resolver
            .poll("run-1", &make_params(&step_id), &make_ctx(&step_id))
            .unwrap();
        assert!(matches!(poll, RkGatePoll::Pending));
    }

    #[test]
    fn poll_returns_approved_with_feedback() {
        let (p, step_id) = make_persistence_with_step();
        p.approve_gate(&step_id, "reviewer", Some("lgtm"), None)
            .unwrap();
        let resolver = make_resolver(p);
        let poll = resolver
            .poll("run-1", &make_params(&step_id), &make_ctx(&step_id))
            .unwrap();
        assert!(
            matches!(&poll, RkGatePoll::Approved(Some(s)) if s == "lgtm"),
            "expected Approved(Some(\"lgtm\")), got {poll:?}"
        );
    }

    #[test]
    fn poll_returns_rejected_with_message() {
        let (p, step_id) = make_persistence_with_step();
        p.reject_gate(&step_id, "reviewer", Some("not good"))
            .unwrap();
        let resolver = make_resolver(p);
        let poll = resolver
            .poll("run-1", &make_params(&step_id), &make_ctx(&step_id))
            .unwrap();
        assert!(
            matches!(&poll, RkGatePoll::Rejected(s) if s == "not good"),
            "expected Rejected(\"not good\"), got {poll:?}"
        );
    }

    #[test]
    fn poll_rejected_uses_gate_name_fallback_when_no_feedback() {
        let (p, step_id) = make_persistence_with_step();
        p.reject_gate(&step_id, "reviewer", None).unwrap();
        let resolver = make_resolver(p);
        let params = make_params(&step_id);
        let poll = resolver
            .poll("run-1", &params, &make_ctx(&step_id))
            .unwrap();
        assert!(
            matches!(&poll, RkGatePoll::Rejected(s) if s.contains("gate")),
            "expected fallback rejection message containing gate name, got {poll:?}"
        );
    }
}
