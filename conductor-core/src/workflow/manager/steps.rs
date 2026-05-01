use std::collections::HashSet;

use chrono::Utc;
use rusqlite::named_params;

use crate::error::{ConductorError, Result};
use crate::workflow::GateType;

use super::WorkflowManager;
use crate::workflow::WorkflowStepStatus;

/// Agent-run metrics to mirror onto a linked `workflow_run_steps` row (Path X.1).
///
/// Grouped to avoid a positional `Option<T>` explosion at call sites.
/// Fields left as `None` are ignored (COALESCE leaves existing DB values intact).
#[derive(Debug, Clone, Default)]
pub struct StepMetrics {
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
}

impl<'a> WorkflowManager<'a> {
    /// Execute a single SQL UPDATE and map the rusqlite error to [`ConductorError`].
    ///
    /// All `mark_step_*` and `set_step_*` methods share this one-liner to avoid
    /// repeating the `execute(...)?; Ok(())` pattern.
    fn execute_step_sql(&self, sql: &str, params: impl rusqlite::Params) -> Result<()> {
        self.conn.execute(sql, params)?;
        Ok(())
    }

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
             VALUES (:id, :workflow_run_id, :step_name, :role, :can_commit, :status, :position, :iteration)",
            named_params![
                ":id": id,
                ":workflow_run_id": workflow_run_id,
                ":step_name": step_name,
                ":role": role,
                ":can_commit": can_commit as i64,
                ":status": "pending",
                ":position": position,
                ":iteration": iteration,
            ],
        )?;
        Ok(id)
    }

    /// Insert a workflow step record already in `running` state.
    ///
    /// Combines what would otherwise be two separate calls (`insert_step` +
    /// `update_step_status(Running)`) into a single atomic `INSERT`, eliminating
    /// the window where a crash between the two statements leaves a row stuck in
    /// `pending` forever.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_step_running(
        &self,
        workflow_run_id: &str,
        step_name: &str,
        role: &str,
        can_commit: bool,
        position: i64,
        iteration: i64,
        retry_count: i64,
    ) -> Result<String> {
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, can_commit, status, position, iteration, \
              started_at, retry_count) \
             VALUES (:id, :workflow_run_id, :step_name, :role, :can_commit, 'running', :position, :iteration, :started_at, :retry_count)",
            named_params![
                ":id": id,
                ":workflow_run_id": workflow_run_id,
                ":step_name": step_name,
                ":role": role,
                ":can_commit": can_commit as i64,
                ":position": position,
                ":iteration": iteration,
                ":started_at": now,
                ":retry_count": retry_count,
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

    /// Mark a step as starting (Running or Waiting), recording `started_at` and the child run.
    pub fn mark_step_running(
        &self,
        step_id: &str,
        status: WorkflowStepStatus,
        child_run_id: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.execute_step_sql(
            "UPDATE workflow_run_steps SET status = :status, child_run_id = :child_run_id, \
             started_at = :started_at WHERE id = :id",
            named_params![":status": status, ":child_run_id": child_run_id, ":started_at": now, ":id": step_id],
        )
    }

    /// Mark a step as terminal (Completed, Failed, Skipped, TimedOut), recording all output fields.
    #[allow(clippy::too_many_arguments)]
    pub fn mark_step_terminal(
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
        self.execute_step_sql(
            "UPDATE workflow_run_steps SET status = :status, \
             child_run_id = COALESCE(:child_run_id, child_run_id), \
             ended_at = :ended_at, result_text = :result_text, context_out = :context_out, \
             markers_out = :markers_out, \
             retry_count = COALESCE(:retry_count, retry_count), \
             structured_output = :structured_output, step_error = :step_error \
             WHERE id = :id",
            named_params![
                ":status": status,
                ":child_run_id": child_run_id,
                ":ended_at": now,
                ":result_text": result_text,
                ":context_out": context_out,
                ":markers_out": markers_out,
                ":retry_count": retry_count,
                ":structured_output": structured_output,
                ":step_error": step_error,
                ":id": step_id,
            ],
        )
    }

    /// Mark a step with a non-starting, non-terminal status (e.g. Pending), updating only `status`.
    pub fn mark_step_pending(&self, step_id: &str, status: WorkflowStepStatus) -> Result<()> {
        self.execute_step_sql(
            "UPDATE workflow_run_steps SET status = :status WHERE id = :id",
            named_params![":status": status, ":id": step_id],
        )
    }

    /// Update a step's status with all fields including structured_output and step_error.
    ///
    /// Dispatches to [`mark_step_running`], [`mark_step_terminal`], or [`mark_step_pending`]
    /// based on the status. Prefer calling those named methods directly when the caller knows
    /// which branch applies.
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
        let is_starting = status.is_starting();
        let is_terminal = status.is_terminal();

        if is_starting {
            self.mark_step_running(step_id, status, child_run_id)
        } else if is_terminal {
            self.mark_step_terminal(
                step_id,
                status,
                child_run_id,
                result_text,
                context_out,
                markers_out,
                retry_count,
                structured_output,
                step_error,
            )
        } else {
            self.mark_step_pending(step_id, status)
        }
    }

    /// Write the child run ID back to a parent step immediately after the child run is created.
    pub fn update_step_child_run_id(&self, step_id: &str, child_run_id: &str) -> Result<()> {
        self.execute_step_sql(
            "UPDATE workflow_run_steps SET child_run_id = :child_run_id WHERE id = :id",
            named_params![":child_run_id": child_run_id, ":id": step_id],
        )
    }

    /// Persist or clear the subprocess PID for a script step.
    ///
    /// Pass `Some(pid)` after a successful `cmd.spawn()` to record the child PID.
    /// Pass `None` after the step reaches any terminal state to clear the PID
    /// and prevent OS PID reuse from tripping the orphan reaper.
    pub fn set_step_subprocess_pid(&self, step_id: &str, pid: Option<u32>) -> Result<()> {
        self.execute_step_sql(
            "UPDATE workflow_run_steps SET subprocess_pid = :pid WHERE id = :id",
            named_params![":pid": pid.map(|p| p as i64), ":id": step_id],
        )
    }

    /// Persist the stdout capture file path for a script step.
    pub fn set_step_output_file(&self, step_id: &str, output_file: &str) -> Result<()> {
        self.execute_step_sql(
            "UPDATE workflow_run_steps SET output_file = :output_file WHERE id = :id",
            named_params![":output_file": output_file, ":id": step_id],
        )
    }

    /// Update gate-specific columns on a step.
    pub fn set_step_gate_info(
        &self,
        step_id: &str,
        gate_type: GateType,
        gate_prompt: Option<&str>,
        gate_timeout: &str,
    ) -> Result<()> {
        self.execute_step_sql(
            "UPDATE workflow_run_steps SET gate_type = :gate_type, gate_prompt = :gate_prompt, \
             gate_timeout = :gate_timeout WHERE id = :id",
            named_params![":gate_type": gate_type.to_string(), ":gate_prompt": gate_prompt, ":gate_timeout": gate_timeout, ":id": step_id],
        )
    }

    /// Set parallel_group_id on a step.
    pub fn set_step_parallel_group(&self, step_id: &str, group_id: &str) -> Result<()> {
        self.execute_step_sql(
            "UPDATE workflow_run_steps SET parallel_group_id = :group_id WHERE id = :id",
            named_params![":group_id": group_id, ":id": step_id],
        )
    }

    /// Store the resolved gate options JSON on a step (called at gate start).
    pub fn set_step_gate_options(&self, step_id: &str, options_json: &str) -> Result<()> {
        self.execute_step_sql(
            "UPDATE workflow_run_steps SET gate_options = :options_json WHERE id = :id",
            named_params![":options_json": options_json, ":id": step_id],
        )
    }

    /// Mirror agent-run metrics onto the linked workflow step row (Path X.1).
    ///
    /// Keyed by `child_run_id` so callers need not know the step's own id.
    /// Uses COALESCE so a `None` field in [`StepMetrics`] leaves the existing column untouched.
    /// Called by `AgentManager` after writing the same values to `agent_runs`,
    /// keeping `workflow_run_steps` readable without a JOIN.
    pub fn mirror_step_metrics_from_run(&self, run_id: &str, metrics: StepMetrics) -> Result<()> {
        self.execute_step_sql(
            "UPDATE workflow_run_steps \
             SET cost_usd = COALESCE(:cost_usd, cost_usd), \
                 num_turns = COALESCE(:num_turns, num_turns), \
                 duration_ms = COALESCE(:duration_ms, duration_ms), \
                 input_tokens = COALESCE(:input_tokens, input_tokens), \
                 output_tokens = COALESCE(:output_tokens, output_tokens), \
                 cache_read_input_tokens = COALESCE(:cache_read_input_tokens, cache_read_input_tokens), \
                 cache_creation_input_tokens = COALESCE(:cache_creation_input_tokens, cache_creation_input_tokens) \
             WHERE child_run_id = :run_id",
            named_params![
                ":cost_usd": metrics.cost_usd,
                ":num_turns": metrics.num_turns,
                ":duration_ms": metrics.duration_ms,
                ":input_tokens": metrics.input_tokens,
                ":output_tokens": metrics.output_tokens,
                ":cache_read_input_tokens": metrics.cache_read_input_tokens,
                ":cache_creation_input_tokens": metrics.cache_creation_input_tokens,
                ":run_id": run_id,
            ],
        )
    }

    /// Approve a gate: set gate_approved_at, gate_approved_by, optional feedback, and optional selections.
    pub fn approve_gate(
        &self,
        step_id: &str,
        approved_by: &str,
        feedback: Option<&str>,
        selections: Option<&[String]>,
        context_out: Option<String>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        // Validate selections against stored gate options if provided
        if let Some(selections) = selections {
            self.validate_gate_selections(step_id, selections)?;
        }

        let selections_json = crate::workflow::helpers::serialize_gate_selections(selections)?;

        self.execute_step_sql(
            "UPDATE workflow_run_steps SET gate_approved_at = :now, gate_approved_by = :approved_by, \
             gate_feedback = :feedback, gate_selections = :selections_json, \
             context_out = COALESCE(:context_out, context_out), \
             status = 'completed', ended_at = :now WHERE id = :id",
            named_params![
                ":now": now,
                ":approved_by": approved_by,
                ":feedback": feedback,
                ":selections_json": selections_json,
                ":context_out": context_out,
                ":id": step_id,
            ],
        )
    }

    /// Reject a gate: set step to failed.
    pub fn reject_gate(
        &self,
        step_id: &str,
        rejected_by: &str,
        feedback: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.execute_step_sql(
            "UPDATE workflow_run_steps SET gate_approved_by = :rejected_by, gate_feedback = :feedback, \
             status = 'failed', ended_at = :ended_at WHERE id = :id",
            named_params![":rejected_by": rejected_by, ":feedback": feedback, ":ended_at": now, ":id": step_id],
        )
    }

    /// Returns true if the predecessor step (position - 1) has status 'completed'.
    /// Always returns true when position == 0 (no predecessor).
    pub fn predecessor_completed(&self, workflow_run_id: &str, position: i64) -> Result<bool> {
        if position == 0 {
            return Ok(true);
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT 1 FROM workflow_run_steps \
             WHERE workflow_run_id = :wrid AND position = :pos \
             AND status = 'completed' LIMIT 1",
        )?;
        let exists = stmt
            .exists(named_params![
                ":wrid": workflow_run_id,
                ":pos": position - 1,
            ])
            .map_err(ConductorError::Database)?;
        Ok(exists)
    }

    /// Returns true if a step row that should block re-insertion already exists
    /// at the given position/iteration/step_name combination: statuses
    /// `pending`, `running`, `waiting`, and `completed` all return true.
    /// `failed`, `skipped`, and `timed_out` return false so retries are permitted.
    /// Including step_name ensures parallel steps at the same position
    /// (different names) are not blocked.
    pub fn active_step_exists(
        &self,
        workflow_run_id: &str,
        position: i64,
        iteration: i64,
        step_name: &str,
    ) -> Result<bool> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT 1 FROM workflow_run_steps \
             WHERE workflow_run_id = :wrid AND position = :pos AND iteration = :iter \
             AND step_name = :name \
             AND status IN ('pending', 'running', 'waiting', 'completed') LIMIT 1",
        )?;
        let exists = stmt
            .exists(named_params![
                ":wrid": workflow_run_id,
                ":pos": position,
                ":iter": iteration,
                ":name": step_name,
            ])
            .map_err(ConductorError::Database)?;
        Ok(exists)
    }

    /// Query a step's gate approval state from the DB.
    pub fn get_gate_approval_state(
        &self,
        step_id: &str,
    ) -> Result<runkon_flow::traits::persistence::GateApprovalState> {
        use rusqlite::OptionalExtension;
        #[allow(clippy::type_complexity)]
        let row: Option<(Option<String>, String, Option<String>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT gate_approved_at, status, gate_feedback, gate_selections \
                 FROM workflow_run_steps WHERE id = ?1",
                rusqlite::params![step_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(ConductorError::Database)?;

        let Some((approved_at, status_str, feedback, selections_json)) = row else {
            return Ok(runkon_flow::traits::persistence::GateApprovalState::Pending);
        };

        let status = status_str
            .parse::<WorkflowStepStatus>()
            .unwrap_or_else(|_| {
                tracing::warn!(
                    step_id = %step_id,
                    status = %status_str,
                    "get_gate_approval_state: unrecognised step status; treating as Waiting",
                );
                WorkflowStepStatus::Waiting
            });
        let selections = selections_json.and_then(|json| {
            serde_json::from_str::<Vec<String>>(&json)
                .map_err(|e| {
                    tracing::warn!(
                        step_id = %step_id,
                        "get_gate_approval_state: failed to deserialize gate_selections: {e}",
                    );
                })
                .ok()
        });

        Ok(
            runkon_flow::traits::persistence::gate_approval_state_from_fields(
                approved_at.as_deref(),
                status,
                feedback,
                selections,
            ),
        )
    }

    /// Validate that gate selections are within the allowed options for this step.
    fn validate_gate_selections(&self, step_id: &str, selections: &[String]) -> Result<()> {
        // Get the stored gate options for this step
        let mut stmt = self
            .conn
            .prepare_cached("SELECT gate_options FROM workflow_run_steps WHERE id = :id")?;
        let gate_options: Option<String> = stmt
            .query_row(named_params![":id": step_id], |row| {
                row.get::<_, Option<String>>("gate_options")
            })
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
        let allowed_set: HashSet<String> = allowed_options
            .iter()
            .filter_map(|opt: &serde_json::Value| {
                opt.get("value")
                    .and_then(|v: &serde_json::Value| v.as_str().map(|s: &str| s.to_string()))
            })
            .collect();

        if allowed_set.is_empty() {
            return Err(ConductorError::InvalidInput(
                "No valid options found in gate configuration".to_string(),
            ));
        }

        // Validate that all selections are in the allowed values
        for selection in selections {
            if !allowed_set.contains(selection.as_str()) {
                let mut sorted: Vec<&str> = allowed_set.iter().map(|s| s.as_str()).collect();
                sorted.sort_unstable();
                return Err(ConductorError::InvalidInput(format!(
                    "Invalid gate selection '{}' - not in allowed options: [{}]",
                    selection,
                    sorted.join(", ")
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;
    use crate::workflow::WorkflowStepStatus;

    fn setup(conn: &rusqlite::Connection) -> (String, String) {
        let parent_id = test_helpers::make_agent_parent_id(conn);
        let mgr = WorkflowManager::new(conn);
        let run = mgr
            .create_workflow_run("wf-test", Some("w1"), &parent_id, false, "manual", None)
            .unwrap();
        let step_id = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        (run.id, step_id)
    }

    #[test]
    fn mark_step_running_sets_status_and_child_run_id() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        let mgr = WorkflowManager::new(&conn);

        mgr.mark_step_running(&step_id, WorkflowStepStatus::Running, Some("child-run-1"))
            .unwrap();

        let step = crate::workflow::get_step_by_id(mgr.conn(), &step_id)
            .unwrap()
            .unwrap();
        assert_eq!(step.status, WorkflowStepStatus::Running);
        assert_eq!(step.child_run_id.as_deref(), Some("child-run-1"));
        assert!(step.started_at.is_some(), "started_at should be set");
    }

    #[test]
    fn mark_step_running_without_child_run_id() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        let mgr = WorkflowManager::new(&conn);

        mgr.mark_step_running(&step_id, WorkflowStepStatus::Waiting, None)
            .unwrap();

        let step = crate::workflow::get_step_by_id(mgr.conn(), &step_id)
            .unwrap()
            .unwrap();
        assert_eq!(step.status, WorkflowStepStatus::Waiting);
        assert!(step.child_run_id.is_none());
    }

    #[test]
    fn mark_step_terminal_sets_all_output_fields() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        let mgr = WorkflowManager::new(&conn);

        mgr.mark_step_terminal(
            &step_id,
            WorkflowStepStatus::Completed,
            Some("child-run-2"),
            Some("result text"),
            Some("ctx out"),
            Some("marker-a"),
            Some(1),
            Some(r#"{"ok":true}"#),
            None,
        )
        .unwrap();

        let step = crate::workflow::get_step_by_id(mgr.conn(), &step_id)
            .unwrap()
            .unwrap();
        assert_eq!(step.status, WorkflowStepStatus::Completed);
        assert_eq!(step.child_run_id.as_deref(), Some("child-run-2"));
        assert_eq!(step.result_text.as_deref(), Some("result text"));
        assert_eq!(step.context_out.as_deref(), Some("ctx out"));
        assert_eq!(step.markers_out.as_deref(), Some("marker-a"));
        assert_eq!(step.retry_count, 1);
        assert_eq!(step.structured_output.as_deref(), Some(r#"{"ok":true}"#));
        assert!(step.ended_at.is_some(), "ended_at should be set");
    }

    #[test]
    fn mark_step_pending_updates_status_only() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        let mgr = WorkflowManager::new(&conn);

        // Advance to Running first so we can observe the rollback to Pending.
        mgr.mark_step_running(&step_id, WorkflowStepStatus::Running, Some("child-x"))
            .unwrap();
        // Now reset to Pending — only status should change.
        mgr.mark_step_pending(&step_id, WorkflowStepStatus::Pending)
            .unwrap();

        let step = crate::workflow::get_step_by_id(mgr.conn(), &step_id)
            .unwrap()
            .unwrap();
        assert_eq!(step.status, WorkflowStepStatus::Pending);
        // started_at and child_run_id must be left as-is (mark_step_pending touches status only).
        assert!(step.started_at.is_some(), "started_at should be preserved");
        assert_eq!(
            step.child_run_id.as_deref(),
            Some("child-x"),
            "child_run_id should be preserved"
        );
    }

    #[test]
    fn mark_step_terminal_preserves_existing_child_run_id_when_none_passed() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        let mgr = WorkflowManager::new(&conn);

        mgr.mark_step_running(&step_id, WorkflowStepStatus::Running, Some("child-run-3"))
            .unwrap();
        mgr.mark_step_terminal(
            &step_id,
            WorkflowStepStatus::Failed,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("something went wrong"),
        )
        .unwrap();

        let step = crate::workflow::get_step_by_id(mgr.conn(), &step_id)
            .unwrap()
            .unwrap();
        assert_eq!(step.status, WorkflowStepStatus::Failed);
        // COALESCE in SQL keeps the existing child_run_id when None is passed.
        assert_eq!(step.child_run_id.as_deref(), Some("child-run-3"));
        assert_eq!(step.step_error.as_deref(), Some("something went wrong"));
    }

    #[test]
    fn mirror_step_metrics_writes_all_fields() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        let mgr = WorkflowManager::new(&conn);

        mgr.mark_step_running(&step_id, WorkflowStepStatus::Running, Some("run-mirror-1"))
            .unwrap();

        mgr.mirror_step_metrics_from_run(
            "run-mirror-1",
            StepMetrics {
                cost_usd: Some(1.23),
                num_turns: Some(5),
                duration_ms: Some(1000),
                input_tokens: Some(100),
                output_tokens: Some(200),
                cache_read_input_tokens: Some(50),
                cache_creation_input_tokens: Some(25),
            },
        )
        .unwrap();

        let step = crate::workflow::get_step_by_id(mgr.conn(), &step_id)
            .unwrap()
            .unwrap();
        assert_eq!(step.cost_usd, Some(1.23));
        assert_eq!(step.num_turns, Some(5));
        assert_eq!(step.duration_ms, Some(1000));
        assert_eq!(step.input_tokens, Some(100));
        assert_eq!(step.output_tokens, Some(200));
        assert_eq!(step.cache_read_input_tokens, Some(50));
        assert_eq!(step.cache_creation_input_tokens, Some(25));
    }

    #[test]
    fn mirror_step_metrics_coalesce_preserves_existing_when_none() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        let mgr = WorkflowManager::new(&conn);

        mgr.mark_step_running(&step_id, WorkflowStepStatus::Running, Some("run-mirror-2"))
            .unwrap();

        // Write full metrics first.
        mgr.mirror_step_metrics_from_run(
            "run-mirror-2",
            StepMetrics {
                cost_usd: Some(9.99),
                num_turns: Some(3),
                ..Default::default()
            },
        )
        .unwrap();

        // Second call — only token fields, cost_usd/num_turns must be preserved.
        mgr.mirror_step_metrics_from_run(
            "run-mirror-2",
            StepMetrics {
                input_tokens: Some(42),
                output_tokens: Some(84),
                ..Default::default()
            },
        )
        .unwrap();

        let step = crate::workflow::get_step_by_id(mgr.conn(), &step_id)
            .unwrap()
            .unwrap();
        assert_eq!(step.cost_usd, Some(9.99), "cost_usd must be preserved");
        assert_eq!(step.num_turns, Some(3), "num_turns must be preserved");
        assert_eq!(step.input_tokens, Some(42));
        assert_eq!(step.output_tokens, Some(84));
    }

    #[test]
    fn mirror_step_metrics_no_op_for_unknown_run_id() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        let mgr = WorkflowManager::new(&conn);

        // No step has child_run_id = "does-not-exist", so the UPDATE affects 0 rows.
        // The method must succeed (not error) in this case.
        mgr.mirror_step_metrics_from_run(
            "does-not-exist",
            StepMetrics {
                cost_usd: Some(1.0),
                ..Default::default()
            },
        )
        .unwrap();

        // Original step untouched.
        let step = crate::workflow::get_step_by_id(mgr.conn(), &step_id)
            .unwrap()
            .unwrap();
        assert!(step.cost_usd.is_none());
    }
}
