use chrono::Utc;
use rusqlite::params;

use crate::error::Result;
use crate::workflow_dsl::GateType;

use super::WorkflowManager;
use crate::workflow::status::WorkflowStepStatus;

impl<'a> WorkflowManager<'a> {
    /// Insert a workflow step record.
    pub fn insert_step(
        &self,
        workflow_run_id: &str,
        step_name: &str,
        role: &str,
        can_commit: bool,
        position: i64,
        iteration: i64,
    ) -> Result<String> {
        let id = crate::new_id();
        self.conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, can_commit, status, position, iteration) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                workflow_run_id,
                step_name,
                role,
                can_commit as i64,
                "pending",
                position,
                iteration,
            ],
        )?;
        Ok(id)
    }

    /// Update a step's status and associated fields.
    #[allow(clippy::too_many_arguments)]
    pub fn update_step_status(
        &self,
        step_id: &str,
        status: WorkflowStepStatus,
        child_run_id: Option<&str>,
        result_text: Option<&str>,
        context_out: Option<&str>,
        markers_out: Option<&str>,
        retry_count: Option<i64>,
    ) -> Result<()> {
        self.update_step_status_full(
            step_id,
            status,
            child_run_id,
            result_text,
            context_out,
            markers_out,
            retry_count,
            None,
        )
    }

    /// Update a step's status with all fields including structured_output.
    #[allow(clippy::too_many_arguments)]
    pub fn update_step_status_full(
        &self,
        step_id: &str,
        status: WorkflowStepStatus,
        child_run_id: Option<&str>,
        result_text: Option<&str>,
        context_out: Option<&str>,
        markers_out: Option<&str>,
        retry_count: Option<i64>,
        structured_output: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let is_starting = status == WorkflowStepStatus::Running;
        let is_terminal = matches!(
            status,
            WorkflowStepStatus::Completed
                | WorkflowStepStatus::Failed
                | WorkflowStepStatus::Skipped
                | WorkflowStepStatus::TimedOut
        );

        if is_starting {
            self.conn.execute(
                "UPDATE workflow_run_steps SET status = ?1, child_run_id = ?2, started_at = ?3 \
                 WHERE id = ?4",
                params![status, child_run_id, now, step_id],
            )?;
        } else if is_terminal {
            self.conn.execute(
                "UPDATE workflow_run_steps SET status = ?1, child_run_id = ?2, ended_at = ?3, \
                 result_text = ?4, context_out = ?5, markers_out = ?6, \
                 retry_count = COALESCE(?7, retry_count), structured_output = ?8 \
                 WHERE id = ?9",
                params![
                    status,
                    child_run_id,
                    now,
                    result_text,
                    context_out,
                    markers_out,
                    retry_count,
                    structured_output,
                    step_id,
                ],
            )?;
        } else {
            self.conn.execute(
                "UPDATE workflow_run_steps SET status = ?1 WHERE id = ?2",
                params![status, step_id],
            )?;
        }
        Ok(())
    }

    /// Persist the stdout capture file path for a script step.
    pub fn set_step_output_file(&self, step_id: &str, output_file: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET output_file = ?1 WHERE id = ?2",
            params![output_file, step_id],
        )?;
        Ok(())
    }

    /// Update gate-specific columns on a step.
    pub fn set_step_gate_info(
        &self,
        step_id: &str,
        gate_type: GateType,
        gate_prompt: Option<&str>,
        gate_timeout: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET gate_type = ?1, gate_prompt = ?2, gate_timeout = ?3 \
             WHERE id = ?4",
            params![gate_type, gate_prompt, gate_timeout, step_id],
        )?;
        Ok(())
    }

    /// Set parallel_group_id on a step.
    pub fn set_step_parallel_group(&self, step_id: &str, group_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET parallel_group_id = ?1 WHERE id = ?2",
            params![group_id, step_id],
        )?;
        Ok(())
    }

    /// Store the resolved gate options JSON on a step (called at gate start).
    pub fn set_step_gate_options(&self, step_id: &str, options_json: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET gate_options = ?1 WHERE id = ?2",
            params![options_json, step_id],
        )?;
        Ok(())
    }

    /// Approve a gate: set gate_approved_at, gate_approved_by, optional feedback, and optional selections.
    pub fn approve_gate(
        &self,
        step_id: &str,
        approved_by: &str,
        feedback: Option<&str>,
        selections: Option<&[String]>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let selections_json = selections
            .map(|s| serde_json::to_string(s).unwrap_or_default());

        // Build a context_out snippet when selections are present.
        let context_out = selections.filter(|s| !s.is_empty()).map(|items| {
            let mut out = String::from("User selected the following items:\n");
            for item in items {
                out.push_str(&format!("- {item}\n"));
            }
            out
        });

        self.conn.execute(
            "UPDATE workflow_run_steps SET gate_approved_at = ?1, gate_approved_by = ?2, \
             gate_feedback = ?3, gate_selections = ?4, context_out = COALESCE(?5, context_out), \
             status = 'completed', ended_at = ?1 WHERE id = ?6",
            params![now, approved_by, feedback, selections_json, context_out, step_id],
        )?;
        Ok(())
    }

    /// Reject a gate: set step to failed.
    pub fn reject_gate(
        &self,
        step_id: &str,
        rejected_by: &str,
        feedback: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_run_steps SET gate_approved_by = ?1, gate_feedback = ?2, status = 'failed', ended_at = ?3 \
             WHERE id = ?4",
            params![rejected_by, feedback, now, step_id],
        )?;
        Ok(())
    }
}
