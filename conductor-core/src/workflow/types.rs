use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Time granularity for workflow analytics queries.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeGranularity {
    Daily,
    Weekly,
}

impl std::str::FromStr for TimeGranularity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "daily" => Ok(TimeGranularity::Daily),
            "weekly" => Ok(TimeGranularity::Weekly),
            _ => Err(format!(
                "Invalid granularity: {s}. Must be 'daily' or 'weekly'"
            )),
        }
    }
}

/// A step key is a `(name, iteration)` pair used for skip-set and step-map lookups.
pub(super) type StepKey = (String, u32);

/// Shared slot used to communicate the workflow run ID from [`super::execute_workflow`] back to
/// the caller before any steps execute. The `Condvar` is notified once the ID is written.
pub type RunIdSlot = std::sync::Arc<(std::sync::Mutex<Option<String>>, std::sync::Condvar)>;

/// Resolved execution context for a workflow that targets a prior workflow run.
/// Returned by [`WorkflowManager::resolve_run_context`].
#[derive(Debug, Clone)]
pub struct WorkflowRunContext {
    /// Directory the workflow should execute in (worktree path, or repo root if no worktree).
    pub working_dir: String,
    /// Root path of the repository.
    pub repo_path: String,
    /// Worktree ID from the prior run (if any).
    pub worktree_id: Option<String>,
    /// Repo ID from the prior run (if any).
    pub repo_id: Option<String>,
}

/// A single entry in a step's metadata, either a key-value field or a
/// multi-line section with a heading and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataEntry {
    /// Short key-value pair (e.g. "Status" → "completed").
    Field { label: &'static str, value: String },
    /// Longer block with a heading and free-form body text.
    Section { heading: &'static str, body: String },
}

/// Extension trait for [`runkon_flow::types::WorkflowRunStep`] providing conductor-specific
/// helper methods.
pub trait WorkflowRunStepExt {
    /// Return structured metadata entries for this step.
    ///
    /// Consumers are responsible for choosing how to render the entries (e.g.
    /// fixed-width columns for a TUI, HTML table for a web UI, etc.).
    fn metadata_fields(&self) -> Vec<MetadataEntry>;
}

impl WorkflowRunStepExt for runkon_flow::types::WorkflowRunStep {
    fn metadata_fields(&self) -> Vec<MetadataEntry> {
        let mut entries = vec![
            MetadataEntry::Field {
                label: "Status",
                value: self.status.to_string(),
            },
            MetadataEntry::Field {
                label: "Role",
                value: self.role.clone(),
            },
            MetadataEntry::Field {
                label: "Can commit",
                value: self.can_commit.to_string(),
            },
            MetadataEntry::Field {
                label: "Iteration",
                value: self.iteration.to_string(),
            },
        ];
        if let Some(ref started) = self.started_at {
            entries.push(MetadataEntry::Field {
                label: "Started",
                value: started.clone(),
            });
        }
        if let Some(ref ended) = self.ended_at {
            entries.push(MetadataEntry::Field {
                label: "Ended",
                value: ended.clone(),
            });
        }
        if let Some(ref gt) = self.gate_type {
            entries.push(MetadataEntry::Field {
                label: "Gate type",
                value: gt.to_string(),
            });
        }
        if let Some(ref gp) = self.gate_prompt {
            entries.push(MetadataEntry::Section {
                heading: "Gate Prompt",
                body: gp.clone(),
            });
        }
        if let Some(ref gf) = self.gate_feedback {
            entries.push(MetadataEntry::Section {
                heading: "Gate Feedback",
                body: gf.clone(),
            });
        }
        if let Some(ref rt) = self.result_text {
            entries.push(MetadataEntry::Section {
                heading: "Result",
                body: rt.clone(),
            });
        }
        if let Some(ref ctx) = self.context_out {
            entries.push(MetadataEntry::Section {
                heading: "Context Out",
                body: ctx.clone(),
            });
        }
        if let Some(ref mk) = self.markers_out {
            entries.push(MetadataEntry::Section {
                heading: "Markers Out",
                body: mk.clone(),
            });
        }
        entries
    }
}

/// An enriched pending gate row used by the TUI repo detail pane (right workflow column).
#[derive(Debug, Clone)]
pub struct PendingGateRow {
    pub step: runkon_flow::types::WorkflowRunStep,
    pub workflow_name: String,
    pub target_label: Option<String>,
    /// Worktree branch (None for ephemeral PR runs).
    pub branch: Option<String>,
    /// Ticket source_id (e.g. "1151") from the linked ticket, if any.
    pub ticket_ref: Option<String>,
    /// Human-readable title extracted from `definition_snapshot`. Use `display_name()` for rendering.
    pub workflow_title: Option<String>,
}

impl PendingGateRow {
    /// Returns the human-readable display name for this gate's workflow.
    /// Uses `workflow_title` if present; falls back to `workflow_name`.
    pub fn display_name(&self) -> &str {
        self.workflow_title
            .as_deref()
            .unwrap_or(&self.workflow_name)
    }
}

/// Counts of active workflow runs (pending / running / waiting) for a single repo.
#[derive(Debug, Clone, Default)]
pub struct ActiveWorkflowCounts {
    pub pending: u32,
    pub running: u32,
    pub waiting: u32,
}

/// Input parameters for workflow execution.
pub struct WorkflowExecInput<'a> {
    pub conn: &'a rusqlite::Connection,
    pub config: &'a crate::config::Config,
    pub workflow: &'a runkon_flow::dsl::WorkflowDef,
    /// `None` for ephemeral PR runs with no registered worktree.
    pub worktree_id: Option<&'a str>,
    pub working_dir: &'a str,
    pub repo_path: &'a str,
    pub model: Option<&'a str>,
    pub exec_config: &'a runkon_flow::types::WorkflowExecConfig,
    pub inputs: HashMap<String, String>,
    pub ticket_id: Option<&'a str>,
    pub repo_id: Option<&'a str>,
    /// Current nesting depth for sub-workflow calls (0 = top-level).
    pub depth: u32,
    /// The parent workflow run ID when this is a sub-workflow invocation.
    pub parent_workflow_run_id: Option<&'a str>,
    /// Human-readable label for the target (e.g. `repo_slug/wt_slug`, `owner/repo#N`).
    pub target_label: Option<&'a str>,
    /// Default named GitHub App bot identity for call nodes that have no explicit `as =`.
    /// Set by a `call workflow { as = "..." }` node when it invokes a sub-workflow.
    pub default_bot_name: Option<String>,
    /// Loop iteration number (0-indexed) from the parent's loop context.
    /// Stored on the child `WorkflowRun` record so the TUI can filter
    /// children to show only the latest loop iteration without cross-referencing
    /// parent step records.
    pub iteration: u32,
    /// If set, the workflow run ID is written here immediately after the run record is
    /// created (before any steps execute). Used by callers that need to return the ID
    /// to an external client while execution continues in the background.
    ///
    /// The `Condvar` is notified once the ID has been written, allowing waiters to
    /// block efficiently instead of spinning.
    pub run_id_notify: Option<RunIdSlot>,
    /// Whether this run was triggered by a workflow hook (prevents infinite chains).
    pub triggered_by_hook: bool,
    /// Directory containing the conductor binary, injected into script step PATH.
    /// Resolved by the caller (binary crate) so the library doesn't call `current_exe()`.
    pub conductor_bin_dir: Option<std::path::PathBuf>,
    /// When true, bypass the WorkflowRunAlreadyActive guard by cancelling the
    /// existing run before starting a new one. Only applies to top-level runs
    /// (depth == 0); not propagated to child workflows or hook-triggered runs.
    /// Part of: process-escape-hatch@1.0.0
    pub force: bool,
    /// Additional plugin directories passed via `--plugin-dir` CLI flag.
    /// Appended to repo-level `plugin_dirs` when spawning agent sessions.
    pub extra_plugin_dirs: Vec<String>,
    /// The parent step ID that triggered this child workflow invocation.
    /// When set, `execute_workflow` writes the child run ID back to the parent
    /// step record immediately after the child run is created, enabling TUI
    /// drill-in while the child is still running.
    pub parent_step_id: Option<String>,
}

/// Owned inputs for [`execute_workflow_standalone`], avoiding lifetime issues
/// when spawning background threads.
pub struct WorkflowExecStandalone {
    pub config: crate::config::Config,
    pub workflow: runkon_flow::dsl::WorkflowDef,
    /// `None` for ephemeral PR runs with no registered worktree.
    pub worktree_id: Option<String>,
    pub working_dir: String,
    pub repo_path: String,
    pub ticket_id: Option<String>,
    pub repo_id: Option<String>,
    pub model: Option<String>,
    pub exec_config: runkon_flow::types::WorkflowExecConfig,
    pub inputs: HashMap<String, String>,
    /// Human-readable label for the target (e.g. `repo_slug/wt_slug`, `owner/repo#N`).
    pub target_label: Option<String>,
    /// If set, the workflow run ID is written here immediately after the run record is
    /// created (before any steps execute). See [`WorkflowExecInput::run_id_notify`].
    pub run_id_notify: Option<RunIdSlot>,
    /// Whether this run was triggered by a workflow hook (prevents infinite chains).
    pub triggered_by_hook: bool,
    /// Directory containing the conductor binary, injected into script step PATH.
    pub conductor_bin_dir: Option<std::path::PathBuf>,
    /// When true, bypass the WorkflowRunAlreadyActive guard. Part of: process-escape-hatch@1.0.0
    pub force: bool,
    /// Additional plugin directories passed via `--plugin-dir` CLI flag.
    /// Appended to repo-level `plugin_dirs` when spawning agent sessions.
    pub extra_plugin_dirs: Vec<String>,
    /// Override the database path. Uses the default conductor db when `None`.
    /// Useful for tests that operate on a temporary database.
    pub db_path: Option<std::path::PathBuf>,
    /// Optional parent workflow run ID. Links this run as a child of a foreach step.
    pub parent_workflow_run_id: Option<String>,
    /// Current nesting depth (0 = top-level). Used to skip the active-run guard
    /// for child workflows dispatched from foreach / call steps.
    pub depth: u32,
    /// ID of the parent step that dispatched this run. Written back to that step
    /// record immediately after the child run is created, enabling TUI drill-in.
    pub parent_step_id: Option<String>,
    /// Default bot name for the workflow run (propagated from foreach context).
    pub default_bot_name: Option<String>,
    /// Iteration number for foreach child runs (0 for top-level runs).
    pub iteration: u32,
}

/// Parameters for [`spawn_heartbeat_resume`].
///
/// Groups execution parameters (`run_id`, `config`, …) together with
/// notification-only parameters (`workflow_name`, `target_label`) so the
/// execution API surface does not expose notification concerns as positional
/// arguments.
pub struct SpawnHeartbeatResumeParams {
    pub run_id: String,
    /// Workflow name — used only in the stuck-run failure notification.
    pub workflow_name: String,
    /// Target label — used only in the stuck-run failure notification.
    pub target_label: Option<String>,
    pub config: crate::config::Config,
    pub conductor_bin_dir: Option<std::path::PathBuf>,
    pub db_path: Option<std::path::PathBuf>,
}

/// Owned inputs for [`resume_workflow_standalone`], avoiding lifetime issues
/// when spawning background threads.
pub struct WorkflowResumeStandalone {
    pub config: crate::config::Config,
    pub workflow_run_id: String,
    pub model: Option<String>,
    pub from_step: Option<String>,
    pub restart: bool,
    /// Override the database path. Uses the default conductor db when `None`.
    /// Useful for tests that operate on a temporary database.
    pub db_path: Option<std::path::PathBuf>,
    /// Directory containing the conductor binary, injected into script step PATH.
    pub conductor_bin_dir: Option<std::path::PathBuf>,
    /// Shutdown signal for graceful cancellation. `None` means the run cannot
    /// be aborted externally (e.g. auto-resume watchdog threads).
    pub shutdown: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

/// Input parameters for resuming a workflow run.
///
/// `resume_workflow` is fully self-contained: it opens its own database connection
/// from `db_path` (defaulting to `crate::config::db_path()` when `None`), using
/// a single connection for both the pre-execution phase and the FlowEngine execution
/// phase. This matches the `execute_workflow_standalone` pattern.
pub struct WorkflowResumeInput<'a> {
    pub config: &'a crate::config::Config,
    pub workflow_run_id: &'a str,
    /// Optional model override for agent steps.
    pub model: Option<&'a str>,
    /// Resume from a specific step name (re-runs steps from that point).
    pub from_step: Option<&'a str>,
    /// Restart from the beginning (clear all step results).
    pub restart: bool,
    /// Directory containing the conductor binary, injected into script step PATH.
    pub conductor_bin_dir: Option<std::path::PathBuf>,
    /// Event sinks for run observability. Defaults to empty (no sinks).
    pub event_sinks: Vec<std::sync::Arc<dyn runkon_flow::events::EventSink>>,
    /// Database path. When `None`, falls back to `crate::config::db_path()`.
    pub db_path: Option<std::path::PathBuf>,
    /// Shutdown signal for graceful cancellation. `None` means the run cannot
    /// be aborted externally (e.g. auto-resume watchdog threads).
    pub shutdown: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

/// Resolve the directory containing the current executable.
///
/// Binary crates call this once at startup and pass the result into workflow
/// input structs, keeping `current_exe()` out of the library's runtime path.
pub fn resolve_conductor_bin_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// Per-workflow aggregate token usage, averaged across completed runs.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowTokenAggregate {
    pub workflow_name: String,
    pub avg_input: f64,
    pub avg_output: f64,
    pub avg_cache_read: f64,
    pub avg_cache_creation: f64,
    pub run_count: i64,
    /// Percentage of terminal runs (completed or failed) that completed successfully.
    /// Range: 0.0–100.0.
    pub success_rate: f64,
    /// Human-readable title extracted from any definition_snapshot for this workflow.
    pub workflow_title: Option<String>,
}

/// Token totals for a time-series trend row (daily or weekly bucket).
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowTokenTrendRow {
    pub period: String,
    pub total_input: i64,
    pub total_output: i64,
    pub total_cache_read: i64,
    pub total_cache_creation: i64,
}

/// Per-step token averages across recent runs of the same workflow.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct StepTokenHeatmapRow {
    pub step_name: String,
    pub avg_input: f64,
    pub avg_output: f64,
    pub avg_cache_read: f64,
    pub run_count: i64,
}

/// Failure rate per time period for a specific workflow.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowFailureRateTrendRow {
    pub period: String,
    pub total_runs: i64,
    pub failed_runs: i64,
    /// Percentage of runs in this period that completed successfully. Range: 0.0–100.0.
    pub success_rate: f64,
}

/// Per-step failure statistics across recent terminal runs of a workflow.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct StepFailureHeatmapRow {
    pub step_name: String,
    pub total_executions: i64,
    pub failed_executions: i64,
    /// Percentage of executions that failed. Range: 0.0–100.0.
    pub failure_rate: f64,
    pub avg_retry_count: f64,
}

/// Per-step retry statistics across recent terminal runs of a workflow.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct StepRetryAnalyticsRow {
    pub step_name: String,
    pub total_executions: i64,
    pub executions_with_retries: i64,
    /// Percentage of executions that needed at least one retry. Range: 0.0–100.0.
    pub retry_rate: f64,
    /// Average retry count among executions that had at least one retry.
    pub avg_retry_count: f64,
    /// Percentage of retried executions that completed successfully. Range: 0.0–100.0.
    pub retry_success_rate: f64,
}

/// P50/P75/P95/P99 percentile distributions for duration, cost, and tokens.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowPercentiles {
    // Duration percentiles (milliseconds)
    pub p50_duration_ms: Option<f64>,
    pub p75_duration_ms: Option<f64>,
    pub p95_duration_ms: Option<f64>,
    pub p99_duration_ms: Option<f64>,
    // Cost percentiles (USD)
    pub p50_cost_usd: Option<f64>,
    pub p75_cost_usd: Option<f64>,
    pub p95_cost_usd: Option<f64>,
    pub p99_cost_usd: Option<f64>,
    // Total token percentiles (input + output)
    pub p50_total_tokens: Option<f64>,
    pub p75_total_tokens: Option<f64>,
    pub p95_total_tokens: Option<f64>,
    pub p99_total_tokens: Option<f64>,
    pub run_count: i64,
}

/// Spike detection baseline for a workflow: average cost and P75 duration over a rolling window.
#[derive(Debug, Clone)]
pub struct SpikeBaseline {
    pub avg_cost_usd: f64,
    pub p75_duration_ms: f64,
    pub run_count: i64,
}

/// Passive regression signal for a single workflow.
///
/// Compares a recent window (last N days) against a baseline window (prior M days)
/// across three signals: P75 duration, P75 cost, and failure rate.
/// Boolean regression flags are set in Rust after the query, using threshold constants.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRegressionSignal {
    pub workflow_name: String,
    pub workflow_title: Option<String>,
    pub recent_runs: i64,
    pub baseline_runs: i64,
    // Duration (ms) — P75
    pub recent_p75_duration_ms: Option<f64>,
    pub baseline_p75_duration_ms: Option<f64>,
    pub duration_change_pct: Option<f64>,
    // Cost (USD) — P75
    pub recent_p75_cost_usd: Option<f64>,
    pub baseline_p75_cost_usd: Option<f64>,
    pub cost_change_pct: Option<f64>,
    // Failure rate (0–100 %)
    pub recent_failure_rate: f64,
    pub baseline_failure_rate: f64,
    pub failure_rate_change_pp: f64,
    // Regression flags
    pub duration_regressed: bool,
    pub cost_regressed: bool,
    pub failure_rate_regressed: bool,
}

/// Per-gate-step aggregate analytics for a workflow (one row per step_name).
/// Approval is inferred from status: completed = approved, failed = rejected.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateAnalyticsRow {
    pub step_name: String,
    pub total_gate_hits: i64,
    pub approved_count: i64,
    pub rejected_count: i64,
    pub approval_rate: f64, // 0–100 %
    pub avg_wait_ms: Option<f64>,
    pub p50_wait_ms: Option<f64>,
    pub p95_wait_ms: Option<f64>,
    pub avg_feedback_length: Option<f64>, // proxy for feedback quality
}

/// Cross-workflow snapshot of all currently-waiting gate steps (one row per step).
/// Distinct from `PendingGateRow` which is TUI-enriched and repo-scoped.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingGateAnalyticsRow {
    pub step_id: String,
    pub step_name: String,
    pub gate_type: String,
    pub gate_prompt: Option<String>,
    pub workflow_name: String,
    pub workflow_run_id: String,
    pub started_at: String,
    pub wait_ms_so_far: i64,
}

/// Raw per-run metrics for histogram distribution (one row per completed run).
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunMetricsRow {
    pub run_id: String,
    pub started_at: String,
    pub duration_ms: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub worktree_id: Option<String>,
    pub repo_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_granularity_from_str_success() {
        use std::str::FromStr;
        assert_eq!(
            TimeGranularity::from_str("daily"),
            Ok(TimeGranularity::Daily)
        );
        assert_eq!(
            TimeGranularity::from_str("weekly"),
            Ok(TimeGranularity::Weekly)
        );
    }

    #[test]
    fn time_granularity_from_str_error() {
        use std::str::FromStr;
        let result = TimeGranularity::from_str("monthly");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            "Invalid granularity: monthly. Must be 'daily' or 'weekly'"
        );

        let result = TimeGranularity::from_str("invalid");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            "Invalid granularity: invalid. Must be 'daily' or 'weekly'"
        );
    }
}
