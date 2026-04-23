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
}
