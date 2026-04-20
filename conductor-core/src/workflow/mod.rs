//! Workflow engine: execute multi-step workflow definitions with conditional
//! branching, loops, parallel execution, gates, and actor/reviewer agent roles.
//!
//! Builds on top of the existing `AgentManager` and orchestrator infrastructure,
//! adding workflow-level tracking in `workflow_runs` / `workflow_run_steps`.

mod batch_validate;
pub(crate) mod constants;
pub(crate) mod engine;
pub mod estimation;
pub(crate) mod executors;
pub(crate) mod helpers;
pub(crate) mod manager;
pub(crate) mod output;
pub(crate) mod prompt_builder;
pub(crate) mod run_context;
pub(crate) mod status;
pub(crate) mod types;

// Re-export DSL types so consumers go through `workflow::` instead of `workflow_dsl::` directly.
pub use crate::workflow_dsl::{
    collect_agent_names, collect_workflow_refs, default_skills_dir, detect_workflow_cycles,
    load_workflow_by_name, make_script_resolver, parse_workflow_str, resolve_script_path,
    validate_script_steps, validate_workflow_semantics, AgentRef, AlwaysNode, CallNode,
    CallWorkflowNode, Condition, DoNode, DoWhileNode, GateNode, GateType, IfNode, InputDecl,
    InputType, OnFail, ParallelNode, UnlessNode, ValidationError, ValidationReport, WhileNode,
    WorkflowDef, WorkflowNode, WorkflowTrigger, WorkflowWarning, MAX_WORKFLOW_DEPTH,
};

// Re-export batch validation from the workflow layer (not DSL).
pub use batch_validate::{
    validate_workflows_batch, BatchValidationResult, BatchValidationWarning,
    WorkflowValidationEntry,
};

// Re-export all public types and functions to preserve existing import paths.
pub use constants::{
    CONDUCTOR_OUTPUT_INSTRUCTION, REGRESSION_COST_THRESHOLD_PCT, REGRESSION_DURATION_THRESHOLD_PCT,
    REGRESSION_FAILURE_RATE_THRESHOLD_PP, REGRESSION_MIN_RECENT_RUNS, STEP_ROLE_FOREACH,
    STEP_ROLE_WORKFLOW,
};
/// Returns the list of variable keys that the workflow engine injects automatically
/// from run context (ticket and repo metadata, plus `workflow_run_id`).
///
/// Use this instead of importing `ENGINE_INJECTED_KEYS` directly.
pub fn injected_variable_keys() -> &'static [&'static str] {
    engine::ENGINE_INJECTED_KEYS
}

pub use engine::{
    apply_workflow_input_defaults, execute_workflow, execute_workflow_standalone, resume_workflow,
    resume_workflow_standalone, validate_resume_preconditions,
};
pub use estimation::{Confidence, Estimate, LiveEstimate, StepEstimates};
pub use manager::recovery::{ReapedStaleRun, StaleWorkflowRun};
pub use manager::{FanOutItemRow, InvalidWorkflowEntry, WorkflowManager};
pub use output::{parse_conductor_output, ConductorOutput};
pub use status::{WorkflowRunStatus, WorkflowStepStatus};
pub use types::{
    resolve_conductor_bin_dir, ActiveWorkflowCounts, BlockedOn, ContextEntry, GateAnalyticsRow,
    MetadataEntry, PendingGateAnalyticsRow, PendingGateRow, RunIdSlot, SpikeBaseline,
    StepFailureHeatmapRow, StepResult, StepRetryAnalyticsRow, StepTokenHeatmapRow, TimeGranularity,
    WorkflowExecConfig, WorkflowExecInput, WorkflowExecStandalone, WorkflowFailureRateTrendRow,
    WorkflowPercentiles, WorkflowRegressionSignal, WorkflowResult, WorkflowResumeInput,
    WorkflowResumeStandalone, WorkflowRun, WorkflowRunContext, WorkflowRunMetricsRow,
    WorkflowRunStep, WorkflowStepSummary, WorkflowTokenAggregate, WorkflowTokenTrendRow,
};

use crate::agent_config::AgentSpec;

/// Convert a DSL `AgentRef` to the `agent_config` layer's `AgentSpec`.
///
/// This is the boundary where the workflow DSL concern (`AgentRef`) maps to
/// the resolution concern (`AgentSpec`).
impl From<&AgentRef> for AgentSpec {
    fn from(r: &AgentRef) -> Self {
        match r {
            AgentRef::Name(s) => AgentSpec::Name(s.clone()),
            AgentRef::Path(s) => AgentSpec::Path(s.clone()),
        }
    }
}

#[cfg(test)]
mod tests;
