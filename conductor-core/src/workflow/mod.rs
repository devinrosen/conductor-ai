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
pub use runkon_flow::events::EventSink;
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
pub(crate) mod persistence_sqlite;
pub(crate) mod prompt_builder;
pub(crate) mod runkon_bridge;
pub(crate) mod runkon_gate_bridge;
pub(crate) mod script_env_provider;
pub(crate) mod types;

// Re-export batch validation from the workflow layer (not DSL).
pub use batch_validate::{
    validate_workflows_batch, BatchValidationResult, BatchValidationWarning,
    WorkflowValidationEntry,
};

// Re-export all public types and functions to preserve existing import paths.
pub use constants::{
    REGRESSION_COST_THRESHOLD_PCT, REGRESSION_DURATION_THRESHOLD_PCT,
    REGRESSION_FAILURE_RATE_THRESHOLD_PP, REGRESSION_MIN_RECENT_RUNS, STEP_ROLE_FOREACH,
    STEP_ROLE_WORKFLOW,
};
pub use runkon_flow::constants::FLOW_OUTPUT_INSTRUCTION;
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
pub use manager::definitions::{
    list_defs, list_defs_with_validation, load_def_by_name, validate_single,
};
pub use manager::queries::{
    active_run_counts_by_repo, find_step_by_name_and_iteration, find_waiting_gate,
    find_waiting_gates_for_runs, get_active_chain_for_run, get_active_run_for_worktree,
    get_active_steps_for_runs, get_all_pending_gates, get_completed_run_durations,
    get_completed_step_durations, get_gate_analytics, get_plan_estimates_for_runs,
    get_progress_steps_for_runs, get_run_metrics, get_step_by_id, get_step_failure_heatmap,
    get_step_retry_analytics, get_step_summaries_for_runs, get_step_token_heatmap,
    get_steps_for_runs, get_workflow_failure_rate_trend, get_workflow_percentiles,
    get_workflow_regression_signals, get_workflow_run, get_workflow_run_ids_for_agent_runs,
    get_workflow_spike_baseline, get_workflow_steps, get_workflow_token_aggregates,
    get_workflow_token_trend, is_run_cancelled, is_workflow_cancelled,
    list_active_non_worktree_workflow_runs, list_active_workflow_runs,
    list_active_workflow_runs_for_repo, list_all_waiting_gate_steps, list_all_workflow_runs,
    list_all_workflow_runs_filtered_paginated, list_child_workflow_runs, list_root_workflow_runs,
    list_runs_by_status, list_waiting_gate_steps_for_repo, list_workflow_runs,
    list_workflow_runs_by_repo_id, list_workflow_runs_by_repo_id_filtered,
    list_workflow_runs_filtered, list_workflow_runs_filtered_paginated,
    list_workflow_runs_for_repo, list_workflow_runs_for_scope, list_workflow_runs_paginated,
    resolve_run_context,
};
pub use manager::recovery::{ReapedStaleRun, StaleWorkflowRun};
pub use manager::{InvalidWorkflowEntry, StepMetrics, WorkflowManager};
pub use output::{parse_flow_output, FlowOutput};
pub use runkon_flow::traits::persistence::{
    FanOutItemStatus, FanOutItemUpdate, GateApprovalState, NewRun, NewStep, StepUpdate,
    WorkflowPersistence,
};
pub use runkon_flow::types::FanOutItemRow;
pub use types::SpawnHeartbeatResumeParams;
pub use types::WorkflowRunStepExt;
pub use types::{
    resolve_conductor_bin_dir, ActiveWorkflowCounts, GateAnalyticsRow, MetadataEntry,
    PendingGateAnalyticsRow, PendingGateRow, RunIdSlot, SpikeBaseline, StepFailureHeatmapRow,
    StepRetryAnalyticsRow, StepTokenHeatmapRow, TimeGranularity, WorkflowExecInput,
    WorkflowExecStandalone, WorkflowFailureRateTrendRow, WorkflowPercentiles,
    WorkflowRegressionSignal, WorkflowResumeInput, WorkflowResumeStandalone, WorkflowRunContext,
    WorkflowRunMetricsRow, WorkflowTokenAggregate, WorkflowTokenTrendRow,
};

// Re-export DSL types and helpers that downstream crates (conductor-web,
// conductor-cli, etc.) import through `conductor_core::workflow`.
pub use runkon_flow::dsl::{
    collect_agent_names, collect_workflow_refs, default_skills_dir, detect_workflow_cycles,
    load_workflow_by_name, make_script_resolver, parse_workflow_str, resolve_script_path,
    validate_script_steps, validate_workflow_semantics, AgentRef, AlwaysNode, CallNode,
    CallWorkflowNode, Condition, DoNode, DoWhileNode, GateNode, GateType, IfNode, InputDecl,
    InputType, OnFail, ParallelNode, UnlessNode, ValidationError, ValidationReport, WhileNode,
    WorkflowDef, WorkflowNode, WorkflowTrigger, WorkflowWarning, MAX_WORKFLOW_DEPTH,
};

// Re-export unified runkon-flow types so downstream crates can import them from
// `conductor_core::workflow` as before.
pub use runkon_flow::status::{WorkflowRunStatus, WorkflowStepStatus};
pub use runkon_flow::types::{
    extract_workflow_title, BlockedOn, ContextEntry, StepResult, WorkflowExecConfig,
    WorkflowResult, WorkflowRun, WorkflowRunStep, WorkflowStepSummary,
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
