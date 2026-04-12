use std::path::{Path, PathBuf};

use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{get_arg, open_db_and_config, pagination_hint, tool_err, tool_ok};
use crate::mcp::resources::{
    format_run_detail_with_log, format_run_summary_line, format_run_summary_line_with_repo,
};

pub(super) fn tool_list_runs(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::{WorkflowManager, WorkflowRunStatus};
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = get_arg(args, "repo");
    let worktree_slug = get_arg(args, "worktree");
    let status_str = get_arg(args, "status");

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

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wf_mgr = WorkflowManager::new(&conn);

    if let Some(slug) = repo_slug {
        // Per-repo path (existing behaviour)
        let repo_mgr = RepoManager::new(&conn, &config);
        let repo = match repo_mgr.get_by_slug(slug) {
            Ok(r) => r,
            Err(e) => return tool_err(e),
        };

        let runs = if let Some(wt_slug) = worktree_slug {
            let wt_mgr = WorktreeManager::new(&conn, &config);
            let wt = match wt_mgr.get_by_slug_or_branch(&repo.id, wt_slug) {
                Ok(w) => w,
                Err(e) => return tool_err(e),
            };
            match wf_mgr.list_workflow_runs_filtered_paginated(&wt.id, status, limit, offset) {
                Ok(r) => r,
                Err(e) => return tool_err(e),
            }
        } else {
            match wf_mgr.list_workflow_runs_by_repo_id_filtered(&repo.id, limit, offset, status) {
                Ok(r) => r,
                Err(e) => return tool_err(e),
            }
        };

        if runs.is_empty() {
            return tool_ok(format!("No workflow runs for {slug}."));
        }

        // Bulk-fetch all worktrees for this repo once, then build a lookup map.
        // This avoids N+1 DB queries and config file reads (one per run).
        let wt_mgr = WorktreeManager::new(&conn, &config);
        let worktrees = match wt_mgr.list_by_repo_id(&repo.id, false) {
            Ok(wts) => wts,
            Err(e) => return tool_err(e),
        };
        let wt_map: std::collections::HashMap<&str, (&str, &str)> = worktrees
            .iter()
            .map(|wt| (wt.id.as_str(), (wt.slug.as_str(), wt.branch.as_str())))
            .collect();

        let mut out = String::new();
        for run in &runs {
            let (wt_slug, wt_branch) = run
                .worktree_id
                .as_deref()
                .and_then(|id| wt_map.get(id).copied())
                .unzip();
            out.push_str(&format_run_summary_line(run, wt_slug, wt_branch));
        }
        if runs.len() == limit {
            out.push_str(&pagination_hint(offset, runs.len(), limit));
        }
        tool_ok(out)
    } else {
        // Cross-repo path: return runs across all registered repos
        let repo_mgr = RepoManager::new(&conn, &config);
        let repos = match repo_mgr.list() {
            Ok(r) => r,
            Err(e) => return tool_err(e),
        };
        let repo_map: std::collections::HashMap<String, String> =
            repos.into_iter().map(|r| (r.id, r.slug)).collect();

        let runs = match wf_mgr.list_all_workflow_runs_filtered_paginated(status, limit, offset) {
            Ok(r) => r,
            Err(e) => return tool_err(e),
        };

        if runs.is_empty() {
            return tool_ok("No workflow runs.".to_string());
        }
        let mut out = String::new();
        for run in &runs {
            let slug_for_run = run
                .repo_id
                .as_deref()
                .and_then(|id| repo_map.get(id).map(|s| s.as_str()));
            out.push_str(&format_run_summary_line_with_repo(run, slug_for_run));
        }
        if runs.len() == limit {
            out.push_str(&pagination_hint(offset, runs.len(), limit));
        }
        tool_ok(out)
    }
}

pub(super) fn tool_get_run(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::workflow::WorkflowManager;

    let run_id = require_arg!(args, "run_id");
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let run = match wf_mgr.get_workflow_run(run_id) {
        Ok(Some(r)) => r,
        Ok(None) => return tool_err(format!("Workflow run {run_id} not found")),
        Err(e) => return tool_err(e),
    };
    let steps = match wf_mgr.get_workflow_steps(run_id) {
        Ok(s) => s,
        Err(e) => return tool_err(e),
    };
    let claude_dir = config.general.resolve_optional_claude_dir();
    tool_ok(format_run_detail_with_log(
        &conn,
        &run,
        &steps,
        claude_dir.as_deref(),
    ))
}

pub(super) fn tool_cancel_run(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::workflow::WorkflowManager;

    let run_id = require_arg!(args, "run_id");
    let (conn, _config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let run = match wf_mgr.get_workflow_run(run_id) {
        Ok(Some(r)) => r,
        Ok(None) => return tool_err(format!("Workflow run not found: {run_id}")),
        Err(e) => return tool_err(e),
    };
    match wf_mgr.cancel_run(run_id, "Cancelled via MCP conductor_cancel_run") {
        Ok(()) => tool_ok(format!(
            "Workflow run {} ('{}') cancelled.",
            run_id, run.workflow_name
        )),
        Err(e) => tool_err(e),
    }
}

pub(super) fn tool_resume_run(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::workflow::{
        resume_workflow_standalone, validate_resume_preconditions, WorkflowManager,
        WorkflowResumeStandalone,
    };
    use std::sync::{Arc, Mutex};

    let run_id = require_arg!(args, "run_id");
    let from_step = get_arg(args, "from_step").map(str::to_string);
    let model = get_arg(args, "model").map(str::to_string);

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let run = match wf_mgr.get_workflow_run(run_id) {
        Ok(Some(r)) => r,
        Ok(None) => return tool_err(format!("Workflow run not found: {run_id}")),
        Err(e) => return tool_err(e),
    };

    if let Err(e) = validate_resume_preconditions(&run.status, false, from_step.as_deref()) {
        return tool_err(e);
    }

    let params = WorkflowResumeStandalone {
        config,
        workflow_run_id: run_id.to_string(),
        model,
        from_step,
        restart: false,
        db_path: Some(db_path.to_path_buf()),
        conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
    };

    // Error slot: captures any error that occurs before steps begin executing.
    let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let error_slot_bg = Arc::clone(&error_slot);
    // Notify pair: the background thread signals this when it fails (error = true).
    let notify_pair: Arc<(Mutex<bool>, std::sync::Condvar)> =
        Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let notify_pair_bg = Arc::clone(&notify_pair);

    std::thread::spawn(move || {
        if let Err(e) = resume_workflow_standalone(&params) {
            *error_slot_bg.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
            // Wake the waiter so startup errors are surfaced immediately.
            *notify_pair_bg.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
            notify_pair_bg.1.notify_one();
        }
    });

    // Block until an error is signalled or 2 s elapses (workflow is running in background).
    let (lock, cvar) = notify_pair.as_ref();
    let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let _ = cvar
        .wait_timeout_while(guard, std::time::Duration::from_secs(2), |v| !*v)
        .unwrap_or_else(|e| e.into_inner());

    // Surface any startup error before reporting success.
    if let Some(err) = error_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        return tool_err(format!("Failed to resume workflow run: {err}"));
    }

    tool_ok(format!(
        "Workflow run {} ('{}') is resuming. Use conductor_get_run to check progress.",
        run_id, run.workflow_name
    ))
}

pub(super) fn tool_get_step_log(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::agent::AgentManager;
    use conductor_core::workflow::WorkflowManager;

    let run_id = require_arg!(args, "run_id");
    let step_name = require_arg!(args, "step_name");

    let (conn, _config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };

    let wf_mgr = WorkflowManager::new(&conn);

    // Verify the workflow run exists.
    match wf_mgr.get_workflow_run(run_id) {
        Ok(Some(_)) => {}
        Ok(None) => return tool_err(format!("Workflow run {run_id} not found")),
        Err(e) => return tool_err(e),
    }

    // Find all steps for this run and pick the last matching step_name.
    let steps = match wf_mgr.get_workflow_steps(run_id) {
        Ok(s) => s,
        Err(e) => return tool_err(e),
    };
    let step = steps
        .into_iter()
        .filter(|s| s.step_name == step_name)
        .max_by_key(|s| s.iteration);
    let step = match step {
        Some(s) => s,
        None => {
            return tool_err(format!(
                "Step '{step_name}' not found in workflow run {run_id}"
            ))
        }
    };

    // Gate/skipped steps have no child_run_id.
    let child_run_id = match step.child_run_id.as_deref() {
        Some(id) => id.to_string(),
        None => {
            return tool_err(format!(
                "Step '{step_name}' has no associated agent run \
                 (gate steps and skipped steps do not produce logs)"
            ))
        }
    };

    // Resolve the log file path.
    let agent_mgr = AgentManager::new(&conn);
    let log_path = match agent_mgr.get_run(&child_run_id) {
        Ok(Some(agent_run)) => match agent_run.log_file {
            Some(path) => PathBuf::from(path),
            None => conductor_core::config::agent_log_path(&child_run_id),
        },
        Ok(None) => conductor_core::config::agent_log_path(&child_run_id),
        Err(e) => return tool_err(e),
    };

    match std::fs::read_to_string(&log_path) {
        Ok(contents) => tool_ok(contents),
        Err(e) => tool_err(format!(
            "Log file not found for step '{step_name}' (agent run {child_run_id}) at '{}': {e}",
            log_path.display()
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn make_test_db() -> (tempfile::NamedTempFile, std::path::PathBuf) {
        use conductor_core::db::open_database;
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        open_database(&path).expect("open_database");
        (file, path)
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
        use conductor_core::agent::AgentManager;
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowManager;

        let conn = open_database(db_path).expect("open db");
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(None, "workflow", None, None)
            .expect("create agent run");
        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test-wf", None, &parent.id, false, "manual", None)
            .expect("create workflow run");
        if !matches!(status, conductor_core::workflow::WorkflowRunStatus::Pending) {
            mgr.update_workflow_status(&run.id, status, None, None)
                .expect("update status");
        }
        run.id
    }

    /// Helper: create an agent run and set its log_file in one shot. Returns the run id.
    fn create_run_with_log(conn: &rusqlite::Connection, log_path: &str) -> String {
        use conductor_core::agent::AgentManager;
        let mgr = AgentManager::new(conn);
        let run = mgr
            .create_run(None, "agent", None, None)
            .expect("create agent run");
        mgr.update_run_log_file(&run.id, log_path)
            .expect("set log_file");
        run.id
    }

    /// Helper: create a workflow run with one step. Returns (run_id, step_id).
    fn make_run_with_step(db_path: &std::path::Path, step_name: &str) -> (String, String) {
        use conductor_core::agent::AgentManager;
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowManager;

        let conn = open_database(db_path).expect("open db");
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(None, "workflow", None, None)
            .expect("create parent run");
        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test-wf", None, &parent.id, false, "manual", None)
            .expect("create workflow run");
        let step_id = mgr
            .insert_step(&run.id, step_name, "actor", false, 0, 0)
            .expect("insert step");
        (run.id, step_id)
    }

    #[test]
    fn test_dispatch_get_run_missing_run_id_arg() {
        let (_f, db) = make_test_db();
        let result = tool_get_run(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_get_run_nonexistent_run() {
        let (_f, db) = make_test_db();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_get_run(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_list_runs_missing_repo_arg() {
        // repo is now optional — empty-args call should succeed (empty result)
        let (_f, db) = make_test_db();
        let result = tool_list_runs(&db, &empty_args());
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
        let (_f, db) = make_test_db();
        let args = args_with("worktree", "some-wt");
        let result = tool_list_runs(&db, &args);
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
        use conductor_core::agent::AgentManager;
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowManager;

        let (_f, db) = make_test_db();
        {
            let conn = open_database(&db).expect("open db");

            // Register two repos (make_test_db only runs migrations, no seed data)
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

            let agent_mgr = AgentManager::new(&conn);
            let p1 = agent_mgr
                .create_run(Some("w1"), "wf-a", None, None)
                .unwrap();
            let p2 = agent_mgr
                .create_run(Some("w2"), "wf-b", None, None)
                .unwrap();

            let wf_mgr = WorkflowManager::new(&conn);
            wf_mgr
                .create_workflow_run_with_targets(
                    "flow-a",
                    Some("w1"),
                    None,
                    Some("r1"),
                    &p1.id,
                    false,
                    "manual",
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
            wf_mgr
                .create_workflow_run_with_targets(
                    "flow-b",
                    Some("w2"),
                    None,
                    Some("r2"),
                    &p2.id,
                    false,
                    "manual",
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
        }

        let result = tool_list_runs(&db, &empty_args());
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
        use conductor_core::workflow::WorkflowManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let mgr = WorkflowManager::new(&conn);
        let runs = mgr
            .list_workflow_runs_by_repo_id("nonexistent-repo-id", 50, 0)
            .expect("query should succeed");
        assert!(runs.is_empty(), "expected no runs for unknown repo");
    }

    #[test]
    fn test_list_workflow_runs_by_repo_id_scoped() {
        use conductor_core::agent::AgentManager;
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;
        use conductor_core::workflow::WorkflowManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let config = Config::default();
        let repo_mgr = RepoManager::new(&conn, &config);
        let repo_a = repo_mgr
            .register("repo-a", "/tmp/repo-a", "https://github.com/x/a", None)
            .expect("register repo-a");
        let repo_b = repo_mgr
            .register("repo-b", "/tmp/repo-b", "https://github.com/x/b", None)
            .expect("register repo-b");

        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);

        let parent = agent_mgr
            .create_run(None, "workflow", None, None)
            .expect("create agent run");

        // Create one run for repo-A and one for repo-B
        let _run_a = mgr
            .create_workflow_run_with_targets(
                "wf-a",
                None,
                None,
                Some(&repo_a.id),
                &parent.id,
                false,
                "manual",
                None,
                None,
                None,
                None,
            )
            .expect("create run A");
        let _run_b = mgr
            .create_workflow_run_with_targets(
                "wf-b",
                None,
                None,
                Some(&repo_b.id),
                &parent.id,
                false,
                "manual",
                None,
                None,
                None,
                None,
            )
            .expect("create run B");

        let runs_a = mgr
            .list_workflow_runs_by_repo_id(&repo_a.id, 50, 0)
            .expect("query A");
        let runs_b = mgr
            .list_workflow_runs_by_repo_id(&repo_b.id, 50, 0)
            .expect("query B");

        assert_eq!(runs_a.len(), 1, "expected 1 run for repo-a");
        assert_eq!(runs_a[0].workflow_name, "wf-a");
        assert_eq!(runs_b.len(), 1, "expected 1 run for repo-b");
        assert_eq!(runs_b[0].workflow_name, "wf-b");
    }

    #[test]
    fn test_dispatch_cancel_run_missing_arg() {
        let (_f, db) = make_test_db();
        let result = tool_cancel_run(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_cancel_run_not_found() {
        let (_f, db) = make_test_db();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_cancel_run(&db, &args);
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
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Completed);
        let args = args_with("run_id", &run_id);
        let result = tool_cancel_run(&db, &args);
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
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Failed);
        let args = args_with("run_id", &run_id);
        let result = tool_cancel_run(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_cancel_run_already_cancelled() {
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Cancelled);
        let args = args_with("run_id", &run_id);
        let result = tool_cancel_run(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_cancel_run_running() {
        use conductor_core::db::open_database;
        use conductor_core::workflow::{WorkflowManager, WorkflowRunStatus};
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Running);
        let args = args_with("run_id", &run_id);
        let result = tool_cancel_run(&db, &args);
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
        let conn = open_database(&db).expect("open db");
        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .get_workflow_run(&run_id)
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
        let (_f, db) = make_test_db();
        let result = tool_resume_run(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_resume_run_not_found() {
        let (_f, db) = make_test_db();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_resume_run(&db, &args);
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
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Running);
        let args = args_with("run_id", &run_id);
        let result = tool_resume_run(&db, &args);
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
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Completed);
        let args = args_with("run_id", &run_id);
        let result = tool_resume_run(&db, &args);
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
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Cancelled);
        let args = args_with("run_id", &run_id);
        let result = tool_resume_run(&db, &args);
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
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Failed);
        let args = args_with("run_id", &run_id);
        let result = tool_resume_run(&db, &args);
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
    }

    #[test]
    fn test_list_runs_includes_worktree_slug() {
        use conductor_core::agent::AgentManager;
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;
        use conductor_core::workflow::WorkflowManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
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
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(None, "workflow", None, None)
            .expect("create agent run");
        WorkflowManager::new(&conn)
            .create_workflow_run_with_targets(
                "my-wf",
                Some(wt_id),
                None,
                Some(&repo.id),
                &parent.id,
                false,
                "manual",
                None,
                None,
                None,
                None,
            )
            .expect("create workflow run");

        // Call tool_list_runs and verify worktree_slug appears in output.
        let args = args_with("repo", "slug-test-repo");
        let result = tool_list_runs(&db, &args);
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
    fn test_dispatch_get_step_log_missing_run_id() {
        let (_f, db) = make_test_db();
        let result = tool_get_step_log(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_missing_step_name() {
        let (_f, db) = make_test_db();
        let result = tool_get_step_log(&db, &args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX"));
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_nonexistent_run() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert(
            "run_id".to_string(),
            Value::String("01HXXXXXXXXXXXXXXXXXXXXXXX".to_string()),
        );
        args.insert("step_name".to_string(), Value::String("build".to_string()));
        let result = tool_get_step_log(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("not found"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_step_not_found() {
        let (_f, db) = make_test_db();
        let (run_id, _step_id) = make_run_with_step(&db, "build");
        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id));
        args.insert(
            "step_name".to_string(),
            Value::String("nonexistent-step".to_string()),
        );
        let result = tool_get_step_log(&db, &args);
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
        let (_f, db) = make_test_db();
        let (run_id, _step_id) = make_run_with_step(&db, "review-gate");
        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id));
        args.insert(
            "step_name".to_string(),
            Value::String("review-gate".to_string()),
        );
        let result = tool_get_step_log(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("no associated agent run"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_step_log_log_file_missing() {
        // Step has a child_run_id but no log file exists on disk.
        use conductor_core::agent::AgentManager;
        use conductor_core::db::open_database;
        use conductor_core::workflow::{WorkflowManager, WorkflowStepStatus};

        let (_f, db) = make_test_db();
        let (run_id, step_id) = make_run_with_step(&db, "build");

        // Create a child agent run with a known nonexistent log_file path.
        let conn = open_database(&db).expect("open db");
        let agent_mgr = AgentManager::new(&conn);
        let child_run = agent_mgr
            .create_run(None, "agent", None, None)
            .expect("create child run");
        conn.execute(
            "UPDATE agent_runs SET log_file = ?1 WHERE id = ?2",
            rusqlite::params!["/nonexistent/path/log.txt", child_run.id],
        )
        .expect("set log_file");
        let mgr = WorkflowManager::new(&conn);
        mgr.update_step_status(
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
        let result = tool_get_step_log(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Log file not found"), "got: {text}");
        assert!(
            text.contains("/nonexistent/path/log.txt"),
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

    #[test]
    fn test_dispatch_get_step_log_success() {
        // Happy path: step has child_run linked to an agent run with a log file.
        use conductor_core::db::open_database;
        use conductor_core::workflow::{WorkflowManager, WorkflowStepStatus};
        use std::io::Write as _;

        let (_f, db) = make_test_db();
        let (run_id, step_id) = make_run_with_step(&db, "test-step");

        // Write a temporary log file.
        let log_file = tempfile::NamedTempFile::new().expect("temp log file");
        writeln!(log_file.as_file(), "agent log line 1").expect("write");
        writeln!(log_file.as_file(), "agent log line 2").expect("write");
        let log_path = log_file.path().to_str().unwrap().to_string();

        let conn = open_database(&db).expect("open db");
        let child_run_id = create_run_with_log(&conn, &log_path);

        let mgr = WorkflowManager::new(&conn);
        mgr.update_step_status(
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
        let result = tool_get_step_log(&db, &args);
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
        use conductor_core::workflow::{WorkflowManager, WorkflowStepStatus};
        use std::io::Write as _;

        let (_f, db) = make_test_db();
        let (run_id, step0_id) = make_run_with_step(&db, "build");

        // Write log files for each iteration.
        let log_iter0 = tempfile::NamedTempFile::new().expect("temp log iter0");
        writeln!(log_iter0.as_file(), "iteration 0 log").expect("write iter0");
        let log_iter1 = tempfile::NamedTempFile::new().expect("temp log iter1");
        writeln!(log_iter1.as_file(), "iteration 1 log").expect("write iter1");
        let path0 = log_iter0.path().to_str().unwrap().to_string();
        let path1 = log_iter1.path().to_str().unwrap().to_string();

        let conn = open_database(&db).expect("open db");
        let child0_id = create_run_with_log(&conn, &path0);

        let mgr = WorkflowManager::new(&conn);
        mgr.update_step_status(
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
        let step1_id = mgr
            .insert_step(&run_id, "build", "actor", false, 0, 1)
            .expect("insert step iter1");
        let child1_id = create_run_with_log(&conn, &path1);
        mgr.update_step_status(
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
        let result = tool_get_step_log(&db, &args);
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
        use conductor_core::workflow::{WorkflowManager, WorkflowStepStatus};
        use std::io::Write as _;

        let (_f, db) = make_test_db();
        let (run_id, build_step_id) = make_run_with_step(&db, "build");

        // Write log files.
        let log_build = tempfile::NamedTempFile::new().expect("temp log build");
        writeln!(log_build.as_file(), "build step log").expect("write build");
        let log_test0 = tempfile::NamedTempFile::new().expect("temp log test iter0");
        writeln!(log_test0.as_file(), "test iteration 0 log").expect("write test0");
        let log_test1 = tempfile::NamedTempFile::new().expect("temp log test iter1");
        writeln!(log_test1.as_file(), "test iteration 1 log").expect("write test1");
        let path_build = log_build.path().to_str().unwrap().to_string();
        let path_test0 = log_test0.path().to_str().unwrap().to_string();
        let path_test1 = log_test1.path().to_str().unwrap().to_string();

        let conn = open_database(&db).expect("open db");
        let mgr = WorkflowManager::new(&conn);

        // Link build step to its agent run.
        let child_build_id = create_run_with_log(&conn, &path_build);
        mgr.update_step_status(
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
        let test_step0_id = mgr
            .insert_step(&run_id, "test", "actor", false, 0, 0)
            .expect("insert test step iter0");
        let child_test0_id = create_run_with_log(&conn, &path_test0);
        mgr.update_step_status(
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
        let test_step1_id = mgr
            .insert_step(&run_id, "test", "actor", false, 0, 1)
            .expect("insert test step iter1");
        let child_test1_id = create_run_with_log(&conn, &path_test1);
        mgr.update_step_status(
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
        let result = tool_get_step_log(&db, &args);
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
}
