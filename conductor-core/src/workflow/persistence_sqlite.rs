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
    FanOutItemStatus, FanOutItemUpdate, GateApprovalState, NewRun, NewStep, StepUpdate,
    WorkflowPersistence,
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
        conn.pragma_update(None, "busy_timeout", 5000)
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

impl SqliteWorkflowPersistence {
    /// Acquire the connection lock, instantiate a `WorkflowManager`, run `f`,
    /// and map any `ConductorError` to `EngineError::Persistence`.
    fn with_manager<F, T>(&self, f: F) -> Result<T, EngineError>
    where
        F: for<'c> FnOnce(WorkflowManager<'c>) -> crate::error::Result<T>,
    {
        let guard = self.conn.lock().map_err(|_| lock_err())?;
        f(WorkflowManager::new(&guard)).map_err(to_engine_err)
    }
}

impl WorkflowPersistence for SqliteWorkflowPersistence {
    fn create_run(&self, new_run: NewRun) -> Result<WorkflowRun, EngineError> {
        self.with_manager(|mgr| {
            mgr.create_workflow_run_with_targets(
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
        })
    }

    fn get_run(&self, run_id: &str) -> Result<Option<WorkflowRun>, EngineError> {
        self.with_manager(|mgr| mgr.get_workflow_run(run_id))
    }

    fn list_active_runs(
        &self,
        statuses: &[WorkflowRunStatus],
    ) -> Result<Vec<WorkflowRun>, EngineError> {
        self.with_manager(|mgr| mgr.list_active_workflow_runs(statuses))
    }

    fn update_run_status(
        &self,
        run_id: &str,
        status: WorkflowRunStatus,
        result_summary: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), EngineError> {
        self.with_manager(|mgr| mgr.update_workflow_status(run_id, status, result_summary, error))
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
        self.with_manager(|mgr| mgr.get_workflow_steps(run_id))
    }

    fn insert_fan_out_item(
        &self,
        step_run_id: &str,
        item_type: &str,
        item_id: &str,
        item_ref: &str,
    ) -> Result<String, EngineError> {
        self.with_manager(|mgr| mgr.insert_fan_out_item(step_run_id, item_type, item_id, item_ref))
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
        let status_str = status_filter.map(|s| s.as_str());
        self.with_manager(|mgr| mgr.get_fan_out_items(step_run_id, status_str))
    }

    fn get_gate_approval(&self, step_id: &str) -> Result<GateApprovalState, EngineError> {
        self.with_manager(|mgr| mgr.get_gate_approval_state(step_id))
    }

    fn approve_gate(
        &self,
        step_id: &str,
        approved_by: &str,
        feedback: Option<&str>,
        selections: Option<&[String]>,
    ) -> Result<(), EngineError> {
        self.with_manager(|mgr| mgr.approve_gate(step_id, approved_by, feedback, selections))
    }

    fn reject_gate(
        &self,
        step_id: &str,
        rejected_by: &str,
        feedback: Option<&str>,
    ) -> Result<(), EngineError> {
        self.with_manager(|mgr| mgr.reject_gate(step_id, rejected_by, feedback))
    }
}

// ---------------------------------------------------------------------------
// Type converters between runkon-flow and conductor-core persistence types
// ---------------------------------------------------------------------------
// Re-exported from rk_types so call-sites in this file continue to use the
// `rk_conv::*` name without modification.
pub(super) use super::rk_types as rk_conv;

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

    // ---------------------------------------------------------------------------
    // is_run_cancelled()
    // ---------------------------------------------------------------------------

    #[test]
    fn is_run_cancelled_returns_true_for_cancelled_status() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        p.update_run_status(&run.id, WorkflowRunStatus::Cancelled, None, None)
            .unwrap();
        assert!(
            runkon_flow::traits::persistence::WorkflowPersistence::is_run_cancelled(&p, &run.id)
                .unwrap()
        );
    }

    #[test]
    fn is_run_cancelled_returns_true_for_cancelling_status() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        p.update_run_status(&run.id, WorkflowRunStatus::Cancelling, None, None)
            .unwrap();
        assert!(
            runkon_flow::traits::persistence::WorkflowPersistence::is_run_cancelled(&p, &run.id)
                .unwrap()
        );
    }

    #[test]
    fn is_run_cancelled_returns_false_for_running_status() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        p.update_run_status(&run.id, WorkflowRunStatus::Running, None, None)
            .unwrap();
        assert!(
            !runkon_flow::traits::persistence::WorkflowPersistence::is_run_cancelled(&p, &run.id)
                .unwrap()
        );
    }

    #[test]
    fn is_run_cancelled_returns_false_for_nonexistent_run() {
        let (p, _) = make_persistence();
        assert!(
            !runkon_flow::traits::persistence::WorkflowPersistence::is_run_cancelled(
                &p,
                "nonexistent-run-id"
            )
            .unwrap()
        );
    }

    /// `persist_metrics` swaps `cost_usd` (rk pos 6) and `num_turns` (rk pos 7) when
    /// forwarding to `persist_workflow_metrics`, which expects `total_turns` before
    /// `total_cost_usd`. This test guards against a future signature drift that would
    /// silently swap the columns in the DB.
    #[test]
    fn persist_metrics_maps_cost_and_turns_to_correct_columns() {
        let (p, parent_id) = make_persistence();
        // Create the run via the Core trait before bringing the Rk trait into scope
        // to avoid ambiguous `create_run` calls.
        let run_id = {
            let run = WorkflowPersistence::create_run(&p, make_new_run(parent_id)).unwrap();
            run.id
        };

        // Use distinguishable values so a swap is immediately visible.
        let cost_usd = 42.5_f64;
        let num_turns = 7_i64;

        // Explicitly disambiguate using the runkon-flow trait path.
        runkon_flow::traits::persistence::WorkflowPersistence::persist_metrics(
            &p, &run_id, 0, 0, 0, 0, cost_usd, num_turns, 1000,
        )
        .unwrap();

        let fetched = runkon_flow::traits::persistence::WorkflowPersistence::get_run(&p, &run_id)
            .unwrap()
            .expect("run should exist");

        assert_eq!(
            fetched.total_cost_usd,
            Some(cost_usd),
            "total_cost_usd should match the cost_usd argument"
        );
        assert_eq!(
            fetched.total_turns,
            Some(num_turns),
            "total_turns should match the num_turns argument"
        );
    }
}
