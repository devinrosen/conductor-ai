use std::collections::HashMap;

use chrono::Utc;
use rusqlite::{named_params, Connection};
use serde_json;

use crate::agent::AgentManager;
use crate::error::{ConductorError, Result};

use crate::workflow::{extract_workflow_title, WorkflowRun};
use crate::workflow::{WorkflowRunStatus, WorkflowStepStatus};

pub fn create_workflow_run(
    conn: &Connection,
    workflow_name: &str,
    worktree_id: Option<&str>,
    parent_run_id: &str,
    dry_run: bool,
    trigger: &str,
    definition_snapshot: Option<&str>,
) -> Result<WorkflowRun> {
    create_workflow_run_with_targets(
        conn,
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

#[allow(clippy::too_many_arguments)]
pub fn create_workflow_run_with_targets(
    conn: &Connection,
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

    let workflow_title = extract_workflow_title(definition_snapshot);
    conn.execute(
        "INSERT INTO workflow_runs (id, workflow_name, worktree_id, ticket_id, repo_id, \
             parent_run_id, status, dry_run, trigger, started_at, definition_snapshot, \
             parent_workflow_run_id, target_label, workflow_title) \
             VALUES (:id, :workflow_name, :worktree_id, :ticket_id, :repo_id, :parent_run_id, \
             :status, :dry_run, :trigger, :started_at, :definition_snapshot, \
             :parent_workflow_run_id, :target_label, :workflow_title)",
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
            ":workflow_title": workflow_title,
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
        dismissed: false,
        owner_token: None,
        lease_until: None,
        generation: 0,
    })
}

pub fn set_workflow_run_iteration(conn: &Connection, run_id: &str, iteration: i64) -> Result<()> {
    conn.execute(
        "UPDATE workflow_runs SET iteration = :iteration WHERE id = :id",
        named_params![":iteration": iteration, ":id": run_id],
    )?;
    Ok(())
}

pub fn set_workflow_run_default_bot_name(
    conn: &Connection,
    run_id: &str,
    bot_name: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE workflow_runs SET default_bot_name = :bot_name WHERE id = :id",
        named_params![":bot_name": bot_name, ":id": run_id],
    )?;
    Ok(())
}

pub fn set_workflow_run_inputs(
    conn: &Connection,
    run_id: &str,
    inputs: &HashMap<String, String>,
) -> Result<()> {
    let inputs_json = serde_json::to_string(inputs).map_err(|e| {
        ConductorError::Workflow(format!("Failed to serialize workflow inputs: {e}"))
    })?;
    conn.execute(
        "UPDATE workflow_runs SET inputs = :inputs_json WHERE id = :id",
        named_params![":inputs_json": inputs_json, ":id": run_id],
    )?;
    Ok(())
}

pub fn update_workflow_status(
    conn: &Connection,
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
    conn.execute(
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

pub fn set_waiting_blocked_on(
    conn: &Connection,
    workflow_run_id: &str,
    blocked_on: &crate::workflow::BlockedOn,
) -> Result<()> {
    let json = serde_json::to_string(blocked_on)
        .map_err(|e| ConductorError::Workflow(format!("Failed to serialize blocked_on: {e}")))?;
    conn.execute(
            "UPDATE workflow_runs SET status = :status, blocked_on = :blocked_on WHERE id = :id",
            named_params![":status": WorkflowRunStatus::Waiting, ":blocked_on": json, ":id": workflow_run_id],
        )?;
    Ok(())
}

pub fn cancel_run(conn: &Connection, run_id: &str, reason: &str) -> Result<()> {
    let run = super::queries::get_workflow_run(conn, run_id)?
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

    // Engine already set the run to Cancelling — cooperative cleanup is in
    // progress. A second cancel_run() call would race the engine; return
    // success without re-running the cancellation sequence.
    if run.status == WorkflowRunStatus::Cancelling {
        return Ok(());
    }

    let agent_mgr = AgentManager::new(conn);
    if let Ok(steps) = super::queries::get_workflow_steps(conn, run_id) {
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
            if let Err(e) = super::steps::update_step_status(
                conn,
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
    if let Ok(children) = super::queries::list_child_workflow_runs(conn, run_id) {
        for child in children {
            if matches!(
                child.status,
                WorkflowRunStatus::Running
                    | WorkflowRunStatus::Pending
                    | WorkflowRunStatus::Waiting
            ) {
                if let Err(e) = cancel_run(conn, &child.id, reason) {
                    tracing::warn!(
                        child_run_id = %child.id,
                        "Failed to cancel child workflow run during parent cancellation: {e}"
                    );
                }
            }
        }
    }

    update_workflow_status(
        conn,
        run_id,
        WorkflowRunStatus::Cancelled,
        Some(reason),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn persist_workflow_metrics(
    conn: &Connection,
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

// NOTE (#2731/#2796): lease refresh (FlowEngine's refresh_lease_loop) is now the
// load-bearing ownership mechanism. This heartbeat write is retained only for UI
// staleness display — detect_stuck_workflow_run_ids reads last_heartbeat.
pub fn tick_heartbeat(conn: &Connection, run_id: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE workflow_runs SET last_heartbeat = :now \
             WHERE id = :id AND status = 'running'",
        named_params![":now": now, ":id": run_id],
    )?;
    Ok(())
}

pub fn set_dismissed(conn: &Connection, run_id: &str, dismissed: bool) -> Result<()> {
    let val: i64 = if dismissed { 1 } else { 0 };
    conn.execute(
        "UPDATE workflow_runs SET dismissed = ?1 WHERE id = ?2",
        rusqlite::params![val, run_id],
    )?;
    Ok(())
}

pub fn fail_workflow_run(
    conn: &Connection,
    workflow_run_id: &str,
    error_msg: &str,
) -> Result<String> {
    update_workflow_status(
        conn,
        workflow_run_id,
        WorkflowRunStatus::Failed,
        Some(error_msg),
        Some(error_msg),
    )?;
    if let Ok(Some(run)) = super::queries::get_workflow_run(conn, workflow_run_id) {
        Ok(run.parent_run_id)
    } else {
        Err(ConductorError::InvalidInput(format!(
            "Workflow run not found: {workflow_run_id}"
        )))
    }
}
