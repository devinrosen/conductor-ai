use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{ConductorError, Result};
use crate::tickets::TicketInput;

/// A GitHub repository discovered via the `gh` CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredRepo {
    pub name: String,
    /// Full "owner/repo" name
    pub full_name: String,
    pub description: String,
    /// HTTPS clone URL
    pub clone_url: String,
    /// SSH clone URL
    pub ssh_url: String,
    pub default_branch: String,
    pub private: bool,
}

/// List GitHub organizations the authenticated user belongs to via the `gh` CLI.
/// Returns org login names (e.g. `["myorg", "another-org"]`).
pub fn list_github_orgs() -> Result<Vec<String>> {
    let output = Command::new("gh")
        .args(["api", "user/orgs", "--jq", ".[].login"])
        .output()
        .map_err(|e| ConductorError::TicketSync(format!("failed to run gh: {e}")))?;

    if !output.status.success() {
        return Err(ConductorError::TicketSync(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let orgs = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();

    Ok(orgs)
}

/// Discover GitHub repos for a given owner via the `gh` CLI.
/// Pass `None` for the authenticated user's personal repos, or `Some("orgname")` for an org.
/// Returns up to 200 repos.
pub fn discover_github_repos(owner: Option<&str>) -> Result<Vec<DiscoveredRepo>> {
    let mut args = vec!["repo", "list"];
    if let Some(o) = owner {
        args.push(o);
    }
    args.extend([
        "--limit",
        "200",
        "--json",
        "name,nameWithOwner,description,url,sshUrl,defaultBranchRef,isPrivate",
    ]);

    let output = Command::new("gh")
        .args(&args)
        .output()
        .map_err(|e| ConductorError::TicketSync(format!("failed to run gh: {e}")))?;

    if !output.status.success() {
        return Err(ConductorError::TicketSync(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let items: Vec<serde_json::Value> = serde_json::from_str(&json_str)
        .map_err(|e| ConductorError::TicketSync(format!("failed to parse gh output: {e}")))?;

    let repos = items
        .into_iter()
        .map(|item| {
            let default_branch = item["defaultBranchRef"]["name"]
                .as_str()
                .unwrap_or("main")
                .to_string();
            DiscoveredRepo {
                name: item["name"].as_str().unwrap_or("").to_string(),
                full_name: item["nameWithOwner"].as_str().unwrap_or("").to_string(),
                description: item["description"].as_str().unwrap_or("").to_string(),
                clone_url: item["url"].as_str().unwrap_or("").to_string(),
                ssh_url: item["sshUrl"].as_str().unwrap_or("").to_string(),
                default_branch,
                private: item["isPrivate"].as_bool().unwrap_or(false),
            }
        })
        .collect();

    Ok(repos)
}

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

/// Create a new GitHub issue via the `gh` CLI.
/// Returns `(source_id, url)` where `source_id` is the issue number as a string.
pub fn create_github_issue(
    owner: &str,
    repo: &str,
    title: &str,
    body: &str,
) -> Result<(String, String)> {
    let output = Command::new("gh")
        .args([
            "issue",
            "create",
            "--repo",
            &format!("{owner}/{repo}"),
            "--title",
            title,
            "--body",
            body,
        ])
        .output()
        .map_err(|e| ConductorError::TicketSync(format!("failed to run gh: {e}")))?;

    if !output.status.success() {
        return Err(ConductorError::TicketSync(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    // `gh issue create` prints the issue URL on stdout, e.g.
    // https://github.com/owner/repo/issues/42
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Extract the issue number from the URL's last path segment.
    let number = url.rsplit('/').next().unwrap_or("").to_string();

    if number.is_empty() {
        return Err(ConductorError::TicketSync(format!(
            "unexpected output from gh issue create: {url}"
        )));
    }

    Ok((number, url))
}

/// Search for existing GitHub issues matching a query, filtered by label.
/// Returns parsed JSON objects with `number`, `title`, and `url` fields.
pub fn list_issues_by_search(
    owner: &str,
    repo: &str,
    query: &str,
    label: &str,
    limit: u32,
) -> Result<Vec<serde_json::Value>> {
    let output = Command::new("gh")
        .args([
            "issue",
            "list",
            "--repo",
            &format!("{owner}/{repo}"),
            "--search",
            query,
            "--label",
            label,
            "--json",
            "number,title,url",
            "--limit",
            &limit.to_string(),
        ])
        .output()
        .map_err(|e| ConductorError::TicketSync(format!("failed to run gh: {e}")))?;

    if !output.status.success() {
        return Err(ConductorError::TicketSync(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let issues: Vec<serde_json::Value> = serde_json::from_str(json_str.trim())
        .map_err(|e| ConductorError::TicketSync(format!("failed to parse gh output: {e}")))?;

    Ok(issues)
}

/// Add a label to an existing GitHub issue (best-effort via `gh issue edit`).
pub fn add_label_to_issue(owner: &str, repo: &str, issue_url: &str, label: &str) -> Result<()> {
    let output = Command::new("gh")
        .args([
            "issue",
            "edit",
            issue_url,
            "--repo",
            &format!("{owner}/{repo}"),
            "--add-label",
            label,
        ])
        .output()
        .map_err(|e| ConductorError::TicketSync(format!("failed to run gh: {e}")))?;

    if !output.status.success() {
        return Err(ConductorError::TicketSync(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    Ok(())
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

/// Detect the PR number for a branch using `gh pr list`.
pub fn detect_pr_number(remote_url: &str, branch: &str) -> Option<i64> {
    let (owner, repo) = parse_github_remote(remote_url)?;
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--repo",
            &format!("{owner}/{repo}"),
            "--head",
            branch,
            "--json",
            "number",
            "--limit",
            "1",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    json.first()?.get("number")?.as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ssh_remote() {
        let (owner, repo) =
            parse_github_remote("git@github.com:devinrosen/conductor-ai.git").unwrap();
        assert_eq!(owner, "devinrosen");
        assert_eq!(repo, "conductor-ai");
    }

    #[test]
    fn test_parse_https_remote() {
        let (owner, repo) =
            parse_github_remote("https://github.com/devinrosen/conductor-ai.git").unwrap();
        assert_eq!(owner, "devinrosen");
        assert_eq!(repo, "conductor-ai");
    }

    #[test]
    fn test_parse_https_no_suffix() {
        let (owner, repo) =
            parse_github_remote("https://github.com/devinrosen/conductor-ai").unwrap();
        assert_eq!(owner, "devinrosen");
        assert_eq!(repo, "conductor-ai");
    }

    #[test]
    fn test_parse_non_github() {
        assert!(parse_github_remote("https://gitlab.com/user/repo.git").is_none());
    }

    #[test]
    fn test_parse_discovered_repo_json() {
        // Simulate the JSON output from `gh repo list --json ...`
        let json = r#"[
            {
                "name": "my-repo",
                "nameWithOwner": "alice/my-repo",
                "description": "A test repo",
                "url": "https://github.com/alice/my-repo",
                "sshUrl": "git@github.com:alice/my-repo.git",
                "defaultBranchRef": {"name": "main"},
                "isPrivate": false
            }
        ]"#;
        let items: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        let item = &items[0];
        let default_branch = item["defaultBranchRef"]["name"].as_str().unwrap_or("main");
        assert_eq!(item["name"].as_str().unwrap(), "my-repo");
        assert_eq!(item["nameWithOwner"].as_str().unwrap(), "alice/my-repo");
        assert_eq!(default_branch, "main");
        assert!(!item["isPrivate"].as_bool().unwrap());
    }

    #[test]
    fn test_parse_discovered_repo_null_branch() {
        // Empty repos may have a null defaultBranchRef
        let json = r#"[{"name": "empty", "nameWithOwner": "alice/empty",
                         "description": null, "url": "https://github.com/alice/empty",
                         "sshUrl": "git@github.com:alice/empty.git",
                         "defaultBranchRef": null, "isPrivate": true}]"#;
        let items: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        let item = &items[0];
        let default_branch = item["defaultBranchRef"]["name"].as_str().unwrap_or("main");
        assert_eq!(default_branch, "main");
        assert!(item["isPrivate"].as_bool().unwrap());
    }
}
