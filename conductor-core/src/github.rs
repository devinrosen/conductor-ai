use std::process::Command;

use crate::error::{ConductorError, Result};
use crate::tickets::TicketInput;

/// Sync open GitHub issues for a repo using the `gh` CLI.
/// Returns a list of normalized TicketInputs ready for upsert.
pub fn sync_github_issues(owner: &str, repo: &str) -> Result<Vec<TicketInput>> {
    let output = Command::new("gh")
        .args([
            "issue",
            "list",
            "--repo",
            &format!("{owner}/{repo}"),
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number,title,body,labels,assignees,state,url",
        ])
        .output()
        .map_err(|e| ConductorError::TicketSync(format!("failed to run gh: {e}")))?;

    if !output.status.success() {
        return Err(ConductorError::TicketSync(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let issues: Vec<serde_json::Value> = serde_json::from_str(&json_str)
        .map_err(|e| ConductorError::TicketSync(format!("failed to parse gh output: {e}")))?;

    let tickets = issues
        .into_iter()
        .map(|issue| {
            let number = issue["number"].as_u64().unwrap_or(0);
            let labels: Vec<String> = issue["labels"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|l| l["name"].as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let assignee = issue["assignees"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|a| a["login"].as_str())
                .map(|s| s.to_string());

            TicketInput {
                source_type: "github".to_string(),
                source_id: number.to_string(),
                title: issue["title"].as_str().unwrap_or("").to_string(),
                body: issue["body"].as_str().unwrap_or("").to_string(),
                state: "open".to_string(),
                labels: serde_json::to_string(&labels).unwrap_or_else(|_| "[]".to_string()),
                assignee,
                priority: None,
                url: issue["url"].as_str().unwrap_or("").to_string(),
                raw_json: serde_json::to_string(&issue).unwrap_or_else(|_| "{}".to_string()),
            }
        })
        .collect();

    Ok(tickets)
}

/// Parse a GitHub remote URL to extract owner and repo name.
/// Handles both SSH (git@github.com:owner/repo.git) and HTTPS (https://github.com/owner/repo.git).
pub fn parse_github_remote(remote_url: &str) -> Option<(String, String)> {
    // SSH format: git@github.com:owner/repo.git
    if let Some(rest) = remote_url.strip_prefix("git@github.com:") {
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }

    // HTTPS format: https://github.com/owner/repo.git
    if remote_url.contains("github.com/") {
        let after = remote_url.split("github.com/").nth(1)?;
        let after = after.strip_suffix(".git").unwrap_or(after);
        let parts: Vec<&str> = after.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ssh_remote() {
        let (owner, repo) = parse_github_remote("git@github.com:devinrosen/conductor-ai.git").unwrap();
        assert_eq!(owner, "devinrosen");
        assert_eq!(repo, "conductor-ai");
    }

    #[test]
    fn test_parse_https_remote() {
        let (owner, repo) = parse_github_remote("https://github.com/devinrosen/conductor-ai.git").unwrap();
        assert_eq!(owner, "devinrosen");
        assert_eq!(repo, "conductor-ai");
    }

    #[test]
    fn test_parse_https_no_suffix() {
        let (owner, repo) = parse_github_remote("https://github.com/devinrosen/conductor-ai").unwrap();
        assert_eq!(owner, "devinrosen");
        assert_eq!(repo, "conductor-ai");
    }

    #[test]
    fn test_parse_non_github() {
        assert!(parse_github_remote("https://gitlab.com/user/repo.git").is_none());
    }
}
