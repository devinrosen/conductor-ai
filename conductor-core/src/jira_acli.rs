use std::process::Command;

use tracing::warn;

use crate::error::{ConductorError, Result};
use crate::tickets::{TicketComment, TicketInput};

/// A comment returned by `acli jira workitem comment list`.
#[derive(Debug, Clone)]
pub struct JiraComment {
    pub id: String,
    pub author: String,
    pub body: String,
    pub visibility: String,
}

fn parse_jira_comments_value(value: &serde_json::Value) -> Vec<JiraComment> {
    value["comments"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|c| JiraComment {
                    id: c["id"].as_str().unwrap_or("").to_string(),
                    author: c["author"].as_str().unwrap_or("").to_string(),
                    body: c["body"].as_str().unwrap_or("").to_string(),
                    visibility: c["visibility"].as_str().unwrap_or("public").to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a `Vec<JiraComment>` from the JSON produced by `acli jira workitem comment list`.
/// Extracted so it can be unit-tested without requiring the `acli` binary.
#[cfg(test)]
pub(crate) fn parse_jira_comments_json(json_str: &str) -> Vec<JiraComment> {
    match serde_json::from_str::<serde_json::Value>(json_str) {
        Ok(parsed) => parse_jira_comments_value(&parsed),
        Err(_) => vec![],
    }
}

/// Fetch comments for a Jira issue using `acli jira workitem comment list`.
/// Returns an empty vec on any failure (non-fatal).
pub fn fetch_jira_issue_comments(issue_key: &str) -> Vec<JiraComment> {
    let output = match Command::new("acli")
        .args([
            "jira", "workitem", "comment", "list", "--key", issue_key, "--json",
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            warn!("failed to run acli for comments on {issue_key}: {e}");
            return vec![];
        }
    };

    if !output.status.success() {
        warn!(
            "acli comment list failed for {issue_key}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return vec![];
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<serde_json::Value>(&json_str) {
        Ok(parsed) => parse_jira_comments_value(&parsed),
        Err(e) => {
            warn!("failed to parse acli comment output for {issue_key}: {e}");
            vec![]
        }
    }
}

/// Sync Jira issues matching a JQL query using the `acli` CLI.
/// Returns a list of normalized TicketInputs ready for upsert.
pub fn sync_jira_issues_acli(jql: &str, base_url: &str) -> Result<Vec<TicketInput>> {
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
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ConductorError::TicketSync(
                    "acli not found. Install the Atlassian CLI (acli) and ensure it is on your PATH.".to_string(),
                )
            } else {
                ConductorError::TicketSync(format!("failed to run acli: {e}"))
            }
        })?;

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
/// Uses JQL `key = <issue_key>` with a limit of 1 to retrieve only the
/// requested issue, reusing the existing `parse_jira_issues` parser.
pub fn fetch_jira_issue(issue_key: &str, base_url: &str) -> Result<TicketInput> {
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
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ConductorError::TicketSync(
                    "acli not found. Install the Atlassian CLI (acli) and ensure it is on your PATH.".to_string(),
                )
            } else {
                ConductorError::TicketSync(format!("failed to run acli: {e}"))
            }
        })?;

    if !output.status.success() {
        return Err(ConductorError::TicketSync(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let mut tickets = parse_jira_issues(&json_str, base_url)?;
    let mut ticket = tickets
        .pop()
        .ok_or_else(|| ConductorError::TicketNotFound {
            id: issue_key.to_string(),
        })?;

    // Fetch comments lazily and merge into ticket.
    let jira_comments = fetch_jira_issue_comments(issue_key);
    ticket.comments = jira_comments
        .iter()
        .map(|c| TicketComment {
            id: c.id.clone(),
            author: c.author.clone(),
            body: c.body.clone(),
        })
        .collect();

    // Merge comments array into raw_json so ticket_raw_json is self-contained.
    if !jira_comments.is_empty() {
        if let Some(ref raw) = ticket.raw_json {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(raw) {
                let comments_json: Vec<serde_json::Value> = jira_comments
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "id": c.id,
                            "author": c.author,
                            "body": c.body,
                            "visibility": c.visibility,
                        })
                    })
                    .collect();
                v["comments"] = serde_json::Value::Array(comments_json);
                if let Ok(serialized) = serde_json::to_string(&v) {
                    ticket.raw_json = Some(serialized);
                }
                // If serialization fails, keep the original raw_json unchanged.
            }
        }
    }

    Ok(ticket)
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
                comments: vec![],
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

    #[test]
    fn test_parse_jira_comments_json_full() {
        let json = r#"{
            "comments": [
                {"id": "1", "author": "Kate", "body": "Max 30 chars", "visibility": "public"},
                {"id": "2", "author": "Bob", "body": "Agreed", "visibility": "internal"}
            ]
        }"#;
        let comments = parse_jira_comments_json(json);
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].id, "1");
        assert_eq!(comments[0].author, "Kate");
        assert_eq!(comments[0].body, "Max 30 chars");
        assert_eq!(comments[0].visibility, "public");
        assert_eq!(comments[1].author, "Bob");
        assert_eq!(comments[1].visibility, "internal");
    }

    #[test]
    fn test_parse_jira_comments_json_empty_array() {
        let comments = parse_jira_comments_json(r#"{"comments": []}"#);
        assert!(comments.is_empty());
    }

    #[test]
    fn test_parse_jira_comments_json_missing_comments_key() {
        let comments = parse_jira_comments_json(r#"{}"#);
        assert!(comments.is_empty());
    }

    #[test]
    fn test_parse_jira_comments_json_invalid() {
        let comments = parse_jira_comments_json("not json");
        assert!(comments.is_empty());
    }

    #[test]
    fn test_parse_jira_comments_json_missing_optional_fields() {
        let json = r#"{"comments": [{"id": "1"}]}"#;
        let comments = parse_jira_comments_json(json);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id, "1");
        assert_eq!(comments[0].author, "");
        assert_eq!(comments[0].body, "");
        assert_eq!(comments[0].visibility, "public");
    }
}
