use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension};

use crate::error::ConductorError;
use crate::workflow::engine_error::EngineError;
use crate::workflow::manager::FanOutItemRow;
use crate::workflow::manager::WorkflowManager;
use crate::workflow::status::WorkflowRunStatus;
use crate::workflow::types::{WorkflowRun, WorkflowRunStep};

use super::persistence::{
    gate_approval_state_from_fields, FanOutItemStatus, FanOutItemUpdate, GateApprovalState, NewRun,
    NewStep, StepUpdate, WorkflowPersistence,
};

/// SQLite-backed implementation of `WorkflowPersistence`.
///
/// Wraps a `rusqlite::Connection` behind `Arc<Mutex<_>>` so it satisfies the
/// `Send + Sync` requirement of `WorkflowPersistence`. Every method acquires the
/// lock, instantiates a transient `WorkflowManager`, delegates to it, and maps
/// `ConductorError` → `EngineError::Persistence`.
pub struct SqliteWorkflowPersistence {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteWorkflowPersistence {
    /// Open a new SQLite connection at `path`, configured for WAL mode and
    /// foreign key enforcement. Creates the file if it does not already exist.
    pub fn open(path: &Path) -> crate::error::Result<Self> {
        let conn = Connection::open(path).map_err(ConductorError::Database)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(ConductorError::Database)?;
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(ConductorError::Database)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Wrap an existing shared connection. Used by `execute_workflow_standalone`
    /// to share one `Connection` between the setup phase and the engine.
    pub fn from_shared_connection(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }
}

fn to_engine_err(e: ConductorError) -> EngineError {
    EngineError::Persistence(e.to_string())
}

fn lock_err() -> EngineError {
    EngineError::Persistence("SqliteWorkflowPersistence: mutex poisoned".into())
}

impl WorkflowPersistence for SqliteWorkflowPersistence {
    fn create_run(&self, new_run: NewRun) -> Result<WorkflowRun, EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        WorkflowManager::new(&guard)
            .create_workflow_run_with_targets(
                &new_run.workflow_name,
                new_run.worktree_id.as_deref(),
                new_run.ticket_id.as_deref(),
                new_run.repo_id.as_deref(),
                &new_run.parent_run_id,
                new_run.dry_run,
                &new_run.trigger,
                new_run.definition_snapshot.as_deref(),
                new_run.parent_workflow_run_id.as_deref(),
                new_run.target_label.as_deref(),
            )
            .map_err(to_engine_err)
    }

    fn get_run(&self, run_id: &str) -> Result<Option<WorkflowRun>, EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        WorkflowManager::new(&guard)
            .get_workflow_run(run_id)
            .map_err(to_engine_err)
    }

    fn list_active_runs(
        &self,
        statuses: &[WorkflowRunStatus],
    ) -> Result<Vec<WorkflowRun>, EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        WorkflowManager::new(&guard)
            .list_active_workflow_runs(statuses)
            .map_err(to_engine_err)
    }

    fn update_run_status(
        &self,
        run_id: &str,
        status: WorkflowRunStatus,
        result_summary: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        WorkflowManager::new(&guard)
            .update_workflow_status(run_id, status, result_summary, error)
            .map_err(to_engine_err)
    }

    fn insert_step(&self, new_step: NewStep) -> Result<String, EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        let mgr = WorkflowManager::new(&guard);
        if let Some(retry_count) = new_step.retry_count {
            mgr.insert_step_running(
                &new_step.workflow_run_id,
                &new_step.step_name,
                &new_step.role,
                new_step.can_commit,
                new_step.position,
                new_step.iteration,
                retry_count,
            )
        } else {
            mgr.insert_step(
                &new_step.workflow_run_id,
                &new_step.step_name,
                &new_step.role,
                new_step.can_commit,
                new_step.position,
                new_step.iteration,
            )
        }
        .map_err(to_engine_err)
    }

    fn update_step(&self, step_id: &str, update: StepUpdate) -> Result<(), EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        WorkflowManager::new(&guard)
            .update_step_status_full(
                step_id,
                update.status,
                update.child_run_id.as_deref(),
                update.result_text.as_deref(),
                update.context_out.as_deref(),
                update.markers_out.as_deref(),
                update.retry_count,
                update.structured_output.as_deref(),
                update.step_error.as_deref(),
            )
            .map_err(to_engine_err)
    }

    fn get_steps(&self, run_id: &str) -> Result<Vec<WorkflowRunStep>, EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        WorkflowManager::new(&guard)
            .get_workflow_steps(run_id)
            .map_err(to_engine_err)
    }

    fn insert_fan_out_item(
        &self,
        step_run_id: &str,
        item_type: &str,
        item_id: &str,
        item_ref: &str,
    ) -> Result<String, EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        WorkflowManager::new(&guard)
            .insert_fan_out_item(step_run_id, item_type, item_id, item_ref)
            .map_err(to_engine_err)
    }

    fn update_fan_out_item(
        &self,
        item_id: &str,
        update: FanOutItemUpdate,
    ) -> Result<(), EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        let mgr = WorkflowManager::new(&guard);
        match update {
            FanOutItemUpdate::Running { child_run_id } => {
                mgr.update_fan_out_item_running(item_id, &child_run_id)
            }
            FanOutItemUpdate::Terminal { status } => {
                mgr.update_fan_out_item_terminal(item_id, status.as_str())
            }
        }
        .map_err(to_engine_err)
    }

    fn get_fan_out_items(
        &self,
        step_run_id: &str,
        status_filter: Option<FanOutItemStatus>,
    ) -> Result<Vec<FanOutItemRow>, EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        let status_str = status_filter.map(|s| s.as_str());
        WorkflowManager::new(&guard)
            .get_fan_out_items(step_run_id, status_str)
            .map_err(to_engine_err)
    }

    #[allow(clippy::type_complexity)]
    fn get_gate_approval(&self, step_id: &str) -> Result<GateApprovalState, EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;

        let row: Option<(Option<String>, String, Option<String>, Option<String>)> = guard
            .query_row(
                "SELECT gate_approved_at, status, gate_feedback, gate_selections \
                 FROM workflow_run_steps WHERE id = ?1",
                rusqlite::params![step_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|e| EngineError::Persistence(e.to_string()))?;

        let Some((approved_at, status_str, feedback, selections_json)) = row else {
            return Ok(GateApprovalState::Pending);
        };

        use crate::workflow::status::WorkflowStepStatus;
        let status = status_str
            .parse::<WorkflowStepStatus>()
            .unwrap_or_else(|_| {
                tracing::warn!(
                    "get_gate_approval: unrecognised step status '{status_str}' for step {step_id}, treating as Waiting"
                );
                WorkflowStepStatus::Waiting
            });
        let selections = selections_json.and_then(|json| {
            serde_json::from_str::<Vec<String>>(&json)
                .map_err(|e| {
                    tracing::warn!(
                        "get_gate_approval: failed to deserialize gate_selections for step {step_id}: {e}"
                    );
                })
                .ok()
        });

        Ok(gate_approval_state_from_fields(
            approved_at.as_deref(),
            status,
            feedback,
            selections,
        ))
    }

    fn approve_gate(
        &self,
        step_id: &str,
        approved_by: &str,
        feedback: Option<&str>,
        selections: Option<&[String]>,
    ) -> Result<(), EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        WorkflowManager::new(&guard)
            .approve_gate(step_id, approved_by, feedback, selections)
            .map_err(to_engine_err)
    }

    fn reject_gate(
        &self,
        step_id: &str,
        rejected_by: &str,
        feedback: Option<&str>,
    ) -> Result<(), EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        WorkflowManager::new(&guard)
            .reject_gate(step_id, rejected_by, feedback)
            .map_err(to_engine_err)
    }
}

// ---------------------------------------------------------------------------
// Type converters between runkon-flow and conductor-core persistence types
// ---------------------------------------------------------------------------

mod rk_conv {
    use crate::workflow::manager::FanOutItemRow as CoreFanOutItemRow;
    use crate::workflow::persistence::{
        FanOutItemStatus as CoreFanOutItemStatus, FanOutItemUpdate as CoreFanOutItemUpdate,
        GateApprovalState as CoreGateApprovalState, NewRun as CoreNewRun, NewStep as CoreNewStep,
        StepUpdate as CoreStepUpdate,
    };
    use crate::workflow::types::{
        BlockedOn as CoreBlockedOn, WorkflowRun as CoreRun, WorkflowRunStep as CoreStep,
    };
    use runkon_flow::traits::persistence::{
        FanOutItemStatus as RkFanOutItemStatus, FanOutItemUpdate as RkFanOutItemUpdate,
        GateApprovalState as RkGateApprovalState, NewRun as RkNewRun, NewStep as RkNewStep,
        StepUpdate as RkStepUpdate,
    };
    use runkon_flow::types::{
        BlockedOn as RkBlockedOn, FanOutItemRow as RkFanOutItemRow, WorkflowRun as RkRun,
        WorkflowRunStep as RkStep,
    };

    pub fn run_status_to_core(
        s: runkon_flow::status::WorkflowRunStatus,
    ) -> crate::workflow::status::WorkflowRunStatus {
        s.to_string()
            .parse()
            .unwrap_or(crate::workflow::status::WorkflowRunStatus::Pending)
    }

    pub fn run_status_to_rk(
        s: crate::workflow::status::WorkflowRunStatus,
    ) -> runkon_flow::status::WorkflowRunStatus {
        s.to_string()
            .parse()
            .unwrap_or(runkon_flow::status::WorkflowRunStatus::Pending)
    }

    pub fn step_status_to_core(
        s: runkon_flow::status::WorkflowStepStatus,
    ) -> crate::workflow::status::WorkflowStepStatus {
        s.to_string()
            .parse()
            .unwrap_or(crate::workflow::status::WorkflowStepStatus::Pending)
    }

    pub fn step_status_to_rk(
        s: crate::workflow::status::WorkflowStepStatus,
    ) -> runkon_flow::status::WorkflowStepStatus {
        s.to_string()
            .parse()
            .unwrap_or(runkon_flow::status::WorkflowStepStatus::Pending)
    }

    pub fn new_run_to_core(r: RkNewRun) -> CoreNewRun {
        CoreNewRun {
            workflow_name: r.workflow_name,
            worktree_id: r.worktree_id,
            ticket_id: r.ticket_id,
            repo_id: r.repo_id,
            parent_run_id: r.parent_run_id,
            dry_run: r.dry_run,
            trigger: r.trigger,
            definition_snapshot: r.definition_snapshot,
            parent_workflow_run_id: r.parent_workflow_run_id,
            target_label: r.target_label,
        }
    }

    pub fn new_step_to_core(s: RkNewStep) -> CoreNewStep {
        CoreNewStep {
            workflow_run_id: s.workflow_run_id,
            step_name: s.step_name,
            role: s.role,
            can_commit: s.can_commit,
            position: s.position,
            iteration: s.iteration,
            retry_count: s.retry_count,
        }
    }

    pub fn step_update_to_core(u: RkStepUpdate) -> CoreStepUpdate {
        CoreStepUpdate {
            status: step_status_to_core(u.status),
            child_run_id: u.child_run_id,
            result_text: u.result_text,
            context_out: u.context_out,
            markers_out: u.markers_out,
            retry_count: u.retry_count,
            structured_output: u.structured_output,
            step_error: u.step_error,
        }
    }

    pub fn fan_out_update_to_core(u: RkFanOutItemUpdate) -> CoreFanOutItemUpdate {
        match u {
            RkFanOutItemUpdate::Running { child_run_id } => {
                CoreFanOutItemUpdate::Running { child_run_id }
            }
            RkFanOutItemUpdate::Terminal { status } => CoreFanOutItemUpdate::Terminal {
                status: fan_out_status_to_core(status),
            },
        }
    }

    pub fn fan_out_status_to_core(s: RkFanOutItemStatus) -> CoreFanOutItemStatus {
        match s {
            RkFanOutItemStatus::Pending => CoreFanOutItemStatus::Pending,
            RkFanOutItemStatus::Running => CoreFanOutItemStatus::Running,
            RkFanOutItemStatus::Completed => CoreFanOutItemStatus::Completed,
            RkFanOutItemStatus::Failed => CoreFanOutItemStatus::Failed,
            RkFanOutItemStatus::Skipped => CoreFanOutItemStatus::Skipped,
        }
    }

    pub fn run_to_rk(r: CoreRun) -> RkRun {
        RkRun {
            id: r.id,
            workflow_name: r.workflow_name,
            worktree_id: r.worktree_id,
            parent_run_id: r.parent_run_id,
            status: run_status_to_rk(r.status),
            dry_run: r.dry_run,
            trigger: r.trigger,
            started_at: r.started_at,
            ended_at: r.ended_at,
            result_summary: r.result_summary,
            error: r.error,
            definition_snapshot: r.definition_snapshot,
            inputs: r.inputs,
            ticket_id: r.ticket_id,
            repo_id: r.repo_id,
            parent_workflow_run_id: r.parent_workflow_run_id,
            target_label: r.target_label,
            default_bot_name: r.default_bot_name,
            iteration: r.iteration,
            blocked_on: r.blocked_on.map(blocked_on_to_rk),
            workflow_title: r.workflow_title,
            total_input_tokens: r.total_input_tokens,
            total_output_tokens: r.total_output_tokens,
            total_cache_read_input_tokens: r.total_cache_read_input_tokens,
            total_cache_creation_input_tokens: r.total_cache_creation_input_tokens,
            total_turns: r.total_turns,
            total_cost_usd: r.total_cost_usd,
            total_duration_ms: r.total_duration_ms,
            model: r.model,
            dismissed: r.dismissed,
        }
    }

    fn blocked_on_to_rk(b: CoreBlockedOn) -> RkBlockedOn {
        match b {
            CoreBlockedOn::HumanApproval {
                gate_name,
                prompt,
                options,
            } => RkBlockedOn::HumanApproval {
                gate_name,
                prompt,
                options,
            },
            CoreBlockedOn::HumanReview {
                gate_name,
                prompt,
                options,
            } => RkBlockedOn::HumanReview {
                gate_name,
                prompt,
                options,
            },
            CoreBlockedOn::PrApproval {
                gate_name,
                approvals_needed,
            } => RkBlockedOn::PrApproval {
                gate_name,
                approvals_needed,
            },
            CoreBlockedOn::PrChecks { gate_name } => RkBlockedOn::PrChecks { gate_name },
        }
    }

    pub fn step_to_rk(s: CoreStep) -> RkStep {
        let gate_type = s.gate_type.as_ref().and_then(|gt| {
            let gt_str = gt.to_string();
            gt_str
                .parse::<runkon_flow::dsl::GateType>()
                .map_err(|_| {
                    tracing::warn!(
                        step_id = %s.id,
                        gate_type = %gt_str,
                        "Unrecognised gate type in step; treating as None",
                    );
                })
                .ok()
        });
        RkStep {
            id: s.id,
            workflow_run_id: s.workflow_run_id,
            step_name: s.step_name,
            role: s.role,
            can_commit: s.can_commit,
            condition_expr: s.condition_expr,
            status: step_status_to_rk(s.status),
            child_run_id: s.child_run_id,
            position: s.position,
            started_at: s.started_at,
            ended_at: s.ended_at,
            result_text: s.result_text,
            condition_met: s.condition_met,
            iteration: s.iteration,
            parallel_group_id: s.parallel_group_id,
            context_out: s.context_out,
            markers_out: s.markers_out,
            retry_count: s.retry_count,
            gate_type,
            gate_prompt: s.gate_prompt,
            gate_timeout: s.gate_timeout,
            gate_approved_by: s.gate_approved_by,
            gate_approved_at: s.gate_approved_at,
            gate_feedback: s.gate_feedback,
            structured_output: s.structured_output,
            output_file: s.output_file,
            gate_options: s.gate_options,
            gate_selections: s.gate_selections,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            cache_read_input_tokens: s.cache_read_input_tokens,
            cache_creation_input_tokens: s.cache_creation_input_tokens,
            fan_out_total: s.fan_out_total,
            fan_out_completed: s.fan_out_completed,
            fan_out_failed: s.fan_out_failed,
            fan_out_skipped: s.fan_out_skipped,
            step_error: s.step_error,
        }
    }

    pub fn fan_out_item_to_rk(r: CoreFanOutItemRow) -> RkFanOutItemRow {
        RkFanOutItemRow {
            id: r.id,
            step_run_id: r.step_run_id,
            item_type: r.item_type,
            item_id: r.item_id,
            item_ref: r.item_ref,
            child_run_id: r.child_run_id,
            status: r.status,
            dispatched_at: r.dispatched_at,
            completed_at: r.completed_at,
        }
    }

    pub fn gate_approval_to_rk(s: CoreGateApprovalState) -> RkGateApprovalState {
        match s {
            CoreGateApprovalState::Pending => RkGateApprovalState::Pending,
            CoreGateApprovalState::Approved {
                feedback,
                selections,
            } => RkGateApprovalState::Approved {
                feedback,
                selections,
            },
            CoreGateApprovalState::Rejected { feedback } => {
                RkGateApprovalState::Rejected { feedback }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// runkon-flow WorkflowPersistence impl — delegates to the core trait impl
// ---------------------------------------------------------------------------

impl runkon_flow::traits::persistence::WorkflowPersistence for SqliteWorkflowPersistence {
    fn create_run(
        &self,
        new_run: runkon_flow::traits::persistence::NewRun,
    ) -> Result<runkon_flow::types::WorkflowRun, EngineError> {
        let core_run =
            <Self as WorkflowPersistence>::create_run(self, rk_conv::new_run_to_core(new_run))?;
        Ok(rk_conv::run_to_rk(core_run))
    }

    fn get_run(
        &self,
        run_id: &str,
    ) -> Result<Option<runkon_flow::types::WorkflowRun>, EngineError> {
        let result = <Self as WorkflowPersistence>::get_run(self, run_id)?;
        Ok(result.map(rk_conv::run_to_rk))
    }

    fn list_active_runs(
        &self,
        statuses: &[runkon_flow::status::WorkflowRunStatus],
    ) -> Result<Vec<runkon_flow::types::WorkflowRun>, EngineError> {
        let core_statuses: Vec<crate::workflow::status::WorkflowRunStatus> = statuses
            .iter()
            .map(|s| rk_conv::run_status_to_core(s.clone()))
            .collect();
        let result = <Self as WorkflowPersistence>::list_active_runs(self, &core_statuses)?;
        Ok(result.into_iter().map(rk_conv::run_to_rk).collect())
    }

    fn update_run_status(
        &self,
        run_id: &str,
        status: runkon_flow::status::WorkflowRunStatus,
        result_summary: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), EngineError> {
        <Self as WorkflowPersistence>::update_run_status(
            self,
            run_id,
            rk_conv::run_status_to_core(status),
            result_summary,
            error,
        )
    }

    fn insert_step(
        &self,
        new_step: runkon_flow::traits::persistence::NewStep,
    ) -> Result<String, EngineError> {
        <Self as WorkflowPersistence>::insert_step(self, rk_conv::new_step_to_core(new_step))
    }

    fn update_step(
        &self,
        step_id: &str,
        update: runkon_flow::traits::persistence::StepUpdate,
    ) -> Result<(), EngineError> {
        <Self as WorkflowPersistence>::update_step(
            self,
            step_id,
            rk_conv::step_update_to_core(update),
        )
    }

    fn get_steps(
        &self,
        run_id: &str,
    ) -> Result<Vec<runkon_flow::types::WorkflowRunStep>, EngineError> {
        let result = <Self as WorkflowPersistence>::get_steps(self, run_id)?;
        Ok(result.into_iter().map(rk_conv::step_to_rk).collect())
    }

    fn insert_fan_out_item(
        &self,
        step_run_id: &str,
        item_type: &str,
        item_id: &str,
        item_ref: &str,
    ) -> Result<String, EngineError> {
        <Self as WorkflowPersistence>::insert_fan_out_item(
            self,
            step_run_id,
            item_type,
            item_id,
            item_ref,
        )
    }

    fn update_fan_out_item(
        &self,
        item_id: &str,
        update: runkon_flow::traits::persistence::FanOutItemUpdate,
    ) -> Result<(), EngineError> {
        <Self as WorkflowPersistence>::update_fan_out_item(
            self,
            item_id,
            rk_conv::fan_out_update_to_core(update),
        )
    }

    fn get_fan_out_items(
        &self,
        step_run_id: &str,
        status_filter: Option<runkon_flow::traits::persistence::FanOutItemStatus>,
    ) -> Result<Vec<runkon_flow::types::FanOutItemRow>, EngineError> {
        let core_filter = status_filter.map(rk_conv::fan_out_status_to_core);
        let result =
            <Self as WorkflowPersistence>::get_fan_out_items(self, step_run_id, core_filter)?;
        Ok(result
            .into_iter()
            .map(rk_conv::fan_out_item_to_rk)
            .collect())
    }

    fn get_gate_approval(
        &self,
        step_id: &str,
    ) -> Result<runkon_flow::traits::persistence::GateApprovalState, EngineError> {
        let result = <Self as WorkflowPersistence>::get_gate_approval(self, step_id)?;
        Ok(rk_conv::gate_approval_to_rk(result))
    }

    fn approve_gate(
        &self,
        step_id: &str,
        approved_by: &str,
        feedback: Option<&str>,
        selections: Option<&[String]>,
    ) -> Result<(), EngineError> {
        <Self as WorkflowPersistence>::approve_gate(
            self,
            step_id,
            approved_by,
            feedback,
            selections,
        )
    }

    fn reject_gate(
        &self,
        step_id: &str,
        rejected_by: &str,
        feedback: Option<&str>,
    ) -> Result<(), EngineError> {
        <Self as WorkflowPersistence>::reject_gate(self, step_id, rejected_by, feedback)
    }

    fn is_run_cancelled(&self, run_id: &str) -> Result<bool, EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        let status: Option<String> = guard
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| EngineError::Persistence(e.to_string()))?;
        Ok(matches!(
            status.as_deref(),
            Some("cancelled") | Some("cancelling")
        ))
    }

    fn tick_heartbeat(&self, run_id: &str) -> Result<(), EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        WorkflowManager::new(&guard)
            .tick_heartbeat(run_id)
            .map_err(to_engine_err)
    }

    #[allow(clippy::too_many_arguments)]
    fn persist_metrics(
        &self,
        run_id: &str,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_input_tokens: i64,
        cache_creation_input_tokens: i64,
        cost_usd: f64,
        num_turns: i64,
        duration_ms: i64,
    ) -> Result<(), EngineError> {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        WorkflowManager::new(&guard)
            .persist_workflow_metrics(
                run_id,
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                num_turns, // rk pos 7 → core pos 6
                cost_usd,  // rk pos 6 → core pos 7
                duration_ms,
                None,
            )
            .map_err(to_engine_err)
    }
}

#[cfg(test)]
impl SqliteWorkflowPersistence {
    fn from_connection(conn: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentManager;
    use crate::workflow::persistence::{GateApprovalState, NewRun, NewStep};

    fn make_persistence() -> (SqliteWorkflowPersistence, String) {
        let conn = crate::test_helpers::setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
        (SqliteWorkflowPersistence::from_connection(conn), parent.id)
    }

    fn make_new_run(parent_run_id: String) -> NewRun {
        NewRun {
            workflow_name: "test-wf".to_string(),
            worktree_id: Some("w1".to_string()),
            ticket_id: None,
            repo_id: None,
            parent_run_id,
            dry_run: false,
            trigger: "manual".to_string(),
            definition_snapshot: None,
            parent_workflow_run_id: None,
            target_label: None,
        }
    }

    #[test]
    fn get_gate_approval_returns_pending_for_unknown_step() {
        let (p, _) = make_persistence();
        let result = p.get_gate_approval("nonexistent-step");
        assert!(matches!(result, Ok(GateApprovalState::Pending)));
    }

    #[test]
    fn create_run_and_get_run_roundtrip() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        assert_eq!(run.workflow_name, "test-wf");
        let fetched = p.get_run(&run.id).unwrap();
        assert_eq!(fetched.map(|r| r.id), Some(run.id));
    }

    #[test]
    fn approve_gate_then_get_approval_returns_approved() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        let step_id = p
            .insert_step(NewStep {
                workflow_run_id: run.id,
                step_name: "approval-gate".to_string(),
                role: "gate".to_string(),
                can_commit: false,
                position: 0,
                iteration: 0,
                retry_count: None,
            })
            .unwrap();
        p.approve_gate(&step_id, "human", Some("looks good"), None)
            .unwrap();
        let state = p.get_gate_approval(&step_id).unwrap();
        assert!(matches!(state, GateApprovalState::Approved { .. }));
    }

    #[test]
    fn reject_gate_then_get_approval_returns_rejected() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        let step_id = p
            .insert_step(NewStep {
                workflow_run_id: run.id,
                step_name: "review-gate".to_string(),
                role: "gate".to_string(),
                can_commit: false,
                position: 0,
                iteration: 0,
                retry_count: None,
            })
            .unwrap();
        p.reject_gate(&step_id, "human", Some("needs work"))
            .unwrap();
        let state = p.get_gate_approval(&step_id).unwrap();
        assert!(matches!(state, GateApprovalState::Rejected { .. }));
    }

    #[test]
    fn update_run_status_roundtrip() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        p.update_run_status(&run.id, WorkflowRunStatus::Running, None, None)
            .unwrap();
        let active = p.list_active_runs(&[WorkflowRunStatus::Running]).unwrap();
        assert!(active.iter().any(|r| r.id == run.id));
    }

    // ---------------------------------------------------------------------------
    // from_shared_connection()
    // ---------------------------------------------------------------------------

    #[test]
    fn from_shared_connection_creates_working_persistence() {
        let conn = crate::test_helpers::setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

        let shared = Arc::new(std::sync::Mutex::new(conn));
        let p = SqliteWorkflowPersistence::from_shared_connection(Arc::clone(&shared));

        let run = p.create_run(make_new_run(parent.id)).unwrap();
        assert_eq!(run.workflow_name, "test-wf");

        let fetched = p.get_run(&run.id).unwrap();
        assert!(
            fetched.is_some(),
            "run should be retrievable after creation"
        );
    }

    #[test]
    fn from_shared_connection_shares_state_with_raw_connection() {
        let conn = crate::test_helpers::setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

        let shared = Arc::new(std::sync::Mutex::new(conn));
        let p = SqliteWorkflowPersistence::from_shared_connection(Arc::clone(&shared));

        let run = p.create_run(make_new_run(parent.id)).unwrap();

        // Verify state is visible through the shared connection handle too.
        let guard = shared.lock().unwrap();
        let mgr = crate::workflow::manager::WorkflowManager::new(&guard);
        let found = mgr.get_workflow_run(&run.id).unwrap();
        assert!(
            found.is_some(),
            "run written via persistence should be visible through shared conn"
        );
    }
}
