use std::path::Path;

use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{get_arg, open_db_and_config, tool_err, tool_ok};

/// Returns `true` if `s` looks like a ULID: exactly 26 uppercase alphanumeric chars.
/// Used to distinguish internal ULIDs (e.g. "01HXYZ...") from external source IDs (e.g. "680").
fn looks_like_ulid(s: &str) -> bool {
    s.len() == 26 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

pub(super) fn tool_list_worktrees(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");
    let active_only = match get_arg(args, "status") {
        None | Some("active") => true,
        Some("all") => false,
        Some(other) => {
            return tool_err(format!(
                "Unknown status value '{other}'. Valid values: 'active', 'all'."
            ))
        }
    };
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wt_mgr = WorktreeManager::new(&conn, &config);
    let worktrees = match wt_mgr.list(Some(repo_slug), active_only) {
        Ok(w) => w,
        Err(e) => return tool_err(e),
    };
    if worktrees.is_empty() {
        let scope = if active_only { "active " } else { "" };
        return tool_ok(format!("No {scope}worktrees for {repo_slug}."));
    }
    let mut out = String::new();
    for wt in worktrees {
        out.push_str(&format!(
            "slug: {}\nbranch: {}\nstatus: {}\npath: {}\n\n",
            wt.slug, wt.branch, wt.status, wt.path
        ));
    }
    tool_ok(out)
}

pub(super) fn tool_get_worktree(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::agent::AgentManager;
    use conductor_core::github::get_pr_detail;
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;
    use conductor_core::workflow::WorkflowManager;
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");
    let wt_slug = require_arg!(args, "slug");

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };

    let repo = match RepoManager::new(&conn, &config).get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };

    let wt = match WorktreeManager::new(&conn, &config).get_by_slug_or_branch(&repo.id, wt_slug) {
        Ok(w) => w,
        Err(e) => return tool_err(e),
    };

    let mut out = format!(
        "slug: {}\nbranch: {}\nstatus: {}\npath: {}\nmodel: {}\ncreated_at: {}\n",
        wt.slug,
        wt.branch,
        wt.status,
        wt.path,
        wt.model.as_deref().unwrap_or("default"),
        wt.created_at,
    );

    // Linked ticket
    if let Some(ticket_id) = &wt.ticket_id {
        let syncer = TicketSyncer::new(&conn);
        match syncer.get_by_id(ticket_id) {
            Ok(ticket) => {
                out.push_str(&format!(
                    "\nlinked_ticket: #{} — {}\nticket_url: {}\n",
                    ticket.source_id, ticket.title, ticket.url
                ));
            }
            Err(e) => {
                out.push_str(&format!("\nlinked_ticket_error: {e}\n"));
            }
        }
    }

    // PR detail (best-effort, synchronous gh call)
    if let Some(pr) = get_pr_detail(&repo.remote_url, &wt.branch) {
        out.push_str(&format!(
            "\npr_number: {}\npr_title: {}\npr_url: {}\npr_state: {}\npr_ci_status: {}\n",
            pr.number, pr.title, pr.url, pr.state, pr.ci_status
        ));
    }

    // Latest agent run
    let agent_mgr = AgentManager::new(&conn);
    match agent_mgr.latest_run_for_worktree(&wt.id) {
        Ok(Some(run)) => {
            out.push_str(&format!(
                "\nlatest_agent_run_id: {}\nlatest_agent_run_status: {}\nlatest_agent_run_started_at: {}\n",
                run.id, run.status, run.started_at,
            ));
            if let Some(ended_at) = &run.ended_at {
                out.push_str(&format!("latest_agent_run_ended_at: {ended_at}\n"));
            }
        }
        Ok(None) => {}
        Err(e) => out.push_str(&format!("\nlatest_agent_run_error: {e}\n")),
    }

    // Latest workflow run
    let wf_mgr = WorkflowManager::new(&conn);
    match wf_mgr.list_workflow_runs(&wt.id) {
        Ok(runs) => {
            if let Some(run) = runs.first() {
                out.push_str(&format!(
                    "\nlatest_workflow_run_id: {}\nlatest_workflow_run_name: {}\nlatest_workflow_run_status: {}\nlatest_workflow_run_started_at: {}\n",
                    run.id, run.workflow_name, run.status, run.started_at,
                ));
            }
        }
        Err(e) => out.push_str(&format!("\nlatest_workflow_run_error: {e}\n")),
    }

    tool_ok(out)
}

pub(super) fn tool_create_worktree(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");
    let name = require_arg!(args, "name");
    let raw_ticket_id = get_arg(args, "ticket_id");

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };

    // Resolve ticket_id: if it looks like a ULID pass it through; otherwise treat
    // it as an external source_id and look up the internal ULID.
    let resolved_ticket_id: Option<String> = match raw_ticket_id {
        None => None,
        Some(id) if looks_like_ulid(id) => Some(id.to_string()),
        Some(source_id) => {
            let repo_mgr = RepoManager::new(&conn, &config);
            let repo = match repo_mgr.get_by_slug(repo_slug) {
                Ok(r) => r,
                Err(e) => return tool_err(e),
            };
            let syncer = TicketSyncer::new(&conn);
            match syncer.get_by_source_id(&repo.id, source_id) {
                Ok(ticket) => Some(ticket.id),
                Err(e) => {
                    return tool_err(format!("Could not resolve ticket ID '{source_id}': {e}"))
                }
            }
        }
    };

    let wt_mgr = WorktreeManager::new(&conn, &config);
    match wt_mgr.create(
        repo_slug,
        name,
        None,
        resolved_ticket_id.as_deref(),
        None,
        false,
    ) {
        Ok((wt, warnings)) => {
            let mut msg = format!(
                "Worktree created.\nslug: {}\nbranch: {}\npath: {}\n",
                wt.slug, wt.branch, wt.path
            );
            for w in warnings {
                msg.push_str(&format!("warning: {w}\n"));
            }
            tool_ok(msg)
        }
        Err(e) => tool_err(e),
    }
}

pub(super) fn tool_delete_worktree(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");
    let slug = require_arg!(args, "slug");
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wt_mgr = WorktreeManager::new(&conn, &config);
    match wt_mgr.delete(repo_slug, slug) {
        Ok(wt) => tool_ok(format!("Deleted worktree {}.", wt.slug)),
        Err(e) => tool_err(e),
    }
}

pub(super) fn tool_push_worktree(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");
    let slug = require_arg!(args, "slug");
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wt_mgr = WorktreeManager::new(&conn, &config);
    match wt_mgr.push(repo_slug, slug) {
        Ok(msg) => tool_ok(msg),
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

    fn result_args(m: serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
        m
    }

    #[test]
    fn test_dispatch_list_worktrees_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = tool_list_worktrees(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_list_worktrees_default_status_active_only() {
        let (_f, db) = make_test_db();
        let args = args_with("repo", "nonexistent-repo");
        let result = tool_list_worktrees(&db, &args);
        // Unknown repo returns empty list (not an error) — confirms default path works.
        assert_eq!(result.is_error, Some(false));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(!text.contains("Unknown status value"), "got: {text}");
        assert!(
            text.contains("active"),
            "default should reference active, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_list_worktrees_explicit_active_status() {
        let (_f, db) = make_test_db();
        let mut args = args_with("repo", "nonexistent-repo");
        args.insert("status".to_string(), Value::String("active".to_string()));
        let result = tool_list_worktrees(&db, &args);
        assert_eq!(result.is_error, Some(false));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(!text.contains("Unknown status value"), "got: {text}");
        assert!(
            text.contains("active"),
            "explicit active should reference active, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_list_worktrees_status_all() {
        let (_f, db) = make_test_db();
        let mut args = args_with("repo", "nonexistent-repo");
        args.insert("status".to_string(), Value::String("all".to_string()));
        let result = tool_list_worktrees(&db, &args);
        assert_eq!(result.is_error, Some(false));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(!text.contains("Unknown status value"), "got: {text}");
        // status=all omits "active" scope qualifier in the empty message
        assert!(
            !text.contains("active "),
            "all-status should not say 'active', got: {text}"
        );
    }

    #[test]
    fn test_dispatch_list_worktrees_unknown_status_returns_error() {
        let (_f, db) = make_test_db();
        let mut args = args_with("repo", "any-repo");
        args.insert("status".to_string(), Value::String("merged".to_string()));
        let result = tool_list_worktrees(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Unknown status value"), "got: {text}");
    }

    #[test]
    fn test_looks_like_ulid() {
        // Valid ULID: 26 uppercase alphanumeric chars
        assert!(looks_like_ulid("01HXYZABCDEFGHJKMNPQRSTVWX"));
        assert!(looks_like_ulid("01JRKBDR0B7W72V1EHNH78WKTF"));
        // GitHub issue numbers should NOT look like ULIDs
        assert!(!looks_like_ulid("680"));
        assert!(!looks_like_ulid("42"));
        // Too short / too long
        assert!(!looks_like_ulid("01HXYZ"));
        assert!(!looks_like_ulid("01HXYZABCDEFGHJKMNPQRSTVWXYZ"));
    }

    #[test]
    fn test_create_worktree_unknown_external_ticket_id_returns_error() {
        // Passing a numeric source_id that doesn't exist should return is_error=true
        // with a clear message mentioning the source_id.
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let config = Config::default();
        RepoManager::new(&conn, &config)
            .register(
                "test-repo",
                "/tmp/test-repo",
                "https://github.com/x/y",
                None,
            )
            .expect("register repo");

        let mut args = serde_json::Map::new();
        args.insert("repo".to_string(), Value::String("test-repo".to_string()));
        args.insert("name".to_string(), Value::String("feat-test".to_string()));
        args.insert("ticket_id".to_string(), Value::String("999".to_string()));
        let result = tool_create_worktree(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("999"),
            "error should mention the source_id, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_create_worktree_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = tool_create_worktree(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_create_worktree_missing_name_arg() {
        let (_f, db) = make_test_db();
        let result = tool_create_worktree(&db, &args_with("repo", "my-repo"));
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_create_worktree_unknown_repo() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("name".to_string(), Value::String("feat-test".to_string()));
        let result = tool_create_worktree(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_delete_worktree_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = tool_delete_worktree(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_delete_worktree_missing_slug_arg() {
        let (_f, db) = make_test_db();
        let result = tool_delete_worktree(&db, &args_with("repo", "my-repo"));
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_delete_worktree_unknown_repo() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("slug".to_string(), Value::String("feat-wt".to_string()));
        let result = tool_delete_worktree(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_push_worktree_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = tool_push_worktree(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_push_worktree_missing_slug_arg() {
        let (_f, db) = make_test_db();
        let result = tool_push_worktree(&db, &args_with("repo", "my-repo"));
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_push_worktree_unknown_repo() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("slug".to_string(), Value::String("feat-wt".to_string()));
        let result = tool_push_worktree(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_get_worktree_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = tool_get_worktree(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_worktree_missing_slug_arg() {
        let (_f, db) = make_test_db();
        let result = tool_get_worktree(&db, &args_with("repo", "my-repo"));
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_get_worktree_not_found() {
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, db) = make_test_db();

        // Register a repo so the repo lookup succeeds but the worktree is absent.
        {
            let conn = open_database(&db).expect("open db");
            let config = conductor_core::config::Config::default();
            RepoManager::new(&conn, &config)
                .register(
                    "my-repo",
                    "/tmp/my-repo",
                    "https://github.com/org/my-repo.git",
                    None,
                )
                .expect("register repo");
        }

        let mut args = serde_json::Map::new();
        args.insert("repo".into(), Value::String("my-repo".into()));
        args.insert("slug".into(), Value::String("feat-nonexistent".into()));
        let result = tool_get_worktree(&db, &result_args(args));
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_get_worktree_by_branch() {
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let config = conductor_core::config::Config::default();

        let repo = RepoManager::new(&conn, &config)
            .register(
                "my-repo",
                "/tmp/my-repo",
                "https://github.com/org/my-repo.git",
                None,
            )
            .expect("register repo");

        // Insert a worktree directly to avoid git subprocess calls.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, '/tmp/wt', 'active', datetime('now'))",
            rusqlite::params![
                "01JTEST0000000000000000WTB",
                repo.id,
                "feat-my-feature",
                "feat/my-feature",
            ],
        )
        .expect("insert worktree");

        // Look up by branch name instead of slug.
        let mut args = serde_json::Map::new();
        args.insert("repo".into(), Value::String("my-repo".into()));
        args.insert("slug".into(), Value::String("feat/my-feature".into()));
        let result = tool_get_worktree(&db, &result_args(args));
        assert_ne!(
            result.is_error,
            Some(true),
            "lookup by branch name should succeed; got: {:?}",
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
            text.contains("slug: feat-my-feature"),
            "expected slug in output, got: {text}"
        );
    }
}
