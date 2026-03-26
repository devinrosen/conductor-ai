use std::path::Path;

use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{get_arg, open_db_and_config, tool_err, tool_ok};

pub(super) fn tool_create_gh_issue(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::agent::AgentManager;
    use conductor_core::github;
    use conductor_core::repo::RepoManager;

    let repo_slug = require_arg!(args, "repo");
    let title = require_arg!(args, "title");
    let body = require_arg!(args, "body");
    let labels_raw = get_arg(args, "labels").unwrap_or("");
    let labels: Vec<&str> = labels_raw
        .split(',')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let run_id = get_arg(args, "run_id");

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };

    let repo = match RepoManager::new(&conn, &config).get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };

    let (owner, gh_repo) = match github::parse_github_remote(&repo.remote_url) {
        Some(pair) => pair,
        None => {
            return tool_err(format!(
                "Could not parse GitHub owner/repo from remote URL: {}",
                repo.remote_url
            ))
        }
    };

    let (number, url) =
        match github::create_github_issue(&owner, &gh_repo, title, body, &labels, None) {
            Ok(v) => v,
            Err(e) => return tool_err(format!("Failed to create issue: {e}")),
        };

    // If run_id is provided, record the created issue for tracking
    if let Some(rid) = run_id {
        let agent_mgr = AgentManager::new(&conn);
        if let Err(e) =
            agent_mgr.record_created_issue(rid, &repo.id, "github", &number, title, &url)
        {
            // Non-fatal: issue was created, but tracking failed
            return tool_ok(format!(
                "Created issue #{number}: {title}\nURL: {url}\nWarning: failed to record issue link for run {rid}: {e}"
            ));
        }
    }

    tool_ok(format!("Created issue #{number}: {title}\nURL: {url}"))
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

    fn args_with(pairs: &[(&str, &str)]) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), Value::String(v.to_string()));
        }
        m
    }

    #[test]
    fn test_create_gh_issue_missing_repo() {
        let (_f, db) = make_test_db();
        let result = tool_create_gh_issue(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Missing required argument"),
            "expected missing arg error, got: {text}"
        );
    }

    #[test]
    fn test_create_gh_issue_missing_title() {
        let (_f, db) = make_test_db();
        let result = tool_create_gh_issue(&db, &args_with(&[("repo", "test-repo")]));
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Missing required argument"),
            "expected missing arg error, got: {text}"
        );
    }

    #[test]
    fn test_create_gh_issue_missing_body() {
        let (_f, db) = make_test_db();
        let result = tool_create_gh_issue(
            &db,
            &args_with(&[("repo", "test-repo"), ("title", "Test issue")]),
        );
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Missing required argument"),
            "expected missing arg error, got: {text}"
        );
    }

    #[test]
    fn test_create_gh_issue_unknown_repo() {
        let (_f, db) = make_test_db();
        let result = tool_create_gh_issue(
            &db,
            &args_with(&[
                ("repo", "ghost-repo"),
                ("title", "Test"),
                ("body", "Test body"),
            ]),
        );
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_create_gh_issue_non_github_remote() {
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let config = Config::default();
        RepoManager::new(&conn, &config)
            .register("gitlab-repo", "/tmp/gitlab", "https://gitlab.com/x/y", None)
            .expect("register repo");
        drop(conn);

        let result = tool_create_gh_issue(
            &db,
            &args_with(&[("repo", "gitlab-repo"), ("title", "Test"), ("body", "Body")]),
        );
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Could not parse GitHub owner/repo"),
            "expected parse error, got: {text}"
        );
    }
}
