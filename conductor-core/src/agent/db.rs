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
    "SELECT id, worktree_id, repo_id, claude_session_id, prompt, status, result_text, \
     cost_usd, num_turns, duration_ms, started_at, ended_at, log_file, \
     model, plan, parent_run_id, \
     input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, \
     bot_name, conversation_id, subprocess_pid, \
     COALESCE(runtime, 'claude') AS runtime FROM agent_runs";

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
            "repo_id, ",
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
            "bot_name, ",
            $alias,
            "conversation_id, ",
            $alias,
            "subprocess_pid, ",
            "COALESCE(",
            $alias,
            "runtime, 'claude') AS runtime"
        )
    };
    ($alias:literal, null_plan) => {
        concat!(
            $alias,
            "id, ",
            $alias,
            "worktree_id, ",
            $alias,
            "repo_id, ",
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
            "bot_name, ",
            $alias,
            "conversation_id, ",
            $alias,
            "subprocess_pid, ",
            "COALESCE(",
            $alias,
            "runtime, 'claude') AS runtime"
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
    let options_json: Option<String> = row.get("options_json")?;
    let options: Option<Vec<FeedbackOption>> =
        options_json.and_then(|j| serde_json::from_str(&j).ok());
    Ok(FeedbackRequest {
        id: row.get("id")?,
        run_id: row.get("run_id")?,
        prompt: row.get("prompt")?,
        response: row.get("response")?,
        status: row.get("status")?,
        created_at: row.get("created_at")?,
        responded_at: row.get("responded_at")?,
        feedback_type: row.get("feedback_type")?,
        options,
        timeout_secs: row.get("timeout_secs")?,
    })
}

pub(super) fn row_to_agent_run_event(row: &rusqlite::Row) -> rusqlite::Result<AgentRunEvent> {
    Ok(AgentRunEvent {
        id: row.get("id")?,
        run_id: row.get("run_id")?,
        kind: row.get("kind")?,
        summary: row.get("summary")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        metadata: row.get("metadata")?,
    })
}

pub(super) fn row_to_agent_created_issue(
    row: &rusqlite::Row,
) -> rusqlite::Result<AgentCreatedIssue> {
    Ok(AgentCreatedIssue {
        id: row.get("id")?,
        agent_run_id: row.get("agent_run_id")?,
        repo_id: row.get("repo_id")?,
        source_type: row.get("source_type")?,
        source_id: row.get("source_id")?,
        title: row.get("title")?,
        url: row.get("url")?,
        created_at: row.get("created_at")?,
    })
}

pub(super) fn row_to_agent_run(row: &rusqlite::Row) -> rusqlite::Result<AgentRun> {
    // Plan is populated separately from agent_run_steps table by the caller.
    // The "plan" column is still selected for SQL compatibility but ignored here.
    Ok(AgentRun {
        id: row.get("id")?,
        worktree_id: row.get("worktree_id")?,
        repo_id: row.get("repo_id")?,
        claude_session_id: row.get("claude_session_id")?,
        prompt: row.get("prompt")?,
        status: row.get("status")?,
        result_text: row.get("result_text")?,
        cost_usd: row.get("cost_usd")?,
        num_turns: row.get("num_turns")?,
        duration_ms: row.get("duration_ms")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        log_file: row.get("log_file")?,
        model: row.get("model")?,
        plan: None,
        parent_run_id: row.get("parent_run_id")?,
        input_tokens: row.get("input_tokens")?,
        output_tokens: row.get("output_tokens")?,
        cache_read_input_tokens: row.get("cache_read_input_tokens")?,
        cache_creation_input_tokens: row.get("cache_creation_input_tokens")?,
        bot_name: row.get("bot_name")?,
        conversation_id: row.get("conversation_id")?,
        subprocess_pid: row.get("subprocess_pid")?,
        runtime: row.get("runtime")?,
    })
}

pub(super) fn row_to_plan_step(row: &rusqlite::Row) -> rusqlite::Result<PlanStep> {
    let status: StepStatus = row.get("status")?;
    let done = status == StepStatus::Completed;
    Ok(PlanStep {
        id: Some(row.get("id")?),
        description: row.get("description")?,
        done,
        status,
        position: Some(row.get("position")?),
        started_at: row.get("started_at")?,
        completed_at: row.get("completed_at")?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Expected column names in order, matching `row_to_agent_run` positional indices.
    const EXPECTED_AGENT_RUN_COLS: &[&str] = &[
        "id",
        "worktree_id",
        "repo_id",
        "claude_session_id",
        "prompt",
        "status",
        "result_text",
        "cost_usd",
        "num_turns",
        "duration_ms",
        "started_at",
        "ended_at",
        "log_file",
        "model",
        "plan",
        "parent_run_id",
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
        "bot_name",
        "conversation_id",
        "subprocess_pid",
        "runtime",
    ];

    #[test]
    fn agent_run_cols_a_contains_all_columns() {
        let cols = AGENT_RUN_COLS_A;
        for col in EXPECTED_AGENT_RUN_COLS {
            let prefixed = format!("a.{col}");
            assert!(
                cols.contains(&prefixed),
                "AGENT_RUN_COLS_A missing column: {prefixed}"
            );
        }
    }

    #[test]
    fn agent_run_cols_ar_contains_all_columns() {
        let cols = AGENT_RUN_COLS_AR;
        for col in EXPECTED_AGENT_RUN_COLS {
            let prefixed = format!("ar.{col}");
            assert!(
                cols.contains(&prefixed),
                "AGENT_RUN_COLS_AR missing column: {prefixed}"
            );
        }
    }

    #[test]
    fn agent_run_cols_null_plan_replaces_plan() {
        let cols = AGENT_RUN_COLS_A_NULL_PLAN;
        // Should NOT contain a.plan
        assert!(
            !cols.contains("a.plan"),
            "AGENT_RUN_COLS_A_NULL_PLAN should not contain a.plan"
        );
        // Should contain NULL in place of plan
        assert!(
            cols.contains("NULL"),
            "AGENT_RUN_COLS_A_NULL_PLAN should contain NULL"
        );
        // All other columns should still be present
        for col in EXPECTED_AGENT_RUN_COLS {
            if *col == "plan" {
                continue;
            }
            let prefixed = format!("a.{col}");
            assert!(
                cols.contains(&prefixed),
                "AGENT_RUN_COLS_A_NULL_PLAN missing column: {prefixed}"
            );
        }
    }

    /// Create an in-memory database with the full production schema.
    /// Foreign keys are disabled so tests can insert rows without full parent chains.
    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        conn
    }

    /// Insert a minimal agent_run row with only required fields for FK/query tests.
    fn insert_minimal_run(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at) \
             VALUES (?1, 'hi', 'running', '2025-01-01T00:00:00Z')",
            [id],
        )
        .unwrap();
    }

    #[test]
    fn row_to_agent_run_maps_all_fields() {
        let conn = test_db();
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at, worktree_id, repo_id, \
             claude_session_id, result_text, cost_usd, num_turns, duration_ms, ended_at, \
             log_file, model, plan, parent_run_id, \
             input_tokens, output_tokens, cache_read_input_tokens, \
             cache_creation_input_tokens, bot_name) \
             VALUES (:id, :prompt, :status, :started_at, :worktree_id, :repo_id, \
                     :claude_session_id, :result_text, :cost_usd, :num_turns, :duration_ms, :ended_at, \
                     :log_file, :model, :plan, :parent_run_id, \
                     :input_tokens, :output_tokens, :cache_read_input_tokens, \
                     :cache_creation_input_tokens, :bot_name)",
            rusqlite::named_params! {
                ":id": "run-001",
                ":prompt": "do the thing",
                ":status": "running",
                ":started_at": "2025-01-01T00:00:00Z",
                ":worktree_id": "wt-001",
                ":repo_id": "repo-001",
                ":claude_session_id": "sess-001",
                ":result_text": "all done",
                ":cost_usd": 1.5,
                ":num_turns": 10i64,
                ":duration_ms": 5000i64,
                ":ended_at": "2025-01-01T00:05:00Z",
                ":log_file": "/tmp/log.txt",
                ":model": "claude-sonnet-4-6",
                ":plan": "[]",
                ":parent_run_id": "parent-001",
                ":input_tokens": 1000i64,
                ":output_tokens": 2000i64,
                ":cache_read_input_tokens": 500i64,
                ":cache_creation_input_tokens": 100i64,
                ":bot_name": "my-bot",
            },
        )
        .unwrap();

        let run: AgentRun = conn
            .query_row(
                &format!("{AGENT_RUN_SELECT} WHERE id = :id"),
                rusqlite::named_params! { ":id": "run-001" },
                row_to_agent_run,
            )
            .unwrap();

        assert_eq!(run.id, "run-001");
        assert_eq!(run.worktree_id.as_deref(), Some("wt-001"));
        assert_eq!(run.repo_id.as_deref(), Some("repo-001"));
        assert_eq!(run.claude_session_id.as_deref(), Some("sess-001"));
        assert_eq!(run.prompt, "do the thing");
        assert_eq!(run.status, crate::agent::status::AgentRunStatus::Running);
        assert_eq!(run.result_text.as_deref(), Some("all done"));
        assert_eq!(run.cost_usd, Some(1.5));
        assert_eq!(run.num_turns, Some(10));
        assert_eq!(run.duration_ms, Some(5000));
        assert_eq!(run.started_at, "2025-01-01T00:00:00Z");
        assert_eq!(run.ended_at.as_deref(), Some("2025-01-01T00:05:00Z"));
        assert_eq!(run.log_file.as_deref(), Some("/tmp/log.txt"));
        assert_eq!(run.model.as_deref(), Some("claude-sonnet-4-6"));
        // plan is always None (populated separately by caller)
        assert!(run.plan.is_none());
        assert_eq!(run.parent_run_id.as_deref(), Some("parent-001"));
        assert_eq!(run.input_tokens, Some(1000));
        assert_eq!(run.output_tokens, Some(2000));
        assert_eq!(run.cache_read_input_tokens, Some(500));
        assert_eq!(run.cache_creation_input_tokens, Some(100));
        assert_eq!(run.bot_name.as_deref(), Some("my-bot"));
        assert!(run.conversation_id.is_none());
        assert!(run.subprocess_pid.is_none());
    }

    #[test]
    fn row_to_agent_run_handles_null_optionals() {
        let conn = test_db();
        insert_minimal_run(&conn, "run-null");

        let run: AgentRun = conn
            .query_row(
                &format!("{AGENT_RUN_SELECT} WHERE id = ?1"),
                ["run-null"],
                row_to_agent_run,
            )
            .unwrap();

        assert_eq!(run.id, "run-null");
        assert!(run.worktree_id.is_none());
        assert!(run.repo_id.is_none());
        assert!(run.claude_session_id.is_none());
        assert!(run.result_text.is_none());
        assert!(run.cost_usd.is_none());
        assert!(run.num_turns.is_none());
        assert!(run.duration_ms.is_none());
        assert!(run.ended_at.is_none());
        assert!(run.log_file.is_none());
        assert!(run.model.is_none());
        assert!(run.plan.is_none());
        assert!(run.parent_run_id.is_none());
        assert!(run.input_tokens.is_none());
        assert!(run.output_tokens.is_none());
        assert!(run.cache_read_input_tokens.is_none());
        assert!(run.cache_creation_input_tokens.is_none());
        assert!(run.bot_name.is_none());
        assert!(run.conversation_id.is_none());
        assert!(run.subprocess_pid.is_none());
    }

    #[test]
    fn row_to_plan_step_maps_fields() {
        let conn = test_db();
        // Insert a parent run first (FK constraint)
        insert_minimal_run(&conn, "run-step");
        conn.execute(
            "INSERT INTO agent_run_steps (id, run_id, position, description, status, \
             started_at, completed_at) \
             VALUES ('step-1', 'run-step', 0, 'build it', 'completed', \
                     '2025-01-01T00:00:00Z', '2025-01-01T00:01:00Z')",
            [],
        )
        .unwrap();

        let step: PlanStep = conn
            .query_row(
                &format!("{AGENT_RUN_STEPS_SELECT} WHERE id = ?1"),
                ["step-1"],
                row_to_plan_step,
            )
            .unwrap();

        assert_eq!(step.id, Some("step-1".to_string()));
        assert_eq!(step.description, "build it");
        assert!(step.done);
        assert_eq!(step.status, crate::agent::status::StepStatus::Completed);
        assert_eq!(step.position, Some(0));
        assert_eq!(step.started_at.as_deref(), Some("2025-01-01T00:00:00Z"));
        assert_eq!(step.completed_at.as_deref(), Some("2025-01-01T00:01:00Z"));
    }

    #[test]
    fn row_to_agent_run_event_maps_fields() {
        let conn = test_db();
        insert_minimal_run(&conn, "run-evt");
        conn.execute(
            "INSERT INTO agent_run_events (id, run_id, kind, summary, started_at, ended_at, metadata) \
             VALUES ('evt-1', 'run-evt', 'tool_error', 'something broke', \
                     '2025-01-01T00:00:00Z', '2025-01-01T00:00:01Z', '{\"key\":\"val\"}')",
            [],
        )
        .unwrap();

        let event: AgentRunEvent = conn
            .query_row(
                &format!("{AGENT_RUN_EVENTS_SELECT} WHERE id = ?1"),
                ["evt-1"],
                row_to_agent_run_event,
            )
            .unwrap();

        assert_eq!(event.id, "evt-1");
        assert_eq!(event.run_id, "run-evt");
        assert_eq!(event.kind, "tool_error");
        assert_eq!(event.summary, "something broke");
        assert_eq!(event.started_at, "2025-01-01T00:00:00Z");
        assert_eq!(event.ended_at.as_deref(), Some("2025-01-01T00:00:01Z"));
        assert_eq!(event.metadata.as_deref(), Some("{\"key\":\"val\"}"));
    }

    #[test]
    fn row_to_feedback_request_maps_fields() {
        let conn = test_db();
        insert_minimal_run(&conn, "run-fb");

        let options_json = r#"[{"value":"yes","label":"Yes"},{"value":"no","label":"No"}]"#;
        conn.execute(
            "INSERT INTO feedback_requests (id, run_id, prompt, response, status, created_at, \
             responded_at, feedback_type, options_json, timeout_secs) \
             VALUES ('fb-1', 'run-fb', 'approve?', 'yes', 'responded', \
                     '2025-01-01T00:00:00Z', '2025-01-01T00:00:05Z', 'single_select', ?1, 30)",
            [options_json],
        )
        .unwrap();

        let fb: FeedbackRequest = conn
            .query_row(
                &format!("{FEEDBACK_SELECT} WHERE id = ?1"),
                ["fb-1"],
                row_to_feedback_request,
            )
            .unwrap();

        assert_eq!(fb.id, "fb-1");
        assert_eq!(fb.run_id, "run-fb");
        assert_eq!(fb.prompt, "approve?");
        assert_eq!(fb.response.as_deref(), Some("yes"));
        assert_eq!(fb.status, crate::agent::status::FeedbackStatus::Responded);
        assert_eq!(fb.created_at, "2025-01-01T00:00:00Z");
        assert_eq!(fb.responded_at.as_deref(), Some("2025-01-01T00:00:05Z"));
        assert_eq!(
            fb.feedback_type,
            crate::agent::status::FeedbackType::SingleSelect
        );
        let opts = fb.options.unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].value, "yes");
        assert_eq!(opts[0].label, "Yes");
        assert_eq!(opts[1].value, "no");
        assert_eq!(opts[1].label, "No");
        assert_eq!(fb.timeout_secs, Some(30));
    }

    #[test]
    fn row_to_agent_created_issue_maps_fields() {
        let conn = test_db();
        insert_minimal_run(&conn, "run-aci");
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('repo-aci', 'test-repo', '/tmp/repo', 'https://github.com/org/repo', \
                     '/tmp/ws', '2025-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_created_issues (id, agent_run_id, repo_id, source_type, \
             source_id, title, url, created_at) \
             VALUES ('aci-1', 'run-aci', 'repo-aci', 'github', '42', \
                     'Fix the bug', 'https://github.com/org/repo/issues/42', \
                     '2025-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let issue: AgentCreatedIssue = conn
            .query_row(
                &format!("{AGENT_CREATED_ISSUES_SELECT} WHERE id = ?1"),
                ["aci-1"],
                row_to_agent_created_issue,
            )
            .unwrap();

        assert_eq!(issue.id, "aci-1");
        assert_eq!(issue.agent_run_id, "run-aci");
        assert_eq!(issue.repo_id, "repo-aci");
        assert_eq!(issue.source_type, "github");
        assert_eq!(issue.source_id, "42");
        assert_eq!(issue.title, "Fix the bug");
        assert_eq!(issue.url, "https://github.com/org/repo/issues/42");
        assert_eq!(issue.created_at, "2025-01-01T00:00:00Z");
    }

    #[test]
    fn optional_row_returns_none_on_no_rows() {
        let result: rusqlite::Result<String> = Err(rusqlite::Error::QueryReturnedNoRows);
        let opt = optional_row(result).unwrap();
        assert!(opt.is_none());
    }

    #[test]
    fn optional_row_propagates_other_errors() {
        let result: rusqlite::Result<String> =
            Err(rusqlite::Error::InvalidColumnName("bad".to_string()));
        let opt = optional_row(result);
        assert!(opt.is_err());
    }
}
