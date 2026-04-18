use std::collections::HashMap;

use chrono::Utc;
use rusqlite::named_params;
use serde_json;

use crate::agent::AgentManager;
use crate::error::{ConductorError, Result};

use super::WorkflowManager;
use crate::workflow::status::{WorkflowRunStatus, WorkflowStepStatus};
use crate::workflow::types::{extract_workflow_title, WorkflowRun};

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
    ) -> Result<WorkflowRun> {
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, worktree_id, ticket_id, repo_id, \
             parent_run_id, status, dry_run, trigger, started_at, definition_snapshot, \
             parent_workflow_run_id, target_label) \
             VALUES (:id, :workflow_name, :worktree_id, :ticket_id, :repo_id, :parent_run_id, \
             :status, :dry_run, :trigger, :started_at, :definition_snapshot, \
             :parent_workflow_run_id, :target_label)",
            named_params![
                ":id": id,
                ":workflow_name": workflow_name,
                ":worktree_id": worktree_id,
                ":ticket_id": ticket_id,
                ":repo_id": repo_id,
                ":parent_run_id": parent_run_id,
                ":status": "pending",
                ":dry_run": dry_run as i64,
                ":trigger": trigger,
                ":started_at": now,
                ":definition_snapshot": definition_snapshot,
                ":parent_workflow_run_id": parent_workflow_run_id,
                ":target_label": target_label,
            ],
        )?;

        let workflow_title = extract_workflow_title(definition_snapshot);
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
            error: None,
            definition_snapshot: definition_snapshot.map(String::from),
            inputs: HashMap::new(),
            ticket_id: ticket_id.map(String::from),
            repo_id: repo_id.map(String::from),
            parent_workflow_run_id: parent_workflow_run_id.map(String::from),
            target_label: target_label.map(String::from),
            default_bot_name: None,
            iteration: 0,
            blocked_on: None,
            workflow_title,
            total_input_tokens: None,
            total_output_tokens: None,
            total_cache_read_input_tokens: None,
            total_cache_creation_input_tokens: None,
            total_turns: None,
            total_cost_usd: None,
            total_duration_ms: None,
            model: None,
        })
    }

    /// Persist the loop iteration number for a workflow run.
    pub fn set_workflow_run_iteration(&self, run_id: &str, iteration: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_runs SET iteration = :iteration WHERE id = :id",
            named_params![":iteration": iteration, ":id": run_id],
        )?;
        Ok(())
    }

    /// Persist the default bot name for a workflow run.
    pub fn set_workflow_run_default_bot_name(&self, run_id: &str, bot_name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_runs SET default_bot_name = :bot_name WHERE id = :id",
            named_params![":bot_name": bot_name, ":id": run_id],
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
            "UPDATE workflow_runs SET inputs = :inputs_json WHERE id = :id",
            named_params![":inputs_json": inputs_json, ":id": run_id],
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
        error: Option<&str>,
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
            "UPDATE workflow_runs SET status = :status, result_summary = :result_summary, \
             ended_at = :ended_at, blocked_on = NULL, error = :error WHERE id = :id",
            named_params![
                ":status": status,
                ":result_summary": result_summary,
                ":ended_at": ended_at,
                ":error": error,
                ":id": workflow_run_id,
            ],
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
            "UPDATE workflow_runs SET status = :status, blocked_on = :blocked_on WHERE id = :id",
            named_params![":status": WorkflowRunStatus::Waiting, ":blocked_on": json, ":id": workflow_run_id],
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
                    let subprocess_pid = agent_mgr
                        .get_run(child_id)
                        .ok()
                        .flatten()
                        .and_then(|r| r.subprocess_pid);
                    if let Err(e) = agent_mgr.cancel_run(child_id, subprocess_pid) {
                        tracing::warn!(
                            step_id = %step.id,
                            child_run_id = %child_id,
                            "Failed to cancel child agent run during workflow cancellation: {e}"
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

        // Recursively cancel child workflow runs (sub-workflows spawned by call_workflow steps)
        if let Ok(children) = self.list_child_workflow_runs(run_id) {
            for child in children {
                if matches!(
                    child.status,
                    WorkflowRunStatus::Running
                        | WorkflowRunStatus::Pending
                        | WorkflowRunStatus::Waiting
                ) {
                    if let Err(e) = self.cancel_run(&child.id, reason) {
                        tracing::warn!(
                            child_run_id = %child.id,
                            "Failed to cancel child workflow run during parent cancellation: {e}"
                        );
                    }
                }
            }
        }

        self.update_workflow_status(run_id, WorkflowRunStatus::Cancelled, Some(reason), None)
    }

    /// Persist aggregated metrics for a completed (or failed) workflow run.
    ///
    /// Called after the terminal status transition so metrics are recorded even when
    /// the run fails.  Uses a separate UPDATE to avoid touching the existing
    /// `update_workflow_status` signature (which is called in many test fixtures).
    #[allow(clippy::too_many_arguments)]
    pub fn persist_workflow_metrics(
        &self,
        workflow_run_id: &str,
        total_input_tokens: i64,
        total_output_tokens: i64,
        total_cache_read_input_tokens: i64,
        total_cache_creation_input_tokens: i64,
        total_turns: i64,
        total_cost_usd: f64,
        total_duration_ms: i64,
        model: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_runs SET \
             total_input_tokens = :total_input_tokens, \
             total_output_tokens = :total_output_tokens, \
             total_cache_read_input_tokens = :total_cache_read_input_tokens, \
             total_cache_creation_input_tokens = :total_cache_creation_input_tokens, \
             total_turns = :total_turns, \
             total_cost_usd = :total_cost_usd, \
             total_duration_ms = :total_duration_ms, \
             model = :model \
             WHERE id = :id",
            named_params![
                ":total_input_tokens": total_input_tokens,
                ":total_output_tokens": total_output_tokens,
                ":total_cache_read_input_tokens": total_cache_read_input_tokens,
                ":total_cache_creation_input_tokens": total_cache_creation_input_tokens,
                ":total_turns": total_turns,
                ":total_cost_usd": total_cost_usd,
                ":total_duration_ms": total_duration_ms,
                ":model": model,
                ":id": workflow_run_id,
            ],
        )?;
        Ok(())
    }

    /// Update the `last_heartbeat` timestamp for a workflow run.
    ///
    /// Called by the engine every ~5s to signal that the executor process is
    /// still alive.  The `AND status = 'running'` guard prevents writing a
    /// heartbeat after a watchdog has already flipped the run to `failed`.
    pub fn tick_heartbeat(&self, run_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_runs SET last_heartbeat = :now \
             WHERE id = :id AND status = 'running'",
            named_params![":now": now, ":id": run_id],
        )?;
        Ok(())
    }

    /// Mark a workflow run as failed and also fail its parent agent run.
    ///
    /// Marks a workflow run as failed.
    ///
    /// This is used by the background executor when the workflow thread crashes
    /// after the run ID has already been returned to the caller.
    ///
    /// Returns the parent agent run ID so callers can handle updating it
    /// separately to avoid cross-manager coupling.
    pub fn fail_workflow_run(&self, workflow_run_id: &str, error_msg: &str) -> Result<String> {
        self.update_workflow_status(
            workflow_run_id,
            WorkflowRunStatus::Failed,
            Some(error_msg),
            Some(error_msg),
        )?;
        if let Ok(Some(run)) = self.get_workflow_run(workflow_run_id) {
            Ok(run.parent_run_id)
        } else {
            Err(ConductorError::InvalidInput(format!(
                "Workflow run not found: {workflow_run_id}"
            )))
        }
    }
}
