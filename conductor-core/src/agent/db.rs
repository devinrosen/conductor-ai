use super::status::StepStatus;
use super::types::{
    AgentCreatedIssue, AgentRun, AgentRunEvent, FeedbackOption, FeedbackRequest, PlanStep,
};
use crate::error::Result;

/// Shared SELECT column list for the `feedback_requests` table.
pub(super) const FEEDBACK_SELECT: &str =
    "SELECT id, run_id, prompt, response, status, created_at, responded_at, \
     feedback_type, options_json, timeout_secs FROM feedback_requests";

/// Shared SELECT column list for the `agent_runs` table (plain, unaliased form).
pub(super) const AGENT_RUN_SELECT: &str =
    "SELECT id, worktree_id, claude_session_id, prompt, status, result_text, \
     cost_usd, num_turns, duration_ms, started_at, ended_at, tmux_window, log_file, \
     model, plan, parent_run_id, \
     input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, \
     bot_name FROM agent_runs";

/// Generate an `agent_runs` column list with a given table alias.
///
/// All three aliased variants (`a.`, `ar.`, and `a.` with NULL plan) are produced
/// from this single macro so a schema change only needs one edit here.
///
/// Usage:
/// - `agent_run_cols!("a.")` — standard columns aliased as `a.`
/// - `agent_run_cols!("ar.")` — standard columns aliased as `ar.`
/// - `agent_run_cols!("a.", null_plan)` — `a.` alias but `NULL` for the `plan` column
macro_rules! agent_run_cols {
    ($alias:literal) => {
        concat!(
            $alias,
            "id, ",
            $alias,
            "worktree_id, ",
            $alias,
            "claude_session_id, ",
            $alias,
            "prompt, ",
            $alias,
            "status, ",
            $alias,
            "result_text, ",
            $alias,
            "cost_usd, ",
            $alias,
            "num_turns, ",
            $alias,
            "duration_ms, ",
            $alias,
            "started_at, ",
            $alias,
            "ended_at, ",
            $alias,
            "tmux_window, ",
            $alias,
            "log_file, ",
            $alias,
            "model, ",
            $alias,
            "plan, ",
            $alias,
            "parent_run_id, ",
            $alias,
            "input_tokens, ",
            $alias,
            "output_tokens, ",
            $alias,
            "cache_read_input_tokens, ",
            $alias,
            "cache_creation_input_tokens, ",
            $alias,
            "bot_name"
        )
    };
    ($alias:literal, null_plan) => {
        concat!(
            $alias,
            "id, ",
            $alias,
            "worktree_id, ",
            $alias,
            "claude_session_id, ",
            $alias,
            "prompt, ",
            $alias,
            "status, ",
            $alias,
            "result_text, ",
            $alias,
            "cost_usd, ",
            $alias,
            "num_turns, ",
            $alias,
            "duration_ms, ",
            $alias,
            "started_at, ",
            $alias,
            "ended_at, ",
            $alias,
            "tmux_window, ",
            $alias,
            "log_file, ",
            $alias,
            "model, ",
            "NULL, ",
            $alias,
            "parent_run_id, ",
            $alias,
            "input_tokens, ",
            $alias,
            "output_tokens, ",
            $alias,
            "cache_read_input_tokens, ",
            $alias,
            "cache_creation_input_tokens, ",
            $alias,
            "bot_name"
        )
    };
}

/// Column list for `agent_runs` with the `a.` table alias, including `a.plan`.
/// Use this in JOINs/CTEs where the table is aliased as `a`.
pub(super) const AGENT_RUN_COLS_A: &str = agent_run_cols!("a.");

/// Column list for `agent_runs` with the `ar.` table alias, including `ar.plan`.
/// Use this in JOINs where the table is aliased as `ar` (e.g. `list_agent_runs`).
pub(super) const AGENT_RUN_COLS_AR: &str = agent_run_cols!("ar.");

/// Like [`AGENT_RUN_COLS_A`] but substitutes `NULL` for the `plan` column.
/// Use this when plan steps are intentionally omitted (populated separately via
/// `populate_plans` to avoid loading steps for every row in a JOIN).
pub(super) const AGENT_RUN_COLS_A_NULL_PLAN: &str = agent_run_cols!("a.", null_plan);

/// Shared SELECT column list for the `agent_run_steps` table.
pub(super) const AGENT_RUN_STEPS_SELECT: &str =
    "SELECT id, run_id, position, description, status, started_at, completed_at \
     FROM agent_run_steps";

/// Shared SELECT column list for the `agent_run_events` table (plain, unaliased form).
/// The aliased `e.` variant used in the JOIN query is left inline.
pub(super) const AGENT_RUN_EVENTS_SELECT: &str =
    "SELECT id, run_id, kind, summary, started_at, ended_at, metadata \
     FROM agent_run_events";

/// Shared SELECT column list for the `agent_created_issues` table (plain, unaliased form).
/// The aliased `aci.` variant used in the JOIN query is left inline.
pub(super) const AGENT_CREATED_ISSUES_SELECT: &str =
    "SELECT id, agent_run_id, repo_id, source_type, source_id, title, url, created_at \
     FROM agent_created_issues";

pub(super) fn row_to_feedback_request(row: &rusqlite::Row) -> rusqlite::Result<FeedbackRequest> {
    let options_json: Option<String> = row.get(8)?;
    let options: Option<Vec<FeedbackOption>> =
        options_json.and_then(|j| serde_json::from_str(&j).ok());
    Ok(FeedbackRequest {
        id: row.get(0)?,
        run_id: row.get(1)?,
        prompt: row.get(2)?,
        response: row.get(3)?,
        status: row.get(4)?,
        created_at: row.get(5)?,
        responded_at: row.get(6)?,
        feedback_type: row.get(7)?,
        options,
        timeout_secs: row.get(9)?,
    })
}

pub(super) fn row_to_agent_run_event(row: &rusqlite::Row) -> rusqlite::Result<AgentRunEvent> {
    Ok(AgentRunEvent {
        id: row.get(0)?,
        run_id: row.get(1)?,
        kind: row.get(2)?,
        summary: row.get(3)?,
        started_at: row.get(4)?,
        ended_at: row.get(5)?,
        metadata: row.get(6)?,
    })
}

pub(super) fn row_to_agent_created_issue(
    row: &rusqlite::Row,
) -> rusqlite::Result<AgentCreatedIssue> {
    Ok(AgentCreatedIssue {
        id: row.get(0)?,
        agent_run_id: row.get(1)?,
        repo_id: row.get(2)?,
        source_type: row.get(3)?,
        source_id: row.get(4)?,
        title: row.get(5)?,
        url: row.get(6)?,
        created_at: row.get(7)?,
    })
}

pub(super) fn row_to_agent_run(row: &rusqlite::Row) -> rusqlite::Result<AgentRun> {
    // Plan is populated separately from agent_run_steps table by the caller.
    // Column 14 (plan JSON) is still selected for SQL compatibility but ignored.
    Ok(AgentRun {
        id: row.get(0)?,
        worktree_id: row.get(1)?,
        claude_session_id: row.get(2)?,
        prompt: row.get(3)?,
        status: row.get(4)?,
        result_text: row.get(5)?,
        cost_usd: row.get(6)?,
        num_turns: row.get(7)?,
        duration_ms: row.get(8)?,
        started_at: row.get(9)?,
        ended_at: row.get(10)?,
        tmux_window: row.get(11)?,
        log_file: row.get(12)?,
        model: row.get(13)?,
        plan: None,
        parent_run_id: row.get(15)?,
        input_tokens: row.get(16)?,
        output_tokens: row.get(17)?,
        cache_read_input_tokens: row.get(18)?,
        cache_creation_input_tokens: row.get(19)?,
        bot_name: row.get(20)?,
    })
}

pub(super) fn row_to_plan_step(row: &rusqlite::Row) -> rusqlite::Result<PlanStep> {
    let status: StepStatus = row.get(4)?;
    let done = status == StepStatus::Completed;
    Ok(PlanStep {
        id: Some(row.get(0)?),
        description: row.get(3)?,
        done,
        status,
        position: Some(row.get(2)?),
        started_at: row.get(5)?,
        completed_at: row.get(6)?,
    })
}

/// Convert a `rusqlite::Result<T>` into `Result<Option<T>>`, treating
/// `QueryReturnedNoRows` as `Ok(None)`.
pub(super) fn optional_row<T>(result: rusqlite::Result<T>) -> Result<Option<T>> {
    match result {
        Ok(val) => Ok(Some(val)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
