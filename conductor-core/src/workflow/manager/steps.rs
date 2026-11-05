use std::collections::HashSet;

use chrono::Utc;
use rusqlite::named_params;
use rusqlite::Connection;

use super::helpers::execute_sql;
use crate::error::{ConductorError, Result};
use crate::workflow::{GateType, WorkflowStepStatus};

pub fn insert_step(
    conn: &Connection,
    workflow_run_id: &str,
    step_name: &str,
    role: &str,
    can_commit: bool,
    position: i64,
    iteration: i64,
) -> Result<String> {
    let id = crate::new_id();
    conn.execute(
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

#[allow(clippy::too_many_arguments)]
pub fn insert_step_running(
    conn: &Connection,
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
    conn.execute(
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

#[allow(clippy::too_many_arguments)]
pub fn update_step_status(
    conn: &Connection,
    step_id: &str,
    status: WorkflowStepStatus,
    child_run_id: Option<&str>,
    result_text: Option<&str>,
    context_out: Option<&str>,
    markers_out: Option<&str>,
    retry_count: Option<i64>,
) -> Result<()> {
    update_step_status_full(
        conn,
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

pub fn mark_step_running(
    conn: &Connection,
    step_id: &str,
    status: WorkflowStepStatus,
    child_run_id: Option<&str>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    execute_sql(
        conn,
        "UPDATE workflow_run_steps SET status = :status, child_run_id = :child_run_id, \
             started_at = :started_at WHERE id = :id",
        named_params![":status": status, ":child_run_id": child_run_id, ":started_at": now, ":id": step_id],
    )
}

#[allow(clippy::too_many_arguments)]
pub fn mark_step_terminal(
    conn: &Connection,
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
    execute_sql(
        conn,
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

pub fn mark_step_pending(
    conn: &Connection,
    step_id: &str,
    status: WorkflowStepStatus,
) -> Result<()> {
    execute_sql(
        conn,
        "UPDATE workflow_run_steps SET status = :status WHERE id = :id",
        named_params![":status": status, ":id": step_id],
    )
}

#[allow(clippy::too_many_arguments)]
pub fn update_step_status_full(
    conn: &Connection,
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
        mark_step_running(conn, step_id, status, child_run_id)
    } else if is_terminal {
        mark_step_terminal(
            conn,
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
        mark_step_pending(conn, step_id, status)
    }
}

pub fn update_step_child_run_id(
    conn: &Connection,
    step_id: &str,
    child_run_id: &str,
) -> Result<()> {
    execute_sql(
        conn,
        "UPDATE workflow_run_steps SET child_run_id = :child_run_id WHERE id = :id",
        named_params![":child_run_id": child_run_id, ":id": step_id],
    )
}

pub fn set_step_subprocess_pid(conn: &Connection, step_id: &str, pid: Option<u32>) -> Result<()> {
    execute_sql(
        conn,
        "UPDATE workflow_run_steps SET subprocess_pid = :pid WHERE id = :id",
        named_params![":pid": pid.map(|p| p as i64), ":id": step_id],
    )
}

pub fn set_step_output_file(conn: &Connection, step_id: &str, output_file: &str) -> Result<()> {
    execute_sql(
        conn,
        "UPDATE workflow_run_steps SET output_file = :output_file WHERE id = :id",
        named_params![":output_file": output_file, ":id": step_id],
    )
}

pub fn set_step_gate_info(
    conn: &Connection,
    step_id: &str,
    gate_type: &GateType,
    gate_prompt: Option<&str>,
    gate_timeout: &str,
) -> Result<()> {
    execute_sql(
        conn,
        "UPDATE workflow_run_steps SET gate_type = :gate_type, gate_prompt = :gate_prompt, \
             gate_timeout = :gate_timeout WHERE id = :id",
        named_params![":gate_type": gate_type.as_str(), ":gate_prompt": gate_prompt, ":gate_timeout": gate_timeout, ":id": step_id],
    )
}

pub fn set_step_parallel_group(conn: &Connection, step_id: &str, group_id: &str) -> Result<()> {
    execute_sql(
        conn,
        "UPDATE workflow_run_steps SET parallel_group_id = :group_id WHERE id = :id",
        named_params![":group_id": group_id, ":id": step_id],
    )
}

pub fn set_step_gate_options(conn: &Connection, step_id: &str, options_json: &str) -> Result<()> {
    execute_sql(
        conn,
        "UPDATE workflow_run_steps SET gate_options = :options_json WHERE id = :id",
        named_params![":options_json": options_json, ":id": step_id],
    )
}

/// Usage columns shared by `agent_runs` and `workflow_run_steps`.
const STEP_USAGE_COLUMNS: [&str; 7] = [
    "cost_usd",
    "num_turns",
    "duration_ms",
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];

/// Add an agent run's usage onto a `workflow_run_steps` row.
///
/// Additive, so a retried step (one row, several agent runs) ends up holding the
/// total across attempts. A column whose agent-run value is NULL keeps its current
/// value. Does nothing when either id is unknown.
pub fn add_step_metrics_from_agent_run(
    conn: &Connection,
    step_id: &str,
    agent_run_id: &str,
) -> Result<()> {
    // One column list drives both the SET list and the agent_runs subselect, so a
    // column can't be added to one and forgotten in the other.
    let set_list = STEP_USAGE_COLUMNS
        .iter()
        .map(|c| format!("{c} = COALESCE(COALESCE(workflow_run_steps.{c}, 0) + a.{c}, workflow_run_steps.{c})"))
        .collect::<Vec<_>>()
        .join(", ");
    let select_list = STEP_USAGE_COLUMNS.join(", ");
    let sql = format!(
        "UPDATE workflow_run_steps SET {set_list} \
         FROM (SELECT {select_list} FROM agent_runs WHERE id = :agent_run_id) AS a \
         WHERE workflow_run_steps.id = :step_id"
    );
    execute_sql(
        conn,
        &sql,
        named_params![":agent_run_id": agent_run_id, ":step_id": step_id],
    )
}

/// API-layer bypass: writes a gate approval directly to SQLite.
///
/// Called from conductor-core web/TUI handlers, not from within the engine.
/// The engine-facing path is `GateApprovalStore::approve_gate` on the
/// `WorkflowPersistence` trait in runkon-flow.
pub fn approve_gate(
    conn: &Connection,
    step_id: &str,
    approved_by: &str,
    feedback: Option<&str>,
    selections: Option<&[String]>,
    context_out: Option<String>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();

    // Validate selections against stored gate options if provided
    if let Some(selections) = selections {
        validate_gate_selections(conn, step_id, selections)?;
    }

    let selections_json = crate::workflow::helpers::serialize_gate_selections(selections)?;

    execute_sql(
        conn,
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

/// API-layer bypass: writes a gate rejection directly to SQLite.
///
/// See `approve_gate` above — same boundary note applies.
pub fn reject_gate(
    conn: &Connection,
    step_id: &str,
    rejected_by: &str,
    feedback: Option<&str>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    execute_sql(conn,
            "UPDATE workflow_run_steps SET gate_approved_by = :rejected_by, gate_feedback = :feedback, \
             status = 'failed', ended_at = :ended_at WHERE id = :id",
            named_params![":rejected_by": rejected_by, ":feedback": feedback, ":ended_at": now, ":id": step_id],
        )
}

pub fn predecessor_completed(
    conn: &Connection,
    workflow_run_id: &str,
    position: i64,
) -> Result<bool> {
    if position == 0 {
        return Ok(true);
    }
    let mut stmt = conn.prepare_cached(
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

pub fn active_step_exists(
    conn: &Connection,
    workflow_run_id: &str,
    position: i64,
    iteration: i64,
    step_name: &str,
) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
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

/// API-layer bypass: reads gate approval state directly from SQLite.
///
/// See `approve_gate` above — same boundary note applies. The engine-facing
/// path is `GateApprovalStore::get_gate_approval` on the `WorkflowPersistence`
/// trait in runkon-flow.
pub fn get_gate_approval_state(
    conn: &Connection,
    step_id: &str,
) -> Result<runkon_flow::traits::persistence::GateApprovalState> {
    use rusqlite::OptionalExtension;
    #[allow(clippy::type_complexity)]
    let row: Option<(Option<String>, String, Option<String>, Option<String>)> = conn
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

fn validate_gate_selections(conn: &Connection, step_id: &str, selections: &[String]) -> Result<()> {
    // Get the stored gate options for this step
    let mut stmt =
        conn.prepare_cached("SELECT gate_options FROM workflow_run_steps WHERE id = :id")?;
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
                    "Gate selections provided but no options configured for this gate".to_string(),
                ));
            }
            return Ok(());
        }
    };

    // Parse the stored options
    let allowed_options: Vec<serde_json::Value> =
        serde_json::from_str(&options_json).map_err(|e| {
            ConductorError::InvalidInput(format!("Invalid gate options JSON in database: {}", e))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;
    use crate::workflow::WorkflowStepStatus;

    fn setup(conn: &rusqlite::Connection) -> (String, String) {
        let parent_id = test_helpers::make_agent_parent_id(conn);
        let run = crate::workflow::create_workflow_run(
            conn,
            "wf-test",
            Some("w1"),
            &parent_id,
            false,
            "manual",
            None,
        )
        .unwrap();
        let step_id =
            crate::workflow::insert_step(conn, &run.id, "step-a", "actor", false, 0, 0).unwrap();
        (run.id.clone(), step_id)
    }

    #[test]
    fn mark_step_running_sets_status_and_child_run_id() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);

        crate::workflow::mark_step_running(
            &conn,
            &step_id,
            WorkflowStepStatus::Running,
            Some("child-run-1"),
        )
        .unwrap();

        let step = crate::workflow::get_step_by_id(&conn, &step_id)
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

        crate::workflow::mark_step_running(&conn, &step_id, WorkflowStepStatus::Waiting, None)
            .unwrap();

        let step = crate::workflow::get_step_by_id(&conn, &step_id)
            .unwrap()
            .unwrap();
        assert_eq!(step.status, WorkflowStepStatus::Waiting);
        assert!(step.child_run_id.is_none());
    }

    #[test]
    fn mark_step_terminal_sets_all_output_fields() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);

        crate::workflow::mark_step_terminal(
            &conn,
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

        let step = crate::workflow::get_step_by_id(&conn, &step_id)
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

        // Advance to Running first so we can observe the rollback to Pending.
        crate::workflow::mark_step_running(
            &conn,
            &step_id,
            WorkflowStepStatus::Running,
            Some("child-x"),
        )
        .unwrap();
        // Now reset to Pending — only status should change.
        crate::workflow::mark_step_pending(&conn, &step_id, WorkflowStepStatus::Pending).unwrap();

        let step = crate::workflow::get_step_by_id(&conn, &step_id)
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

        crate::workflow::mark_step_running(
            &conn,
            &step_id,
            WorkflowStepStatus::Running,
            Some("child-run-3"),
        )
        .unwrap();
        crate::workflow::mark_step_terminal(
            &conn,
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

        let step = crate::workflow::get_step_by_id(&conn, &step_id)
            .unwrap()
            .unwrap();
        assert_eq!(step.status, WorkflowStepStatus::Failed);
        // COALESCE in SQL keeps the existing child_run_id when None is passed.
        assert_eq!(step.child_run_id.as_deref(), Some("child-run-3"));
        assert_eq!(step.step_error.as_deref(), Some("something went wrong"));
    }

    /// Insert an `agent_runs` row with the given usage and return its id.
    fn insert_agent_run_with_usage(
        conn: &Connection,
        id: &str,
        cost: Option<f64>,
        turns: Option<i64>,
        input: Option<i64>,
        output: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at, cost_usd, num_turns, \
             duration_ms, input_tokens, output_tokens, cache_read_input_tokens, \
             cache_creation_input_tokens) \
             VALUES (?1, 'Workflow step: x', 'completed', '2026-01-01T00:00:00Z', ?2, ?3, \
             ?3, ?4, ?5, ?4, ?5)",
            rusqlite::params![id, cost, turns, input, output],
        )
        .unwrap();
    }

    type StepUsage = (
        Option<f64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    );

    fn step_usage(conn: &Connection, step_id: &str) -> StepUsage {
        conn.query_row(
            "SELECT cost_usd, num_turns, duration_ms, input_tokens, output_tokens, \
             cache_read_input_tokens, cache_creation_input_tokens \
             FROM workflow_run_steps WHERE id = ?1",
            rusqlite::params![step_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap()
    }

    #[test]
    fn add_step_metrics_writes_all_fields() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        insert_agent_run_with_usage(&conn, "ar-1", Some(1.23), Some(5), Some(100), Some(200));

        crate::workflow::add_step_metrics_from_agent_run(&conn, &step_id, "ar-1").unwrap();

        assert_eq!(
            step_usage(&conn, &step_id),
            (
                Some(1.23),
                Some(5),
                Some(5),
                Some(100),
                Some(200),
                Some(100),
                Some(200)
            )
        );
    }

    #[test]
    fn add_step_metrics_adds_across_attempts() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        insert_agent_run_with_usage(&conn, "ar-a", Some(1.0), Some(2), Some(10), Some(20));
        insert_agent_run_with_usage(&conn, "ar-b", Some(0.5), Some(3), Some(1), Some(2));

        crate::workflow::add_step_metrics_from_agent_run(&conn, &step_id, "ar-a").unwrap();
        crate::workflow::add_step_metrics_from_agent_run(&conn, &step_id, "ar-b").unwrap();

        let u = step_usage(&conn, &step_id);
        assert_eq!(u.0, Some(1.5));
        assert_eq!(u.1, Some(5));
        assert_eq!(u.3, Some(11));
        assert_eq!(u.4, Some(22));
    }

    #[test]
    fn add_step_metrics_keeps_columns_when_agent_run_has_none() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        insert_agent_run_with_usage(&conn, "ar-1", Some(9.99), Some(3), None, None);
        crate::workflow::add_step_metrics_from_agent_run(&conn, &step_id, "ar-1").unwrap();
        insert_agent_run_with_usage(&conn, "ar-2", None, None, Some(42), Some(84));
        crate::workflow::add_step_metrics_from_agent_run(&conn, &step_id, "ar-2").unwrap();

        let u = step_usage(&conn, &step_id);
        assert_eq!(u.0, Some(9.99), "cost_usd must be preserved");
        assert_eq!(u.1, Some(3), "num_turns must be preserved");
        assert_eq!(u.3, Some(42));
        assert_eq!(u.4, Some(84));
    }

    #[test]
    fn add_step_metrics_no_op_for_unknown_ids() {
        let conn = test_helpers::setup_db();
        let (_run_id, step_id) = setup(&conn);
        insert_agent_run_with_usage(&conn, "ar-1", Some(1.0), Some(1), Some(1), Some(1));

        crate::workflow::add_step_metrics_from_agent_run(&conn, &step_id, "missing").unwrap();
        crate::workflow::add_step_metrics_from_agent_run(&conn, "missing", "ar-1").unwrap();

        assert_eq!(
            step_usage(&conn, &step_id),
            (None, None, None, None, None, None, None)
        );
    }
}
