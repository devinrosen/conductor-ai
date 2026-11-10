use conductor_core::Conductor;
use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{
    get_arg, get_arg_usize, get_bool_arg, pagination_hint, tool_err, tool_ok,
};
use crate::mcp::resources::{
    format_run_detail_with_log, format_run_extra_lines, format_run_summary_line,
    format_run_summary_line_with_repo,
};

/// Append a row (as produced by a summary formatter) plus extra lines and a blank separator.
fn push_row(out: &mut String, row: &str, extras: &str) {
    out.push_str(row.trim_end_matches('\n'));
    out.push('\n');
    out.push_str(extras);
    out.push('\n');
}

/// Bulk-load ticket id -> source id for the distinct tickets the runs reference,
/// in one query. A ticket that no longer exists is simply absent from the map. If
/// the lookup itself fails, the error is logged and every referenced ticket maps to
/// a `<lookup failed: ...>` marker so rows don't silently look like "no ticket".
fn ticket_source_map(
    conn: &rusqlite::Connection,
    runs: &[conductor_core::workflow::ConductorWorkflowRun],
) -> std::collections::HashMap<String, String> {
    let mut ids: Vec<&str> = runs.iter().filter_map(|r| r.ticket_id.as_deref()).collect();
    ids.sort_unstable();
    ids.dedup();
    match conductor_core::tickets::TicketSyncer::new(conn).get_by_ids(&ids) {
        Ok(tickets) => tickets.into_iter().map(|t| (t.id, t.source_id)).collect(),
        Err(e) => {
            tracing::warn!("failed to resolve tickets for run list: {e}");
            ids.into_iter()
                .map(|id| (id.to_string(), format!("<lookup failed: {e}>")))
                .collect()
        }
    }
}

/// Bulk-load worktree id -> (slug, branch) for the distinct worktrees the runs
/// reference, in one query. Shared by the per-repo and cross-repo paths.
fn worktree_map(
    conn: &rusqlite::Connection,
    config: &conductor_core::config::Config,
    runs: &[conductor_core::workflow::ConductorWorkflowRun],
) -> conductor_core::error::Result<WorktreeMap> {
    let mut ids: Vec<&str> = runs
        .iter()
        .filter_map(|r| r.worktree_id.as_deref())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let worktrees =
        conductor_core::worktree::WorktreeManager::new(conn, config).get_by_ids(&ids)?;
    Ok(worktrees
        .into_iter()
        .map(|wt| (wt.id, (wt.slug, wt.branch)))
        .collect())
}

type WorktreeMap = std::collections::HashMap<String, (String, String)>;

/// A run's worktree (slug, branch) from the bulk map, if it has one.
fn run_worktree<'a>(
    map: &'a WorktreeMap,
    run: &conductor_core::workflow::ConductorWorkflowRun,
) -> (Option<&'a str>, Option<&'a str>) {
    run.worktree_id
        .as_deref()
        .and_then(|id| map.get(id))
        .map(|(s, b)| (s.as_str(), b.as_str()))
        .unzip()
}

/// A run's ticket source id (or lookup-failed marker) from the bulk map, if any.
fn run_ticket<'a>(
    map: &'a std::collections::HashMap<String, String>,
    run: &conductor_core::workflow::ConductorWorkflowRun,
) -> Option<&'a str> {
    run.ticket_id
        .as_deref()
        .and_then(|id| map.get(id))
        .map(String::as_str)
}

pub(super) fn tool_list_runs(
    conductor: &Conductor,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::WorkflowRunStatus;
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = get_arg(args, "repo");
    let worktree_slug = get_arg(args, "worktree");
    let status_str = get_arg(args, "status");
    let include_inactive = get_bool_arg(args, "include_inactive");
    let top_level_only = get_bool_arg(args, "top_level_only");

    // worktree filter is repo-scoped and meaningless without a repo
    if worktree_slug.is_some() && repo_slug.is_none() {
        return tool_err("worktree filter requires a repo argument");
    }

    let status: Option<WorkflowRunStatus> = match status_str {
        Some(s) => match s.parse::<WorkflowRunStatus>() {
            Ok(v) => Some(v),
            Err(e) => return tool_err(e),
        },
        None => None,
    };

    let limit: usize = get_arg(args, "limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let offset: usize = get_arg(args, "offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let conn = &conductor.conn;
    let config = &conductor.config;

    if let Some(slug) = repo_slug {
        // Per-repo path (existing behaviour)
        let repo_mgr = RepoManager::new(conn, config);
        let repo = match repo_mgr.get_by_slug(slug) {
            Ok(r) => r,
            Err(e) => return tool_err(e),
        };

        let runs = if let Some(wt_slug) = worktree_slug {
            let wt_mgr = WorktreeManager::new(conn, config);
            let wt = match wt_mgr.get_by_slug_or_branch(&repo.id, wt_slug) {
                Ok(w) => w,
                Err(e) => return tool_err(e),
            };
            match conductor_core::workflow::list_workflow_runs_filtered_paginated(
                conn,
                &wt.id,
                status,
                limit,
                offset,
                top_level_only,
            ) {
                Ok(r) => r,
                Err(e) => return tool_err(e),
            }
        } else {
            match conductor_core::workflow::list_workflow_runs_by_repo_id_filtered(
                conn,
                &repo.id,
                limit,
                offset,
                status,
                include_inactive,
                top_level_only,
            ) {
                Ok(r) => r,
                Err(e) => return tool_err(e),
            }
        };

        if runs.is_empty() {
            return tool_ok(format!("No workflow runs for {slug}."));
        }

        let wt_map = match worktree_map(conn, config, &runs) {
            Ok(m) => m,
            Err(e) => return tool_err(e),
        };

        let ticket_map = ticket_source_map(conn, &runs);
        let mut out = String::new();
        for run in &runs {
            let (wt_slug, wt_branch) = run_worktree(&wt_map, run);
            push_row(
                &mut out,
                &format_run_summary_line(run, wt_slug, wt_branch),
                &format_run_extra_lines(run, run_ticket(&ticket_map, run)),
            );
        }
        if runs.len() == limit {
            out.push_str(&pagination_hint(offset, runs.len(), limit));
        }
        tool_ok(out)
    } else {
        // Cross-repo path: return runs across all registered repos
        let repo_mgr = RepoManager::new(conn, config);
        let repos = match repo_mgr.list() {
            Ok(r) => r,
            Err(e) => return tool_err(e),
        };
        let repo_map: std::collections::HashMap<String, String> =
            repos.into_iter().map(|r| (r.id, r.slug)).collect();

        let runs = match conductor_core::workflow::list_all_workflow_runs_filtered_paginated(
            conn,
            status,
            limit,
            offset,
            include_inactive,
            top_level_only,
        ) {
            Ok(r) => r,
            Err(e) => return tool_err(e),
        };

        if runs.is_empty() {
            return tool_ok("No workflow runs.".to_string());
        }
        let wt_map = match worktree_map(conn, config, &runs) {
            Ok(m) => m,
            Err(e) => return tool_err(e),
        };
        let ticket_map = ticket_source_map(conn, &runs);
        let mut out = String::new();
        for run in &runs {
            let slug_for_run = run
                .repo_id
                .as_deref()
                .and_then(|id| repo_map.get(id).map(|s| s.as_str()));
            let (wt_slug, wt_branch) = run_worktree(&wt_map, run);
            push_row(
                &mut out,
                &format_run_summary_line_with_repo(run, slug_for_run, wt_slug, wt_branch),
                &format_run_extra_lines(run, run_ticket(&ticket_map, run)),
            );
        }
        if runs.len() == limit {
            out.push_str(&pagination_hint(offset, runs.len(), limit));
        }
        tool_ok(out)
    }
}

pub(super) fn tool_get_run(
    conductor: &Conductor,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    let run_id = require_arg!(args, "run_id");
    let conn = &conductor.conn;
    let config = &conductor.config;
    let run = match conductor_core::workflow::get_workflow_run(conn, run_id) {
        Ok(Some(r)) => r,
        Ok(None) => return tool_err(format!("Workflow run {run_id} not found")),
        Err(e) => return tool_err(e),
    };
    let steps = match conductor_core::workflow::get_workflow_steps(conn, run_id) {
        Ok(s) => s,
        Err(e) => return tool_err(e),
    };
    let claude_dir = config.general.resolve_optional_claude_dir();
    let mut out = format_run_detail_with_log(conn, &run, &steps, claude_dir.as_deref());
    if let Some(warning) = stale_process_warning() {
        out.push_str("\n\n");
        out.push_str(&warning);
    }
    tool_ok(out)
}

/// Warning text listing conductor processes running an older build than their
/// binary on disk. This MCP server's own staleness comes first.
fn stale_process_warning() -> Option<String> {
    use conductor_core::process_registry::{own_staleness, stale_processes, ProcessKind};
    build_stale_warning(stale_processes(), own_staleness(ProcessKind::Mcp))
}

fn build_stale_warning(
    mut stale: Vec<conductor_core::process_registry::StaleInfo>,
    own: Option<conductor_core::process_registry::StaleInfo>,
) -> Option<String> {
    stale.retain(|s| !s.is_self);
    if let Some(own) = own {
        stale.insert(0, own);
    }
    let body = conductor_core::process_registry::format_stale_warning(&stale)?;
    Some(format!(
        "⚠ Stale conductor processes (running an older build than their binary; \
         this MCP server is listed first if it is one of them):\n{body}"
    ))
}

/// Default and maximum `timeout_secs` for `conductor_wait_run`.
pub(super) const WAIT_DEFAULT_TIMEOUT_SECS: usize = 600;
pub(super) const WAIT_MAX_TIMEOUT_SECS: usize = 3600;
/// Maximum `timeout_secs` when the client sent no progress token. Claude Code aborts an MCP
/// call that is silent for 1800s, and without a token the server cannot send progress, so
/// stay well below that.
pub(super) const WAIT_MAX_TIMEOUT_NO_PROGRESS_SECS: usize = 1500;

/// Returns the timeout to use, plus the cap when one applied.
pub(super) fn effective_timeout(requested: usize, has_progress: bool) -> (usize, Option<usize>) {
    let cap = if has_progress {
        WAIT_MAX_TIMEOUT_SECS
    } else {
        WAIT_MAX_TIMEOUT_NO_PROGRESS_SECS
    };
    if requested > cap {
        (cap, Some(cap))
    } else {
        (requested, None)
    }
}

pub(super) fn tool_wait_run(
    conductor: &Conductor,
    args: &serde_json::Map<String, Value>,
    progress: Option<&dyn Fn(String)>,
) -> CallToolResult {
    use conductor_core::workflow::{
        format_wait_progress, format_wait_summary, wait_for_workflow_run_with_progress, WaitOptions,
    };
    use std::time::Duration;

    let run_id = require_arg!(args, "run_id");
    let requested = get_arg_usize(args, "timeout_secs").unwrap_or(WAIT_DEFAULT_TIMEOUT_SECS);
    let (timeout_secs, capped) = effective_timeout(requested, progress.is_some());
    let opts = WaitOptions::with_overrides(
        Some(Duration::from_secs(timeout_secs as u64)),
        get_arg_usize(args, "stall_after_secs").map(|s| Duration::from_secs(s as u64)),
    );
    let mut on_progress = |p: &conductor_core::workflow::WaitProgress| {
        if let Some(send) = progress {
            send(format_wait_progress(p));
        }
    };
    match wait_for_workflow_run_with_progress(&conductor.conn, run_id, &opts, &mut on_progress) {
        Ok(summary) => {
            let mut text = format_wait_summary(&summary);
            if let Some(cap) = capped {
                if progress.is_some() {
                    text.push_str(&format!("note: timeout_secs was capped at {cap}s.\n"));
                } else {
                    text.push_str(&format!(
                        "note: timeout_secs was capped at {cap}s because the client sent no \
                         progress token and clients abort silent calls after 30 minutes. \
                         If the outcome is timeout, call conductor_wait_run again.\n"
                    ));
                }
            }
            tool_ok(text)
        }
        Err(e) => tool_err(e),
    }
}

pub(super) fn tool_cancel_run(
    conductor: &Conductor,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    let run_id = require_arg!(args, "run_id");
    let conn = &conductor.conn;
    let run = match conductor_core::workflow::get_workflow_run(conn, run_id) {
        Ok(Some(r)) => r,
        Ok(None) => return tool_err(format!("Workflow run not found: {run_id}")),
        Err(e) => return tool_err(e),
    };
    match conductor_core::workflow::cancel_run(
        conn,
        run_id,
        "Cancelled via MCP conductor_cancel_run",
    ) {
        Ok(()) => tool_ok(format!(
            "Workflow run {} ('{}') cancelled.",
            run_id, run.workflow_name
        )),
        Err(e) => tool_err(e),
    }
}

pub(super) fn tool_resume_run(
    conductor: &Conductor,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::workflow::validate_resume_preconditions;

    let run_id = require_arg!(args, "run_id");
    let from_step = get_arg(args, "from_step").map(str::to_string);
    let model = get_arg(args, "model").map(str::to_string);

    let conn = &conductor.conn;
    let run = match conductor_core::workflow::get_workflow_run(conn, run_id) {
        Ok(Some(r)) => r,
        Ok(None) => return tool_err(format!("Workflow run not found: {run_id}")),
        Err(e) => return tool_err(e),
    };

    if let Err(e) = validate_resume_preconditions(&run.status, false, from_step.as_deref()) {
        return tool_err(e);
    }

    // The resume runs in its own process on the installed binary (#3124).
    let spec = crate::background::LaunchSpec::Resume {
        run_id: run_id.to_string(),
        from_step,
        model,
        db_path: Some(conductor.conn_db_path()),
    };
    let launched = match crate::mcp::launch::launch(&spec) {
        Ok(l) => l,
        Err(e) => return tool_err(format!("Failed to resume workflow run: {e:#}")),
    };

    tool_ok(format!(
        "Workflow run {} ('{}') is resuming. Use conductor_get_run to check progress.{}",
        run_id,
        run.workflow_name,
        launched.build_line()
    ))
}

pub(super) fn tool_get_step_log(
    conductor: &Conductor,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::workflow::{render_step_log, resolve_step_log_target, StepLogMode};

    let run_id = require_arg!(args, "run_id");
    let step_name = require_arg!(args, "step_name");

    let mode = match StepLogMode::parse(get_arg(args, "mode"), get_arg_usize(args, "lines")) {
        Ok(m) => m,
        Err(e) => return tool_err(e),
    };
    let target = match resolve_step_log_target(&conductor.conn, run_id, step_name) {
        Ok(t) => t,
        Err(e) => return tool_err(e),
    };
    match render_step_log(&target, mode) {
        Ok(text) => tool_ok(text),
        Err(e) => tool_err(e),
    }
}

/// Split a comma-separated list of run ids, dropping blanks.
fn split_run_ids(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

pub(super) fn tool_run_stats(
    conductor: &Conductor,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    let run_ids = split_run_ids(require_arg!(args, "run_id"));
    if run_ids.is_empty() {
        return tool_err("run_id must contain at least one workflow run id");
    }
    let compare_ids = get_arg(args, "compare_run_id")
        .map(split_run_ids)
        .unwrap_or_default();
    let as_json = args.get("json").and_then(Value::as_bool).unwrap_or(false);

    match conductor_core::workflow::run_stats::render_run_stats(
        &conductor.conn,
        &run_ids,
        &compare_ids,
        as_json,
    ) {
        Ok(t) => tool_ok(t),
        Err(e) => tool_err(e),
    }
}

/// `conductor_get_review`: a review-pr run's findings, scope, reviewers and posted review.
pub(super) fn tool_get_review(
    conductor: &Conductor,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::workflow::review_report::{find_review_run_by_slug, load_review_report};

    let run_id = get_arg(args, "run_id")
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let repo_slug = get_arg(args, "repo")
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let pr = match args.get("pr") {
        None | Some(Value::Null) => None,
        Some(_) => match get_arg_usize(args, "pr") {
            Some(n) if n > 0 => Some(n as u64),
            _ => return tool_err("pr must be a positive PR number"),
        },
    };
    let report = find_review_run_by_slug(&conductor.conn, &conductor.config, run_id, repo_slug, pr)
        .and_then(|id| load_review_report(&conductor.conn, &id));
    match report {
        Ok(r) => match serde_json::to_string_pretty(&r) {
            Ok(t) => tool_ok(t),
            Err(e) => tool_err(e),
        },
        Err(e) => tool_err(e),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{create_list_run, make_test_conductor};
    use super::*;
    use serde_json::Value;

    fn stale_info(pid: u32, is_self: bool) -> conductor_core::process_registry::StaleInfo {
        use conductor_core::process_registry::{ProcessKind, ProcessRecord, StaleInfo};
        StaleInfo {
            record: ProcessRecord {
                pid,
                kind: ProcessKind::Tui,
                exe: "/x/conductor".into(),
                git_sha: "abc".into(),
                commit_time: "unknown".into(),
                version: "0".into(),
                started_at: "2026-01-01T00:00:00+00:00".into(),
                parent_pid: None,
                cwd: None,
            },
            exe_modified_at: None,
            disk_git_sha: None,
            reason: "binary removed".into(),
            is_self,
        }
    }

    #[test]
    fn build_stale_warning_none_when_nothing_stale() {
        assert!(build_stale_warning(vec![], None).is_none());
        // A self entry from the registry alone is dropped.
        assert!(build_stale_warning(vec![stale_info(1, true)], None).is_none());
    }

    #[test]
    fn build_stale_warning_lists_own_first_and_drops_self_duplicates() {
        let text = build_stale_warning(
            vec![stale_info(10, false), stale_info(20, true)],
            Some(stale_info(20, true)),
        )
        .unwrap();
        assert!(text.starts_with("⚠ Stale conductor processes"));
        let own = text.find("pid 20").unwrap();
        let other = text.find("pid 10").unwrap();
        assert!(own < other);
        assert_eq!(text.matches("pid 20").count(), 1);
    }

    fn empty_args() -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }

    fn args_with(key: &str, val: &str) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert(key.to_string(), Value::String(val.to_string()));
        m
    }

    /// Helper: create a workflow run in the given status. Returns the run id.
    fn make_workflow_run_with_status(
        db_path: &std::path::Path,
        status: conductor_core::workflow::WorkflowRunStatus,
    ) -> String {
        use conductor_core::db::open_database;

        let conn = open_database(db_path).expect("open db");
        let agent_mgr = conductor_core::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(None, "workflow", None)
            .expect("create agent run");
        let run = conductor_core::workflow::create_workflow_run(
            &conn, "test-wf", None, &parent.id, false, "manual", None,
        )
        .expect("create workflow run");
        if !matches!(status, conductor_core::workflow::WorkflowRunStatus::Pending) {
            conductor_core::workflow::update_workflow_status(&conn, &run.id, status, None, None)
                .expect("update status");
        }
        run.id.clone()
    }

    /// Helper: create an agent run and set its log_file in one shot. Returns the run id.
    fn create_run_with_log(conn: &rusqlite::Connection, log_path: &str) -> String {
        use conductor_core::agent::AgentManager;
        let mgr = AgentManager::new(conn);
        let run = mgr
            .create_run(None, "agent", None)
            .expect("create agent run");
        mgr.update_run_log_file(&run.id, log_path)
            .expect("set log_file");
        run.id
    }

    /// Helper: create a workflow run with one step. Returns (run_id, step_id).
    fn make_run_with_step(db_path: &std::path::Path, step_name: &str) -> (String, String) {
        use conductor_core::db::open_database;

        let conn = open_database(db_path).expect("open db");
        let agent_mgr = conductor_core::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(None, "workflow", None)
            .expect("create parent run");
        let run = conductor_core::workflow::create_workflow_run(
            &conn, "test-wf", None, &parent.id, false, "manual", None,
        )
        .expect("create workflow run");
        let step_id =
            conductor_core::workflow::insert_step(&conn, &run.id, step_name, "actor", false, 0, 0)
                .expect("insert step");
        (run.id.clone(), step_id)
    }

    #[test]
    fn test_dispatch_get_run_missing_run_id_arg() {
        let (_f, conductor) = make_test_conductor();
        let result = tool_get_run(&conductor, &empty_args());
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_get_run_nonexistent_run() {
        let (_f, conductor) = make_test_conductor();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_get_run(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_wait_run_missing_run_id_arg() {
        let (_f, conductor) = make_test_conductor();
        let result = tool_wait_run(&conductor, &empty_args(), None);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_wait_run_nonexistent_run() {
        let (_f, conductor) = make_test_conductor();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_wait_run(&conductor, &args, None);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_wait_run_completed_returns_immediately() {
        let (_f, conductor) = make_test_conductor();
        let parent = conductor_core::agent::AgentManager::new(&conductor.conn)
            .create_run(None, "workflow", None)
            .expect("create parent run");
        let run = conductor_core::workflow::create_workflow_run(
            &conductor.conn,
            "test-wf",
            None,
            &parent.id,
            false,
            "manual",
            None,
        )
        .expect("create workflow run");
        conductor
            .conn
            .execute(
                "UPDATE workflow_runs SET status = 'completed' WHERE id = ?1",
                [&run.id],
            )
            .unwrap();
        let result = tool_wait_run(&conductor, &args_with("run_id", &run.id), None);
        assert_ne!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.starts_with("outcome: completed"), "got: {text}");
    }

    /// Create a workflow run in `status` with the given `started_at` / `lease_until` SQL.
    fn make_wait_run(
        conductor: &Conductor,
        status: &str,
        started_at: &str,
        lease_until: &str,
    ) -> String {
        let parent = conductor_core::agent::AgentManager::new(&conductor.conn)
            .create_run(None, "workflow", None)
            .expect("create parent run");
        let run = conductor_core::workflow::create_workflow_run(
            &conductor.conn,
            "test-wf",
            None,
            &parent.id,
            false,
            "manual",
            None,
        )
        .expect("create workflow run");
        conductor
            .conn
            .execute(
                &format!(
                    "UPDATE workflow_runs SET status = ?1, started_at = {started_at}, \
                     lease_until = {lease_until} WHERE id = ?2"
                ),
                [status, run.id.as_str()],
            )
            .unwrap();
        run.id.clone()
    }

    fn result_text(result: &CallToolResult) -> String {
        result.content[0]
            .as_text()
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    fn wait_args(run_id: &str, extra: &[(&str, u64)]) -> serde_json::Map<String, Value> {
        let mut m = args_with("run_id", run_id);
        for (k, v) in extra {
            m.insert((*k).to_string(), Value::from(*v));
        }
        m
    }

    #[test]
    fn test_wait_run_timeout_outcome() {
        let (_f, conductor) = make_test_conductor();
        let id = make_wait_run(
            &conductor,
            "running",
            "strftime('%Y-%m-%dT%H:%M:%SZ','now')",
            "datetime('now','+60 seconds')",
        );
        let result = tool_wait_run(&conductor, &wait_args(&id, &[("timeout_secs", 0)]), None);
        assert_ne!(result.is_error, Some(true));
        let text = result_text(&result);
        assert!(text.starts_with("outcome: timeout"), "got: {text}");
        assert!(!text.contains("capped"));
    }

    #[test]
    fn test_wait_run_stall_after_override_reports_stalled() {
        let (_f, conductor) = make_test_conductor();
        let id = make_wait_run(
            &conductor,
            "running",
            "strftime('%Y-%m-%dT%H:%M:%SZ','now','-1 hours')",
            "datetime('now','-300 seconds')",
        );
        let result = tool_wait_run(
            &conductor,
            &wait_args(&id, &[("timeout_secs", 30), ("stall_after_secs", 10)]),
            None,
        );
        let text = result_text(&result);
        assert!(text.starts_with("outcome: stalled"), "got: {text}");
        assert!(text.contains("conductor workflow resume"));
    }

    #[test]
    fn test_wait_run_stall_override_larger_than_age_does_not_stall() {
        let (_f, conductor) = make_test_conductor();
        let id = make_wait_run(
            &conductor,
            "running",
            "strftime('%Y-%m-%dT%H:%M:%SZ','now','-1 hours')",
            "datetime('now','-300 seconds')",
        );
        let result = tool_wait_run(
            &conductor,
            &wait_args(&id, &[("timeout_secs", 0), ("stall_after_secs", 100000)]),
            None,
        );
        assert!(result_text(&result).starts_with("outcome: timeout"));
    }

    #[test]
    fn test_wait_run_caps_timeout_and_notes_it() {
        let (_f, conductor) = make_test_conductor();
        let id = make_wait_run(
            &conductor,
            "completed",
            "strftime('%Y-%m-%dT%H:%M:%SZ','now')",
            "NULL",
        );
        let over = (WAIT_MAX_TIMEOUT_NO_PROGRESS_SECS + 1) as u64;
        let result = tool_wait_run(&conductor, &wait_args(&id, &[("timeout_secs", over)]), None);
        let text = result_text(&result);
        assert!(text.starts_with("outcome: completed"), "got: {text}");
        assert!(
            text.contains(&format!("capped at {WAIT_MAX_TIMEOUT_NO_PROGRESS_SECS}s")),
            "got: {text}"
        );
    }

    #[test]
    fn test_wait_run_with_progress_uses_higher_cap_and_short_note() {
        let (_f, conductor) = make_test_conductor();
        let id = make_wait_run(
            &conductor,
            "completed",
            "strftime('%Y-%m-%dT%H:%M:%SZ','now')",
            "NULL",
        );
        let messages = std::cell::RefCell::new(Vec::<String>::new());
        let sink = |m: String| messages.borrow_mut().push(m);
        let over = (WAIT_MAX_TIMEOUT_SECS + 1) as u64;
        let result = tool_wait_run(
            &conductor,
            &wait_args(&id, &[("timeout_secs", over)]),
            Some(&sink),
        );
        let text = result_text(&result);
        assert!(text.starts_with("outcome: completed"), "got: {text}");
        assert!(
            text.contains(&format!("capped at {WAIT_MAX_TIMEOUT_SECS}s")),
            "got: {text}"
        );
        assert!(!text.contains("progress token"), "got: {text}");
        // A run that is already terminal never reports progress.
        assert!(messages.borrow().is_empty());
    }

    #[test]
    fn test_wait_run_with_progress_below_cap_has_no_note() {
        let (_f, conductor) = make_test_conductor();
        let id = make_wait_run(
            &conductor,
            "completed",
            "strftime('%Y-%m-%dT%H:%M:%SZ','now')",
            "NULL",
        );
        // Above the no-progress cap but within the progress cap: no note.
        let t = (WAIT_MAX_TIMEOUT_NO_PROGRESS_SECS + 1) as u64;
        let result = tool_wait_run(
            &conductor,
            &wait_args(&id, &[("timeout_secs", t)]),
            Some(&|_m: String| {}),
        );
        assert!(!result_text(&result).contains("capped"));
    }

    #[test]
    fn test_effective_timeout() {
        assert_eq!(effective_timeout(3600, false), (1500, Some(1500)));
        assert_eq!(effective_timeout(600, false), (600, None));
        assert_eq!(effective_timeout(3600, true), (3600, None));
        assert_eq!(effective_timeout(5000, true), (3600, Some(3600)));
    }

    #[test]
    fn test_wait_run_schema_defaults_match_constants() {
        let tool = super::super::conductor_tools()
            .into_iter()
            .find(|t| t.name == "conductor_wait_run")
            .expect("conductor_wait_run registered");
        let schema = serde_json::to_string(&*tool.input_schema).unwrap();
        assert!(schema.contains(&format!(
            "default {WAIT_DEFAULT_TIMEOUT_SECS}, max {WAIT_MAX_TIMEOUT_SECS}"
        )));
        let stall = conductor_core::workflow::WaitOptions::default()
            .stall_after
            .as_secs();
        assert!(schema.contains(&format!("(default {stall})")));
    }

    #[test]
    fn test_dispatch_list_runs_missing_repo_arg() {
        // repo is now optional — empty-args call should succeed (empty result)
        let (_f, conductor) = make_test_conductor();
        let result = tool_list_runs(&conductor, &empty_args());
        assert_ne!(
            result.is_error,
            Some(true),
            "empty repo should succeed, got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
    }

    #[test]
    fn test_dispatch_list_runs_worktree_without_repo_fails() {
        let (_f, conductor) = make_test_conductor();
        let args = args_with("worktree", "some-wt");
        let result = tool_list_runs(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("worktree filter requires a repo"),
            "got: {text}"
        );
    }

    #[test]
    fn test_dispatch_list_runs_cross_repo() {
        use conductor_core::db::open_database;

        let (_f, conductor) = make_test_conductor();
        {
            let conn = open_database(_f.path()).expect("open db");

            // Register two repos (make_test_conductor only runs migrations, no seed data)
            conn.execute(
                "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
                 VALUES ('r1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
                 VALUES ('r2', 'other-repo', '/tmp/other', 'https://github.com/test/other.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
                [],
            ).unwrap();
            // Add active worktrees for both repos
            conn.execute(
                "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
                 VALUES ('w1', 'r1', 'feat-test', 'feat/test', '/tmp/ws/feat-test', 'active', '2024-01-01T00:00:00Z')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
                 VALUES ('w2', 'r2', 'feat-other', 'feat/other', '/tmp/ws2/other', 'active', '2024-01-01T00:00:00Z')",
                [],
            ).unwrap();

            create_list_run(&conn, "flow-a", Some("w1"), Some("r1"));
            create_list_run(&conn, "flow-b", Some("w2"), Some("r2"));
        }

        let result = tool_list_runs(&conductor, &empty_args());
        assert_ne!(
            result.is_error,
            Some(true),
            "cross-repo list should succeed, got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("test-repo"),
            "should include test-repo slug, got: {text}"
        );
        assert!(
            text.contains("other-repo"),
            "should include other-repo slug, got: {text}"
        );
        assert!(
            text.contains("flow-a"),
            "should include flow-a, got: {text}"
        );
        assert!(
            text.contains("flow-b"),
            "should include flow-b, got: {text}"
        );
    }

    #[test]
    fn test_list_workflow_runs_by_repo_id_empty() {
        use conductor_core::db::open_database;

        let (_f, _conductor) = make_test_conductor();
        let conn = open_database(_f.path()).expect("open db");
        let runs = conductor_core::workflow::list_workflow_runs_by_repo_id(
            &conn,
            "nonexistent-repo-id",
            50,
            0,
        )
        .expect("query should succeed");
        assert!(runs.is_empty(), "expected no runs for unknown repo");
    }

    #[test]
    fn test_list_workflow_runs_by_repo_id_scoped() {
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, _conductor) = make_test_conductor();
        let conn = open_database(_f.path()).expect("open db");
        let config = Config::default();
        let repo_mgr = RepoManager::new(&conn, &config);
        let repo_a = repo_mgr
            .register("repo-a", "/tmp/repo-a", "https://github.com/x/a", None)
            .expect("register repo-a");
        let repo_b = repo_mgr
            .register("repo-b", "/tmp/repo-b", "https://github.com/x/b", None)
            .expect("register repo-b");

        let _run_a = create_list_run(&conn, "wf-a", None, Some(&repo_a.id));
        let _run_b = create_list_run(&conn, "wf-b", None, Some(&repo_b.id));

        let runs_a =
            conductor_core::workflow::list_workflow_runs_by_repo_id(&conn, &repo_a.id, 50, 0)
                .expect("query A");
        let runs_b =
            conductor_core::workflow::list_workflow_runs_by_repo_id(&conn, &repo_b.id, 50, 0)
                .expect("query B");

        assert_eq!(runs_a.len(), 1, "expected 1 run for repo-a");
        assert_eq!(runs_a[0].workflow_name, "wf-a");
        assert_eq!(runs_b.len(), 1, "expected 1 run for repo-b");
        assert_eq!(runs_b[0].workflow_name, "wf-b");
    }

    #[test]
    fn test_dispatch_cancel_run_missing_arg() {
        let (_f, conductor) = make_test_conductor();
        let result = tool_cancel_run(&conductor, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_cancel_run_not_found() {
        let (_f, conductor) = make_test_conductor();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_cancel_run(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("not found"), "got: {text}");
    }

    #[test]
    fn test_dispatch_cancel_run_already_completed() {
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, conductor) = make_test_conductor();
        let run_id = make_workflow_run_with_status(_f.path(), WorkflowRunStatus::Completed);
        let args = args_with("run_id", &run_id);
        let result = tool_cancel_run(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("terminal state"), "got: {text}");
    }

    #[test]
    fn test_dispatch_cancel_run_already_failed() {
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, conductor) = make_test_conductor();
        let run_id = make_workflow_run_with_status(_f.path(), WorkflowRunStatus::Failed);
        let args = args_with("run_id", &run_id);
        let result = tool_cancel_run(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_cancel_run_already_cancelled() {
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, conductor) = make_test_conductor();
        let run_id = make_workflow_run_with_status(_f.path(), WorkflowRunStatus::Cancelled);
        let args = args_with("run_id", &run_id);
        let result = tool_cancel_run(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_cancel_run_running() {
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, conductor) = make_test_conductor();
        let run_id = make_workflow_run_with_status(_f.path(), WorkflowRunStatus::Running);
        let args = args_with("run_id", &run_id);
        let result = tool_cancel_run(&conductor, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "cancel_run should succeed; got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("cancelled"), "got: {text}");

        // Verify the run status was updated in the DB.
        let conn = open_database(_f.path()).expect("open db");
        let run = conductor_core::workflow::get_workflow_run(&conn, &run_id)
            .expect("query")
            .expect("run exists");
        assert_eq!(run.status, WorkflowRunStatus::Cancelled);
        assert_eq!(
            run.result_summary.as_deref(),
            Some("Cancelled via MCP conductor_cancel_run")
        );
    }

    #[test]
    fn test_dispatch_resume_run_missing_arg() {
        let (_f, conductor) = make_test_conductor();
        let result = tool_resume_run(&conductor, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_resume_run_not_found() {
        let (_f, conductor) = make_test_conductor();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_resume_run(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("not found"), "got: {text}");
    }

    #[test]
    fn test_dispatch_resume_run_already_running() {
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, conductor) = make_test_conductor();
        let run_id = make_workflow_run_with_status(_f.path(), WorkflowRunStatus::Running);
        let args = args_with("run_id", &run_id);
        let result = tool_resume_run(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("already running"), "got: {text}");
    }

    #[test]
    fn test_dispatch_resume_run_already_completed() {
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, conductor) = make_test_conductor();
        let run_id = make_workflow_run_with_status(_f.path(), WorkflowRunStatus::Completed);
        let args = args_with("run_id", &run_id);
        let result = tool_resume_run(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Cannot resume a completed"), "got: {text}");
    }

    #[test]
    fn test_dispatch_resume_run_already_cancelled() {
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, conductor) = make_test_conductor();
        let run_id = make_workflow_run_with_status(_f.path(), WorkflowRunStatus::Cancelled);
        let args = args_with("run_id", &run_id);
        let result = tool_resume_run(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("cancelled"), "got: {text}");
    }

    #[test]
    fn test_dispatch_resume_run_failed() {
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, conductor) = make_test_conductor();
        let run_id = make_workflow_run_with_status(_f.path(), WorkflowRunStatus::Failed);
        let args = args_with("run_id", &run_id);
        let result = tool_resume_run(&conductor, &args);
        // Status validation passes for Failed runs — any error must come from setup
        // (e.g. missing snapshot), not from the status check.
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            !text.contains("already running"),
            "should not get 'already running' for a Failed run; got: {text}"
        );
        assert!(
            !text.contains("Cannot resume a completed"),
            "should not get 'completed' error for a Failed run; got: {text}"
        );
        assert!(
            !text.contains("Cannot resume a cancelled"),
            "should not get 'cancelled' error for a Failed run; got: {text}"
        );
        // Preconditions pass, so it reaches the launch, which fails in tests
        // (the harness binary is never exec'd as `conductor`).
        assert!(
            text.contains("Failed to resume workflow run"),
            "got: {text}"
        );
    }

    #[test]
    fn test_list_runs_includes_worktree_slug() {
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, conductor) = make_test_conductor();
        let conn = open_database(_f.path()).expect("open db");
        let config = Config::default();

        // Register a repo.
        let repo = RepoManager::new(&conn, &config)
            .register(
                "slug-test-repo",
                "/tmp/slug-test-repo",
                "https://github.com/x/y",
                None,
            )
            .expect("register repo");

        // Insert a worktree row directly (avoids git subprocess calls).
        let wt_id = "01JTEST0000000000000000001";
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'active', datetime('now'))",
            rusqlite::params![
                wt_id,
                repo.id,
                "feat-my-feature",
                "feat/my-feature",
                "/tmp/wt"
            ],
        )
        .expect("insert worktree");

        // Create a workflow run linked to both the worktree and the repo.
        create_list_run(&conn, "my-wf", Some(wt_id), Some(&repo.id));

        // Call tool_list_runs and verify worktree_slug appears in output.
        let args = args_with("repo", "slug-test-repo");
        let result = tool_list_runs(&conductor, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "list_runs should succeed; got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("worktree_slug: feat-my-feature"),
            "expected worktree_slug in output, got: {text}"
        );
        assert!(
            text.contains("worktree_branch: feat/my-feature"),
            "expected worktree_branch in output, got: {text}"
        );
    }

    #[test]
    fn test_list_runs_shows_child_ended_at_and_top_level_only() {
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, conductor) = make_test_conductor();
        let conn = open_database(_f.path()).expect("open db");
        let config = Config::default();
        let repo = RepoManager::new(&conn, &config)
            .register(
                "child-repo",
                "/tmp/child-repo",
                "https://github.com/x/y",
                None,
            )
            .expect("register repo");

        let ids: Vec<String> = ["parent-wf", "child-wf"]
            .iter()
            .map(|name| create_list_run(&conn, name, None, Some(&repo.id)))
            .collect();
        conn.execute(
            "UPDATE workflow_runs SET parent_workflow_run_id = ?1, ended_at = '2026-01-02T03:04:05Z', \
             inputs = '{\"pr_number\":\"42\"}', target_label = 'x/y#42' WHERE id = ?2",
            rusqlite::params![ids[0], ids[1]],
        )
        .expect("update child");

        let all = result_text(&tool_list_runs(
            &conductor,
            &args_with("repo", "child-repo"),
        ));
        assert!(all.contains("child: true"), "got: {all}");
        assert!(
            all.contains(&format!("parent_run_id: {}", ids[0])),
            "got: {all}"
        );
        assert!(all.contains("ended_at: 2026-01-02T03:04:05Z"), "got: {all}");
        assert!(all.contains("pr: 42"), "got: {all}");
        assert!(all.contains("target: x/y#42"), "got: {all}");
        assert!(all.contains("child-wf") && all.contains("parent-wf"));

        let mut args = args_with("repo", "child-repo");
        args.insert("top_level_only".into(), Value::Bool(true));
        let top = result_text(&tool_list_runs(&conductor, &args));
        assert!(top.contains("parent-wf"), "got: {top}");
        assert!(!top.contains("child-wf"), "got: {top}");
        assert!(!top.contains("child: true"), "got: {top}");
    }

    /// Open a connection to the conductor's database with
    /// foreign keys off, so tests can plant dangling references and break tables.
    fn lenient_conn(file: &tempfile::NamedTempFile) -> rusqlite::Connection {
        let conn = conductor_core::db::open_database(file.path()).expect("open db");
        conn.execute_batch("PRAGMA foreign_keys = OFF;")
            .expect("fk off");
        conn
    }

    #[test]
    fn test_list_runs_omits_ticket_line_for_deleted_ticket() {
        let (f, conductor) = make_test_conductor();
        let conn = lenient_conn(&f);
        let id = create_list_run(&conn, "orphan-wf", None, None);
        conn.execute(
            "UPDATE workflow_runs SET ticket_id = 'gone-ticket' WHERE id = ?1",
            rusqlite::params![id],
        )
        .expect("link missing ticket");

        let result = tool_list_runs(&conductor, &empty_args());
        assert_ne!(result.is_error, Some(true));
        let text = result_text(&result);
        assert!(text.contains("orphan-wf"), "got: {text}");
        assert!(!text.contains("ticket:"), "got: {text}");
    }

    #[test]
    fn test_list_runs_marks_ticket_lookup_failure() {
        let (f, conductor) = make_test_conductor();
        let conn = lenient_conn(&f);
        let id = create_list_run(&conn, "broken-wf", None, None);
        conn.execute(
            "UPDATE workflow_runs SET ticket_id = 'some-ticket' WHERE id = ?1",
            rusqlite::params![id],
        )
        .expect("link ticket");
        conn.execute_batch("DROP TABLE tickets;")
            .expect("break tickets");

        let result = tool_list_runs(&conductor, &empty_args());
        assert_ne!(result.is_error, Some(true));
        let text = result_text(&result);
        assert!(text.contains("ticket: <lookup failed:"), "got: {text}");
    }

    #[test]
    fn test_list_runs_returns_error_when_worktree_lookup_fails() {
        let (f, conductor) = make_test_conductor();
        let conn = lenient_conn(&f);
        create_list_run(&conn, "wt-wf", Some("wt-1"), None);
        // The list query only joins on worktrees.id/status; renaming `branch`
        // breaks only the bulk worktree lookup.
        conn.execute_batch("ALTER TABLE worktrees RENAME COLUMN branch TO branch_x;")
            .expect("break worktrees");
        let mut args = empty_args();
        args.insert("include_inactive".into(), Value::Bool(true));

        let result = tool_list_runs(&conductor, &args);
        assert_eq!(result.is_error, Some(true), "got: {}", result_text(&result));
    }

    #[test]
    fn test_list_runs_all_repos_shows_worktree_and_ticket() {
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;
        use conductor_core::tickets::{TicketInput, TicketSyncer};

        let (_f, conductor) = make_test_conductor();
        let conn = open_database(_f.path()).expect("open db");
        let config = Config::default();
        let repo = RepoManager::new(&conn, &config)
            .register("all-repo", "/tmp/all-repo", "https://github.com/x/y", None)
            .expect("register repo");
        let wt_id = "01JTEST0000000000000000003";
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES (?1, ?2, 'feat-all', 'feat/all', '/tmp/wt-all', 'active', datetime('now'))",
            rusqlite::params![wt_id, repo.id],
        )
        .expect("insert worktree");
        let syncer = TicketSyncer::new(&conn);
        syncer.sync_and_close_tickets(
            &repo.id,
            "github",
            &[TicketInput {
                source_type: "github".into(),
                source_id: "777".into(),
                title: "t".into(),
                body: String::new(),
                state: "open".into(),
                labels: vec![],
                assignee: None,
                priority: None,
                url: "https://github.com/x/y/issues/777".into(),
                raw_json: None,
                label_details: vec![],
                blocked_by: vec![],
                children: vec![],
                parent: None,
            }],
        );
        let ticket = syncer.get_by_source_id(&repo.id, "777").expect("ticket");

        let ids: Vec<String> = [("linked-wf", Some(wt_id)), ("bare-wf", None)]
            .iter()
            .map(|(name, wt)| create_list_run(&conn, name, *wt, Some(&repo.id)))
            .collect();
        conn.execute(
            "UPDATE workflow_runs SET ticket_id = ?1 WHERE id = ?2",
            rusqlite::params![ticket.id, ids[0]],
        )
        .expect("link ticket");

        let result = tool_list_runs(&conductor, &empty_args());
        assert_ne!(result.is_error, Some(true));
        let text = result_text(&result);
        let rows: Vec<&str> = text
            .split("\n\n")
            .filter(|r| !r.trim().is_empty())
            .collect();
        let linked = rows
            .iter()
            .find(|r| r.contains("linked-wf"))
            .expect("linked row");
        let bare = rows
            .iter()
            .find(|r| r.contains("bare-wf"))
            .expect("bare row");
        assert!(linked.contains("worktree_slug: feat-all"), "got: {linked}");
        assert!(
            linked.contains("worktree_branch: feat/all"),
            "got: {linked}"
        );
        assert!(linked.contains("ticket: 777"), "got: {linked}");
        assert!(!bare.contains("worktree_slug"), "got: {bare}");
        assert!(!bare.contains("ticket:"), "got: {bare}");
    }

    #[test]
    fn test_list_runs_top_level_only_keeps_paging_hint_and_pages_past_children() {
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, conductor) = make_test_conductor();
        let conn = open_database(_f.path()).expect("open db");
        let config = Config::default();
        let repo = RepoManager::new(&conn, &config)
            .register(
                "page-repo",
                "/tmp/page-repo",
                "https://github.com/x/y",
                None,
            )
            .expect("register repo");
        let make = |name: &str| create_list_run(&conn, name, None, Some(&repo.id));
        let root_a = make("root-a");
        let _root_b = make("root-b");
        let _root_c = make("root-c");
        // Three newer child runs: a page of 2 without the filter would be all children.
        for i in 0..3 {
            let id = make(&format!("child-{i}"));
            conn.execute(
                "UPDATE workflow_runs SET parent_workflow_run_id = ?1, started_at = '2999-01-01T00:00:00Z' WHERE id = ?2",
                rusqlite::params![root_a, id],
            )
            .expect("mark child");
        }

        let text_of =
            |args: &serde_json::Map<String, Value>| result_text(&tool_list_runs(&conductor, args));
        let mut args = args_with("repo", "page-repo");
        args.insert("top_level_only".into(), Value::Bool(true));
        args.insert("limit".into(), Value::String("2".into()));
        let page1 = text_of(&args);
        assert!(!page1.contains("child-"), "got: {page1}");
        assert_eq!(page1.matches("workflow: root-").count(), 2, "got: {page1}");
        assert!(
            page1.contains("offset"),
            "paging hint expected, got: {page1}"
        );

        args.insert("offset".into(), Value::String("2".into()));
        let page2 = text_of(&args);
        assert_eq!(page2.matches("workflow: root-").count(), 1, "got: {page2}");
        assert!(!page2.contains("child-"), "got: {page2}");
    }

    #[test]
    fn test_list_runs_include_inactive_shows_merged_worktree_runs() {
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, conductor) = make_test_conductor();
        let conn = open_database(_f.path()).expect("open db");
        let config = Config::default();

        let repo = RepoManager::new(&conn, &config)
            .register(
                "inactive-test-repo",
                "/tmp/inactive-test-repo",
                "https://github.com/x/y",
                None,
            )
            .expect("register repo");

        let wt_id = "01JTEST0000000000000000002";
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'merged', datetime('now'))",
            rusqlite::params![wt_id, repo.id, "feat-merged", "feat/merged", "/tmp/wt-m"],
        )
        .expect("insert worktree");

        let run_id = create_list_run(&conn, "merged-wf", Some(wt_id), Some(&repo.id));

        let text_of = |args: &serde_json::Map<String, Value>| {
            let result = tool_list_runs(&conductor, args);
            assert_ne!(result.is_error, Some(true));
            result.content[0]
                .as_text()
                .map(|t| t.text.clone())
                .unwrap_or_default()
        };

        for with_repo in [true, false] {
            let mut args = if with_repo {
                args_with("repo", "inactive-test-repo")
            } else {
                empty_args()
            };
            let default_text = text_of(&args);
            assert!(
                !default_text.contains(&run_id),
                "merged run must be hidden by default (repo={with_repo}): {default_text}"
            );
            args.insert("include_inactive".into(), Value::Bool(true));
            let text = text_of(&args);
            assert!(
                text.contains(&run_id),
                "merged run must appear with include_inactive (repo={with_repo}): {text}"
            );
            if with_repo {
                assert!(text.contains("worktree_slug: feat-merged"), "got: {text}");
            }
        }
    }

    #[test]
    fn test_dispatch_get_step_log_missing_run_id() {
        let (_f, conductor) = make_test_conductor();
        let result = tool_get_step_log(&conductor, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_missing_step_name() {
        let (_f, conductor) = make_test_conductor();
        let result = tool_get_step_log(
            &conductor,
            &args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX"),
        );
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_nonexistent_run() {
        let (_f, conductor) = make_test_conductor();
        let mut args = serde_json::Map::new();
        args.insert(
            "run_id".to_string(),
            Value::String("01HXXXXXXXXXXXXXXXXXXXXXXX".to_string()),
        );
        args.insert("step_name".to_string(), Value::String("build".to_string()));
        let result = tool_get_step_log(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("not found"), "got: {text}");
    }

    #[test]
    fn test_get_run_not_blocked_by_open_write_transaction() {
        // Regression for #3093: a write transaction held open on another
        // connection must not make get_run wait (WAL readers don't block).
        let (_f, conductor) = make_test_conductor();
        let (run_id, _step_id) = make_run_with_step(_f.path(), "build");

        let writer = rusqlite::Connection::open(_f.path()).expect("open writer");
        writer
            .execute_batch("BEGIN IMMEDIATE; UPDATE workflow_runs SET workflow_name = 'held';")
            .expect("hold write txn");

        let start = std::time::Instant::now();
        let result = tool_get_run(&conductor, &args_with("run_id", &run_id));
        let elapsed = start.elapsed();

        writer.execute_batch("ROLLBACK").expect("rollback");

        assert_ne!(result.is_error, Some(true), "get_run errored: {result:?}");
        assert!(
            serde_json::to_string(&result).unwrap().contains(&run_id),
            "result should mention the run id"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "get_run blocked for {elapsed:?}"
        );
    }

    #[test]
    fn test_dispatch_get_step_log_step_not_found() {
        let (_f, conductor) = make_test_conductor();
        let (run_id, _step_id) = make_run_with_step(_f.path(), "build");
        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id));
        args.insert(
            "step_name".to_string(),
            Value::String("nonexistent-step".to_string()),
        );
        let result = tool_get_step_log(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("not found"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_no_child_run() {
        // A step with no child_run_id (gate/skipped step) should return an error.
        let (_f, conductor) = make_test_conductor();
        let (run_id, _step_id) = make_run_with_step(_f.path(), "review-gate");
        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id));
        args.insert(
            "step_name".to_string(),
            Value::String("review-gate".to_string()),
        );
        let result = tool_get_step_log(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("has no agent run"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_log_file_missing() {
        // Step has a child_run_id but no log file exists on disk.
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowStepStatus;

        let (_f, conductor) = make_test_conductor();
        let (run_id, step_id) = make_run_with_step(_f.path(), "build");

        // Create a child agent run with a nonexistent log_file path inside agent_log_dir.
        let log_dir = conductor_core::test_helpers::ensure_agent_log_dir();
        let nonexistent = log_dir.join("test-nonexistent-missing.log");
        let nonexistent_str = nonexistent.to_str().unwrap().to_string();

        let conn = open_database(_f.path()).expect("open db");
        let agent_mgr = conductor_core::agent::AgentManager::new(&conn);
        let child_run = agent_mgr
            .create_run(None, "agent", None)
            .expect("create child run");
        conn.execute(
            "UPDATE agent_runs SET log_file = ?1 WHERE id = ?2",
            rusqlite::params![nonexistent_str, child_run.id],
        )
        .expect("set log_file");
        conductor_core::workflow::update_step_status(
            &conn,
            &step_id,
            WorkflowStepStatus::Completed,
            Some(&child_run.id),
            Some("done"),
            None,
            None,
            None,
        )
        .expect("update step");

        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id));
        args.insert("step_name".to_string(), Value::String("build".to_string()));
        let result = tool_get_step_log(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Log file not found"), "got: {text}");
        assert!(
            text.contains("test-nonexistent-missing.log"),
            "error should include log file path; got: {text}"
        );
        assert!(
            text.contains("build"),
            "error should include step name; got: {text}"
        );
        assert!(
            text.contains(&child_run.id),
            "error should include agent run id; got: {text}"
        );
    }

    /// Create a completed step whose agent run logs `log`. Returns the workflow run id.
    fn make_step_with_log(
        db_path: &std::path::Path,
        step_name: &str,
        log: &str,
        step_error: Option<&str>,
    ) -> String {
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowStepStatus;

        let (run_id, step_id) = make_run_with_step(db_path, step_name);
        let log_dir = conductor_core::test_helpers::ensure_agent_log_dir();
        let log_file = tempfile::Builder::new()
            .tempfile_in(&log_dir)
            .expect("temp log file");
        std::fs::write(log_file.path(), log).expect("write log");
        let (_, log_path) = log_file.keep().expect("keep log");
        let conn = open_database(db_path).expect("open db");
        let child = create_run_with_log(&conn, log_path.to_str().unwrap());
        conductor_core::workflow::update_step_status(
            &conn,
            &step_id,
            WorkflowStepStatus::Completed,
            Some(&child),
            Some("done"),
            None,
            None,
            None,
        )
        .expect("update step");
        if let Some(err) = step_error {
            conn.execute(
                "UPDATE workflow_run_steps SET step_error = ?1 WHERE id = ?2",
                rusqlite::params![err, step_id],
            )
            .expect("set step_error");
        }
        run_id
    }

    fn step_log_text(conductor: &Conductor, run_id: &str, extra: &[(&str, Value)]) -> String {
        let mut args = serde_json::Map::new();
        args.insert("run_id".into(), Value::String(run_id.into()));
        args.insert("step_name".into(), Value::String("s".into()));
        for (k, v) in extra {
            args.insert((*k).into(), v.clone());
        }
        let result = tool_get_step_log(conductor, &args);
        result.content[0]
            .as_text()
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn test_dispatch_get_step_log_defaults_to_result() {
        let (_f, conductor) = make_test_conductor();
        let log = "{\"type\":\"assistant\",\"note\":\"chatter\"}\n{\"type\":\"result\",\"result\":\"the final answer\"}\n";
        let run_id = make_step_with_log(_f.path(), "s", log, None);
        let text = step_log_text(&conductor, &run_id, &[]);
        assert!(text.contains("the final answer"), "got: {text}");
        assert!(text.contains("step: s"), "got: {text}");
        assert!(!text.contains("chatter"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_tail_honours_lines() {
        let (_f, conductor) = make_test_conductor();
        let log: String = (0..10).map(|i| format!("{{\"i\":{i}}}\n")).collect();
        let run_id = make_step_with_log(_f.path(), "s", &log, None);
        let text = step_log_text(
            &conductor,
            &run_id,
            &[
                ("mode", Value::String("tail".into())),
                ("lines", Value::from(2)),
            ],
        );
        assert!(text.contains("last 2 of 10 messages"), "got: {text}");
        assert!(
            text.contains("{\"i\":9}") && !text.contains("{\"i\":7}"),
            "got: {text}"
        );
    }

    #[test]
    fn test_dispatch_get_step_log_invalid_mode_errors() {
        let (_f, conductor) = make_test_conductor();
        let run_id = make_step_with_log(_f.path(), "s", "x\n", None);
        let mut args = serde_json::Map::new();
        args.insert("run_id".into(), Value::String(run_id));
        args.insert("step_name".into(), Value::String("s".into()));
        args.insert("mode".into(), Value::String("bogus".into()));
        let result = tool_get_step_log(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("tail"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_shows_parsed_block_excerpt() {
        let (_f, conductor) = make_test_conductor();
        let result_text = "intro\n<<<FLOW_OUTPUT>>>\n{\"markers\": [oops]}\n<<<END_FLOW_OUTPUT>>>";
        let log = format!(
            "{}\n",
            serde_json::json!({"type": "result", "result": result_text})
        );
        let run_id = make_step_with_log(
            _f.path(),
            "s",
            &log,
            Some("structured output validation: Invalid JSON in FLOW_OUTPUT: expected value"),
        );
        let text = step_log_text(&conductor, &run_id, &[]);
        assert!(
            text.contains("parsed FLOW_OUTPUT block (line 2 of result)"),
            "got: {text}"
        );
        assert!(text.contains("oops"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_success() {
        // Happy path: step has child_run linked to an agent run with a log file.
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowStepStatus;
        use std::io::Write as _;

        let (_f, conductor) = make_test_conductor();
        let (run_id, step_id) = make_run_with_step(_f.path(), "test-step");

        // Write a temporary log file inside agent_log_dir so validation passes.
        let log_dir = conductor_core::test_helpers::ensure_agent_log_dir();
        let log_file = tempfile::Builder::new()
            .tempfile_in(&log_dir)
            .expect("temp log file");
        writeln!(log_file.as_file(), "agent log line 1").expect("write");
        writeln!(log_file.as_file(), "agent log line 2").expect("write");
        let log_path = log_file.path().to_str().unwrap().to_string();

        let conn = open_database(_f.path()).expect("open db");
        let child_run_id = create_run_with_log(&conn, &log_path);
        conductor_core::workflow::update_step_status(
            &conn,
            &step_id,
            WorkflowStepStatus::Completed,
            Some(&child_run_id),
            Some("done"),
            None,
            None,
            None,
        )
        .expect("update step");

        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id));
        args.insert(
            "step_name".to_string(),
            Value::String("test-step".to_string()),
        );
        args.insert("mode".to_string(), Value::String("full".to_string()));
        let result = tool_get_step_log(&conductor, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "get_step_log should succeed; got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("agent log line 1"), "got: {text}");
        assert!(text.contains("agent log line 2"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_multi_iteration_returns_last() {
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowStepStatus;
        use std::io::Write as _;

        let (_f, conductor) = make_test_conductor();
        let (run_id, step0_id) = make_run_with_step(_f.path(), "build");

        // Write log files inside agent_log_dir so validation passes.
        let log_dir = conductor_core::test_helpers::ensure_agent_log_dir();
        let log_iter0 = tempfile::Builder::new()
            .tempfile_in(&log_dir)
            .expect("temp log iter0");
        writeln!(log_iter0.as_file(), "iteration 0 log").expect("write iter0");
        let log_iter1 = tempfile::Builder::new()
            .tempfile_in(&log_dir)
            .expect("temp log iter1");
        writeln!(log_iter1.as_file(), "iteration 1 log").expect("write iter1");
        let path0 = log_iter0.path().to_str().unwrap().to_string();
        let path1 = log_iter1.path().to_str().unwrap().to_string();

        let conn = open_database(_f.path()).expect("open db");
        let child0_id = create_run_with_log(&conn, &path0);
        conductor_core::workflow::update_step_status(
            &conn,
            &step0_id,
            WorkflowStepStatus::Completed,
            Some(&child0_id),
            Some("done"),
            None,
            None,
            None,
        )
        .expect("update step0");

        // Insert iteration 1 for the same step_name.
        let step1_id =
            conductor_core::workflow::insert_step(&conn, &run_id, "build", "actor", false, 0, 1)
                .expect("insert step iter1");
        let child1_id = create_run_with_log(&conn, &path1);
        conductor_core::workflow::update_step_status(
            &conn,
            &step1_id,
            WorkflowStepStatus::Running,
            Some(&child1_id),
            None,
            None,
            None,
            None,
        )
        .expect("update step1");

        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id));
        args.insert("step_name".to_string(), Value::String("build".to_string()));
        args.insert("mode".to_string(), Value::String("full".to_string()));
        let result = tool_get_step_log(&conductor, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "get_step_log should succeed; got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("iteration 1 log"),
            "expected iteration 1 log, got: {text}"
        );
        assert!(
            !text.contains("iteration 0 log"),
            "should not contain iteration 0 log, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_get_step_log_multi_step_name_isolation() {
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowStepStatus;
        use std::io::Write as _;

        let (_f, conductor) = make_test_conductor();
        let (run_id, build_step_id) = make_run_with_step(_f.path(), "build");

        // Write log files inside agent_log_dir so validation passes.
        let log_dir = conductor_core::test_helpers::ensure_agent_log_dir();
        let log_build = tempfile::Builder::new()
            .tempfile_in(&log_dir)
            .expect("temp log build");
        writeln!(log_build.as_file(), "build step log").expect("write build");
        let log_test0 = tempfile::Builder::new()
            .tempfile_in(&log_dir)
            .expect("temp log test iter0");
        writeln!(log_test0.as_file(), "test iteration 0 log").expect("write test0");
        let log_test1 = tempfile::Builder::new()
            .tempfile_in(&log_dir)
            .expect("temp log test iter1");
        writeln!(log_test1.as_file(), "test iteration 1 log").expect("write test1");
        let path_build = log_build.path().to_str().unwrap().to_string();
        let path_test0 = log_test0.path().to_str().unwrap().to_string();
        let path_test1 = log_test1.path().to_str().unwrap().to_string();

        let conn = open_database(_f.path()).expect("open db");

        // Link build step to its agent run.
        let child_build_id = create_run_with_log(&conn, &path_build);
        conductor_core::workflow::update_step_status(
            &conn,
            &build_step_id,
            WorkflowStepStatus::Completed,
            Some(&child_build_id),
            Some("done"),
            None,
            None,
            None,
        )
        .expect("update build step");

        // Insert test step iteration 0.
        let test_step0_id =
            conductor_core::workflow::insert_step(&conn, &run_id, "test", "actor", false, 0, 0)
                .expect("insert test step iter0");
        let child_test0_id = create_run_with_log(&conn, &path_test0);
        conductor_core::workflow::update_step_status(
            &conn,
            &test_step0_id,
            WorkflowStepStatus::Completed,
            Some(&child_test0_id),
            Some("done"),
            None,
            None,
            None,
        )
        .expect("update test step 0");

        // Insert test step iteration 1.
        let test_step1_id =
            conductor_core::workflow::insert_step(&conn, &run_id, "test", "actor", false, 0, 1)
                .expect("insert test step iter1");
        let child_test1_id = create_run_with_log(&conn, &path_test1);
        conductor_core::workflow::update_step_status(
            &conn,
            &test_step1_id,
            WorkflowStepStatus::Running,
            Some(&child_test1_id),
            None,
            None,
            None,
            None,
        )
        .expect("update test step 1");

        // Request the "test" step log — should get iteration 1, not "build".
        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id));
        args.insert("step_name".to_string(), Value::String("test".to_string()));
        args.insert("mode".to_string(), Value::String("full".to_string()));
        let result = tool_get_step_log(&conductor, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "get_step_log should succeed; got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("test iteration 1 log"),
            "expected test iteration 1 log, got: {text}"
        );
        assert!(
            !text.contains("build step log"),
            "should not contain build step log, got: {text}"
        );
        assert!(
            !text.contains("test iteration 0 log"),
            "should not contain test iteration 0 log, got: {text}"
        );
    }

    #[test]
    fn test_run_stats_missing_run_id_arg() {
        let (_f, conductor) = make_test_conductor();
        let result = tool_run_stats(&conductor, &empty_args());
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_run_stats_nonexistent_run() {
        let (_f, conductor) = make_test_conductor();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_run_stats(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_run_stats_happy_path_text_and_json() {
        let (f, conductor) = make_test_conductor();
        let (run_id, _step_id) = make_run_with_step(f.path(), "implement");

        let result = tool_run_stats(&conductor, &args_with("run_id", &run_id));
        assert_ne!(result.is_error, Some(true));
        let text = result.content[0].as_text().map(|t| t.text.clone()).unwrap();
        assert!(text.contains(&run_id), "{text}");
        assert!(text.contains("implement"), "{text}");

        let mut args = args_with("run_id", &run_id);
        args.insert("compare_run_id".into(), Value::String(run_id.clone()));
        args.insert("json".into(), Value::Bool(true));
        let result = tool_run_stats(&conductor, &args);
        assert_ne!(result.is_error, Some(true));
        let text = result.content[0].as_text().map(|t| t.text.clone()).unwrap();
        let v: Value = serde_json::from_str(&text).expect("json output");
        assert!(v.get("run").is_some() && v.get("compare").is_some());
    }

    #[test]
    fn test_get_review_arg_validation() {
        let (_f, conductor) = make_test_conductor();
        let r = tool_get_review(&conductor, &empty_args());
        assert_eq!(r.is_error, Some(true));
        let mut both = args_with("run_id", "X");
        both.insert("repo".into(), Value::String("r".into()));
        assert_eq!(tool_get_review(&conductor, &both).is_error, Some(true));
        let r = tool_get_review(&conductor, &args_with("pr", "5"));
        assert_eq!(r.is_error, Some(true));
        let r = tool_get_review(&conductor, &args_with("pr", "abc"));
        assert_eq!(r.is_error, Some(true));
    }

    #[test]
    fn test_get_review_nonexistent_run() {
        let (_f, conductor) = make_test_conductor();
        let r = tool_get_review(
            &conductor,
            &args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX"),
        );
        assert_eq!(r.is_error, Some(true));
    }

    #[test]
    fn test_get_review_happy_path() {
        let (_f, conductor) = make_test_conductor();
        let conn = &conductor.conn;
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, parent_run_id, status, dry_run, \
             trigger, started_at, iteration, inputs) \
             VALUES ('RV', 'review-pr', 'PA', 'completed', 0, 'manual', '2026-01-01T00:00:00Z', 0, \
             '{\"pr_number\":\"3\",\"scope\":\"full\",\"since\":\"\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, status, \
             position, started_at, iteration, structured_output) VALUES \
             ('S1', 'RV', 'review-aggregator', 'actor', 'completed', 0, '2026-01-01T00:01:00Z', 0, \
             '{\"overall_approved\":true,\"blocking_findings\":[],\"markers\":[]}')",
            [],
        )
        .unwrap();
        let r = tool_get_review(&conductor, &args_with("run_id", "RV"));
        assert_ne!(r.is_error, Some(true));
        let text = r.content[0].as_text().map(|t| t.text.clone()).unwrap();
        let v: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(v["run_id"], "RV");
        assert_eq!(v["review"]["overall_approved"], true);
        assert!(v["review"].get("markers").is_none());
        assert_eq!(v["scope"]["kind"], "full");
    }

    #[test]
    fn test_split_run_ids_trims_and_drops_blanks() {
        assert_eq!(split_run_ids("a, b ,,c"), vec!["a", "b", "c"]);
        assert_eq!(split_run_ids("  a  "), vec!["a"]);
        assert!(split_run_ids("").is_empty());
        assert!(split_run_ids(" , ,  ").is_empty());
    }

    #[test]
    fn test_run_stats_blank_run_id_is_error() {
        let (_f, conductor) = make_test_conductor();
        let result = tool_run_stats(&conductor, &args_with("run_id", " , "));
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0].as_text().map(|t| t.text.clone()).unwrap();
        assert!(text.contains("at least one workflow run id"), "{text}");
    }

    #[test]
    fn test_run_stats_multiple_comma_separated_ids() {
        let (f, conductor) = make_test_conductor();
        let (run_id, _) = make_run_with_step(f.path(), "implement");
        let ids = format!("{run_id}, {run_id}");
        let mut args = args_with("run_id", &ids);
        args.insert(
            "compare_run_id".into(),
            Value::String(format!(" ,{run_id}")),
        );
        let result = tool_run_stats(&conductor, &args);
        assert_ne!(result.is_error, Some(true));
    }
}
