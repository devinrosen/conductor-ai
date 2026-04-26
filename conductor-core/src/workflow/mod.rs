//! Workflow engine: execute multi-step workflow definitions with conditional
//! branching, loops, parallel execution, gates, and actor/reviewer agent roles.
//!
//! Builds on top of the existing `AgentManager` and orchestrator infrastructure,
//! adding workflow-level tracking in `workflow_runs` / `workflow_run_steps`.

pub(crate) mod action_executor;
pub(crate) mod api_call_executor;
mod batch_validate;
pub mod channel_event_sink;
pub use channel_event_sink::ChannelEventSink;
pub(crate) mod cancellation;
pub(crate) mod cancellation_reason;
pub(crate) mod claude_agent_executor;
pub(crate) mod constants;
pub(crate) mod coordinator;
pub(crate) mod engine_error;
pub mod estimation;
pub(crate) mod executors;
pub mod helpers;
pub(crate) mod item_provider;
pub(crate) mod manager;
pub(crate) mod output;
pub mod persistence;
#[cfg(any(test, feature = "test-helpers"))]
pub(crate) mod persistence_memory;
pub(crate) mod persistence_sqlite;
pub(crate) mod prompt_builder;
pub(crate) mod rk_types;
pub(crate) mod runkon_bridge;
pub(crate) mod runkon_gate_bridge;
pub(crate) mod script_env_provider;
pub(crate) mod status;
pub(crate) mod types;

// Unstable migration scaffolding: these re-exports will be removed once conductor-core
// and runkon-flow types are fully unified (planned post-Phase 3.3).
// Removal is tracked in issue #2631 — do not add new re-exports to this block.
#[doc(hidden)]
pub use runkon_flow::dsl::{
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
    coordinator::ENGINE_INJECTED_KEYS
}

pub use coordinator::{
    apply_workflow_input_defaults, execute_workflow_standalone, resume_workflow,
    resume_workflow_standalone, spawn_claimed_runs, spawn_heartbeat_resume, spawn_workflow_resume,
    validate_resume_preconditions,
};
pub use estimation::{Confidence, Estimate, LiveEstimate, StepEstimates};
pub use manager::recovery::{ReapedStaleRun, StaleWorkflowRun};
pub use manager::{FanOutItemRow, InvalidWorkflowEntry, WorkflowManager};
pub use output::{parse_conductor_output, ConductorOutput};
pub use persistence::{
    FanOutItemStatus, FanOutItemUpdate, GateApprovalState, NewRun, NewStep, StepUpdate,
    WorkflowPersistence,
};
pub use status::{WorkflowRunStatus, WorkflowStepStatus};
pub use types::SpawnHeartbeatResumeParams;
pub use types::{
    resolve_conductor_bin_dir, ActiveWorkflowCounts, BlockedOn, ContextEntry, GateAnalyticsRow,
    GateKind, MetadataEntry, PendingGateAnalyticsRow, PendingGateRow, RunIdSlot, SpikeBaseline,
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
