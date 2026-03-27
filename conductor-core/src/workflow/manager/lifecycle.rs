use std::collections::HashMap;

use chrono::Utc;
use rusqlite::params;
use serde_json;

use crate::agent::AgentManager;
use crate::error::{ConductorError, Result};

use super::WorkflowManager;
use crate::workflow::status::{WorkflowRunStatus, WorkflowStepStatus};
use crate::workflow::types::WorkflowRun;

impl<'a> WorkflowManager<'a> {
    pub fn create_workflow_run(
        &self,
        workflow_name: &str,
        worktree_id: Option<&str>,
        parent_run_id: &str,
        dry_run: bool,
        trigger: &str,
        definition_snapshot: Option<&str>,
    ) -> Result<WorkflowRun> {
        self.create_workflow_run_with_targets(
            workflow_name,
            worktree_id,
            None,
            None,
            parent_run_id,
            dry_run,
            trigger,
            definition_snapshot,
            None,
            None,
            None,
        )
    }

    /// Create a workflow run record with ticket and repo target IDs in a single INSERT.
    #[allow(clippy::too_many_arguments)]
    pub fn create_workflow_run_with_targets(
        &self,
        workflow_name: &str,
        worktree_id: Option<&str>,
        ticket_id: Option<&str>,
        repo_id: Option<&str>,
        parent_run_id: &str,
        dry_run: bool,
        trigger: &str,
        definition_snapshot: Option<&str>,
        parent_workflow_run_id: Option<&str>,
        target_label: Option<&str>,
        feature_id: Option<&str>,
    ) -> Result<WorkflowRun> {
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, worktree_id, ticket_id, repo_id, \
             parent_run_id, status, dry_run, trigger, started_at, definition_snapshot, \
             parent_workflow_run_id, target_label, feature_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                id,
                workflow_name,
                worktree_id,
                ticket_id,
                repo_id,
                parent_run_id,
                "pending",
                dry_run as i64,
                trigger,
                now,
                definition_snapshot,
                parent_workflow_run_id,
                target_label,
                feature_id,
            ],
        )?;

        Ok(WorkflowRun {
            id,
            workflow_name: workflow_name.to_string(),
            worktree_id: worktree_id.map(String::from),
            parent_run_id: parent_run_id.to_string(),
            status: WorkflowRunStatus::Pending,
            dry_run,
            trigger: trigger.to_string(),
            started_at: now,
            ended_at: None,
            result_summary: None,
            definition_snapshot: definition_snapshot.map(String::from),
            inputs: HashMap::new(),
            ticket_id: ticket_id.map(String::from),
            repo_id: repo_id.map(String::from),
            parent_workflow_run_id: parent_workflow_run_id.map(String::from),
            target_label: target_label.map(String::from),
            default_bot_name: None,
            iteration: 0,
            blocked_on: None,
            feature_id: feature_id.map(String::from),
        })
    }

    /// Persist the loop iteration number for a workflow run.
    pub fn set_workflow_run_iteration(&self, run_id: &str, iteration: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_runs SET iteration = ?1 WHERE id = ?2",
            params![iteration, run_id],
        )?;
        Ok(())
    }

    /// Persist the default bot name for a workflow run.
    pub fn set_workflow_run_default_bot_name(&self, run_id: &str, bot_name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_runs SET default_bot_name = ?1 WHERE id = ?2",
            params![bot_name, run_id],
        )?;
        Ok(())
    }

    /// Persist the input variables for a workflow run.
    pub fn set_workflow_run_inputs(
        &self,
        run_id: &str,
        inputs: &HashMap<String, String>,
    ) -> Result<()> {
        let inputs_json = serde_json::to_string(inputs).map_err(|e| {
            ConductorError::Workflow(format!("Failed to serialize workflow inputs: {e}"))
        })?;
        self.conn.execute(
            "UPDATE workflow_runs SET inputs = ?1 WHERE id = ?2",
            params![inputs_json, run_id],
        )?;
        Ok(())
    }

    /// Update workflow run status.
    ///
    /// Returns [`ConductorError::InvalidInput`] if called with `Waiting` —
    /// use [`set_waiting_blocked_on`] instead to atomically set both status
    /// and blocked_on context.
    pub fn update_workflow_status(
        &self,
        workflow_run_id: &str,
        status: WorkflowRunStatus,
        result_summary: Option<&str>,
    ) -> Result<()> {
        if matches!(status, WorkflowRunStatus::Waiting) {
            return Err(ConductorError::InvalidInput(
                "Use set_waiting_blocked_on() to transition to Waiting status".into(),
            ));
        }

        let now = Utc::now().to_rfc3339();
        let is_terminal = matches!(
            status,
            WorkflowRunStatus::Completed | WorkflowRunStatus::Failed | WorkflowRunStatus::Cancelled
        );
        let ended_at = if is_terminal {
            Some(now.as_str())
        } else {
            None
        };

        // Always clear blocked_on — the only way to enter Waiting (which sets
        // blocked_on) is through set_waiting_blocked_on().
        self.conn.execute(
            "UPDATE workflow_runs SET status = ?1, result_summary = ?2, ended_at = ?3, blocked_on = NULL WHERE id = ?4",
            params![status, result_summary, ended_at, workflow_run_id],
        )?;
        Ok(())
    }

    /// Atomically transition a workflow run to `Waiting` status and record what it
    /// is blocked on.  This avoids a two-phase write where status and blocked_on
    /// are set in separate statements.
    pub fn set_waiting_blocked_on(
        &self,
        workflow_run_id: &str,
        blocked_on: &crate::workflow::types::BlockedOn,
    ) -> Result<()> {
        let json = serde_json::to_string(blocked_on).map_err(|e| {
            ConductorError::Workflow(format!("Failed to serialize blocked_on: {e}"))
        })?;
        self.conn.execute(
            "UPDATE workflow_runs SET status = ?1, blocked_on = ?2 WHERE id = ?3",
            params![WorkflowRunStatus::Waiting, json, workflow_run_id],
        )?;
        Ok(())
    }

    /// Cancel a workflow run, best-effort cancelling any in-progress steps and
    /// their child agent runs before marking the run itself as cancelled.
    ///
    /// Returns an error only if the run is not found or is already in a
    /// terminal state (`completed`, `failed`, or `cancelled`).  Step and
    /// child-run cancellation failures are silently ignored (best-effort).
    pub fn cancel_run(&self, run_id: &str, reason: &str) -> Result<()> {
        let run = self
            .get_workflow_run(run_id)?
            .ok_or_else(|| ConductorError::Workflow(format!("Workflow run not found: {run_id}")))?;

        if matches!(
            run.status,
            WorkflowRunStatus::Completed | WorkflowRunStatus::Failed | WorkflowRunStatus::Cancelled
        ) {
            return Err(ConductorError::Workflow(format!(
                "Run {run_id} is already in terminal state: {}",
                run.status
            )));
        }

        let agent_mgr = AgentManager::new(self.conn);
        if let Ok(steps) = self.get_workflow_steps(run_id) {
            for step in steps {
                if matches!(
                    step.status,
                    WorkflowStepStatus::Completed
                        | WorkflowStepStatus::Failed
                        | WorkflowStepStatus::Skipped
                        | WorkflowStepStatus::TimedOut
                ) {
                    continue;
                }
                if let Some(ref child_id) = step.child_run_id {
                    if let Err(e) = agent_mgr.update_run_cancelled(child_id) {
                        tracing::warn!(
                            step_id = %step.id,
                            child_run_id = %child_id,
                            "Failed to mark child agent run as cancelled during workflow cancellation: {e}"
                        );
                    }
                }
                if let Err(e) = self.update_step_status(
                    &step.id,
                    WorkflowStepStatus::Failed,
                    step.child_run_id.as_deref(),
                    Some(reason),
                    None,
                    None,
                    None,
                ) {
                    tracing::warn!(
                        step_id = %step.id,
                        "Failed to update step status to Failed during workflow cancellation: {e}"
                    );
                }
            }
        }

        self.update_workflow_status(run_id, WorkflowRunStatus::Cancelled, Some(reason))
    }
}
