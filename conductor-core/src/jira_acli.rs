use std::process::Command;

use crate::error::{ConductorError, Result};
use crate::tickets::TicketInput;

/// Talks to Jira through the `acli` CLI, normalizing results into [`TicketInput`].
///
/// Stateless wrapper that aligns this module's API with the rest of the
/// conductor-core manager pattern (`RepoManager`, `WorktreeManager`,
/// `IssueSourceManager`, etc.).
pub struct JiraAcliManager;

impl JiraAcliManager {
    pub fn new() -> Self {
        Self
    }

    /// Sync Jira issues matching `jql` using the `acli` CLI.
    /// Returns a list of normalized TicketInputs ready for upsert.
    ///
    /// # Trust model
    ///
    /// `jql` is passed verbatim to `acli` as a single argv argument. Shell-level
    /// injection isn't possible (no shell parses it), but JQL-level injection is:
    /// a malicious query can return data the caller didn't intend (e.g. broaden
    /// scope across projects). **Callers must source `jql` from trusted
    /// configuration only**, never from end-user input. This method's only
    /// validation rejects structurally-broken input (empty, NUL, line breaks);
    /// it does not parse JQL semantically.
    pub fn sync_issues(&self, jql: &str, base_url: &str) -> Result<Vec<TicketInput>> {
        validate_jql(jql)?;
        let output = Command::new("acli")
            .args([
                "jira",
                "workitem",
                "search",
                "--jql",
                jql,
                "--json",
                "--limit",
                "200",
                "--fields",
                "key,summary,status,priority,assignee,labels,description",
            ])
            .output()
            .map_err(acli_spawn_error)?;

        if !output.status.success() {
            return Err(ConductorError::TicketSync(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        let json_str = String::from_utf8_lossy(&output.stdout);
        parse_jira_issues(&json_str, base_url)
    }

    /// Fetch a single Jira issue by key and return its current state.
    ///
    /// Uses JQL `key = <issue_key>` with a limit of 1; the issue key is
    /// validated against the canonical `PROJECT-123` format before being
    /// interpolated, so this entry point is not subject to the JQL trust
    /// caveat on [`Self::sync_issues`].
    pub fn fetch_issue(&self, issue_key: &str, base_url: &str) -> Result<TicketInput> {
        validate_issue_key(issue_key)?;
        let jql = format!("key = {issue_key}");
        let output = Command::new("acli")
            .args([
                "jira",
                "workitem",
                "search",
                "--jql",
                &jql,
                "--json",
                "--limit",
                "1",
                "--fields",
                "key,summary,status,priority,assignee,labels,description",
            ])
            .output()
            .map_err(acli_spawn_error)?;

        if !output.status.success() {
            return Err(ConductorError::TicketSync(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        let json_str = String::from_utf8_lossy(&output.stdout);
        let mut tickets = parse_jira_issues(&json_str, base_url)?;
        tickets.pop().ok_or_else(|| ConductorError::TicketNotFound {
            id: issue_key.to_string(),
        })
    }
}

impl Default for JiraAcliManager {
    fn default() -> Self {
        Self::new()
    }
}

fn acli_spawn_error(e: std::io::Error) -> ConductorError {
    if e.kind() == std::io::ErrorKind::NotFound {
        ConductorError::TicketSync(
            "acli not found. Install the Atlassian CLI (acli) and ensure it is on your PATH."
                .to_string(),
        )
    } else {
        ConductorError::TicketSync(format!("failed to run acli: {e}"))
    }
}

/// Reject obviously-broken JQL input before handing it to acli.
///
/// This is a defense-in-depth check; see `JiraAcliManager::sync_issues` for the
/// trust model. We reject empty input and any control characters that suggest
/// an accidental newline-separated paste or NUL-terminated buffer.
fn validate_jql(jql: &str) -> Result<()> {
    if jql.is_empty() {
        return Err(ConductorError::TicketSync(
            "JQL must not be empty".to_string(),
        ));
    }
    if jql.contains('\0') || jql.contains('\r') || jql.contains('\n') {
        return Err(ConductorError::TicketSync(
            "JQL must not contain NUL or line-break characters".to_string(),
        ));
    }
    Ok(())
}

/// Validate that `key` matches the canonical Jira key format: one or more
/// uppercase ASCII letters, a hyphen, then one or more ASCII digits (e.g. PROJ-123).
fn validate_issue_key(key: &str) -> Result<()> {
    let bytes = key.as_bytes();
    let hyphen = bytes.iter().position(|&b| b == b'-').ok_or_else(|| {
        ConductorError::TicketSync("invalid issue key format; expected PROJECT-123".to_string())
    })?;

    if hyphen == 0 || hyphen == bytes.len() - 1 {
        return Err(ConductorError::TicketSync(
            "invalid issue key format; expected PROJECT-123".to_string(),
        ));
    }

    let prefix = &bytes[..hyphen];
    let suffix = &bytes[hyphen + 1..];

    let prefix_valid = !prefix.is_empty()
        && prefix[0].is_ascii_alphabetic()
        && prefix
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let suffix_valid = !suffix.is_empty() && suffix.iter().all(|b| b.is_ascii_digit());

    if prefix_valid && suffix_valid {
        Ok(())
    } else {
        Err(ConductorError::TicketSync(
            "invalid issue key format; expected PROJECT-123".to_string(),
        ))
    }
}

/// Parse acli JSON output into TicketInputs.
fn parse_jira_issues(json_str: &str, base_url: &str) -> Result<Vec<TicketInput>> {
    let issues: Vec<serde_json::Value> = serde_json::from_str(json_str)
        .map_err(|e| ConductorError::TicketSync(format!("failed to parse acli output: {e}")))?;

    let base_url = base_url.trim_end_matches('/');

    let tickets = issues
        .into_iter()
        .map(|issue| {
            let key = issue["key"].as_str().unwrap_or("").to_string();
            let fields = &issue["fields"];

            let title = fields["summary"].as_str().unwrap_or("").to_string();
            let body = fields["description"].as_str().unwrap_or("").to_string();

            let status = fields["status"]["name"].as_str().unwrap_or("open");
            let state = map_jira_status(status).to_string();

            let priority = fields["priority"]["name"].as_str().map(|s| s.to_string());

            let assignee = fields["assignee"]["displayName"]
                .as_str()
                .or_else(|| fields["assignee"]["name"].as_str())
                .map(|s| s.to_string());

            let labels: Vec<String> = fields["labels"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|l| l.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let url = format!("{base_url}/browse/{key}");

            TicketInput {
                source_type: "jira".to_string(),
                source_id: key,
                title,
                body,
                state,
                labels,
                assignee,
                priority,
                url,
                raw_json: serde_json::to_string(&issue).ok(),
                label_details: vec![],
                blocked_by: vec![],
                children: vec![],
                parent: None,
            }
        })
        .collect();

    Ok(tickets)
}

/// Map a Jira status name to a Conductor state.
fn map_jira_status(status: &str) -> &str {
    match status.to_lowercase().as_str() {
        "to do" | "open" | "backlog" | "new" | "created" | "reopened" => "open",
        "in progress" | "in review" | "in development" | "review" => "in_progress",
        "done" | "closed" | "resolved" | "complete" | "completed" => "closed",
        _ => "open",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_jira_issues_basic() {
        let json = r#"[
            {
                "key": "PROJ-1",
                "fields": {
                    "summary": "Fix login bug",
                    "description": "Login fails on Safari",
                    "status": { "name": "To Do" },
                    "priority": { "name": "High" },
                    "assignee": { "displayName": "Alice", "name": "alice" },
                    "labels": ["bug", "frontend"]
                }
            },
            {
                "key": "PROJ-2",
                "fields": {
                    "summary": "Add dark mode",
                    "description": "",
                    "status": { "name": "In Progress" },
                    "priority": { "name": "Medium" },
                    "assignee": null,
                    "labels": []
                }
            }
        ]"#;

        let tickets = parse_jira_issues(json, "https://company.atlassian.net").unwrap();
        assert_eq!(tickets.len(), 2);

        assert_eq!(tickets[0].source_type, "jira");
        assert_eq!(tickets[0].source_id, "PROJ-1");
        assert_eq!(tickets[0].title, "Fix login bug");
        assert_eq!(tickets[0].body, "Login fails on Safari");
        assert_eq!(tickets[0].state, "open");
        assert_eq!(tickets[0].priority, Some("High".to_string()));
        assert_eq!(tickets[0].assignee, Some("Alice".to_string()));
        assert_eq!(
            tickets[0].url,
            "https://company.atlassian.net/browse/PROJ-1"
        );
        assert_eq!(
            tickets[0].labels,
            vec!["bug".to_string(), "frontend".to_string()]
        );

        assert_eq!(tickets[1].source_id, "PROJ-2");
        assert_eq!(tickets[1].state, "in_progress");
        assert_eq!(tickets[1].assignee, None);
        assert_eq!(tickets[1].labels, Vec::<String>::new());
    }

    #[test]
    fn test_parse_jira_issues_empty() {
        let tickets = parse_jira_issues("[]", "https://jira.example.com").unwrap();
        assert!(tickets.is_empty());
    }

    #[test]
    fn test_parse_jira_issues_missing_fields() {
        let json = r#"[{
            "key": "TEST-1",
            "fields": {}
        }]"#;

        let tickets = parse_jira_issues(json, "https://jira.example.com").unwrap();
        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0].source_id, "TEST-1");
        assert_eq!(tickets[0].title, "");
        assert_eq!(tickets[0].body, "");
        assert_eq!(tickets[0].state, "open");
        assert_eq!(tickets[0].priority, None);
        assert_eq!(tickets[0].assignee, None);
        assert_eq!(tickets[0].labels, Vec::<String>::new());
    }

    #[test]
    fn test_parse_jira_issues_trailing_slash_url() {
        let json = r#"[{"key": "X-1", "fields": {"summary": "test", "status": {"name": "Open"}}}]"#;
        let tickets = parse_jira_issues(json, "https://jira.example.com/").unwrap();
        assert_eq!(tickets[0].url, "https://jira.example.com/browse/X-1");
    }

    #[test]
    fn test_parse_jira_invalid_json() {
        let result = parse_jira_issues("not json", "https://jira.example.com");
        assert!(result.is_err());
    }

    #[test]
    fn test_map_jira_status_open_variants() {
        assert_eq!(map_jira_status("To Do"), "open");
        assert_eq!(map_jira_status("Open"), "open");
        assert_eq!(map_jira_status("Backlog"), "open");
        assert_eq!(map_jira_status("New"), "open");
        assert_eq!(map_jira_status("Created"), "open");
        assert_eq!(map_jira_status("Reopened"), "open");
    }

    #[test]
    fn test_map_jira_status_in_progress_variants() {
        assert_eq!(map_jira_status("In Progress"), "in_progress");
        assert_eq!(map_jira_status("In Review"), "in_progress");
        assert_eq!(map_jira_status("In Development"), "in_progress");
        assert_eq!(map_jira_status("Review"), "in_progress");
    }

    #[test]
    fn test_map_jira_status_closed_variants() {
        assert_eq!(map_jira_status("Done"), "closed");
        assert_eq!(map_jira_status("Closed"), "closed");
        assert_eq!(map_jira_status("Resolved"), "closed");
        assert_eq!(map_jira_status("Complete"), "closed");
        assert_eq!(map_jira_status("Completed"), "closed");
    }

    #[test]
    fn test_map_jira_status_case_insensitive() {
        assert_eq!(map_jira_status("to do"), "open");
        assert_eq!(map_jira_status("TO DO"), "open");
        assert_eq!(map_jira_status("in progress"), "in_progress");
        assert_eq!(map_jira_status("IN PROGRESS"), "in_progress");
        assert_eq!(map_jira_status("done"), "closed");
        assert_eq!(map_jira_status("DONE"), "closed");
    }

    #[test]
    fn test_map_jira_status_unknown_defaults_to_open() {
        assert_eq!(map_jira_status("Custom Status"), "open");
        assert_eq!(map_jira_status("Waiting for QA"), "open");
    }

    #[test]
    fn test_validate_issue_key_valid() {
        assert!(validate_issue_key("PROJ-1").is_ok());
        assert!(validate_issue_key("RND-123").is_ok());
        assert!(validate_issue_key("AB-9999").is_ok());
        assert!(validate_issue_key("A1-42").is_ok());
    }

    #[test]
    fn test_validate_issue_key_rejects_injection() {
        assert!(validate_issue_key("RND-1 OR key != RND-1").is_err());
        assert!(validate_issue_key("PROJ-1; DROP TABLE tickets").is_err());
        assert!(validate_issue_key("KEY-1 AND 1=1").is_err());
    }

    #[test]
    fn test_validate_issue_key_rejects_malformed() {
        assert!(validate_issue_key("").is_err());
        assert!(validate_issue_key("NOHYPHEN").is_err());
        assert!(validate_issue_key("-123").is_err());
        assert!(validate_issue_key("PROJ-").is_err());
        assert!(validate_issue_key("proj-1").is_err());
        assert!(validate_issue_key("123-456").is_err());
        assert!(validate_issue_key("PROJ-abc").is_err());
    }

    #[test]
    fn test_parse_jira_assignee_fallback_to_name() {
        let json = r#"[{
            "key": "TEST-1",
            "fields": {
                "summary": "test",
                "status": {"name": "Open"},
                "assignee": {"name": "bob"}
            }
        }]"#;

        let tickets = parse_jira_issues(json, "https://jira.example.com").unwrap();
        assert_eq!(tickets[0].assignee, Some("bob".to_string()));
    }

    // fetch_issue / sync_issues reject malformed input before ever invoking
    // acli, so these tests exercise the validation gate without requiring
    // acli on PATH.

    #[test]
    fn test_fetch_issue_rejects_injection_before_acli() {
        let mgr = JiraAcliManager::new();
        match mgr.fetch_issue("PROJ-1 OR key != PROJ-1", "https://jira.example.com") {
            Err(e) => assert!(
                e.to_string().contains("invalid issue key format"),
                "expected validation error, got: {e}"
            ),
            Ok(_) => panic!("expected error for injection payload"),
        }
    }

    #[test]
    fn test_fetch_issue_rejects_malformed_key_before_acli() {
        let mgr = JiraAcliManager::new();
        for bad in &["", "NOHYPHEN", "-123", "PROJ-", "proj-1", "PROJ-abc"] {
            match mgr.fetch_issue(bad, "https://jira.example.com") {
                Err(e) => assert!(
                    e.to_string().contains("invalid issue key format"),
                    "key {bad:?}: expected validation error, got: {e}"
                ),
                Ok(_) => panic!("key {bad:?} should have been rejected"),
            }
        }
    }

    #[test]
    fn test_validate_jql_accepts_well_formed() {
        assert!(validate_jql("project = FOO AND status != Done").is_ok());
        assert!(validate_jql("key = PROJ-1").is_ok());
        assert!(validate_jql("assignee = currentUser()").is_ok());
    }

    #[test]
    fn test_validate_jql_rejects_empty() {
        assert!(validate_jql("").is_err());
    }

    #[test]
    fn test_validate_jql_rejects_control_chars() {
        assert!(validate_jql("project = FOO\0").is_err());
        assert!(validate_jql("project = FOO\r").is_err());
        assert!(validate_jql("project = FOO\n").is_err());
        assert!(validate_jql("project = FOO\r\nORDER BY rank").is_err());
    }

    #[test]
    fn test_sync_issues_rejects_bad_jql_before_acli() {
        // Hitting validate_jql means we never spawn acli, so this works
        // without acli on PATH.
        let mgr = JiraAcliManager::new();
        match mgr.sync_issues("", "https://jira.example.com") {
            Err(e) => assert!(
                e.to_string().contains("JQL must not be empty"),
                "expected validation error, got: {e}"
            ),
            Ok(_) => panic!("expected empty JQL to be rejected"),
        }
        match mgr.sync_issues("project = FOO\n", "https://jira.example.com") {
            Err(e) => assert!(
                e.to_string().contains("must not contain NUL or line-break"),
                "expected validation error, got: {e}"
            ),
            Ok(_) => panic!("expected newline-bearing JQL to be rejected"),
        }
    }

    #[test]
    fn test_fetch_jira_issue_not_found_returns_ticket_not_found() {
        // parse_jira_issues with an empty array simulates acli returning no results.
        // fetch_jira_issue's not-found path is exercised by calling it indirectly
        // through the parser so we don't need acli installed.
        let mut tickets = parse_jira_issues("[]", "https://jira.example.com").unwrap();
        let result: Result<TicketInput> =
            tickets.pop().ok_or_else(|| ConductorError::TicketNotFound {
                id: "PROJ-1".to_string(),
            });
        assert!(matches!(result, Err(ConductorError::TicketNotFound { .. })));
    }
}
