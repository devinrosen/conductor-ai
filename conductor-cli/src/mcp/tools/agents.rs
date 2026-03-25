use std::path::Path;

use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{get_arg, open_db_and_config, tool_err, tool_ok};

pub(super) fn tool_list_agent_runs(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::agent::{AgentManager, AgentRunStatus};
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::WorkflowManager;
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = get_arg(args, "repo");
    let worktree_slug = get_arg(args, "worktree");
    let status_str = get_arg(args, "status");

    // worktree filter is repo-scoped and meaningless without a repo
    if worktree_slug.is_some() && repo_slug.is_none() {
        return tool_err("worktree filter requires a repo argument");
    }

    let status: Option<AgentRunStatus> = match status_str {
        Some(s) => match s.parse::<AgentRunStatus>() {
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
    let agent_mgr = AgentManager::new(&conn);

    // Resolve repo / worktree IDs when provided
    let (resolved_repo_id, resolved_worktree_id) = if let Some(slug) = repo_slug {
        let repo_mgr = RepoManager::new(&conn, &config);
        let repo = match repo_mgr.get_by_slug(slug) {
            Ok(r) => r,
            Err(e) => return tool_err(e),
        };
        if let Some(wt_slug) = worktree_slug {
            let wt_mgr = WorktreeManager::new(&conn, &config);
            let wt = match wt_mgr.get_by_slug_or_branch(&repo.id, wt_slug) {
                Ok(w) => w,
                Err(e) => return tool_err(e),
            };
            (None::<String>, Some(wt.id))
        } else {
            (Some(repo.id), None::<String>)
        }
    } else {
        (None, None)
    };

    let runs = match agent_mgr.list_agent_runs(
        resolved_worktree_id.as_deref(),
        resolved_repo_id.as_deref(),
        status.as_ref(),
        limit,
        offset,
    ) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };

    if runs.is_empty() {
        return tool_ok("No agent runs.".to_string());
    }

    // Batch-load worktree info for all unique worktree_ids
    let wt_ids: Vec<&str> = runs
        .iter()
        .filter_map(|r| r.worktree_id.as_deref())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let wt_mgr = WorktreeManager::new(&conn, &config);
    let wt_map: std::collections::HashMap<String, (String, String)> =
        match wt_mgr.get_by_ids(&wt_ids) {
            Ok(wts) => wts
                .into_iter()
                .map(|wt| (wt.id, (wt.slug, wt.branch)))
                .collect(),
            Err(e) => return tool_err(e),
        };

    // Batch-load workflow run IDs for all agent run IDs
    let run_ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    let wf_mgr = WorkflowManager::new(&conn);
    let workflow_id_map = match wf_mgr.get_workflow_run_ids_for_agent_runs(&run_ids) {
        Ok(m) => m,
        Err(e) => return tool_err(e),
    };

    // Batch-load pending feedback for all waiting runs (avoids N+1 queries)
    let waiting_run_ids: Vec<&str> = runs
        .iter()
        .filter(|r| r.status == conductor_core::agent::AgentRunStatus::WaitingForFeedback)
        .map(|r| r.id.as_str())
        .collect();
    let feedback_map = match agent_mgr.pending_feedback_for_runs(&waiting_run_ids) {
        Ok(m) => m,
        Err(e) => return tool_err(e),
    };

    let mut out = String::new();
    for run in &runs {
        out.push_str(&format!("run_id: {}\n", run.id));
        out.push_str(&format!("status: {}\n", run.status));
        if let Some(wt_id) = &run.worktree_id {
            if let Some((slug, branch)) = wt_map.get(wt_id) {
                out.push_str(&format!("worktree: {slug}\n"));
                out.push_str(&format!("branch: {branch}\n"));
            }
        }
        if let Some(wf_run_id) = workflow_id_map.get(&run.id) {
            out.push_str(&format!("workflow_run_id: {wf_run_id}\n"));
        }
        out.push_str(&format!("started_at: {}\n", run.started_at));
        if let Some(ended) = &run.ended_at {
            out.push_str(&format!("ended_at: {ended}\n"));
        }
        // Show pending feedback details for waiting runs
        if let Some(fb) = feedback_map.get(&run.id) {
            out.push_str(&format!("feedback_prompt: {}\n", fb.prompt));
            out.push_str(&format!("feedback_type: {}\n", fb.feedback_type));
            if let Some(ref opts) = fb.options {
                for opt in opts {
                    out.push_str(&format!("  option: {} ({})\n", opt.label, opt.value));
                }
            }
            if let Some(timeout) = fb.timeout_secs {
                out.push_str(&format!("feedback_timeout_secs: {timeout}\n"));
            }
        }
        out.push('\n');
    }

    if runs.len() == limit {
        out.push_str(&format!(
            "Showing {offset}–{end} (limit {limit}). Pass offset={next} for more.",
            end = offset + runs.len(),
            next = offset + limit,
        ));
    }

    tool_ok(out)
}

pub(super) fn tool_submit_agent_feedback(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::agent::AgentManager;

    let run_id = require_arg!(args, "run_id");
    let feedback = require_arg!(args, "feedback");

    let (conn, _config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let mgr = AgentManager::new(&conn);
    let pending = match mgr.pending_feedback_for_run(run_id) {
        Ok(Some(fb)) => fb,
        Ok(None) => {
            return tool_err(format!(
                "No pending feedback request found for run {run_id}. \
                 The run may not be waiting for feedback."
            ))
        }
        Err(e) => return tool_err(e),
    };

    // For select types, include the options in the response so the caller
    // knows what values are valid.
    let type_hint = format!("feedback_type: {}", pending.feedback_type);
    let options_hint = pending
        .options
        .as_ref()
        .map(|opts| {
            let items: Vec<String> = opts
                .iter()
                .map(|o| format!("  - {} ({})", o.label, o.value))
                .collect();
            format!("\nOptions:\n{}", items.join("\n"))
        })
        .unwrap_or_default();

    match mgr.submit_feedback(&pending.id, feedback) {
        Ok(_) => tool_ok(format!(
            "Feedback submitted for run {run_id}. Agent has been resumed.\n\
             [{type_hint}]{options_hint}"
        )),
        Err(e) => tool_err(e),
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

    #[test]
    fn test_dispatch_list_agent_runs_empty() {
        let (_f, db) = make_test_db();
        let result = tool_list_agent_runs(&db, &empty_args());
        assert_ne!(
            result.is_error,
            Some(true),
            "empty call should succeed, got: {:?}",
            result.content
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("No agent runs"), "got: {text}");
    }

    #[test]
    fn test_dispatch_list_agent_runs_worktree_requires_repo() {
        let (_f, db) = make_test_db();
        let args = args_with("worktree", "some-wt");
        let result = tool_list_agent_runs(&db, &args);
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
    fn test_dispatch_list_agent_runs_status_filter() {
        use conductor_core::agent::AgentManager;
        use conductor_core::db::open_database;

        let (_f, db) = make_test_db();
        {
            let conn = open_database(&db).expect("open db");
            conn.execute(
                "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
                 VALUES ('r1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
                 VALUES ('w1', 'r1', 'feat-test', 'feat/test', '/tmp/ws/feat-test', 'active', '2024-01-01T00:00:00Z')",
                [],
            ).unwrap();
            let mgr = AgentManager::new(&conn);
            let r1 = mgr
                .create_run(Some("w1"), "running task", None, None)
                .unwrap();
            let r2 = mgr
                .create_run(Some("w1"), "completed task", None, None)
                .unwrap();
            mgr.update_run_completed(
                &r2.id,
                None,
                Some("Done"),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let _ = (r1, r2);
        }

        // Filter by running — should see only the running task
        let args = args_with("status", "running");
        let result = tool_list_agent_runs(&db, &args);
        assert_ne!(result.is_error, Some(true), "should not error");
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("status: running"), "got: {text}");
        assert!(!text.contains("status: completed"), "got: {text}");
    }

    #[test]
    fn test_dispatch_list_agent_runs_waiting_for_feedback() {
        use conductor_core::agent::AgentManager;
        use conductor_core::db::open_database;

        let (_f, db) = make_test_db();
        {
            let conn = open_database(&db).expect("open db");
            conn.execute(
                "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
                 VALUES ('r1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
                 VALUES ('w1', 'r1', 'feat-test', 'feat/test', '/tmp/ws/feat-test', 'active', '2024-01-01T00:00:00Z')",
                [],
            ).unwrap();
            let mgr = AgentManager::new(&conn);
            let run = mgr
                .create_run(Some("w1"), "needs feedback", None, None)
                .unwrap();
            // Transition to waiting_for_feedback via request_feedback
            mgr.request_feedback(&run.id, "Please approve?", None)
                .unwrap();
        }

        let args = args_with("status", "waiting_for_feedback");
        let result = tool_list_agent_runs(&db, &args);
        assert_ne!(result.is_error, Some(true), "should not error");
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("status: waiting_for_feedback"), "got: {text}");
    }

    #[test]
    fn test_dispatch_submit_agent_feedback_missing_run_id() {
        let (_f, db) = make_test_db();
        let args = args_with("feedback", "some response");
        let result = tool_submit_agent_feedback(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_submit_agent_feedback_missing_feedback() {
        let (_f, db) = make_test_db();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_submit_agent_feedback(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_submit_agent_feedback_no_pending() {
        use conductor_core::agent::AgentManager;
        use conductor_core::db::open_database;

        let (_f, db) = make_test_db();
        // Create an agent run (not waiting for feedback)
        let conn = open_database(&db).expect("open db");
        let mgr = AgentManager::new(&conn);
        let run = mgr
            .create_run(None, "do something", None, None)
            .expect("create run");

        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run.id.clone()));
        args.insert(
            "feedback".to_string(),
            Value::String("some response".to_string()),
        );
        let result = tool_submit_agent_feedback(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("No pending feedback request"), "got: {text}");
    }

    #[test]
    fn test_dispatch_submit_agent_feedback_success() {
        use conductor_core::agent::{AgentManager, AgentRunStatus};
        use conductor_core::db::open_database;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let mgr = AgentManager::new(&conn);
        let run = mgr
            .create_run(None, "do something", None, None)
            .expect("create run");
        // Create a pending feedback request (this also sets run status to waiting_for_feedback)
        mgr.request_feedback(&run.id, "Should I proceed?", None)
            .expect("request feedback");

        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run.id.clone()));
        args.insert(
            "feedback".to_string(),
            Value::String("Yes, proceed.".to_string()),
        );
        let result = tool_submit_agent_feedback(&db, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "submit_agent_feedback should succeed; got: {:?}",
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
        assert!(text.contains("Feedback submitted"), "got: {text}");

        // Verify run status is back to running
        let conn2 = open_database(&db).expect("open db");
        let mgr2 = AgentManager::new(&conn2);
        let updated = mgr2.get_run(&run.id).expect("query").expect("run exists");
        assert_eq!(updated.status, AgentRunStatus::Running);
    }
}
