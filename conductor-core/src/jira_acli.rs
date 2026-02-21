use std::process::Command;

use crate::error::{ConductorError, Result};
use crate::tickets::TicketInput;

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
                labels: serde_json::to_string(&labels).unwrap_or_else(|_| "[]".to_string()),
                assignee,
                priority,
                url,
                raw_json: serde_json::to_string(&issue).unwrap_or_else(|_| "{}".to_string()),
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
        assert_eq!(tickets[0].labels, r#"["bug","frontend"]"#);

        assert_eq!(tickets[1].source_id, "PROJ-2");
        assert_eq!(tickets[1].state, "in_progress");
        assert_eq!(tickets[1].assignee, None);
        assert_eq!(tickets[1].labels, "[]");
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
        assert_eq!(tickets[0].labels, "[]");
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
}
