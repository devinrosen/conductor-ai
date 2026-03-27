use crate::db::sql_placeholders;
use crate::workflow::types::{BlockedOn, PendingGateRow, WorkflowRun, WorkflowRunStep};

/// Returns `(where_clause, params)` where `params` is a `Vec<String>` whose
/// elements bind to the positional placeholders in the clause.
pub(super) fn purge_where_clause(
    statuses: &[&str],
    repo_id: Option<&str>,
) -> (String, Vec<String>) {
    let n = statuses.len();
    let placeholders = sql_placeholders(n);
    let where_clause = if repo_id.is_some() {
        format!(
            "status IN ({placeholders}) AND worktree_id IN \
             (SELECT id FROM worktrees WHERE repo_id = ?{})",
            n + 1
        )
    } else {
        format!("status IN ({placeholders})")
    };
    let mut params: Vec<String> = statuses.iter().map(|s| s.to_string()).collect();
    if let Some(rid) = repo_id {
        params.push(rid.to_string());
    }
    (where_clause, params)
}

pub(in crate::workflow) fn row_to_workflow_run(
    row: &rusqlite::Row,
) -> rusqlite::Result<WorkflowRun> {
    let id: String = row.get(0)?;
    let dry_run_int: i64 = row.get(5)?;
    let inputs_json: Option<String> = row.get(11)?;
    let inputs: std::collections::HashMap<String, String> = inputs_json
        .as_deref()
        .map(|s| {
            serde_json::from_str(s).unwrap_or_else(|e| {
                tracing::warn!("Malformed inputs JSON in workflow run {id}: {e}");
                std::collections::HashMap::new()
            })
        })
        .unwrap_or_default();
    let ticket_id: Option<String> = row.get(12)?;
    let repo_id: Option<String> = row.get(13)?;
    let parent_workflow_run_id: Option<String> = row.get(14)?;
    let target_label: Option<String> = row.get(15)?;
    let default_bot_name: Option<String> = row.get(16)?;
    let iteration: i64 = row.get(17)?;
    let blocked_on_json: Option<String> = row.get(18)?;
    let blocked_on: Option<BlockedOn> = blocked_on_json.as_deref().and_then(|s| {
        serde_json::from_str(s).unwrap_or_else(|e| {
            tracing::warn!("Malformed blocked_on JSON in workflow run {id}: {e}");
            None
        })
    });
    let feature_id: Option<String> = row.get(19)?;
    Ok(WorkflowRun {
        id,
        workflow_name: row.get(1)?,
        worktree_id: row.get::<_, Option<String>>(2)?,
        parent_run_id: row.get(3)?,
        status: row.get(4)?,
        dry_run: dry_run_int != 0,
        trigger: row.get(6)?,
        started_at: row.get(7)?,
        ended_at: row.get(8)?,
        result_summary: row.get(9)?,
        definition_snapshot: row.get(10)?,
        inputs,
        ticket_id,
        repo_id,
        parent_workflow_run_id,
        target_label,
        default_bot_name,
        iteration,
        blocked_on,
        feature_id,
    })
}

pub(super) fn waiting_gate_step_row_mapper(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(WorkflowRunStep, String, Option<String>)> {
    let step = row_to_workflow_step(row)?;
    let workflow_name: String = row.get("workflow_name")?;
    let target_label: Option<String> = row.get("target_label")?;
    Ok((step, workflow_name, target_label))
}

pub(super) fn pending_gate_row_mapper(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingGateRow> {
    let step = row_to_workflow_step(row)?;
    let workflow_name: String = row.get("workflow_name")?;
    let target_label: Option<String> = row.get("target_label")?;
    let branch: Option<String> = row.get("branch")?;
    let ticket_ref: Option<String> = row.get("ticket_ref")?;
    Ok(PendingGateRow {
        step,
        workflow_name,
        target_label,
        branch,
        ticket_ref,
    })
}

pub(in crate::workflow) fn row_to_workflow_step(
    row: &rusqlite::Row,
) -> rusqlite::Result<WorkflowRunStep> {
    let can_commit_int: i64 = row.get("can_commit")?;
    let condition_met_int: Option<i64> = row.get("condition_met")?;
    Ok(WorkflowRunStep {
        id: row.get("id")?,
        workflow_run_id: row.get("workflow_run_id")?,
        step_name: row.get("step_name")?,
        role: row.get("role")?,
        can_commit: can_commit_int != 0,
        condition_expr: row.get("condition_expr")?,
        status: row.get("status")?,
        child_run_id: row.get("child_run_id")?,
        position: row.get("position")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        result_text: row.get("result_text")?,
        condition_met: condition_met_int.map(|v| v != 0),
        iteration: row.get("iteration")?,
        parallel_group_id: row.get("parallel_group_id")?,
        context_out: row.get("context_out")?,
        markers_out: row.get("markers_out")?,
        retry_count: row.get("retry_count")?,
        gate_type: row.get("gate_type")?,
        gate_prompt: row.get("gate_prompt")?,
        gate_timeout: row.get("gate_timeout")?,
        gate_approved_by: row.get("gate_approved_by")?,
        gate_approved_at: row.get("gate_approved_at")?,
        gate_feedback: row.get("gate_feedback")?,
        structured_output: row.get("structured_output")?,
        output_file: row.get("output_file")?,
        gate_options: row.get("gate_options")?,
        gate_selections: row.get("gate_selections")?,
    })
}
