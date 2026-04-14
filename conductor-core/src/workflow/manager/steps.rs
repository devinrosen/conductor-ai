use chrono::Utc;
use rusqlite::params;

use crate::error::{ConductorError, Result};
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
            None,
        )
    }

    /// Update a step's status with all fields including structured_output and step_error.
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
        step_error: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let is_starting =
            status == WorkflowStepStatus::Running || status == WorkflowStepStatus::Waiting;
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
                 retry_count = COALESCE(?7, retry_count), structured_output = ?8, step_error = ?9 \
                 WHERE id = ?10",
                params![
                    status,
                    child_run_id,
                    now,
                    result_text,
                    context_out,
                    markers_out,
                    retry_count,
                    structured_output,
                    step_error,
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

    /// Persist or clear the subprocess PID for a script step.
    ///
    /// Pass `Some(pid)` after a successful `cmd.spawn()` to record the child PID.
    /// Pass `None` after the step reaches any terminal state to clear the PID
    /// and prevent OS PID reuse from tripping the orphan reaper.
    pub fn set_step_subprocess_pid(&self, step_id: &str, pid: Option<u32>) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET subprocess_pid = ?1 WHERE id = ?2",
            params![pid.map(|p| p as i64), step_id],
        )?;
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

        // Validate selections against stored gate options if provided
        if let Some(selections) = selections {
            self.validate_gate_selections(step_id, selections)?;
        }

        let selections_json = if let Some(s) = selections {
            Some(serde_json::to_string(s).map_err(|e| {
                ConductorError::Workflow(format!("Failed to serialize gate selections: {}", e))
            })?)
        } else {
            None
        };

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
            params![
                now,
                approved_by,
                feedback,
                selections_json,
                context_out,
                step_id
            ],
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

    /// Validate that gate selections are within the allowed options for this step.
    fn validate_gate_selections(&self, step_id: &str, selections: &[String]) -> Result<()> {
        // Get the stored gate options for this step
        let mut stmt = self
            .conn
            .prepare("SELECT gate_options FROM workflow_run_steps WHERE id = ?1")?;
        let gate_options: Option<String> = stmt
            .query_row(params![step_id], |row| row.get::<_, Option<String>>(0))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ConductorError::InvalidInput(format!("Step not found: {}", step_id))
                }
                other => ConductorError::Database(other),
            })?;

        // If no options are stored, reject any selections
        let options_json = match gate_options {
            Some(json) => json,
            None => {
                if !selections.is_empty() {
                    return Err(ConductorError::InvalidInput(
                        "Gate selections provided but no options configured for this gate"
                            .to_string(),
                    ));
                }
                return Ok(());
            }
        };

        // Parse the stored options
        let allowed_options: Vec<serde_json::Value> =
            serde_json::from_str(&options_json).map_err(|e| {
                ConductorError::InvalidInput(format!(
                    "Invalid gate options JSON in database: {}",
                    e
                ))
            })?;

        // Extract allowed values from the options (assuming format [{"value": "...", "label": "..."}, ...])
        let allowed_values: Vec<String> = allowed_options
            .iter()
            .filter_map(|opt: &serde_json::Value| {
                opt.get("value")
                    .and_then(|v: &serde_json::Value| v.as_str().map(|s: &str| s.to_string()))
            })
            .collect();

        if allowed_values.is_empty() {
            return Err(ConductorError::InvalidInput(
                "No valid options found in gate configuration".to_string(),
            ));
        }

        // Validate that all selections are in the allowed values
        for selection in selections {
            if !allowed_values.contains(selection) {
                return Err(ConductorError::InvalidInput(format!(
                    "Invalid gate selection '{}' - not in allowed options: [{}]",
                    selection,
                    allowed_values.join(", ")
                )));
            }
        }

        Ok(())
    }
}
