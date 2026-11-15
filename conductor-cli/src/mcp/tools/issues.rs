use conductor_core::Conductor;
use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{get_arg, get_list_arg, tool_err, tool_ok};

/// The `\nMilestone: <m>` output suffix, empty when no milestone was requested.
fn milestone_line(milestone: Option<&str>) -> String {
    milestone
        .map(|m| format!("\nMilestone: {m}"))
        .unwrap_or_default()
}

pub(super) fn tool_create_gh_issue(
    conductor: &Conductor,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::agent::AgentManager;
    use conductor_core::github;
    use conductor_core::repo::RepoManager;

    let repo_slug = require_arg!(args, "repo");
    let title = require_arg!(args, "title");
    let body = require_arg!(args, "body");
    let label_list = match get_list_arg(args, "labels") {
        Ok(v) => v,
        Err(msg) => return tool_err(msg),
    };
    let labels: Vec<&str> = label_list.iter().map(String::as_str).collect();
    let run_id = get_arg(args, "run_id");
    let milestone = github::normalize_milestone(get_arg(args, "milestone"));

    let conn = &conductor.conn;
    let config = &conductor.config;

    let repo = match RepoManager::new(conn, config).get_by_slug(repo_slug) {
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

    let (number, url) = match github::create_github_issue(
        &owner, &gh_repo, title, body, &labels, milestone, None,
    ) {
        Ok(v) => v,
        Err(e) => return tool_err(format!("Failed to create issue: {e}")),
    };
    let milestone_line = milestone_line(milestone);

    // If run_id is provided, record the created issue for tracking
    if let Some(rid) = run_id {
        let agent_mgr = AgentManager::new(conn);
        if let Err(e) =
            agent_mgr.record_created_issue(rid, &repo.id, "github", &number, title, &url)
        {
            // Non-fatal: issue was created, but tracking failed
            return tool_ok(format!(
                "Created issue #{number}: {title}\nURL: {url}{milestone_line}\nWarning: failed to record issue link for run {rid}: {e}"
            ));
        }
    }

    tool_ok(format!(
        "Created issue #{number}: {title}\nURL: {url}{milestone_line}"
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::test_helpers::make_test_conductor;
    use super::*;
    use serde_json::Value;

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
    fn test_create_gh_issue_invalid_labels_rejected_before_repo_lookup() {
        let (_f, conductor) = make_test_conductor();
        for labels in [serde_json::json!(42), serde_json::json!(["bug", 1])] {
            let mut a = args_with(&[("repo", "nope"), ("title", "t"), ("body", "b")]);
            a.insert("labels".into(), labels);
            let result = tool_create_gh_issue(&conductor, &a);
            assert_eq!(result.is_error, Some(true));
            let text = result.content[0]
                .as_text()
                .map(|t| t.text.as_str())
                .unwrap();
            assert!(text.starts_with("labels must be"), "{text}");
            assert!(!text.contains('\n'));
        }
    }

    #[test]
    fn test_milestone_line() {
        use conductor_core::github::normalize_milestone;
        assert_eq!(milestone_line(None), "");
        assert_eq!(milestone_line(normalize_milestone(Some(""))), "");
        assert_eq!(milestone_line(normalize_milestone(Some("  "))), "");
        assert_eq!(
            milestone_line(normalize_milestone(Some(" 0.20.0 "))),
            "\nMilestone: 0.20.0"
        );
    }

    #[test]
    fn test_create_gh_issue_missing_repo() {
        let (_f, conductor) = make_test_conductor();
        let result = tool_create_gh_issue(&conductor, &empty_args());
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
        let (_f, conductor) = make_test_conductor();
        let result = tool_create_gh_issue(&conductor, &args_with(&[("repo", "test-repo")]));
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
        let (_f, conductor) = make_test_conductor();
        let result = tool_create_gh_issue(
            &conductor,
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
        let (_f, conductor) = make_test_conductor();
        let result = tool_create_gh_issue(
            &conductor,
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
        use conductor_core::repo::RepoManager;

        let (_f, conductor) = make_test_conductor();
        RepoManager::new(&conductor.conn, &Config::default())
            .register("gitlab-repo", "/tmp/gitlab", "https://gitlab.com/x/y", None)
            .expect("register repo");

        let result = tool_create_gh_issue(
            &conductor,
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
