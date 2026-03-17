use std::path::Path;

use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{get_arg, open_db_and_config, tool_err, tool_ok};

fn format_prs_output(
    prs: &[conductor_core::github::GithubPr],
    wt_mgr: &conductor_core::worktree::WorktreeManager<'_>,
    repo_id: &str,
) -> String {
    let mut out = String::new();
    for pr in prs {
        let draft_label = if pr.is_draft { " [DRAFT]" } else { "" };
        let review = pr.review_decision.as_deref().unwrap_or("NONE");
        let worktree_line = match wt_mgr.get_by_branch(repo_id, &pr.head_ref_name) {
            Ok(wt) => format!(
                "  worktree_slug: {}\n  worktree_status: {}\n",
                wt.slug, wt.status
            ),
            Err(_) => String::new(),
        };
        out.push_str(&format!(
            "#{number} — {title}{draft}\n  url: {url}\n  branch: {branch}\n  author: {author}\n  review: {review}\n  ci: {ci}\n{worktree_line}\n",
            number = pr.number,
            title = pr.title,
            draft = draft_label,
            url = pr.url,
            branch = pr.head_ref_name,
            author = pr.author,
            review = review,
            ci = pr.ci_status,
            worktree_line = worktree_line,
        ));
    }
    out
}

pub(super) fn tool_list_prs(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::github::list_open_prs;
    use conductor_core::repo::RepoManager;
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };

    let repo = match RepoManager::new(&conn, &config).get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };

    let prs = match list_open_prs(&repo.remote_url) {
        Ok(p) => p,
        Err(e) => return tool_err(e),
    };

    if prs.is_empty() {
        return tool_ok(format!("No open PRs found for repo '{repo_slug}'."));
    }

    let wt_mgr = WorktreeManager::new(&conn, &config);
    tool_ok(format_prs_output(&prs, &wt_mgr, &repo.id))
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
    fn test_dispatch_list_prs_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = tool_list_prs(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_list_prs_unknown_repo() {
        let (_f, db) = make_test_db();
        let args = args_with("repo", "nonexistent-repo");
        let result = tool_list_prs(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("not found"),
            "expected 'not found' error, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_list_prs_non_github_repo_returns_empty() {
        use conductor_core::db::open_database;
        let (_f, db) = make_test_db();
        {
            // Register a non-GitHub repo (no open PRs can be fetched).
            let conn = open_database(&db).expect("open db");
            conn.execute(
                "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
                 VALUES ('r1', 'local-repo', '/tmp/repo', 'file:///tmp/repo.git', 'main', '/tmp/ws', '2024-01-01T00:00:00Z')",
                [],
            ).unwrap();
        }
        let args = args_with("repo", "local-repo");
        let result = tool_list_prs(&db, &args);
        // Non-GitHub repos yield empty PR list — tool_ok with "No open PRs" message.
        assert_ne!(
            result.is_error,
            Some(true),
            "should not error for non-GitHub repo"
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("No open PRs"), "got: {text}");
    }

    #[test]
    fn test_format_prs_output_includes_worktree_slug_and_status() {
        use conductor_core::db::open_database;
        use conductor_core::github::GithubPr;
        use conductor_core::worktree::WorktreeManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");

        // Insert a repo and a matching worktree.
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
             VALUES ('r1', 'my-repo', '/tmp/repo', 'file:///tmp/repo.git', 'main', '/tmp/ws', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w1', 'r1', 'feat-my-feature', 'feat/my-feature', '/tmp/ws/feat-my-feature', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let config = conductor_core::config::Config::default();
        let wt_mgr = WorktreeManager::new(&conn, &config);

        let prs = vec![GithubPr {
            number: 42,
            title: "My feature".to_string(),
            url: "https://github.com/owner/repo/pull/42".to_string(),
            author: "alice".to_string(),
            state: "OPEN".to_string(),
            head_ref_name: "feat/my-feature".to_string(),
            is_draft: false,
            review_decision: None,
            ci_status: "SUCCESS".to_string(),
        }];

        let out = format_prs_output(&prs, &wt_mgr, "r1");
        assert!(out.contains("worktree_slug: feat-my-feature"), "got: {out}");
        assert!(out.contains("worktree_status: active"), "got: {out}");
        assert!(out.contains("#42"), "got: {out}");
    }

    #[test]
    fn test_format_prs_output_no_worktree_omits_worktree_fields() {
        use conductor_core::db::open_database;
        use conductor_core::github::GithubPr;
        use conductor_core::worktree::WorktreeManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");

        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
             VALUES ('r1', 'my-repo', '/tmp/repo', 'file:///tmp/repo.git', 'main', '/tmp/ws', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let config = conductor_core::config::Config::default();
        let wt_mgr = WorktreeManager::new(&conn, &config);

        let prs = vec![GithubPr {
            number: 7,
            title: "Unlinked PR".to_string(),
            url: "https://github.com/owner/repo/pull/7".to_string(),
            author: "bob".to_string(),
            state: "OPEN".to_string(),
            head_ref_name: "fix/some-bug".to_string(),
            is_draft: false,
            review_decision: None,
            ci_status: "PENDING".to_string(),
        }];

        let out = format_prs_output(&prs, &wt_mgr, "r1");
        assert!(
            !out.contains("worktree_slug"),
            "should not contain worktree_slug, got: {out}"
        );
        assert!(
            !out.contains("worktree_status"),
            "should not contain worktree_status, got: {out}"
        );
        assert!(out.contains("#7"), "got: {out}");
    }
}
