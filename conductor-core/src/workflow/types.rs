use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::workflow_dsl::GateType;

use super::status::{WorkflowRunStatus, WorkflowStepStatus};

/// Describes what a workflow run is currently blocked on when in `Waiting` status.
///
/// Uses internally-tagged JSON (`{"type":"human_approval",...}`) for forward-compatibility
/// with future blocker types and easy consumption by non-Rust consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockedOn {
    HumanApproval {
        gate_name: String,
        prompt: Option<String>,
    },
    HumanReview {
        gate_name: String,
        prompt: Option<String>,
    },
    PrApproval {
        gate_name: String,
        approvals_needed: u32,
    },
    PrChecks {
        gate_name: String,
    },
}

/// A step key is a `(name, iteration)` pair used for skip-set and step-map lookups.
pub(super) type StepKey = (String, u32);

/// Shared slot used to communicate the workflow run ID from [`super::execute_workflow`] back to
/// the caller before any steps execute. The `Condvar` is notified once the ID is written.
pub type RunIdSlot = std::sync::Arc<(std::sync::Mutex<Option<String>>, std::sync::Condvar)>;

/// A workflow run record from the database.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRun {
    pub id: String,
    pub workflow_name: String,
    /// `None` for ephemeral PR runs that have no registered worktree.
    pub worktree_id: Option<String>,
    pub parent_run_id: String,
    pub status: WorkflowRunStatus,
    pub dry_run: bool,
    pub trigger: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub result_summary: Option<String>,
    pub definition_snapshot: Option<String>,
    pub inputs: HashMap<String, String>,
    pub ticket_id: Option<String>,
    pub repo_id: Option<String>,
    /// Link to the parent workflow run when this is a sub-workflow invocation.
    pub parent_workflow_run_id: Option<String>,
    /// Human-readable label for the target (e.g. `repo_slug/wt_slug`, `owner/repo#N`).
    pub target_label: Option<String>,
    /// Default named GitHub App bot identity for this run.
    /// Set when the run is invoked via `call workflow { as = "..." }`.
    pub default_bot_name: Option<String>,
    /// Loop iteration number (0-indexed). Used by the TUI to filter
    /// children of a parent run to show only the latest loop iteration.
    pub iteration: i64,
    /// What the workflow is currently blocked on (only set when status is `Waiting`).
    pub blocked_on: Option<BlockedOn>,
    /// Optional feature ID linking this run to a feature branch.
    pub feature_id: Option<String>,
}

impl WorkflowRun {
    /// Whether this run was triggered by a workflow hook (prevents infinite chains).
    /// Derived from `trigger == "hook"` rather than stored separately.
    pub fn is_triggered_by_hook(&self) -> bool {
        self.trigger == "hook"
    }
}

/// A workflow step execution record from the database.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRunStep {
    pub id: String,
    pub workflow_run_id: String,
    pub step_name: String,
    pub role: String,
    pub can_commit: bool,
    pub condition_expr: Option<String>,
    pub status: WorkflowStepStatus,
    pub child_run_id: Option<String>,
    pub position: i64,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub result_text: Option<String>,
    pub condition_met: Option<bool>,
    pub iteration: i64,
    pub parallel_group_id: Option<String>,
    pub context_out: Option<String>,
    pub markers_out: Option<String>,
    pub retry_count: i64,
    pub gate_type: Option<GateType>,
    pub gate_prompt: Option<String>,
    pub gate_timeout: Option<String>,
    pub gate_approved_by: Option<String>,
    pub gate_approved_at: Option<String>,
    pub gate_feedback: Option<String>,
    /// Full structured output JSON (when schema was used).
    pub structured_output: Option<String>,
    /// Path to the stdout capture file for script steps (persisted for resume).
    pub output_file: Option<String>,
}

/// Lightweight summary of the currently-running step for a workflow run.
/// Used for inline step indicators in the worktrees panel of the TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStepSummary {
    pub step_name: String,
    /// Loop iteration count (0-indexed; 0 = first pass, 1+ = subsequent loop iterations).
    pub iteration: i64,
    /// Ordered list of workflow names from root down to the workflow containing the
    /// currently-running step. E.g. `["ticket-to-pr", "review-pr"]` when `review-pr` is
    /// a sub-workflow of `ticket-to-pr`. Empty for single-level (non-nested) workflows.
    pub workflow_chain: Vec<String>,
}

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

impl WorkflowRunStep {
    /// Return structured metadata entries for this step.
    ///
    /// Consumers are responsible for choosing how to render the entries (e.g.
    /// fixed-width columns for a TUI, HTML table for a web UI, etc.).
    pub fn metadata_fields(&self) -> Vec<MetadataEntry> {
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

/// Configuration for workflow execution.
#[derive(Debug, Clone)]
pub struct WorkflowExecConfig {
    pub poll_interval: Duration,
    pub step_timeout: Duration,
    pub fail_fast: bool,
    pub dry_run: bool,
    /// Optional shutdown flag. When set to `true`, in-flight steps are
    /// cancelled with "workflow cancelled: TUI was closed".
    pub shutdown: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Default for WorkflowExecConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            step_timeout: Duration::from_secs(12 * 60 * 60),
            fail_fast: true,
            dry_run: false,
            shutdown: None,
        }
    }
}

/// Result of executing a workflow.
#[derive(Debug, Clone)]
pub struct WorkflowResult {
    pub workflow_run_id: String,
    /// `None` for ephemeral PR runs with no registered worktree.
    pub worktree_id: Option<String>,
    pub workflow_name: String,
    pub all_succeeded: bool,
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
}

/// Result of a single step execution (kept in memory during execution).
#[derive(Debug, Clone)]
pub struct StepResult {
    pub step_name: String,
    pub status: WorkflowStepStatus,
    pub result_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub markers: Vec<String>,
    pub context: String,
    pub child_run_id: Option<String>,
    /// Raw JSON string of structured output (when schema was used).
    pub structured_output: Option<String>,
    /// Path to the script stdout temp file (script steps only).
    pub output_file: Option<String>,
}

/// An entry in the accumulated context history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub step: String,
    pub iteration: u32,
    pub context: String,
    #[serde(default)]
    pub markers: Vec<String>,
    #[serde(default)]
    pub structured_output: Option<String>,
    #[serde(default)]
    pub output_file: Option<String>,
}

/// An enriched pending gate row used by the TUI repo detail pane (right workflow column).
#[derive(Debug, Clone)]
pub struct PendingGateRow {
    pub step: WorkflowRunStep,
    pub workflow_name: String,
    pub target_label: Option<String>,
    /// Worktree branch (None for ephemeral PR runs).
    pub branch: Option<String>,
    /// Ticket source_id (e.g. "1151") from the linked ticket, if any.
    pub ticket_ref: Option<String>,
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
    pub workflow: &'a crate::workflow_dsl::WorkflowDef,
    /// `None` for ephemeral PR runs with no registered worktree.
    pub worktree_id: Option<&'a str>,
    pub working_dir: &'a str,
    pub repo_path: &'a str,
    pub model: Option<&'a str>,
    pub exec_config: &'a WorkflowExecConfig,
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
    /// Optional feature ID linking this run to a feature branch.
    pub feature_id: Option<&'a str>,
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
}

/// Owned inputs for [`execute_workflow_standalone`], avoiding lifetime issues
/// when spawning background threads.
pub struct WorkflowExecStandalone {
    pub config: crate::config::Config,
    pub workflow: crate::workflow_dsl::WorkflowDef,
    /// `None` for ephemeral PR runs with no registered worktree.
    pub worktree_id: Option<String>,
    pub working_dir: String,
    pub repo_path: String,
    pub ticket_id: Option<String>,
    pub repo_id: Option<String>,
    pub model: Option<String>,
    pub exec_config: WorkflowExecConfig,
    pub inputs: HashMap<String, String>,
    /// Human-readable label for the target (e.g. `repo_slug/wt_slug`, `owner/repo#N`).
    pub target_label: Option<String>,
    /// Optional feature ID linking this run to a feature branch.
    pub feature_id: Option<String>,
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
}

/// Input parameters for resuming a workflow run.
pub struct WorkflowResumeInput<'a> {
    pub conn: &'a rusqlite::Connection,
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
