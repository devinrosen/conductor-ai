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
pub(crate) mod panic_db_sink;
pub(crate) mod persistence_sqlite;
pub(crate) mod prompt_builder;
pub(crate) mod run_context;
pub(crate) mod runkon_bridge;
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
pub use manager::fan_out::{
    cancel_fan_out_items, get_existing_fan_out_item_ids, get_fan_out_items,
    get_fan_out_items_checked, get_fan_out_items_for_steps, insert_fan_out_item,
    refresh_fan_out_counters, reset_running_items_without_child_run, set_fan_out_total,
    skip_fan_out_items_by_item_ids, update_fan_out_item_running, update_fan_out_item_terminal,
};
pub use manager::lifecycle::{
    cancel_run, create_workflow_run, create_workflow_run_with_targets, fail_workflow_run,
    persist_workflow_metrics, set_dismissed, set_waiting_blocked_on,
    set_workflow_run_default_bot_name, set_workflow_run_inputs, set_workflow_run_iteration,
    tick_heartbeat, update_workflow_status,
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
    get_workflow_run_status, get_workflow_spike_baseline, get_workflow_steps,
    get_workflow_token_aggregates, get_workflow_token_trend, is_run_cancelled,
    is_workflow_cancelled, list_active_non_worktree_workflow_runs, list_active_workflow_runs,
    list_active_workflow_runs_for_repo, list_all_waiting_gate_steps, list_all_workflow_runs,
    list_all_workflow_runs_filtered_paginated, list_child_workflow_runs, list_root_workflow_runs,
    list_runs_by_status, list_waiting_gate_steps_for_repo, list_workflow_runs,
    list_workflow_runs_by_repo_id, list_workflow_runs_by_repo_id_filtered,
    list_workflow_runs_filtered, list_workflow_runs_filtered_paginated,
    list_workflow_runs_for_repo, list_workflow_runs_for_scope, list_workflow_runs_paginated,
    resolve_run_context,
};
pub use manager::recovery::{
    claim_and_resume_expired_leases, claim_expired_lease_runs, claim_needs_resume_runs,
    claim_stuck_workflows, classify_resumable_workflows, delete_orphaned_pending_steps, delete_run,
    detect_stale_workflow_runs, detect_stuck_workflow_run_ids, find_resumable_child_run,
    get_completed_step_keys, purge, purge_count, reap_finalization_stuck_workflow_runs,
    reap_orphaned_script_steps, reap_orphaned_workflow_runs, reap_stale_workflow_runs,
    recover_stuck_steps, reset_completed_steps, reset_failed_steps, reset_steps_from_position,
    run_workflow_maintenance, ReapedStaleRun, StaleWorkflowRun,
};
// `count_live_subprocess_steps` is `pub(crate)` (internal-only), so it isn't
// part of the public re-export above. Re-export it at crate-internal visibility
// so internal callers (coordinator, etc.) can address it via the standard
// `crate::workflow::` path instead of reaching into `manager::recovery::`.
pub(crate) use manager::recovery::count_live_subprocess_steps;
pub use manager::recovery::terminate_subprocesses;
pub use manager::steps::{
    active_step_exists, approve_gate, get_gate_approval_state, insert_step, insert_step_running,
    mark_step_pending, mark_step_running, mark_step_terminal, mirror_step_metrics_from_run,
    predecessor_completed, reject_gate, set_step_gate_info, set_step_gate_options,
    set_step_output_file, set_step_parallel_group, set_step_subprocess_pid,
    update_step_child_run_id, update_step_status, update_step_status_full,
};
pub use manager::{InvalidWorkflowEntry, StepMetrics};
pub use output::{parse_flow_output, FlowOutput};
pub use runkon_flow::traits::persistence::{
    FanOutItemStatus, FanOutItemUpdate, GateApprovalState, NewRun, NewStep, StepUpdate,
    WorkflowPersistence,
};
pub use runkon_flow::types::FanOutItemRow;
pub use types::SpawnHeartbeatResumeParams;
pub use types::{
    resolve_conductor_bin_dir, ActiveWorkflowCounts, GateAnalyticsRow, PendingGateAnalyticsRow,
    PendingGateRow, RunIdSlot, SpikeBaseline, StepFailureHeatmapRow, StepRetryAnalyticsRow,
    StepTokenHeatmapRow, TimeGranularity, WorkflowExecInput, WorkflowExecStandalone,
    WorkflowFailureRateTrendRow, WorkflowPercentiles, WorkflowRegressionSignal,
    WorkflowResumeInput, WorkflowResumeStandalone, WorkflowRunContext, WorkflowRunMetricsRow,
    WorkflowTokenAggregate, WorkflowTokenTrendRow,
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
