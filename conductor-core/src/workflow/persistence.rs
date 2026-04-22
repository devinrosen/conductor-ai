use crate::workflow::engine_error::EngineError;
use crate::workflow::manager::FanOutItemRow;
use crate::workflow::status::{WorkflowRunStatus, WorkflowStepStatus};
use crate::workflow::types::{WorkflowRun, WorkflowRunStep};

/// Parameters for creating a new workflow run.
pub struct NewRun {
    pub workflow_name: String,
    pub worktree_id: Option<String>,
    pub ticket_id: Option<String>,
    pub repo_id: Option<String>,
    pub parent_run_id: String,
    pub dry_run: bool,
    pub trigger: String,
    pub definition_snapshot: Option<String>,
    pub parent_workflow_run_id: Option<String>,
    pub target_label: Option<String>,
}

/// Parameters for inserting a new workflow step.
///
/// When `retry_count` is `Some`, the step is inserted with `status = 'running'`
/// and `started_at` set atomically. When `None`, the step starts as `'pending'`.
pub struct NewStep {
    pub workflow_run_id: String,
    pub step_name: String,
    pub role: String,
    pub can_commit: bool,
    pub position: i64,
    pub iteration: i64,
    /// `Some(n)` → insert as running with retry_count=n; `None` → insert as pending.
    pub retry_count: Option<i64>,
}

/// Fields to update on an existing workflow step.
pub struct StepUpdate {
    pub status: WorkflowStepStatus,
    pub child_run_id: Option<String>,
    pub result_text: Option<String>,
    pub context_out: Option<String>,
    pub markers_out: Option<String>,
    pub retry_count: Option<i64>,
    pub structured_output: Option<String>,
    pub step_error: Option<String>,
}

/// Status values for fan-out items, mirroring the string constants stored in the DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FanOutItemStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

impl FanOutItemStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

impl TryFrom<&str> for FanOutItemStatus {
    type Error = String;
    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "skipped" => Ok(Self::Skipped),
            other => Err(format!("unknown FanOutItemStatus: {other}")),
        }
    }
}

/// Update payload for a fan-out item, mapping the two existing update variants.
pub enum FanOutItemUpdate {
    Running { child_run_id: String },
    Terminal { status: FanOutItemStatus },
}

/// Current approval state of a gate step.
#[derive(Debug, Clone)]
pub enum GateApprovalState {
    Pending,
    Approved {
        feedback: Option<String>,
        selections: Option<Vec<String>>,
    },
    Rejected {
        reason: String,
    },
}

/// Abstracts all persistence reads and writes needed by the workflow engine.
///
/// `Send + Sync` are required for use behind `Arc<dyn WorkflowPersistence>`.
/// All methods acquire a lock internally; no external synchronization is needed.
pub trait WorkflowPersistence: Send + Sync {
    // --- Run lifecycle ---

    fn create_run(&self, new_run: NewRun) -> Result<WorkflowRun, EngineError>;
    fn get_run(&self, run_id: &str) -> Result<Option<WorkflowRun>, EngineError>;
    fn list_active_runs(
        &self,
        statuses: &[WorkflowRunStatus],
    ) -> Result<Vec<WorkflowRun>, EngineError>;
    /// Update run status.
    ///
    /// NOTE: must not be called with `WorkflowRunStatus::Waiting` — use the engine's
    /// `set_waiting_blocked_on` path instead.
    fn update_run_status(
        &self,
        run_id: &str,
        status: WorkflowRunStatus,
        result_summary: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), EngineError>;

    // --- Steps ---

    fn insert_step(&self, new_step: NewStep) -> Result<String, EngineError>;
    fn update_step(&self, step_id: &str, update: StepUpdate) -> Result<(), EngineError>;
    fn get_steps(&self, run_id: &str) -> Result<Vec<WorkflowRunStep>, EngineError>;

    // --- Fan-out ---

    fn insert_fan_out_item(
        &self,
        step_run_id: &str,
        item_type: &str,
        item_id: &str,
        item_ref: &str,
    ) -> Result<String, EngineError>;
    fn update_fan_out_item(
        &self,
        item_id: &str,
        update: FanOutItemUpdate,
    ) -> Result<(), EngineError>;
    fn get_fan_out_items(
        &self,
        step_run_id: &str,
        status_filter: Option<FanOutItemStatus>,
    ) -> Result<Vec<FanOutItemRow>, EngineError>;

    // --- Gate approval ---

    fn get_gate_approval(&self, step_id: &str) -> Result<GateApprovalState, EngineError>;
    fn approve_gate(
        &self,
        step_id: &str,
        approved_by: &str,
        feedback: Option<&str>,
        selections: Option<&[String]>,
    ) -> Result<(), EngineError>;
    fn reject_gate(
        &self,
        step_id: &str,
        rejected_by: &str,
        feedback: Option<&str>,
    ) -> Result<(), EngineError>;
}
