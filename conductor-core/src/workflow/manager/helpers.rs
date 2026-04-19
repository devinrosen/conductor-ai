use crate::db::sql_placeholders;
use crate::workflow::types::{
    extract_workflow_title, BlockedOn, PendingGateRow, WorkflowRun, WorkflowRunStep,
};

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
    let id: String = row.get("id")?;
    let dry_run_int: i64 = row.get("dry_run")?;
    let inputs_json: Option<String> = row.get("inputs")?;
    let inputs: std::collections::HashMap<String, String> = inputs_json
        .as_deref()
        .map(|s| {
            serde_json::from_str(s).unwrap_or_else(|e| {
                tracing::warn!("Malformed inputs JSON in workflow run {id}: {e}");
                std::collections::HashMap::new()
            })
        })
        .unwrap_or_default();
    let ticket_id: Option<String> = row.get("ticket_id")?;
    let repo_id: Option<String> = row.get("repo_id")?;
    let parent_workflow_run_id: Option<String> = row.get("parent_workflow_run_id")?;
    let target_label: Option<String> = row.get("target_label")?;
    let default_bot_name: Option<String> = row.get("default_bot_name")?;
    let iteration: i64 = row.get("iteration")?;
    let blocked_on_json: Option<String> = row.get("blocked_on")?;
    let blocked_on: Option<BlockedOn> = blocked_on_json.as_deref().and_then(|s| {
        serde_json::from_str(s).unwrap_or_else(|e| {
            tracing::warn!("Malformed blocked_on JSON in workflow run {id}: {e}");
            None
        })
    });
    let total_input_tokens: Option<i64> = row.get("total_input_tokens")?;
    let total_output_tokens: Option<i64> = row.get("total_output_tokens")?;
    let total_cache_read_input_tokens: Option<i64> = row.get("total_cache_read_input_tokens")?;
    let total_cache_creation_input_tokens: Option<i64> =
        row.get("total_cache_creation_input_tokens")?;
    let total_turns: Option<i64> = row.get("total_turns")?;
    let total_cost_usd: Option<f64> = row.get("total_cost_usd")?;
    let total_duration_ms: Option<i64> = row.get("total_duration_ms")?;
    let model: Option<String> = row.get("model")?;
    let error: Option<String> = row.get("error")?;
    let dismissed_int: i64 = row.get("dismissed")?;
    let definition_snapshot: Option<String> = row.get("definition_snapshot")?;
    let workflow_title = extract_workflow_title(definition_snapshot.as_deref());
    Ok(WorkflowRun {
        id,
        workflow_name: row.get("workflow_name")?,
        worktree_id: row.get::<_, Option<String>>("worktree_id")?,
        parent_run_id: row.get("parent_run_id")?,
        status: row.get("status")?,
        dry_run: dry_run_int != 0,
        trigger: row.get("trigger")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        result_summary: row.get("result_summary")?,
        error,
        definition_snapshot,
        inputs,
        ticket_id,
        repo_id,
        parent_workflow_run_id,
        target_label,
        default_bot_name,
        iteration,
        blocked_on,
        workflow_title,
        total_input_tokens,
        total_output_tokens,
        total_cache_read_input_tokens,
        total_cache_creation_input_tokens,
        total_turns,
        total_cost_usd,
        total_duration_ms,
        model,
        dismissed: dismissed_int != 0,
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
    let definition_snapshot: Option<String> = row.get("definition_snapshot")?;
    let workflow_title = extract_workflow_title(definition_snapshot.as_deref());
    Ok(PendingGateRow {
        step,
        workflow_name,
        target_label,
        branch,
        ticket_ref,
        workflow_title,
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
        input_tokens: row.get::<_, Option<i64>>("input_tokens").unwrap_or(None),
        output_tokens: row.get::<_, Option<i64>>("output_tokens").unwrap_or(None),
        cache_read_input_tokens: row
            .get::<_, Option<i64>>("cache_read_input_tokens")
            .unwrap_or(None),
        cache_creation_input_tokens: row
            .get::<_, Option<i64>>("cache_creation_input_tokens")
            .unwrap_or(None),
        fan_out_total: row.get::<_, Option<i64>>("fan_out_total").unwrap_or(None),
        fan_out_completed: row
            .get::<_, Option<i64>>("fan_out_completed")
            .unwrap_or(None)
            .unwrap_or(0),
        fan_out_failed: row
            .get::<_, Option<i64>>("fan_out_failed")
            .unwrap_or(None)
            .unwrap_or(0),
        fan_out_skipped: row
            .get::<_, Option<i64>>("fan_out_skipped")
            .unwrap_or(None)
            .unwrap_or(0),
        step_error: row.get::<_, Option<String>>("step_error").unwrap_or(None),
    })
}
