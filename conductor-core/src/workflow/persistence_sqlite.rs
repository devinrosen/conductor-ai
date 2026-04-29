use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use rusqlite::{named_params, Connection, OptionalExtension};

#[cfg(test)]
use crate::error::ConductorError;
use crate::workflow::constants::{RUN_COLUMNS, STEP_COLUMNS_WITH_PREFIX};
use crate::workflow::engine_error::EngineError;
use crate::workflow::helpers::{format_gate_selection_context, serialize_gate_selections};
use crate::workflow::manager::{row_to_workflow_run, row_to_workflow_step};
use crate::workflow::WorkflowRunStatus;
use crate::workflow::WorkflowStepStatus;
use crate::workflow::{WorkflowRun, WorkflowRunStep};

use runkon_flow::traits::persistence::{
    gate_approval_state_from_fields, FanOutItemRow, FanOutItemStatus, FanOutItemUpdate,
    GateApprovalState, NewRun, NewStep, StepUpdate, WorkflowPersistence,
};
use runkon_flow::types::extract_workflow_title;

/// SQLite-backed implementation of `WorkflowPersistence`.
///
/// Wraps a `rusqlite::Connection` behind `Arc<Mutex<_>>` so it satisfies the
/// `Send + Sync` requirement of `WorkflowPersistence`. Each method acquires the
/// lock and runs its own SQL directly against the connection — no
/// `WorkflowManager` delegation. Phase 4 step 4.2: this prepares the file for
/// relocation into runkon-flow (step 4.3) by severing all conductor-core
/// manager-layer dependencies on the read/write path.
pub struct SqliteWorkflowPersistence {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteWorkflowPersistence {
    /// Open a new SQLite connection at `path`, configured for WAL mode and
    /// foreign key enforcement. Creates the file if it does not already exist.
    #[cfg(test)]
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

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, EngineError> {
        self.conn
            .lock()
            .map_err(|_| EngineError::Persistence("SqliteWorkflowPersistence: mutex poisoned".into()))
    }
}

fn db_err(e: rusqlite::Error) -> EngineError {
    EngineError::Persistence(e.to_string())
}

impl WorkflowPersistence for SqliteWorkflowPersistence {
    fn create_run(&self, new_run: NewRun) -> Result<WorkflowRun, EngineError> {
        let conn = self.lock()?;
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, worktree_id, ticket_id, repo_id, \
             parent_run_id, status, dry_run, trigger, started_at, definition_snapshot, \
             parent_workflow_run_id, target_label) \
             VALUES (:id, :workflow_name, :worktree_id, :ticket_id, :repo_id, :parent_run_id, \
             :status, :dry_run, :trigger, :started_at, :definition_snapshot, \
             :parent_workflow_run_id, :target_label)",
            named_params![
                ":id": id,
                ":workflow_name": new_run.workflow_name,
                ":worktree_id": new_run.worktree_id,
                ":ticket_id": new_run.ticket_id,
                ":repo_id": new_run.repo_id,
                ":parent_run_id": new_run.parent_run_id,
                ":status": "pending",
                ":dry_run": new_run.dry_run as i64,
                ":trigger": new_run.trigger,
                ":started_at": now,
                ":definition_snapshot": new_run.definition_snapshot,
                ":parent_workflow_run_id": new_run.parent_workflow_run_id,
                ":target_label": new_run.target_label,
            ],
        )
        .map_err(db_err)?;

        let workflow_title = extract_workflow_title(new_run.definition_snapshot.as_deref());
        Ok(WorkflowRun {
            id,
            workflow_name: new_run.workflow_name,
            worktree_id: new_run.worktree_id,
            parent_run_id: new_run.parent_run_id,
            status: WorkflowRunStatus::Pending,
            dry_run: new_run.dry_run,
            trigger: new_run.trigger,
            started_at: now,
            ended_at: None,
            result_summary: None,
            error: None,
            definition_snapshot: new_run.definition_snapshot,
            inputs: HashMap::new(),
            ticket_id: new_run.ticket_id,
            repo_id: new_run.repo_id,
            parent_workflow_run_id: new_run.parent_workflow_run_id,
            target_label: new_run.target_label,
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
            dismissed: false,
        })
    }

    fn get_run(&self, run_id: &str) -> Result<Option<WorkflowRun>, EngineError> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id = :id"),
            named_params! { ":id": run_id },
            row_to_workflow_run,
        )
        .optional()
        .map_err(db_err)
    }

    fn list_active_runs(
        &self,
        statuses: &[WorkflowRunStatus],
    ) -> Result<Vec<WorkflowRun>, EngineError> {
        let conn = self.lock()?;
        let effective: &[WorkflowRunStatus] = if statuses.is_empty() {
            &WorkflowRunStatus::ACTIVE
        } else {
            statuses
        };
        let placeholders = (1..=effective.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT workflow_runs.* \
             FROM workflow_runs \
             LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
             WHERE (workflow_runs.worktree_id IS NULL OR worktrees.status = 'active') \
               AND workflow_runs.status IN ({placeholders}) \
             ORDER BY workflow_runs.started_at DESC \
             LIMIT 500"
        );
        let status_strings: Vec<String> = effective.iter().map(|s| s.to_string()).collect();
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(status_strings.iter()),
                row_to_workflow_run,
            )
            .map_err(db_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)
    }

    fn update_run_status(
        &self,
        run_id: &str,
        status: WorkflowRunStatus,
        result_summary: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), EngineError> {
        if matches!(status, WorkflowRunStatus::Waiting) {
            return Err(EngineError::Persistence(
                "Use set_waiting_blocked_on() to transition a workflow run to Waiting status"
                    .into(),
            ));
        }
        let conn = self.lock()?;
        let now = Utc::now().to_rfc3339();
        let is_terminal = matches!(
            status,
            WorkflowRunStatus::Completed | WorkflowRunStatus::Failed | WorkflowRunStatus::Cancelled
        );
        let ended_at = if is_terminal { Some(now.as_str()) } else { None };

        // Always clear blocked_on — the only path into Waiting (which sets it)
        // is set_waiting_blocked_on() on WorkflowManager.
        conn.execute(
            "UPDATE workflow_runs SET status = :status, result_summary = :result_summary, \
             ended_at = :ended_at, blocked_on = NULL, error = :error WHERE id = :id",
            named_params![
                ":status": status,
                ":result_summary": result_summary,
                ":ended_at": ended_at,
                ":error": error,
                ":id": run_id,
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    fn insert_step(&self, new_step: NewStep) -> Result<String, EngineError> {
        let conn = self.lock()?;
        let id = crate::new_id();
        if let Some(retry_count) = new_step.retry_count {
            // insert directly in `running` status so resumed workflows do not
            // leave a row stuck in `pending` if a crash falls between insert
            // and the follow-up status update.
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO workflow_run_steps \
                 (id, workflow_run_id, step_name, role, can_commit, status, position, iteration, \
                  started_at, retry_count) \
                 VALUES (:id, :workflow_run_id, :step_name, :role, :can_commit, 'running', :position, :iteration, :started_at, :retry_count)",
                named_params![
                    ":id": id,
                    ":workflow_run_id": new_step.workflow_run_id,
                    ":step_name": new_step.step_name,
                    ":role": new_step.role,
                    ":can_commit": new_step.can_commit as i64,
                    ":position": new_step.position,
                    ":iteration": new_step.iteration,
                    ":started_at": now,
                    ":retry_count": retry_count,
                ],
            )
            .map_err(db_err)?;
        } else {
            conn.execute(
                "INSERT INTO workflow_run_steps \
                 (id, workflow_run_id, step_name, role, can_commit, status, position, iteration) \
                 VALUES (:id, :workflow_run_id, :step_name, :role, :can_commit, :status, :position, :iteration)",
                named_params![
                    ":id": id,
                    ":workflow_run_id": new_step.workflow_run_id,
                    ":step_name": new_step.step_name,
                    ":role": new_step.role,
                    ":can_commit": new_step.can_commit as i64,
                    ":status": "pending",
                    ":position": new_step.position,
                    ":iteration": new_step.iteration,
                ],
            )
            .map_err(db_err)?;
        }
        Ok(id)
    }

    fn update_step(&self, step_id: &str, update: StepUpdate) -> Result<(), EngineError> {
        let conn = self.lock()?;
        if update.status.is_starting() {
            // mark_step_running: status + child_run_id + started_at
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE workflow_run_steps SET status = :status, child_run_id = :child_run_id, \
                 started_at = :started_at WHERE id = :id",
                named_params![
                    ":status": update.status,
                    ":child_run_id": update.child_run_id,
                    ":started_at": now,
                    ":id": step_id,
                ],
            )
            .map_err(db_err)?;
        } else if update.status.is_terminal() {
            // mark_step_terminal: full output capture + ended_at, COALESCE child_run_id/retry_count
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE workflow_run_steps SET status = :status, \
                 child_run_id = COALESCE(:child_run_id, child_run_id), \
                 ended_at = :ended_at, result_text = :result_text, context_out = :context_out, \
                 markers_out = :markers_out, \
                 retry_count = COALESCE(:retry_count, retry_count), \
                 structured_output = :structured_output, step_error = :step_error \
                 WHERE id = :id",
                named_params![
                    ":status": update.status,
                    ":child_run_id": update.child_run_id,
                    ":ended_at": now,
                    ":result_text": update.result_text,
                    ":context_out": update.context_out,
                    ":markers_out": update.markers_out,
                    ":retry_count": update.retry_count,
                    ":structured_output": update.structured_output,
                    ":step_error": update.step_error,
                    ":id": step_id,
                ],
            )
            .map_err(db_err)?;
        } else {
            // mark_step_pending: status only — leave started_at / child_run_id intact.
            conn.execute(
                "UPDATE workflow_run_steps SET status = :status WHERE id = :id",
                named_params![":status": update.status, ":id": step_id],
            )
            .map_err(db_err)?;
        }
        Ok(())
    }

    fn get_steps(&self, run_id: &str) -> Result<Vec<WorkflowRunStep>, EngineError> {
        let conn = self.lock()?;
        let sql = format!(
            "SELECT {cols} FROM workflow_run_steps s \
             WHERE s.workflow_run_id = :workflow_run_id \
             ORDER BY s.position",
            cols = &*STEP_COLUMNS_WITH_PREFIX,
        );
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(
                named_params! { ":workflow_run_id": run_id },
                row_to_workflow_step,
            )
            .map_err(db_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)
    }

    fn insert_fan_out_item(
        &self,
        step_run_id: &str,
        item_type: &str,
        item_id: &str,
        item_ref: &str,
    ) -> Result<String, EngineError> {
        let conn = self.lock()?;
        let id = crate::new_id();
        conn.execute(
            "INSERT OR IGNORE INTO workflow_run_step_fan_out_items \
             (id, step_run_id, item_type, item_id, item_ref, status) \
             VALUES (:id, :step_run_id, :item_type, :item_id, :item_ref, 'pending')",
            named_params![
                ":id": id,
                ":step_run_id": step_run_id,
                ":item_type": item_type,
                ":item_id": item_id,
                ":item_ref": item_ref,
            ],
        )
        .map_err(db_err)?;
        Ok(id)
    }

    fn update_fan_out_item(
        &self,
        item_id: &str,
        update: FanOutItemUpdate,
    ) -> Result<(), EngineError> {
        let conn = self.lock()?;
        let now = Utc::now().to_rfc3339();
        match update {
            FanOutItemUpdate::Running { child_run_id } => {
                conn.execute(
                    "UPDATE workflow_run_step_fan_out_items \
                     SET status = 'running', child_run_id = :child_run_id, dispatched_at = :now \
                     WHERE id = :id",
                    named_params![
                        ":child_run_id": child_run_id,
                        ":now": now,
                        ":id": item_id,
                    ],
                )
                .map_err(db_err)?;
            }
            FanOutItemUpdate::Terminal { status } => {
                conn.execute(
                    "UPDATE workflow_run_step_fan_out_items \
                     SET status = :status, completed_at = :now \
                     WHERE id = :id",
                    named_params![
                        ":status": status.as_str(),
                        ":now": now,
                        ":id": item_id,
                    ],
                )
                .map_err(db_err)?;
            }
        }
        Ok(())
    }

    fn get_fan_out_items(
        &self,
        step_run_id: &str,
        status_filter: Option<FanOutItemStatus>,
    ) -> Result<Vec<FanOutItemRow>, EngineError> {
        let conn = self.lock()?;
        let select = "SELECT id, step_run_id, item_type, item_id, item_ref, child_run_id, \
                      status, dispatched_at, completed_at \
                      FROM workflow_run_step_fan_out_items";
        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<FanOutItemRow> {
            Ok(FanOutItemRow {
                id: row.get("id")?,
                step_run_id: row.get("step_run_id")?,
                item_type: row.get("item_type")?,
                item_id: row.get("item_id")?,
                item_ref: row.get("item_ref")?,
                child_run_id: row.get("child_run_id")?,
                status: row.get("status")?,
                dispatched_at: row.get("dispatched_at")?,
                completed_at: row.get("completed_at")?,
            })
        };
        if let Some(status) = status_filter {
            let sql = format!(
                "{select} WHERE step_run_id = :step_run_id AND status = :status ORDER BY id ASC"
            );
            let mut stmt = conn.prepare(&sql).map_err(db_err)?;
            let rows = stmt
                .query_map(
                    named_params![":step_run_id": step_run_id, ":status": status.as_str()],
                    map_row,
                )
                .map_err(db_err)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(db_err)
        } else {
            let sql =
                format!("{select} WHERE step_run_id = :step_run_id ORDER BY id ASC");
            let mut stmt = conn.prepare(&sql).map_err(db_err)?;
            let rows = stmt
                .query_map(named_params![":step_run_id": step_run_id], map_row)
                .map_err(db_err)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(db_err)
        }
    }

    fn get_gate_approval(&self, step_id: &str) -> Result<GateApprovalState, EngineError> {
        let conn = self.lock()?;
        #[allow(clippy::type_complexity)]
        let row: Option<(Option<String>, String, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT gate_approved_at, status, gate_feedback, gate_selections \
                 FROM workflow_run_steps WHERE id = ?1",
                rusqlite::params![step_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(db_err)?;

        let Some((approved_at, status_str, feedback, selections_json)) = row else {
            return Ok(GateApprovalState::Pending);
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
        let context_out = selections
            .filter(|s| !s.is_empty())
            .map(format_gate_selection_context);

        let conn = self.lock()?;
        if let Some(sels) = selections {
            validate_gate_selections(&conn, step_id, sels)?;
        }
        let selections_json = serialize_gate_selections(selections)
            .map_err(|e| EngineError::Persistence(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
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
        .map_err(db_err)?;
        Ok(())
    }

    fn reject_gate(
        &self,
        step_id: &str,
        rejected_by: &str,
        feedback: Option<&str>,
    ) -> Result<(), EngineError> {
        let conn = self.lock()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE workflow_run_steps SET gate_approved_by = :rejected_by, gate_feedback = :feedback, \
             status = 'failed', ended_at = :ended_at WHERE id = :id",
            named_params![
                ":rejected_by": rejected_by,
                ":feedback": feedback,
                ":ended_at": now,
                ":id": step_id,
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    fn is_run_cancelled(&self, run_id: &str) -> Result<bool, EngineError> {
        let conn = self.lock()?;
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        Ok(matches!(
            status.as_deref(),
            Some("cancelled") | Some("cancelling")
        ))
    }

    fn tick_heartbeat(&self, run_id: &str) -> Result<(), EngineError> {
        let conn = self.lock()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE workflow_runs SET last_heartbeat = :now \
             WHERE id = :id AND status = 'running'",
            named_params![":now": now, ":id": run_id],
        )
        .map_err(db_err)?;
        Ok(())
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
        let conn = self.lock()?;
        conn.execute(
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
                ":total_input_tokens": input_tokens,
                ":total_output_tokens": output_tokens,
                ":total_cache_read_input_tokens": cache_read_input_tokens,
                ":total_cache_creation_input_tokens": cache_creation_input_tokens,
                ":total_turns": num_turns,
                ":total_cost_usd": cost_usd,
                ":total_duration_ms": duration_ms,
                ":model": Option::<&str>::None,
                ":id": run_id,
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }
}

/// Validate that gate selections are within the allowed options for this step.
///
/// Inlined from `WorkflowManager::validate_gate_selections`. The allowed-values
/// set is parsed from the step's `gate_options` JSON column, which the engine
/// writes when a gate starts. Returns an `EngineError::Persistence` describing
/// the violation when a selection is outside the configured option set.
fn validate_gate_selections(
    conn: &Connection,
    step_id: &str,
    selections: &[String],
) -> Result<(), EngineError> {
    let gate_options: Option<String> = conn
        .query_row(
            "SELECT gate_options FROM workflow_run_steps WHERE id = :id",
            named_params![":id": step_id],
            |row| row.get::<_, Option<String>>("gate_options"),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                EngineError::Persistence(format!("Step not found: {step_id}"))
            }
            other => db_err(other),
        })?;

    let options_json = match gate_options {
        Some(json) => json,
        None => {
            if !selections.is_empty() {
                return Err(EngineError::Persistence(
                    "Gate selections provided but no options configured for this gate".into(),
                ));
            }
            return Ok(());
        }
    };

    let allowed_options: Vec<serde_json::Value> = serde_json::from_str(&options_json)
        .map_err(|e| EngineError::Persistence(format!("Invalid gate options JSON in database: {e}")))?;

    let allowed_set: HashSet<String> = allowed_options
        .iter()
        .filter_map(|opt| {
            opt.get("value")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        })
        .collect();

    if allowed_set.is_empty() {
        return Err(EngineError::Persistence(
            "No valid options found in gate configuration".into(),
        ));
    }

    for selection in selections {
        if !allowed_set.contains(selection.as_str()) {
            let mut sorted: Vec<&str> = allowed_set.iter().map(|s| s.as_str()).collect();
            sorted.sort_unstable();
            return Err(EngineError::Persistence(format!(
                "Invalid gate selection '{selection}' - not in allowed options: [{}]",
                sorted.join(", ")
            )));
        }
    }
    Ok(())
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
    use runkon_flow::traits::persistence::{GateApprovalState, NewRun, NewStep};

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
        assert!(p.is_run_cancelled(&run.id).unwrap());
    }

    #[test]
    fn is_run_cancelled_returns_true_for_cancelling_status() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        p.update_run_status(&run.id, WorkflowRunStatus::Cancelling, None, None)
            .unwrap();
        assert!(p.is_run_cancelled(&run.id).unwrap());
    }

    #[test]
    fn is_run_cancelled_returns_false_for_running_status() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        p.update_run_status(&run.id, WorkflowRunStatus::Running, None, None)
            .unwrap();
        assert!(!p.is_run_cancelled(&run.id).unwrap());
    }

    #[test]
    fn is_run_cancelled_returns_false_for_nonexistent_run() {
        let (p, _) = make_persistence();
        assert!(!p.is_run_cancelled("nonexistent-run-id").unwrap());
    }

    /// `persist_metrics` must land cost_usd in `total_cost_usd` and num_turns in
    /// `total_turns`. The pre-4.2 implementation went through
    /// `WorkflowManager::persist_workflow_metrics` which expected positional
    /// `total_turns` before `total_cost_usd`; the previous wrapper swapped them.
    /// After inlining the SQL directly, the trait's argument order maps 1:1 to
    /// the named parameters, so the test still guards against future drift.
    #[test]
    fn persist_metrics_maps_cost_and_turns_to_correct_columns() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();

        // Use distinguishable values so a swap is immediately visible.
        let cost_usd = 42.5_f64;
        let num_turns = 7_i64;

        p.persist_metrics(&run.id, 0, 0, 0, 0, cost_usd, num_turns, 1000)
            .unwrap();

        let fetched = p.get_run(&run.id).unwrap().expect("run should exist");
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

    /// `approve_gate` with non-empty `selections` must write `context_out` to the step row.
    #[test]
    fn test_approve_gate_with_selections_sets_context_out() {
        let conn = crate::test_helpers::setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

        let shared = std::sync::Arc::new(std::sync::Mutex::new(conn));
        let p = SqliteWorkflowPersistence::from_shared_connection(std::sync::Arc::clone(&shared));

        let run = p.create_run(make_new_run(parent.id)).unwrap();
        let step_id = p
            .insert_step(NewStep {
                workflow_run_id: run.id.clone(),
                step_name: "review-gate".to_string(),
                role: "gate".to_string(),
                can_commit: false,
                position: 0,
                iteration: 0,
                retry_count: None,
            })
            .unwrap();

        // Configure gate_options so validation passes for the selections.
        // The validation expects an array of objects with a "value" key.
        {
            let conn = shared.lock().unwrap();
            let mgr = crate::workflow::manager::WorkflowManager::new(&conn);
            mgr.set_step_gate_options(
                &step_id,
                r#"[{"value":"item-a"},{"value":"item-b"},{"value":"item-c"}]"#,
            )
            .unwrap();
        }

        let selections = vec!["item-a".to_string(), "item-b".to_string()];
        p.approve_gate(&step_id, "human", None, Some(&selections))
            .unwrap();

        // Read back the step to verify context_out was written.
        let steps = p.get_steps(&run.id).unwrap();
        let step = steps.iter().find(|s| s.id == step_id).unwrap();
        let context_out = step
            .context_out
            .as_deref()
            .expect("context_out should be set when selections are provided");
        assert!(
            context_out.contains("item-a"),
            "context_out should contain the first selection; got: {context_out:?}"
        );
        assert!(
            context_out.contains("item-b"),
            "context_out should contain the second selection; got: {context_out:?}"
        );
    }

    /// `approve_gate` with `selections = Some(&[])` (empty slice) must NOT set
    /// `context_out` — the persistence layer filters empty selections out.
    #[test]
    fn test_approve_gate_with_empty_selections_sets_no_context_out() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        let step_id = p
            .insert_step(NewStep {
                workflow_run_id: run.id.clone(),
                step_name: "review-gate".to_string(),
                role: "gate".to_string(),
                can_commit: false,
                position: 0,
                iteration: 0,
                retry_count: None,
            })
            .unwrap();

        // Pass an empty selections slice — validation is skipped for empty slices,
        // and `format_gate_selection_context` is not called, so context_out stays None.
        p.approve_gate(&step_id, "human", None, Some(&[])).unwrap();

        let steps = p.get_steps(&run.id).unwrap();
        let step = steps.iter().find(|s| s.id == step_id).unwrap();
        assert!(
            step.context_out.is_none(),
            "context_out should be None for empty selections; got: {:?}",
            step.context_out
        );
    }

    /// `get_gate_approval` must preserve `feedback` on the `Approved` variant.
    #[test]
    fn get_gate_approval_approved_preserves_feedback() {
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

        p.approve_gate(&step_id, "human", Some("lgtm"), None)
            .unwrap();

        let state = p.get_gate_approval(&step_id).unwrap();
        match state {
            GateApprovalState::Approved {
                feedback,
                selections,
            } => {
                assert_eq!(
                    feedback,
                    Some("lgtm".to_string()),
                    "feedback must survive the approve_gate/get_gate_approval roundtrip"
                );
                assert!(
                    selections.is_none(),
                    "selections should be None when not provided"
                );
            }
            other => panic!("expected Approved, got {other:?}"),
        }
    }
}
