use std::path::Path;

use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{get_arg, open_db_and_config, tool_err, tool_ok};

pub(super) fn tool_list_repos(db_path: &Path) -> CallToolResult {
    use conductor_core::agent::AgentManager;
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::WorkflowManager;

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repos = match RepoManager::new(&conn, &config).list() {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };
    if repos.is_empty() {
        return tool_ok("No repos registered. Use `conductor repo register` to register one.");
    }
    let agent_counts = match AgentManager::new(&conn).active_run_counts_by_repo() {
        Ok(m) => m,
        Err(e) => return tool_err(e),
    };
    let workflow_counts = match WorkflowManager::new(&conn).active_run_counts_by_repo() {
        Ok(m) => m,
        Err(e) => return tool_err(e),
    };
    let mut out = String::new();
    for r in repos {
        out.push_str(&format!(
            "slug: {}\nlocal_path: {}\nremote_url: {}\ndefault_branch: {}\n",
            r.slug, r.local_path, r.remote_url, r.default_branch
        ));
        let running = agent_counts.get(&r.id).map_or(0, |c| c.running)
            + workflow_counts.get(&r.id).map_or(0, |c| c.running);
        let waiting = agent_counts.get(&r.id).map_or(0, |c| c.waiting)
            + workflow_counts.get(&r.id).map_or(0, |c| c.waiting);
        let pending = workflow_counts.get(&r.id).map_or(0, |c| c.pending);
        let mut parts: Vec<String> = Vec::new();
        if running > 0 {
            parts.push(format!("{running} running"));
        }
        if waiting > 0 {
            parts.push(format!("{waiting} waiting"));
        }
        if pending > 0 {
            parts.push(format!("{pending} pending"));
        }
        if !parts.is_empty() {
            out.push_str(&format!("active_runs: {}\n", parts.join(", ")));
        }
        out.push('\n');
    }
    tool_ok(out)
}

pub(super) fn tool_register_repo(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};

    let remote_url = require_arg!(args, "remote_url");
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let slug = derive_slug_from_url(remote_url);
    let local = match get_arg(args, "local_path") {
        Some(p) => p.to_string(),
        None => derive_local_path(&config, &slug),
    };
    match RepoManager::new(&conn, &config).register(&slug, &local, remote_url, None) {
        Ok(repo) => tool_ok(format!(
            "Registered repo: {slug}\nlocal_path: {local_path}\nremote_url: {remote_url}\ndefault_branch: {default_branch}\n",
            slug = repo.slug,
            local_path = repo.local_path,
            remote_url = repo.remote_url,
            default_branch = repo.default_branch,
        )),
        Err(e) => tool_err(e),
    }
}

pub(super) fn tool_unregister_repo(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::repo::RepoManager;

    let slug = require_arg!(args, "repo");
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    match RepoManager::new(&conn, &config).unregister(slug) {
        Ok(()) => tool_ok(format!(
            "Unregistered repo: {slug}. The local directory was not modified."
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

    // -- tool_list_repos ----------------------------------------------------

    #[test]
    fn test_dispatch_list_repos_empty_db() {
        let (_f, db) = make_test_db();
        let result = tool_list_repos(&db);
        assert_ne!(result.is_error, Some(true), "empty list should succeed");
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("No repos registered"),
            "expected empty message, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_list_repos_populated() {
        use conductor_core::config::load_config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, db) = make_test_db();
        {
            let conn = open_database(&db).expect("open db");
            let config = load_config().expect("load config");
            RepoManager::new(&conn, &config)
                .register(
                    "my-repo",
                    "/tmp/my-repo",
                    "https://github.com/acme/my-repo",
                    None,
                )
                .expect("register repo");
        }
        let result = tool_list_repos(&db);
        assert_ne!(result.is_error, Some(true), "populated list should succeed");
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("my-repo"),
            "expected slug in output, got: {text}"
        );
        assert!(
            text.contains("/tmp/my-repo"),
            "expected local_path in output, got: {text}"
        );
        assert!(
            text.contains("https://github.com/acme/my-repo"),
            "expected remote_url in output, got: {text}"
        );
        // No active runs — active_runs: line must be absent
        assert!(
            !text.contains("active_runs:"),
            "expected no active_runs line when no runs exist, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_list_repos_with_active_runs() {
        use conductor_core::agent::AgentManager;
        use conductor_core::config::load_config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, db) = make_test_db();
        {
            let conn = open_database(&db).expect("open db");
            let config = load_config().expect("load config");
            let repo = RepoManager::new(&conn, &config)
                .register(
                    "active-repo",
                    "/tmp/active-repo",
                    "https://github.com/acme/active-repo",
                    None,
                )
                .expect("register repo");
            // Insert a worktree directly (avoids actual git ops)
            conn.execute(
                "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
                 VALUES ('wt-test-1', ?1, 'feat-x', 'feat/x', '/tmp/active-repo/feat-x', 'active', '2024-01-01T00:00:00Z')",
                rusqlite::params![repo.id],
            ).expect("insert worktree");
            // Create an agent run in running status via AgentManager (default status = running)
            AgentManager::new(&conn)
                .create_run(Some("wt-test-1"), "test prompt", None, None)
                .expect("create run");
        }
        let result = tool_list_repos(&db);
        assert_ne!(result.is_error, Some(true), "should succeed");
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("active_runs: 1 running"),
            "expected active_runs line, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_register_repo_missing_url() {
        let (_f, db) = make_test_db();
        let result = tool_register_repo(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_register_repo_ok() {
        let (_f, db) = make_test_db();
        let args = args_with("remote_url", "https://github.com/acme/my-repo");
        let result = tool_register_repo(&db, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "should succeed; got: {:?}",
            result.content
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("my-repo"),
            "slug missing from output, got: {text}"
        );
        assert!(
            text.contains("https://github.com/acme/my-repo"),
            "remote_url missing, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_register_repo_with_local_path() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert(
            "remote_url".into(),
            Value::String("https://github.com/acme/other-repo".into()),
        );
        args.insert(
            "local_path".into(),
            Value::String("/custom/path/other-repo".into()),
        );
        let result = tool_register_repo(&db, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "should succeed; got: {:?}",
            result.content
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("/custom/path/other-repo"),
            "explicit local_path missing from output, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_register_repo_duplicate() {
        let (_f, db) = make_test_db();
        let args = args_with("remote_url", "https://github.com/acme/dup-repo");
        let first = tool_register_repo(&db, &args);
        assert_ne!(first.is_error, Some(true), "first register should succeed");
        let second = tool_register_repo(&db, &args);
        assert_eq!(
            second.is_error,
            Some(true),
            "duplicate register should fail"
        );
    }

    #[test]
    fn test_dispatch_unregister_repo_missing_arg() {
        let (_f, db) = make_test_db();
        let result = tool_unregister_repo(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_unregister_repo_not_found() {
        let (_f, db) = make_test_db();
        let args = args_with("repo", "ghost-repo");
        let result = tool_unregister_repo(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_unregister_repo_ok() {
        let (_f, db) = make_test_db();
        // Register first
        let reg_args = args_with("remote_url", "https://github.com/acme/to-remove");
        let reg = tool_register_repo(&db, &reg_args);
        assert_ne!(reg.is_error, Some(true), "register should succeed");
        // Now unregister
        let unreg_args = args_with("repo", "to-remove");
        let result = tool_unregister_repo(&db, &unreg_args);
        assert_ne!(
            result.is_error,
            Some(true),
            "unregister should succeed; got: {:?}",
            result.content
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Unregistered repo: to-remove"),
            "expected success message, got: {text}"
        );
        assert!(
            text.contains("local directory was not modified"),
            "expected non-destructive note, got: {text}"
        );
    }
}
