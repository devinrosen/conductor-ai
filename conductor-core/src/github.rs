use std::process::{Command, Output};

use serde::{Deserialize, Serialize};

use crate::error::{ConductorError, Result};
use crate::tickets::{TicketInput, TicketLabelInput};

/// Build an `"owner/repo"` slug from its two components.
fn repo_slug(owner: &str, repo: &str) -> String {
    format!("{owner}/{repo}")
}

/// Run `gh` with the given arguments and return the output.
/// Maps spawn failures and non-zero exit codes to `ConductorError::TicketSync`.
fn run_gh(args: &[&str]) -> Result<Output> {
    run_gh_with_token(args, None)
}

/// Build a `gh` [`Command`] with the given arguments and optional token.
///
/// When `token` is `Some`, the `GH_TOKEN` env var is set so that `gh`
/// authenticates as that identity (e.g. a GitHub App installation).
/// The caller is responsible for spawning and handling the output.
pub(crate) fn build_gh_cmd(args: &[&str], token: Option<&str>) -> Command {
    let mut cmd = Command::new("gh");
    cmd.args(args);
    if let Some(tok) = token {
        cmd.env("GH_TOKEN", tok);
    }
    cmd
}

/// Run `gh` with the given arguments and an optional explicit token.
/// When `token` is `Some`, the `GH_TOKEN` env var is set so that
/// `gh` authenticates as that identity (e.g. a GitHub App installation).
fn run_gh_with_token(args: &[&str], token: Option<&str>) -> Result<Output> {
    let output = build_gh_cmd(args, token)
        .output()
        .map_err(|e| ConductorError::TicketSync(format!("failed to run gh: {e}")))?;

    if !output.status.success() {
        return Err(ConductorError::TicketSync(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    Ok(output)
}

/// A lightweight reference to a GitHub issue (title + URL).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueRef {
    pub title: String,
    pub url: String,
}

/// An open GitHub pull request returned by `gh pr list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubPr {
    pub number: i64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub state: String,
    pub head_ref_name: String,
    pub is_draft: bool,
    pub review_decision: Option<String>,
    pub ci_status: String,
}

/// Intermediate deserialization shape for a single PR entry from `gh pr list --json`.
/// The `author` field from `gh` is a nested object; we flatten it into a plain
/// `String` when constructing [`GithubPr`].
#[derive(Deserialize)]
struct PrAuthor {
    login: String,
}

#[derive(Deserialize)]
struct RawPr {
    number: i64,
    title: String,
    #[serde(default)]
    url: String,
    author: PrAuthor,
    state: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<serde_json::Value>,
}

/// Parse `gh pr list --json` output into [`GithubPr`] values.
/// Returns an empty vec (with a warning) on malformed JSON.
fn parse_prs_json(json: &str) -> Vec<GithubPr> {
    let raw_prs: Vec<RawPr> = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("list_open_prs: failed to parse gh JSON: {e}");
            return vec![];
        }
    };
    raw_prs
        .into_iter()
        .map(|r| {
            let ci_status = reduce_ci_status(&r.status_check_rollup);
            GithubPr {
                number: r.number,
                title: r.title,
                url: r.url,
                author: r.author.login,
                state: r.state,
                head_ref_name: r.head_ref_name,
                is_draft: r.is_draft,
                review_decision: r.review_decision,
                ci_status,
            }
        })
        .collect()
}

/// List open pull requests for a repository identified by its remote URL.
///
/// Returns `Ok(vec![])` for non-GitHub remotes or when `gh` is unavailable /
/// not authenticated — the caller should degrade gracefully rather than error.
pub fn list_open_prs(remote_url: &str) -> Result<Vec<GithubPr>> {
    let Some((owner, repo)) = parse_github_remote(remote_url) else {
        return Ok(vec![]);
    };
    let slug = repo_slug(&owner, &repo);

    let output = match run_gh(&[
        "pr",
        "list",
        "--repo",
        &slug,
        "--state",
        "open",
        "--json",
        "number,title,url,author,state,headRefName,isDraft,reviewDecision,statusCheckRollup",
        "--limit",
        "50",
    ]) {
        Ok(o) => o,
        Err(_) => return Ok(vec![]),
    };

    let json_str = String::from_utf8_lossy(&output.stdout);
    Ok(parse_prs_json(&json_str))
}

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
    let output = run_gh(&["api", "user/orgs", "--jq", ".[].login"])?;

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

    let output = run_gh(&args)?;

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

/// Parse a single GitHub issue JSON value into label details and assignee.
/// Shared by [`sync_github_issues`] and [`fetch_github_issue`].
fn parse_issue_metadata(issue: &serde_json::Value) -> (Vec<TicketLabelInput>, Option<String>) {
    let label_details: Vec<TicketLabelInput> = issue["labels"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|l| {
                    l["name"].as_str().map(|name| TicketLabelInput {
                        name: name.to_string(),
                        color: l["color"].as_str().map(|c| c.to_string()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let assignee = issue["assignees"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|a| a["login"].as_str())
        .map(|s| s.to_string());
    (label_details, assignee)
}

/// Sync open GitHub issues for a repo using the `gh` CLI.
/// Returns a list of normalized TicketInputs ready for upsert.
///
/// When `token` is `Some`, the sync runs under that identity
/// (e.g. a GitHub App installation). When `None`, falls back to the
/// default `gh` CLI user.
pub fn sync_github_issues(
    owner: &str,
    repo: &str,
    token: Option<&str>,
) -> Result<Vec<TicketInput>> {
    let repo_slug = repo_slug(owner, repo);
    let output = run_gh_with_token(
        &[
            "issue",
            "list",
            "--repo",
            &repo_slug,
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number,title,body,labels,assignees,state,url",
        ],
        token,
    )?;

    let json_str = String::from_utf8_lossy(&output.stdout);
    let issues: Vec<serde_json::Value> = serde_json::from_str(&json_str)
        .map_err(|e| ConductorError::TicketSync(format!("failed to parse gh output: {e}")))?;

    let tickets = issues
        .into_iter()
        .map(|issue| {
            let number = issue["number"].as_u64().unwrap_or(0);
            let (label_details, assignee) = parse_issue_metadata(&issue);
            let label_names: Vec<&str> = label_details.iter().map(|l| l.name.as_str()).collect();

            TicketInput {
                source_type: "github".to_string(),
                source_id: number.to_string(),
                title: issue["title"].as_str().unwrap_or("").to_string(),
                body: issue["body"].as_str().unwrap_or("").to_string(),
                state: "open".to_string(),
                labels: serde_json::to_string(&label_names).unwrap_or_else(|_| "[]".to_string()),
                assignee,
                priority: None,
                url: issue["url"].as_str().unwrap_or("").to_string(),
                raw_json: serde_json::to_string(&issue).unwrap_or_else(|_| "{}".to_string()),
                label_details,
            }
        })
        .collect();

    Ok(tickets)
}

/// Fetch a single GitHub issue by number and return its current state.
///
/// Unlike [`sync_github_issues`] (which hardcodes `"open"`), this function
/// reads the actual `state` field from `gh issue view` so the caller gets the
/// real open/closed status.
///
/// When `token` is `Some`, the request runs under that identity
/// (e.g. a GitHub App installation). When `None`, falls back to the
/// default `gh` CLI user.
pub fn fetch_github_issue(
    owner: &str,
    repo: &str,
    issue_number: i64,
    token: Option<&str>,
) -> Result<TicketInput> {
    let repo_slug = repo_slug(owner, repo);
    let number_str = issue_number.to_string();
    let output = run_gh_with_token(
        &[
            "issue",
            "view",
            &number_str,
            "--repo",
            &repo_slug,
            "--json",
            "number,title,body,labels,assignees,state,url",
        ],
        token,
    )?;

    let json_str = String::from_utf8_lossy(&output.stdout);
    let issue: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| ConductorError::TicketSync(format!("failed to parse gh output: {e}")))?;

    let number = issue["number"].as_u64().ok_or_else(|| {
        ConductorError::TicketSync("gh issue view response missing 'number' field".to_string())
    })?;
    let (label_details, assignee) = parse_issue_metadata(&issue);
    let label_names: Vec<&str> = label_details.iter().map(|l| l.name.as_str()).collect();

    // gh issue view returns state as "OPEN" or "CLOSED"; normalize to lowercase
    let raw_state = issue["state"].as_str().unwrap_or("OPEN");
    let state = if raw_state.eq_ignore_ascii_case("open") {
        "open".to_string()
    } else {
        "closed".to_string()
    };

    Ok(TicketInput {
        source_type: "github".to_string(),
        source_id: number.to_string(),
        title: issue["title"].as_str().unwrap_or("").to_string(),
        body: issue["body"].as_str().unwrap_or("").to_string(),
        state,
        labels: serde_json::to_string(&label_names).unwrap_or_else(|_| "[]".to_string()),
        assignee,
        priority: None,
        url: issue["url"].as_str().unwrap_or("").to_string(),
        raw_json: serde_json::to_string(&issue).unwrap_or_else(|_| "{}".to_string()),
        label_details,
    })
}

/// Create a new GitHub issue via the `gh` CLI.
/// Returns `(source_id, url)` where `source_id` is the issue number as a string.
///
/// When `token` is `Some`, the issue is created under that identity
/// (e.g. a GitHub App bot). When `None`, falls back to the default `gh` CLI user.
pub fn create_github_issue(
    owner: &str,
    repo: &str,
    title: &str,
    body: &str,
    labels: &[&str],
    token: Option<&str>,
) -> Result<(String, String)> {
    let repo_slug = repo_slug(owner, repo);
    let mut args = vec![
        "issue", "create", "--repo", &repo_slug, "--title", title, "--body", body,
    ];
    for label in labels {
        args.push("--label");
        args.push(label);
    }
    let output = run_gh_with_token(&args, token)?;

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
///
/// When `token` is `Some`, the search runs under that identity
/// (e.g. a GitHub App bot). When `None`, falls back to the default `gh` CLI user.
pub fn list_issues_by_search(
    owner: &str,
    repo: &str,
    query: &str,
    label: &str,
    limit: u32,
    token: Option<&str>,
) -> Result<Vec<IssueRef>> {
    let repo_slug = repo_slug(owner, repo);
    let limit_str = limit.to_string();
    let output = run_gh_with_token(
        &[
            "issue",
            "list",
            "--repo",
            &repo_slug,
            "--search",
            query,
            "--label",
            label,
            "--json",
            "title,url",
            "--limit",
            &limit_str,
        ],
        token,
    )?;

    let json_str = String::from_utf8_lossy(&output.stdout);
    let issues: Vec<IssueRef> = serde_json::from_str(json_str.trim())
        .map_err(|e| ConductorError::TicketSync(format!("failed to parse gh output: {e}")))?;

    Ok(issues)
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

/// Rich PR detail returned by [`get_pr_detail`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrDetail {
    pub number: i64,
    pub title: String,
    pub url: String,
    pub state: String,     // "OPEN" | "MERGED" | "CLOSED"
    pub ci_status: String, // "passing" | "failing" | "pending" | "none" | "unknown"
}

/// Reduce a `statusCheckRollup` JSON array to a single CI status string.
fn reduce_ci_status(rollup: &[serde_json::Value]) -> String {
    if rollup.is_empty() {
        return "none".to_string();
    }
    let mut any_failure = false;
    let mut any_pending = false;
    let mut all_success = true;
    for check in rollup {
        let conclusion = check.get("conclusion").and_then(|v| v.as_str());
        match conclusion {
            Some("FAILURE") | Some("ERROR") | Some("TIMED_OUT") | Some("CANCELLED") => {
                any_failure = true;
                all_success = false;
            }
            Some("SUCCESS") => {}
            Some("NEUTRAL") | Some("SKIPPED") => {
                // neutral/skipped don't block overall success
            }
            None | Some("PENDING") | Some("") | Some("ACTION_REQUIRED") => {
                any_pending = true;
                all_success = false;
            }
            _ => {
                all_success = false;
            }
        }
    }
    if any_failure {
        "failing".to_string()
    } else if all_success {
        "passing".to_string()
    } else if any_pending {
        "pending".to_string()
    } else {
        "unknown".to_string()
    }
}

/// Intermediate deserialization shape for `get_pr_detail` response.
#[derive(Deserialize)]
struct RawPrDetail {
    number: i64,
    title: String,
    url: String,
    state: String,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<serde_json::Value>,
}

/// Get rich PR detail for a branch from GitHub. Returns `None` on any error
/// (gh unavailable, non-GitHub remote, no PR found). Uses `--state all` so
/// merged/closed PRs are included.
pub fn get_pr_detail(remote_url: &str, branch: &str) -> Option<PrDetail> {
    let (owner, repo) = parse_github_remote(remote_url)?;
    let slug = repo_slug(&owner, &repo);
    let output = run_gh(&[
        "pr",
        "list",
        "--repo",
        &slug,
        "--head",
        branch,
        "--state",
        "all",
        "--json",
        "number,title,url,state,statusCheckRollup",
        "--limit",
        "1",
    ])
    .ok()?;

    let json_str = String::from_utf8_lossy(&output.stdout);
    let items: Vec<RawPrDetail> = serde_json::from_str(json_str.trim()).ok()?;
    let raw = items.into_iter().next()?;
    let ci_status = reduce_ci_status(&raw.status_check_rollup);
    Some(PrDetail {
        number: raw.number,
        title: raw.title,
        url: raw.url,
        state: raw.state,
        ci_status,
    })
}

/// Detect the PR number for a branch using `gh pr list`.
pub fn detect_pr_number(remote_url: &str, branch: &str) -> Option<i64> {
    let (owner, repo) = parse_github_remote(remote_url)?;
    let slug = repo_slug(&owner, &repo);
    let output = run_gh(&[
        "pr", "list", "--repo", &slug, "--head", branch, "--json", "number", "--limit", "1",
    ])
    .ok()?;

    let json: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    json.first()?.get("number")?.as_i64()
}

/// Check whether a merged PR exists for a branch using `gh pr list --state merged`.
pub fn has_merged_pr(remote_url: &str, branch: &str) -> bool {
    let Some((owner, repo)) = parse_github_remote(remote_url) else {
        return false;
    };
    let slug = repo_slug(&owner, &repo);
    let Ok(output) = run_gh(&[
        "pr", "list", "--repo", &slug, "--head", branch, "--state", "merged", "--json", "number",
        "--limit", "1",
    ]) else {
        return false;
    };
    let Ok(json) = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout) else {
        return false;
    };
    !json.is_empty()
}

/// Close a GitHub issue as completed via the `gh` CLI.
pub fn close_github_issue(owner: &str, repo: &str, issue_number: &str) -> Result<()> {
    let repo_slug = repo_slug(owner, repo);
    run_gh(&[
        "issue",
        "close",
        issue_number,
        "--repo",
        &repo_slug,
        "--reason",
        "completed",
    ])?;
    Ok(())
}

/// Squash-merge a PR via the `gh` CLI. Deletes the remote branch after merge.
pub fn squash_merge_pr(owner: &str, repo: &str, pr_number: i64) -> Result<()> {
    let repo_slug = repo_slug(owner, repo);
    let pr_str = pr_number.to_string();
    run_gh(&[
        "pr",
        "merge",
        &pr_str,
        "--repo",
        &repo_slug,
        "--squash",
        "--delete-branch",
    ])?;
    Ok(())
}

/// Add a label to a PR via the `gh` CLI.
pub fn add_pr_label(owner: &str, repo: &str, pr_number: i64, label: &str) -> Result<()> {
    let repo_slug = repo_slug(owner, repo);
    let pr_str = pr_number.to_string();
    run_gh(&[
        "pr",
        "edit",
        &pr_str,
        "--repo",
        &repo_slug,
        "--add-label",
        label,
    ])?;
    Ok(())
}

/// Create a PR with a specific title and body via the `gh` CLI.
/// Returns the PR URL.
pub fn create_pr_with_body(
    owner: &str,
    repo: &str,
    branch: &str,
    title: &str,
    body: &str,
) -> Result<String> {
    let repo_slug = repo_slug(owner, repo);
    let output = run_gh(&[
        "pr", "create", "--repo", &repo_slug, "--head", branch, "--title", title, "--body", body,
    ])?;
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(url)
}

/// Extract the PR number from a GitHub PR URL (e.g. `https://github.com/owner/repo/pull/123`).
pub fn parse_pr_number_from_url(url: &str) -> Option<i64> {
    let segment = url.rsplit('/').next()?;
    segment.parse().ok()
}

/// Fetch the head branch name for a given PR number in the repo identified by `remote_url`.
///
/// Uses `gh pr view <pr_number> --repo <owner>/<repo> --json headRefName --jq '.headRefName'`.
/// Returns a `ConductorError` if the remote is not a GitHub URL, the PR is not found,
/// or `gh` fails.
pub fn get_pr_head_branch(remote_url: &str, pr_number: i64) -> Result<String> {
    let Some((owner, repo)) = parse_github_remote(remote_url) else {
        return Err(ConductorError::TicketSync(format!(
            "remote URL is not a GitHub URL: {remote_url}"
        )));
    };
    let slug = repo_slug(&owner, &repo);
    let pr_str = pr_number.to_string();
    let output = run_gh(&[
        "pr",
        "view",
        &pr_str,
        "--repo",
        &slug,
        "--json",
        "headRefName",
        "--jq",
        ".headRefName",
    ])?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        return Err(ConductorError::TicketSync(format!(
            "PR #{pr_number} not found in {slug}"
        )));
    }
    Ok(branch)
}

/// Get on-diff review comments on a PR via the `gh` CLI.
/// Returns `true` if there are unresolved review comments with file positions.
pub fn has_on_diff_comments(owner: &str, repo: &str, pr_number: i64) -> Result<bool> {
    let repo_slug = repo_slug(owner, repo);
    let pr_str = pr_number.to_string();
    let output = run_gh(&[
        "api",
        &format!("repos/{repo_slug}/pulls/{pr_str}/comments"),
        "--jq",
        "length",
    ])?;
    let count_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let count: i64 = count_str.parse().unwrap_or(0);
    Ok(count > 0)
}

/// Detect the PR number for a branch. Returns (pr_number, pr_url).
pub fn detect_pr(owner: &str, repo: &str, branch: &str) -> Result<Option<(i64, String)>> {
    let slug = repo_slug(owner, repo);
    let output = run_gh(&[
        "pr",
        "list",
        "--repo",
        &slug,
        "--head",
        branch,
        "--json",
        "number,url",
        "--limit",
        "1",
    ])?;

    let json: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)
        .map_err(|e| ConductorError::TicketSync(format!("failed to parse gh output: {e}")))?;
    if let Some(first) = json.first() {
        let number = first.get("number").and_then(|v| v.as_i64());
        let url = first
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(n) = number {
            return Ok(Some((n, url)));
        }
    }
    Ok(None)
}

/// Marker embedded in sticky comments so we can find them later.
pub const COST_COMMENT_MARKER: &str = "<!-- conductor-cost-summary -->";

/// Find the comment ID of an existing sticky cost-summary comment on a PR.
/// Returns `None` if no such comment exists yet.
pub fn find_sticky_comment(
    owner: &str,
    repo: &str,
    pr_number: i64,
    token: Option<&str>,
) -> Result<Option<i64>> {
    let slug = repo_slug(owner, repo);
    let pr_str = pr_number.to_string();
    // Fetch issue comments (not review comments) and search for the marker
    let output = run_gh_with_token(
        &[
            "api",
            &format!("repos/{slug}/issues/{pr_str}/comments"),
            "--jq",
            &format!("[.[] | select(.body | contains(\"{COST_COMMENT_MARKER}\"))] | first | .id"),
        ],
        token,
    )?;
    let id_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if id_str.is_empty() || id_str == "null" {
        return Ok(None);
    }
    Ok(id_str.parse::<i64>().ok())
}

/// Create or update the sticky cost-summary comment on a PR.
/// If a comment with the marker already exists, it is edited in place;
/// otherwise a new comment is created.
///
/// When `token` is `Some`, the comment is posted under the corresponding
/// identity (e.g. a GitHub App bot). When `None`, falls back to the
/// default `gh` CLI user.
pub fn upsert_sticky_comment(
    owner: &str,
    repo: &str,
    pr_number: i64,
    body: &str,
    token: Option<&str>,
) -> Result<()> {
    let slug = repo_slug(owner, repo);

    match find_sticky_comment(owner, repo, pr_number, token)? {
        Some(comment_id) => {
            // Edit existing comment
            let comment_id_str = comment_id.to_string();
            run_gh_with_token(
                &[
                    "api",
                    "--method",
                    "PATCH",
                    &format!("repos/{slug}/issues/comments/{comment_id_str}"),
                    "-f",
                    &format!("body={body}"),
                ],
                token,
            )?;
        }
        None => {
            // Create new comment
            let pr_str = pr_number.to_string();
            run_gh_with_token(
                &[
                    "api",
                    "--method",
                    "POST",
                    &format!("repos/{slug}/issues/{pr_str}/comments"),
                    "-f",
                    &format!("body={body}"),
                ],
                token,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repo_slug() {
        assert_eq!(repo_slug("alice", "my-repo"), "alice/my-repo");
    }

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

    #[test]
    fn test_sync_github_issues_passes_token() {
        // Verify build_gh_cmd wires token correctly — the same path used by sync_github_issues.
        let cmd = build_gh_cmd(
            &[
                "issue",
                "list",
                "--repo",
                "alice/my-repo",
                "--state",
                "open",
                "--limit",
                "200",
                "--json",
                "number,title,body,labels,assignees,state,url",
            ],
            Some("app-token"),
        );
        let gh_token = cmd
            .get_envs()
            .find(|(k, _)| *k == "GH_TOKEN")
            .map(|(_, v)| v);
        assert_eq!(gh_token, Some(Some(std::ffi::OsStr::new("app-token"))));
    }

    #[test]
    fn test_sync_github_issues_no_token() {
        // When token is None, GH_TOKEN must not be set.
        let cmd = build_gh_cmd(&["issue", "list", "--repo", "alice/my-repo"], None);
        let has_gh_token = cmd.get_envs().any(|(k, _)| k == "GH_TOKEN");
        assert!(!has_gh_token);
    }

    #[test]
    fn test_build_gh_cmd_sets_gh_token_when_some() {
        let cmd = build_gh_cmd(&["issue", "list"], Some("my-app-token"));
        let gh_token = cmd
            .get_envs()
            .find(|(k, _)| *k == "GH_TOKEN")
            .map(|(_, v)| v);
        assert_eq!(gh_token, Some(Some(std::ffi::OsStr::new("my-app-token"))));
    }

    #[test]
    fn test_build_gh_cmd_does_not_set_gh_token_when_none() {
        let cmd = build_gh_cmd(&["issue", "list"], None);
        let has_gh_token = cmd.get_envs().any(|(k, _)| k == "GH_TOKEN");
        assert!(!has_gh_token);
    }

    #[test]
    fn test_parse_pr_number_from_url() {
        assert_eq!(
            parse_pr_number_from_url("https://github.com/owner/repo/pull/42"),
            Some(42)
        );
        assert_eq!(
            parse_pr_number_from_url("https://github.com/owner/repo/pull/999"),
            Some(999)
        );
        assert_eq!(
            parse_pr_number_from_url("https://github.com/owner/repo/pull/"),
            None
        );
        assert_eq!(parse_pr_number_from_url("not-a-url"), None);
    }

    #[test]
    fn test_parse_list_open_prs_json() {
        // Exercises the full RawPr -> GithubPr mapping via the real parse_prs_json helper.
        let json = r#"[
            {
                "number": 42,
                "title": "feat: add PR pane",
                "url": "https://github.com/owner/repo/pull/42",
                "author": {"login": "alice"},
                "state": "OPEN",
                "headRefName": "feat/42-add-pr-pane",
                "isDraft": false,
                "reviewDecision": "REVIEW_REQUIRED",
                "statusCheckRollup": [{"conclusion": "SUCCESS"}, {"conclusion": "SUCCESS"}]
            },
            {
                "number": 7,
                "title": "fix: correct typo",
                "url": "https://github.com/owner/repo/pull/7",
                "author": {"login": "bob"},
                "state": "OPEN",
                "headRefName": "fix/7-typo",
                "isDraft": true,
                "reviewDecision": null,
                "statusCheckRollup": [{"conclusion": "FAILURE"}]
            }
        ]"#;

        let prs = parse_prs_json(json);
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 42);
        assert_eq!(prs[0].title, "feat: add PR pane");
        assert_eq!(prs[0].url, "https://github.com/owner/repo/pull/42");
        assert_eq!(prs[0].author, "alice");
        assert_eq!(prs[0].state, "OPEN");
        assert_eq!(prs[0].head_ref_name, "feat/42-add-pr-pane");
        assert!(!prs[0].is_draft);
        assert_eq!(prs[0].review_decision.as_deref(), Some("REVIEW_REQUIRED"));
        assert_eq!(prs[0].ci_status, "passing");
        assert_eq!(prs[1].number, 7);
        assert_eq!(prs[1].url, "https://github.com/owner/repo/pull/7");
        assert_eq!(prs[1].author, "bob");
        assert!(prs[1].is_draft);
        assert!(prs[1].review_decision.is_none());
        assert_eq!(prs[1].ci_status, "failing");
    }

    #[test]
    fn test_parse_list_open_prs_missing_new_fields() {
        // #[serde(default)] must allow old JSON without isDraft/reviewDecision.
        let json = r#"[
            {
                "number": 1,
                "title": "old-style PR",
                "author": {"login": "legacy"},
                "state": "OPEN",
                "headRefName": "feat/1-old"
            }
        ]"#;
        let prs = parse_prs_json(json);
        assert_eq!(prs.len(), 1);
        assert!(!prs[0].is_draft);
        assert!(prs[0].review_decision.is_none());
    }

    #[test]
    fn test_parse_list_open_prs_invalid_json_returns_empty() {
        // Malformed JSON should silently yield an empty list (no panic).
        let prs = parse_prs_json("not json");
        assert!(prs.is_empty());
    }
}
