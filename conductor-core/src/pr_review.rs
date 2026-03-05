//! Multi-agent PR review swarm.
//!
//! Spawns N specialized reviewer agents in parallel, each reviewing the same
//! PR diff with a different focus area. Aggregates findings into a unified
//! review and optionally posts to the GitHub PR.

use std::borrow::Cow;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::process::Command;
use std::thread;
use std::time::Duration;

use rusqlite::Connection;

use crate::agent::{AgentManager, AgentRun, PlanStep};
use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::github;
use crate::merge_queue::MergeQueueManager;
use crate::review_config::{ReviewConfigManager, ReviewerRole};
use crate::worktree::WorktreeManager;

/// A finding in unchanged/removed code that should be filed as a GH issue
/// rather than blocking the PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OffDiffFinding {
    pub title: String,
    pub file: String,
    pub line: u64,
    pub severity: String,
    pub body: String,
    /// Which reviewer role produced this finding.
    pub reviewer: String,
}

/// Outcome of a single reviewer agent.
#[derive(Debug, Clone)]
pub struct ReviewerResult {
    pub role_name: String,
    pub focus: String,
    pub required: bool,
    pub run_id: String,
    pub status: String,
    pub findings: Option<String>,
    pub approved: bool,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    /// Off-diff findings extracted from this reviewer's output.
    pub off_diff_findings: Vec<OffDiffFinding>,
}

/// Aggregated result of the full review swarm.
#[derive(Debug, Clone)]
pub struct ReviewSwarmResult {
    pub parent_run_id: String,
    pub reviewer_results: Vec<ReviewerResult>,
    pub all_required_approved: bool,
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
    pub aggregated_comment: String,
    /// Deduplicated off-diff findings from all reviewers, filed as GH issues.
    pub off_diff_issues_filed: Vec<OffDiffFinding>,
}

/// Configuration for the review swarm process.
#[derive(Debug, Clone)]
pub struct ReviewSwarmConfig {
    /// How often to poll the DB for reviewer completion (default: 5s).
    pub poll_interval: Duration,
    /// Maximum time to wait for a single reviewer run (default: 15min).
    pub reviewer_timeout: Duration,
}

impl Default for ReviewSwarmConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            reviewer_timeout: Duration::from_secs(15 * 60),
        }
    }
}

/// Input parameters for launching a review swarm.
pub struct ReviewSwarmInput<'a> {
    pub conn: &'a Connection,
    pub config: &'a Config,
    pub repo_id: &'a str,
    pub worktree_id: &'a str,
    pub pr_branch: &'a str,
    pub pr_number: Option<i64>,
    pub model: Option<&'a str>,
    pub conductor_bin: &'a str,
    pub swarm_config: &'a ReviewSwarmConfig,
}

/// Launch a PR review swarm for a worktree.
///
/// 1. Creates a parent "review" run with one plan step per reviewer role
/// 2. Spawns all reviewer agents in parallel (each as a child run)
/// 3. Polls for all reviewers to complete
/// 4. Aggregates findings into a unified comment
/// 5. Optionally posts comment to the GitHub PR
/// 6. If all required reviewers approve, enqueues to merge queue
///
/// Returns the aggregated review swarm result.
pub fn run_review_swarm(input: &ReviewSwarmInput<'_>) -> Result<ReviewSwarmResult> {
    let conn = input.conn;
    let config = input.config;
    let repo_id = input.repo_id;
    let worktree_id = input.worktree_id;
    let pr_branch = input.pr_branch;
    let pr_number = input.pr_number;
    let model = input.model;
    let conductor_bin = input.conductor_bin;
    let swarm_config = input.swarm_config;

    let mgr = AgentManager::new(conn);
    let wt_mgr = WorktreeManager::new(conn, config);
    let review_cfg_mgr = ReviewConfigManager::new(conn);

    let worktree = wt_mgr.get_by_id(worktree_id)?;
    let review_config = review_cfg_mgr.get_or_default(repo_id)?;
    let roles = &review_config.roles;

    if roles.is_empty() {
        return Err(ConductorError::Agent(
            "No reviewer roles configured".to_string(),
        ));
    }

    // Get the PR diff for context
    let diff = get_pr_diff(pr_branch)?;

    // Create the parent review run
    let parent_prompt = format!(
        "PR review swarm for branch '{}'. Coordinating {} reviewer agents.",
        pr_branch,
        roles.len()
    );
    let parent_run = mgr.create_run(worktree_id, &parent_prompt, None, model)?;

    // Create plan steps — one per reviewer role
    let plan_steps: Vec<PlanStep> = roles
        .iter()
        .enumerate()
        .map(|(i, role)| PlanStep {
            id: None,
            description: format!("{} review: {}", role.name, role.focus),
            done: false,
            status: "pending".to_string(),
            position: Some(i as i64),
            started_at: None,
            completed_at: None,
        })
        .collect();
    mgr.update_run_plan(&parent_run.id, &plan_steps)?;

    // Re-fetch steps to get their DB-assigned IDs
    let steps = mgr.get_run_steps(&parent_run.id)?;

    // Spawn all reviewer agents in parallel
    let mut child_runs: Vec<(usize, AgentRun, ReviewerRole)> = Vec::new();

    for (i, role) in roles.iter().enumerate() {
        let child_prompt = build_reviewer_prompt(role, &diff, pr_branch);
        let child_window = format!("{}-review-{}", worktree.slug, role.name);

        // Mark step as in_progress
        if let Some(ref step_id) = steps[i].id {
            let _ = mgr.update_step_status(step_id, "in_progress");
        }

        let child_run = mgr.create_child_run(
            worktree_id,
            &child_prompt,
            Some(&child_window),
            model,
            &parent_run.id,
        )?;

        eprintln!(
            "[review-swarm] Spawning {} reviewer (run {})",
            role.name, child_run.id
        );

        let spawn_result = spawn_reviewer_tmux(
            conductor_bin,
            &child_run.id,
            &worktree.path,
            &child_prompt,
            model,
            &child_window,
        );

        if let Err(e) = spawn_result {
            eprintln!("[review-swarm] Failed to spawn {} reviewer: {e}", role.name);
            let _ = mgr.update_run_failed(&child_run.id, &format!("spawn failed: {e}"));
            if let Some(ref step_id) = steps[i].id {
                let _ = mgr.update_step_status(step_id, "failed");
            }
        }

        child_runs.push((i, child_run, role.clone()));
    }

    // Poll all reviewers for completion in parallel (single shared loop)
    let reviewer_results = poll_all_reviewers(
        &mgr,
        &child_runs,
        &steps,
        swarm_config.poll_interval,
        swarm_config.reviewer_timeout,
    );

    // Aggregate results
    let all_required_approved = reviewer_results
        .iter()
        .filter(|r| r.required)
        .all(|r| r.approved);

    let total_cost: f64 = reviewer_results.iter().filter_map(|r| r.cost_usd).sum();
    let total_turns: i64 = reviewer_results.iter().filter_map(|r| r.num_turns).sum();
    let total_duration_ms: i64 = reviewer_results.iter().filter_map(|r| r.duration_ms).sum();

    // Collect and deduplicate off-diff findings from all reviewers
    let all_off_diff: Vec<OffDiffFinding> = reviewer_results
        .iter()
        .flat_map(|r| r.off_diff_findings.clone())
        .collect();
    let deduped_off_diff = deduplicate_off_diff_findings(all_off_diff);

    if !deduped_off_diff.is_empty() {
        eprintln!(
            "[review-swarm] Found {} off-diff findings (after dedup)",
            deduped_off_diff.len()
        );
    }

    // Resolve owner/repo once for GH issue filing and PR commenting
    let repo_mgr = crate::repo::RepoManager::new(conn, config);
    let gh_remote = repo_mgr
        .get_by_id(repo_id)
        .ok()
        .and_then(|repo| github::parse_github_remote(&repo.remote_url));

    let off_diff_issues_filed = if !deduped_off_diff.is_empty() {
        if let Some((ref owner, ref repo_name)) = gh_remote {
            file_off_diff_issues(owner, repo_name, pr_branch, &deduped_off_diff)
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let aggregated_comment = build_aggregated_comment(
        &reviewer_results,
        all_required_approved,
        &off_diff_issues_filed,
    );

    // Update parent run
    let summary = format!(
        "Review swarm: {}/{} reviewers approved (all required approved: {})",
        reviewer_results.iter().filter(|r| r.approved).count(),
        reviewer_results.len(),
        all_required_approved,
    );

    if all_required_approved {
        mgr.update_run_completed(
            &parent_run.id,
            None,
            Some(&summary),
            Some(total_cost),
            Some(total_turns),
            Some(total_duration_ms),
        )?;
        mgr.mark_plan_done(&parent_run.id)?;
    } else {
        mgr.update_run_failed(&parent_run.id, &summary)?;
    }

    let result = ReviewSwarmResult {
        parent_run_id: parent_run.id.clone(),
        reviewer_results,
        all_required_approved,
        total_cost,
        total_turns,
        total_duration_ms,
        aggregated_comment: aggregated_comment.clone(),
        off_diff_issues_filed: off_diff_issues_filed
            .into_iter()
            .map(|(f, _url)| f)
            .collect(),
    };

    // Post to PR if configured
    if review_config.post_to_pr {
        if let Some(pr_num) = pr_number {
            if let Some((ref owner, ref repo_name)) = gh_remote {
                let _ = post_pr_comment(owner, repo_name, pr_num, &aggregated_comment);
            }
        }
    }

    // Auto-merge if all required approved and config says so
    if all_required_approved && review_config.auto_merge {
        let mq = MergeQueueManager::new(conn);
        let _ = mq.enqueue(repo_id, worktree_id, Some(&parent_run.id), None);
        eprintln!("[review-swarm] All required reviewers approved — added to merge queue");
    }

    eprintln!(
        "[review-swarm] Complete: ${:.4}, {} turns, {:.1}s",
        total_cost,
        total_turns,
        total_duration_ms as f64 / 1000.0
    );

    Ok(result)
}

/// Build the blocking findings from failed reviewers into a remediation prompt
/// for the coding agent.
pub fn build_remediation_prompt(swarm_result: &ReviewSwarmResult) -> String {
    let blocking: Vec<&ReviewerResult> = swarm_result
        .reviewer_results
        .iter()
        .filter(|r| r.required && !r.approved)
        .collect();

    if blocking.is_empty() {
        return String::new();
    }

    let mut prompt = String::from(
        "The following review findings must be addressed before this PR can be merged:\n\n",
    );

    for result in &blocking {
        prompt.push_str(&format!(
            "## {} Review ({})\n",
            result.role_name, result.focus
        ));
        if let Some(ref findings) = result.findings {
            prompt.push_str(findings);
        } else {
            prompt.push_str("(reviewer failed without producing findings)");
        }
        prompt.push_str("\n\n");
    }

    prompt.push_str(
        "Please address all blocking findings above. Focus on the critical and warning \
         severity issues first.",
    );
    prompt
}

/// Truncate a string at a char boundary no greater than `max_bytes`.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk backwards from max_bytes to find a char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Get the diff for a PR branch compared to the default branch.
fn get_pr_diff(branch: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", &format!("origin/main...{branch}")])
        .output()
        .map_err(|e| ConductorError::Git(format!("failed to get PR diff: {e}")))?;

    if !output.status.success() {
        // Fall back to diff against HEAD~1 if the branch comparison fails
        let fallback = Command::new("git")
            .args(["diff", "HEAD~1"])
            .output()
            .map_err(|e| ConductorError::Git(format!("failed to get fallback diff: {e}")))?;
        return Ok(String::from_utf8_lossy(&fallback.stdout).into_owned());
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Build a focused prompt for a reviewer agent.
fn build_reviewer_prompt(role: &ReviewerRole, diff: &str, branch: &str) -> String {
    // Truncate diff if very large to stay within context limits
    let max_diff_bytes = 50_000;
    let truncated_diff: Cow<'_, str> = if diff.len() > max_diff_bytes {
        let safe = truncate_str(diff, max_diff_bytes);
        Cow::Owned(format!(
            "{safe}\n\n... (diff truncated, {} bytes omitted)",
            diff.len() - safe.len()
        ))
    } else {
        Cow::Borrowed(diff)
    };

    format!(
        "{system_prompt}\n\n\
         ## Diff Scope Rules\n\
         - Lines starting with `+` are **added/modified code** — these are IN SCOPE for critique.\n\
         - Lines starting with `-` are **removed code** — for context only, do NOT raise issues against them.\n\
         - Lines starting with ` ` (space) are **unchanged context** — for understanding only, do NOT raise issues against them.\n\
         - If you spot a genuine issue in unchanged/removed code, report it in a separate \
         `OFF-DIFF-FINDING` block (see below) instead of including it in your main review.\n\n\
         ## Off-Diff Findings\n\
         For issues found in unchanged or removed code, use this exact format (one block per finding):\n\
         ```\n\
         OFF-DIFF-FINDING\n\
         title: <short title>\n\
         file: <file path>\n\
         line: <line number or 0 if unknown>\n\
         severity: critical | warning | suggestion\n\
         body: <explanation of the issue>\n\
         END-OFF-DIFF-FINDING\n\
         ```\n\
         Off-diff findings will be filed as separate GitHub issues and will NOT block this PR.\n\n\
         ## Focus Scope\n\
         Your declared focus area is: **{focus}**.\n\
         Only raise blocking findings (critical/warning) for issues that fall within your focus area.\n\
         If you notice something outside your focus, you may include it as a `suggestion` severity \
         but it must not affect your verdict.\n\n\
         ## PR Branch\n\
         {branch}\n\n\
         ## PR Diff\n\
         ```diff\n\
         {diff}\n\
         ```\n\n\
         Review the diff above. At the end of your review, include a verdict line:\n\
         - `VERDICT: APPROVE` if no critical or warning issues found in `+` lines\n\
         - `VERDICT: REQUEST_CHANGES` if any critical or warning issues found in `+` lines\n\n\
         Important: `suggestion`-severity findings alone should NOT produce REQUEST_CHANGES.\n\n\
         Be thorough but concise.",
        system_prompt = role.system_prompt,
        branch = branch,
        focus = role.focus,
        diff = truncated_diff,
    )
}

/// Parse OFF-DIFF-FINDING blocks from a reviewer's output text.
fn parse_off_diff_findings(text: &str, reviewer_name: &str) -> Vec<OffDiffFinding> {
    let mut findings = Vec::new();
    let mut in_block = false;
    let mut title = String::new();
    let mut file = String::new();
    let mut line: u64 = 0;
    let mut severity = String::new();
    let mut body = String::new();

    for raw_line in text.lines() {
        let trimmed = raw_line.trim();
        if trimmed == "OFF-DIFF-FINDING" {
            in_block = true;
            title.clear();
            file.clear();
            line = 0;
            severity.clear();
            body.clear();
            continue;
        }
        if trimmed == "END-OFF-DIFF-FINDING" {
            if in_block && !title.is_empty() {
                // Cap AI-generated fields to prevent oversized gh CLI args.
                const MAX_TITLE: usize = 256;
                const MAX_BODY: usize = 65_536;
                let capped_title = if title.len() > MAX_TITLE {
                    let mut t = title[..MAX_TITLE].to_string();
                    t.push('…');
                    t
                } else {
                    title.clone()
                };
                let trimmed_body = body.trim().to_string();
                let capped_body = if trimmed_body.len() > MAX_BODY {
                    let mut b = trimmed_body[..MAX_BODY].to_string();
                    b.push_str("\n\n*(truncated)*");
                    b
                } else {
                    trimmed_body
                };
                findings.push(OffDiffFinding {
                    title: capped_title,
                    file: file.clone(),
                    line,
                    severity: severity.clone(),
                    body: capped_body,
                    reviewer: reviewer_name.to_string(),
                });
            }
            in_block = false;
            continue;
        }
        if in_block {
            if let Some(val) = trimmed.strip_prefix("title:") {
                title = val.trim().to_string();
            } else if let Some(val) = trimmed.strip_prefix("file:") {
                file = val.trim().to_string();
            } else if let Some(val) = trimmed.strip_prefix("line:") {
                line = val.trim().parse().unwrap_or(0);
            } else if let Some(val) = trimmed.strip_prefix("severity:") {
                severity = val.trim().to_string();
            } else if let Some(val) = trimmed.strip_prefix("body:") {
                body = val.trim().to_string();
            } else {
                // Continuation of body
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(trimmed);
            }
        }
    }

    findings
}

/// Check if a reviewer's findings contain only suggestion-severity issues (no critical/warning).
/// Returns true only if there are explicit suggestion-severity findings and no critical/warning.
/// Returns false if there are no structured severity findings at all (respects the raw verdict).
fn has_only_suggestions(result_text: &str) -> bool {
    let text_lower = result_text.to_lowercase();
    let has_critical =
        text_lower.contains("severity: critical") || text_lower.contains("**severity**: critical");
    let has_warning =
        text_lower.contains("severity: warning") || text_lower.contains("**severity**: warning");
    let has_suggestion = text_lower.contains("severity: suggestion")
        || text_lower.contains("**severity**: suggestion");

    // Only override the verdict if there are explicit suggestion findings
    // but no critical/warning findings
    has_suggestion && !has_critical && !has_warning
}

/// Deduplicate off-diff findings across reviewers by file:line.
/// When multiple reviewers flag the same file:line, keep the highest severity one.
fn deduplicate_off_diff_findings(findings: Vec<OffDiffFinding>) -> Vec<OffDiffFinding> {
    use std::collections::HashMap;

    let severity_rank = |s: &str| -> u8 {
        match s.to_lowercase().as_str() {
            "critical" => 3,
            "warning" => 2,
            "suggestion" => 1,
            _ => 0,
        }
    };

    let mut map: HashMap<(String, u64), OffDiffFinding> = HashMap::new();
    for finding in findings {
        let key = (finding.file.clone(), finding.line);
        let entry = map.entry(key);
        entry
            .and_modify(|existing| {
                if severity_rank(&finding.severity) > severity_rank(&existing.severity) {
                    *existing = finding.clone();
                }
            })
            .or_insert(finding);
    }

    let mut deduped: Vec<OffDiffFinding> = map.into_values().collect();
    deduped.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    deduped
}

/// Search for an existing GH issue matching a finding, to avoid duplicates.
fn find_existing_issue(owner: &str, repo: &str, title: &str) -> Option<String> {
    let issues = github::list_issues_by_search(owner, repo, title, "conductor-review", 5).ok()?;

    // Look for a title that matches closely (case-insensitive substring)
    let title_lower = title.to_lowercase();
    for issue in &issues {
        if let Some(existing_title) = issue["title"].as_str() {
            if existing_title.to_lowercase().contains(&title_lower)
                || title_lower.contains(&existing_title.to_lowercase())
            {
                return issue["url"].as_str().map(String::from);
            }
        }
    }

    None
}

/// File off-diff findings as GitHub issues (or reference existing ones).
/// Returns a list of (finding, issue_url) pairs.
fn file_off_diff_issues(
    owner: &str,
    repo: &str,
    pr_branch: &str,
    findings: &[OffDiffFinding],
) -> Vec<(OffDiffFinding, String)> {
    let mut filed = Vec::new();

    for finding in findings {
        // Check if an existing issue already covers this
        if let Some(existing_url) = find_existing_issue(owner, repo, &finding.title) {
            eprintln!(
                "[review-swarm] Off-diff finding '{}' matches existing issue: {}",
                finding.title, existing_url
            );
            filed.push((finding.clone(), existing_url));
            continue;
        }

        let issue_body = format!(
            "**Severity**: {severity}\n\
             **File**: `{file}`:{line}\n\
             **Found by**: {reviewer} reviewer\n\
             **Source PR branch**: `{branch}`\n\n\
             ## Details\n\n\
             {body}\n\n\
             ---\n\
             *Filed automatically by Conductor PR review swarm. \
             This issue was found in unchanged code during a PR review \
             and does not block the originating PR.*",
            severity = finding.severity,
            file = finding.file,
            line = finding.line,
            reviewer = finding.reviewer,
            branch = pr_branch,
            body = finding.body,
        );

        match github::create_github_issue(owner, repo, &finding.title, &issue_body) {
            Ok((_number, url)) => {
                // Try to add the conductor-review label (best-effort)
                let _ = github::add_label_to_issue(owner, repo, &url, "conductor-review");
                eprintln!(
                    "[review-swarm] Filed off-diff issue '{}': {}",
                    finding.title, url
                );
                filed.push((finding.clone(), url));
            }
            Err(e) => {
                eprintln!(
                    "[review-swarm] Failed to file off-diff issue '{}': {e}",
                    finding.title
                );
            }
        }
    }

    filed
}

/// Determine if a reviewer approved based on its result text.
///
/// Requires the verdict to appear on the final non-empty line to prevent
/// prompt injection from diff content that might contain verdict strings.
///
/// A reviewer is considered approved if:
/// 1. They explicitly output `VERDICT: APPROVE`, OR
/// 2. They output `VERDICT: REQUEST_CHANGES` but only have suggestion-severity findings
///    (no critical or warning). Suggestion-only findings should not block.
fn is_review_approved(run: &AgentRun) -> bool {
    if run.status != "completed" {
        return false;
    }
    match &run.result_text {
        Some(text) => {
            let last_line = text
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            let verdict = last_line.trim().to_uppercase();
            if verdict == "VERDICT: APPROVE" {
                return true;
            }
            // If REQUEST_CHANGES but only suggestions, treat as approved
            if verdict == "VERDICT: REQUEST_CHANGES" && has_only_suggestions(text) {
                return true;
            }
            false
        }
        None => false,
    }
}

/// Build the aggregated PR comment from all reviewer results.
fn build_aggregated_comment(
    results: &[ReviewerResult],
    all_required_approved: bool,
    off_diff_issues: &[(OffDiffFinding, String)],
) -> String {
    let mut comment = String::from("# Conductor PR Review\n\n");

    let approved_count = results.iter().filter(|r| r.approved).count();
    comment.push_str(&format!(
        "**{approved_count}/{total}** reviewers approved",
        total = results.len()
    ));
    if all_required_approved {
        comment.push_str(" — all required checks passed\n\n");
    } else {
        comment.push_str(" — **blocking issues found**\n\n");
    }

    for result in results {
        let status = if result.approved {
            "approved"
        } else {
            "changes requested"
        };
        let required_badge = if result.required {
            " *(required)*"
        } else {
            " *(advisory)*"
        };

        comment.push_str(&format!(
            "## {} — {}{}\n",
            result.role_name, status, required_badge
        ));
        comment.push_str(&format!("*Focus: {}*\n\n", result.focus));

        if let Some(ref findings) = result.findings {
            // Take first ~2000 bytes of findings to keep comment reasonable
            if findings.len() > 2000 {
                let safe = truncate_str(findings, 2000);
                comment.push_str(safe);
                comment.push_str("...\n*(truncated)*");
            } else {
                comment.push_str(findings);
            }
        } else {
            comment.push_str("*(no findings reported)*");
        }
        comment.push_str("\n\n---\n\n");
    }

    // Include off-diff issues section if any were filed
    if !off_diff_issues.is_empty() {
        comment.push_str("## Off-Diff Findings (filed as issues)\n\n");
        comment.push_str(
            "The following issues were found in unchanged code and filed as separate GitHub issues:\n\n",
        );
        for (finding, url) in off_diff_issues {
            if url.starts_with("https://") {
                comment.push_str(&format!(
                    "- **{}** (`{}`:{}): [{}]({}) *({})*\n",
                    finding.title, finding.file, finding.line, url, url, finding.severity
                ));
            } else {
                comment.push_str(&format!(
                    "- **{}** (`{}`:{}): {} *({})*\n",
                    finding.title, finding.file, finding.line, url, finding.severity
                ));
            }
        }
        comment.push_str("\n---\n\n");
    }

    let total_cost: f64 = results.iter().filter_map(|r| r.cost_usd).sum();
    comment.push_str(&format!(
        "*Review cost: ${total_cost:.4} across {} reviewers*\n",
        results.len()
    ));

    comment
}

/// Post a comment to a GitHub PR using the `gh` CLI.
fn post_pr_comment(owner: &str, repo: &str, pr_number: i64, comment: &str) -> Result<()> {
    // Write comment to a temp file to avoid exceeding command-line arg limits (~10KB+).
    // Use mode 0o600 to prevent other local users from reading the comment.
    let comment_file = std::env::temp_dir().join(format!(
        "conductor-review-comment-{}.txt",
        ulid::Ulid::new()
    ));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&comment_file)
            .map_err(|e| ConductorError::Agent(format!("failed to write comment file: {e}")))?;
        f.write_all(comment.as_bytes())
            .map_err(|e| ConductorError::Agent(format!("failed to write comment file: {e}")))?;
    }

    let output = Command::new("gh")
        .args([
            "pr",
            "comment",
            &pr_number.to_string(),
            "--repo",
            &format!("{owner}/{repo}"),
            "--body-file",
            &comment_file.to_string_lossy(),
        ])
        .output()
        .map_err(|e| ConductorError::Agent(format!("failed to post PR comment: {e}")))?;

    // Clean up temp file
    let _ = std::fs::remove_file(&comment_file);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("[review-swarm] Failed to post PR comment: {stderr}");
    }

    Ok(())
}

/// Spawn a reviewer agent in a tmux window.
///
/// The prompt is written to a temp file to avoid exceeding tmux/OS command length
/// limits (large PR diffs can easily blow past the ~200KB arg limit).
fn spawn_reviewer_tmux(
    conductor_bin: &str,
    run_id: &str,
    worktree_path: &str,
    prompt: &str,
    model: Option<&str>,
    window_name: &str,
) -> std::result::Result<(), String> {
    // Write prompt to a temp file so the tmux command stays short.
    // Use mode 0o600 to prevent other local users from reading the prompt.
    let prompt_file = std::env::temp_dir().join(format!("conductor-review-{run_id}.txt"));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&prompt_file)
        .map_err(|e| format!("Failed to write prompt file: {e}"))?;
    f.write_all(prompt.as_bytes())
        .map_err(|e| format!("Failed to write prompt file: {e}"))?;

    // Pass args directly to tmux without sh -c to avoid shell injection
    let mut cmd = Command::new("tmux");
    cmd.args(["new-window", "-d", "-n", window_name, "--"]);
    cmd.arg(conductor_bin);
    cmd.args([
        "agent",
        "run",
        "--run-id",
        run_id,
        "--worktree-path",
        worktree_path,
        "--prompt-file",
        &prompt_file.to_string_lossy(),
    ]);
    if let Some(m) = model {
        cmd.args(["--model", m]);
    }

    let result = cmd
        .output()
        .map_err(|e| format!("Failed to spawn tmux: {e}"))?;

    if result.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&result.stderr);
        Err(format!("tmux failed: {stderr}"))
    }
}

/// Poll all reviewer runs in a single shared loop, collecting results as each completes.
/// This avoids sequential O(N × timeout) worst-case wait.
fn poll_all_reviewers(
    mgr: &AgentManager<'_>,
    child_runs: &[(usize, AgentRun, ReviewerRole)],
    steps: &[PlanStep],
    poll_interval: Duration,
    timeout: Duration,
) -> Vec<ReviewerResult> {
    let start = std::time::Instant::now();
    let mut results: Vec<Option<ReviewerResult>> = vec![None; child_runs.len()];
    let mut pending_count = child_runs.len();

    loop {
        if pending_count == 0 {
            break;
        }

        let timed_out = start.elapsed() > timeout;

        for (idx, (step_idx, child_run, role)) in child_runs.iter().enumerate() {
            if results[idx].is_some() {
                continue;
            }

            if timed_out {
                let err = format!(
                    "Reviewer run {} timed out after {:.0}s",
                    child_run.id,
                    timeout.as_secs_f64()
                );
                eprintln!("[review-swarm] {} reviewer error: {err}", role.name);
                if let Some(ref step_id) = steps[*step_idx].id {
                    let _ = mgr.update_step_status(step_id, "failed");
                }
                let _ = mgr.update_run_cancelled(&child_run.id);
                results[idx] = Some(ReviewerResult {
                    role_name: role.name.clone(),
                    focus: role.focus.clone(),
                    required: role.required,
                    run_id: child_run.id.clone(),
                    status: "failed".to_string(),
                    findings: Some(err),
                    approved: false,
                    cost_usd: None,
                    num_turns: None,
                    duration_ms: None,
                    off_diff_findings: Vec::new(),
                });
                pending_count -= 1;
                continue;
            }

            match mgr.get_run(&child_run.id) {
                Ok(Some(run)) => match run.status.as_str() {
                    "completed" | "failed" | "cancelled" => {
                        let findings = run.result_text.clone();
                        let approved = is_review_approved(&run);
                        let off_diff = findings
                            .as_deref()
                            .map(|text| parse_off_diff_findings(text, &role.name))
                            .unwrap_or_default();
                        if let Some(ref step_id) = steps[*step_idx].id {
                            let status = if run.status == "completed" {
                                "completed"
                            } else {
                                "failed"
                            };
                            let _ = mgr.update_step_status(step_id, status);
                        }
                        eprintln!(
                            "[review-swarm] {} reviewer: {} (approved={}, off_diff={})",
                            role.name,
                            run.status,
                            approved,
                            off_diff.len()
                        );
                        results[idx] = Some(ReviewerResult {
                            role_name: role.name.clone(),
                            focus: role.focus.clone(),
                            required: role.required,
                            run_id: run.id.clone(),
                            status: run.status.clone(),
                            findings,
                            approved,
                            cost_usd: run.cost_usd,
                            num_turns: run.num_turns,
                            duration_ms: run.duration_ms,
                            off_diff_findings: off_diff,
                        });
                        pending_count -= 1;
                    }
                    _ => {}
                },
                Ok(None) => {
                    eprintln!(
                        "[review-swarm] {} reviewer run {} not found",
                        role.name, child_run.id
                    );
                    results[idx] = Some(ReviewerResult {
                        role_name: role.name.clone(),
                        focus: role.focus.clone(),
                        required: role.required,
                        run_id: child_run.id.clone(),
                        status: "failed".to_string(),
                        findings: Some(format!("Run {} not found", child_run.id)),
                        approved: false,
                        cost_usd: None,
                        num_turns: None,
                        duration_ms: None,
                        off_diff_findings: Vec::new(),
                    });
                    pending_count -= 1;
                }
                Err(e) => {
                    eprintln!("[review-swarm] {} reviewer DB error: {e}", role.name);
                    results[idx] = Some(ReviewerResult {
                        role_name: role.name.clone(),
                        focus: role.focus.clone(),
                        required: role.required,
                        run_id: child_run.id.clone(),
                        status: "failed".to_string(),
                        findings: Some(format!("Database error: {e}")),
                        approved: false,
                        cost_usd: None,
                        num_turns: None,
                        duration_ms: None,
                        off_diff_findings: Vec::new(),
                    });
                    pending_count -= 1;
                }
            }
        }

        if pending_count > 0 {
            thread::sleep(poll_interval);
        }
    }

    results.into_iter().flatten().collect()
}

/// Poll the database for a single reviewer run to reach a terminal status.
/// Used in tests; the main swarm uses `poll_all_reviewers` instead.
#[allow(dead_code)]
fn poll_reviewer_completion(
    _conn: &Connection,
    run_id: &str,
    poll_interval: Duration,
    timeout: Duration,
) -> std::result::Result<AgentRun, String> {
    let start = std::time::Instant::now();
    let mgr = AgentManager::new(_conn);

    loop {
        if start.elapsed() > timeout {
            return Err(format!(
                "Reviewer run {} timed out after {:.0}s",
                run_id,
                timeout.as_secs_f64()
            ));
        }

        match mgr.get_run(run_id) {
            Ok(Some(run)) => match run.status.as_str() {
                "completed" | "failed" | "cancelled" => return Ok(run),
                "running" => {}
                other => return Err(format!("Unexpected run status: {other}")),
            },
            Ok(None) => return Err(format!("Reviewer run {run_id} not found")),
            Err(e) => return Err(format!("Database error: {e}")),
        }

        thread::sleep(poll_interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::review_config::default_reviewer_roles;
    use chrono::Utc;
    use tempfile::NamedTempFile;

    fn setup_db() -> Connection {
        let tmp = NamedTempFile::new().unwrap();
        let conn = db::open_database(tmp.path()).unwrap();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
             VALUES ('r1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', 'main', '/tmp/ws', ?1)",
            rusqlite::params![now],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w1', 'r1', 'feat-test', 'feat/test', '/tmp/ws/feat-test', 'active', ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn
    }

    fn make_run(status: &str, result_text: Option<&str>) -> AgentRun {
        AgentRun {
            id: "test-run".to_string(),
            worktree_id: "w1".to_string(),
            claude_session_id: None,
            prompt: "test".to_string(),
            status: status.to_string(),
            result_text: result_text.map(String::from),
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: None,
            tmux_window: None,
            log_file: None,
            model: None,
            plan: None,
            parent_run_id: None,
        }
    }

    #[test]
    fn test_is_review_approved_approve() {
        let run = make_run("completed", Some("No issues found.\n\nVERDICT: APPROVE"));
        assert!(is_review_approved(&run));
    }

    #[test]
    fn test_is_review_approved_approve_trailing_whitespace() {
        let run = make_run("completed", Some("No issues found.\n\nVERDICT: APPROVE\n"));
        assert!(is_review_approved(&run));
    }

    #[test]
    fn test_is_review_approved_request_changes() {
        let run = make_run(
            "completed",
            Some("Found issues.\n\nVERDICT: REQUEST_CHANGES"),
        );
        assert!(!is_review_approved(&run));
    }

    #[test]
    fn test_is_review_approved_failed_run() {
        let run = make_run("failed", Some("VERDICT: APPROVE"));
        assert!(!is_review_approved(&run));
    }

    #[test]
    fn test_is_review_approved_no_result() {
        let run = make_run("completed", None);
        assert!(!is_review_approved(&run));
    }

    #[test]
    fn test_is_review_approved_case_insensitive() {
        let run = make_run("completed", Some("verdict: approve"));
        assert!(is_review_approved(&run));
    }

    #[test]
    fn test_is_review_approved_rejects_mid_text_verdict() {
        // Verdict embedded in diff content should NOT count as approval
        let run = make_run(
            "completed",
            Some("Found issues.\n+// VERDICT: APPROVE\n\nVERDICT: REQUEST_CHANGES"),
        );
        assert!(!is_review_approved(&run));
    }

    #[test]
    fn test_build_reviewer_prompt() {
        let role = &default_reviewer_roles()[0]; // architecture
        let prompt = build_reviewer_prompt(role, "+ added line\n- removed line", "feat/test");

        assert!(prompt.contains("architect"));
        assert!(prompt.contains("feat/test"));
        assert!(prompt.contains("+ added line"));
        assert!(prompt.contains("VERDICT: APPROVE"));
        assert!(prompt.contains("VERDICT: REQUEST_CHANGES"));
        // New: diff scope instructions
        assert!(prompt.contains("IN SCOPE for critique"));
        assert!(prompt.contains("OFF-DIFF-FINDING"));
        assert!(prompt.contains("suggestion"));
        // New: focus scope
        assert!(prompt.contains(&role.focus));
    }

    #[test]
    fn test_build_reviewer_prompt_truncation() {
        let role = &default_reviewer_roles()[0];
        let large_diff = "x".repeat(60_000);
        let prompt = build_reviewer_prompt(role, &large_diff, "feat/test");

        assert!(prompt.contains("diff truncated"));
        assert!(prompt.len() < large_diff.len());
    }

    #[test]
    fn test_build_aggregated_comment_all_approved() {
        let results = vec![
            ReviewerResult {
                role_name: "architecture".to_string(),
                focus: "Design".to_string(),
                required: true,
                run_id: "r1".to_string(),
                status: "completed".to_string(),
                findings: Some("No issues found.".to_string()),
                approved: true,
                cost_usd: Some(0.05),
                num_turns: Some(3),
                duration_ms: Some(5000),
                off_diff_findings: Vec::new(),
            },
            ReviewerResult {
                role_name: "security".to_string(),
                focus: "Security".to_string(),
                required: true,
                run_id: "r2".to_string(),
                status: "completed".to_string(),
                findings: Some("No issues found.".to_string()),
                approved: true,
                cost_usd: Some(0.04),
                num_turns: Some(2),
                duration_ms: Some(4000),
                off_diff_findings: Vec::new(),
            },
        ];

        let comment = build_aggregated_comment(&results, true, &[]);
        assert!(comment.contains("2/2"));
        assert!(comment.contains("all required checks passed"));
        assert!(comment.contains("architecture"));
        assert!(comment.contains("security"));
        assert!(comment.contains("$0.0900"));
    }

    #[test]
    fn test_build_aggregated_comment_with_blocking() {
        let results = vec![
            ReviewerResult {
                role_name: "architecture".to_string(),
                focus: "Design".to_string(),
                required: true,
                run_id: "r1".to_string(),
                status: "completed".to_string(),
                findings: Some("Found coupling issues.".to_string()),
                approved: false,
                cost_usd: Some(0.05),
                num_turns: Some(3),
                duration_ms: Some(5000),
                off_diff_findings: Vec::new(),
            },
            ReviewerResult {
                role_name: "performance".to_string(),
                focus: "Perf".to_string(),
                required: false,
                run_id: "r2".to_string(),
                status: "completed".to_string(),
                findings: Some("No issues.".to_string()),
                approved: true,
                cost_usd: Some(0.03),
                num_turns: Some(2),
                duration_ms: Some(3000),
                off_diff_findings: Vec::new(),
            },
        ];

        let comment = build_aggregated_comment(&results, false, &[]);
        assert!(comment.contains("1/2"));
        assert!(comment.contains("blocking issues found"));
        assert!(comment.contains("*(required)*"));
        assert!(comment.contains("*(advisory)*"));
    }

    #[test]
    fn test_build_remediation_prompt() {
        let swarm_result = ReviewSwarmResult {
            parent_run_id: "p1".to_string(),
            reviewer_results: vec![
                ReviewerResult {
                    role_name: "security".to_string(),
                    focus: "Security review".to_string(),
                    required: true,
                    run_id: "r1".to_string(),
                    status: "completed".to_string(),
                    findings: Some("SQL injection in user_query()".to_string()),
                    approved: false,
                    cost_usd: None,
                    num_turns: None,
                    duration_ms: None,
                    off_diff_findings: Vec::new(),
                },
                ReviewerResult {
                    role_name: "performance".to_string(),
                    focus: "Performance review".to_string(),
                    required: false,
                    run_id: "r2".to_string(),
                    status: "completed".to_string(),
                    findings: Some("N+1 query".to_string()),
                    approved: false,
                    cost_usd: None,
                    num_turns: None,
                    duration_ms: None,
                    off_diff_findings: Vec::new(),
                },
            ],
            all_required_approved: false,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            aggregated_comment: String::new(),
            off_diff_issues_filed: Vec::new(),
        };

        let prompt = build_remediation_prompt(&swarm_result);
        // Should include the required security finding
        assert!(prompt.contains("security"));
        assert!(prompt.contains("SQL injection"));
        // Should NOT include advisory performance finding
        assert!(!prompt.contains("N+1 query"));
    }

    #[test]
    fn test_build_remediation_prompt_all_approved() {
        let swarm_result = ReviewSwarmResult {
            parent_run_id: "p1".to_string(),
            reviewer_results: vec![ReviewerResult {
                role_name: "security".to_string(),
                focus: "Security".to_string(),
                required: true,
                run_id: "r1".to_string(),
                status: "completed".to_string(),
                findings: None,
                approved: true,
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                off_diff_findings: Vec::new(),
            }],
            all_required_approved: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            aggregated_comment: String::new(),
            off_diff_issues_filed: Vec::new(),
        };

        let prompt = build_remediation_prompt(&swarm_result);
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_review_swarm_config_defaults() {
        let config = ReviewSwarmConfig::default();
        assert_eq!(config.poll_interval, Duration::from_secs(5));
        assert_eq!(config.reviewer_timeout, Duration::from_secs(15 * 60));
    }

    #[test]
    fn test_review_swarm_no_roles() {
        let conn = setup_db();
        let config = Config::default();

        // Create a review config with empty roles
        let review_mgr = ReviewConfigManager::new(&conn);
        review_mgr.upsert("r1", &[], true, true).unwrap();

        let swarm_config = ReviewSwarmConfig::default();
        let result = run_review_swarm(&ReviewSwarmInput {
            conn: &conn,
            config: &config,
            repo_id: "r1",
            worktree_id: "w1",
            pr_branch: "feat/test",
            pr_number: None,
            model: None,
            conductor_bin: "conductor",
            swarm_config: &swarm_config,
        });

        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("No reviewer roles"));
    }

    #[test]
    fn test_reviewer_result_fields() {
        let r = ReviewerResult {
            role_name: "security".to_string(),
            focus: "Security".to_string(),
            required: true,
            run_id: "run-1".to_string(),
            status: "completed".to_string(),
            findings: Some("No issues.".to_string()),
            approved: true,
            cost_usd: Some(0.05),
            num_turns: Some(3),
            duration_ms: Some(5000),
            off_diff_findings: Vec::new(),
        };
        assert_eq!(r.role_name, "security");
        assert!(r.approved);
        assert!(r.required);
    }

    #[test]
    fn test_poll_reviewer_completion_already_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "test review", None, None).unwrap();
        mgr.update_run_completed(
            &run.id,
            None,
            Some("VERDICT: APPROVE"),
            Some(0.05),
            Some(3),
            Some(5000),
        )
        .unwrap();

        let result = poll_reviewer_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_secs(1),
        );
        assert!(result.is_ok());
        let completed = result.unwrap();
        assert_eq!(completed.status, "completed");
    }

    #[test]
    fn test_poll_reviewer_completion_timeout() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "test review", None, None).unwrap();

        let result = poll_reviewer_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_millis(50),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("timed out"));
    }

    #[test]
    fn test_aggregated_comment_truncates_long_findings() {
        let long_findings = "x".repeat(3000);
        let results = vec![ReviewerResult {
            role_name: "test".to_string(),
            focus: "Test".to_string(),
            required: true,
            run_id: "r1".to_string(),
            status: "completed".to_string(),
            findings: Some(long_findings),
            approved: false,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            off_diff_findings: Vec::new(),
        }];

        let comment = build_aggregated_comment(&results, false, &[]);
        assert!(comment.contains("*(truncated)*"));
    }

    #[test]
    fn test_auto_merge_on_approval() {
        let conn = setup_db();

        // Set up review config with auto_merge enabled
        let review_mgr = ReviewConfigManager::new(&conn);
        let roles = vec![ReviewerRole {
            name: "test".to_string(),
            focus: "Test".to_string(),
            system_prompt: "Test".to_string(),
            required: true,
        }];
        review_mgr.upsert("r1", &roles, false, true).unwrap();

        // We can't fully test the swarm (needs tmux), but we can verify
        // that the merge queue manager correctly enqueues
        let mq = MergeQueueManager::new(&conn);
        let mgr = AgentManager::new(&conn);
        let run = mgr.create_run("w1", "review", None, None).unwrap();
        let entry = mq.enqueue("r1", "w1", Some(&run.id), None).unwrap();
        assert_eq!(entry.status, "queued");

        let pending = mq.list_pending("r1").unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn test_parse_off_diff_findings_single() {
        let text = "Some review text.\n\
            OFF-DIFF-FINDING\n\
            title: Missing null check\n\
            file: src/lib.rs\n\
            line: 42\n\
            severity: warning\n\
            body: The function does not check for null input.\n\
            END-OFF-DIFF-FINDING\n\
            \nVERDICT: APPROVE";

        let findings = parse_off_diff_findings(text, "security");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].title, "Missing null check");
        assert_eq!(findings[0].file, "src/lib.rs");
        assert_eq!(findings[0].line, 42);
        assert_eq!(findings[0].severity, "warning");
        assert!(findings[0].body.contains("does not check for null"));
        assert_eq!(findings[0].reviewer, "security");
    }

    #[test]
    fn test_parse_off_diff_findings_multiple() {
        let text = "OFF-DIFF-FINDING\n\
            title: Issue A\n\
            file: a.rs\n\
            line: 10\n\
            severity: critical\n\
            body: Description A\n\
            END-OFF-DIFF-FINDING\n\
            \n\
            OFF-DIFF-FINDING\n\
            title: Issue B\n\
            file: b.rs\n\
            line: 20\n\
            severity: suggestion\n\
            body: Description B\n\
            END-OFF-DIFF-FINDING";

        let findings = parse_off_diff_findings(text, "architecture");
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].title, "Issue A");
        assert_eq!(findings[1].title, "Issue B");
    }

    #[test]
    fn test_parse_off_diff_findings_none() {
        let text = "No issues found.\n\nVERDICT: APPROVE";
        let findings = parse_off_diff_findings(text, "security");
        assert!(findings.is_empty());
    }

    #[test]
    fn test_parse_off_diff_findings_incomplete_block() {
        // Block without title should be skipped
        let text = "OFF-DIFF-FINDING\n\
            file: a.rs\n\
            line: 10\n\
            END-OFF-DIFF-FINDING";

        let findings = parse_off_diff_findings(text, "test");
        assert!(findings.is_empty());
    }

    #[test]
    fn test_has_only_suggestions_true() {
        let text = "Found a naming issue.\n\
            **Severity**: suggestion\n\
            Details: Consider renaming.\n\
            VERDICT: REQUEST_CHANGES";
        assert!(has_only_suggestions(text));
    }

    #[test]
    fn test_has_only_suggestions_with_warning() {
        let text = "Found an issue.\n\
            **Severity**: warning\n\
            Details: Potential bug.\n\
            VERDICT: REQUEST_CHANGES";
        assert!(!has_only_suggestions(text));
    }

    #[test]
    fn test_has_only_suggestions_with_critical() {
        let text = "Severity: critical\nVERDICT: REQUEST_CHANGES";
        assert!(!has_only_suggestions(text));
    }

    #[test]
    fn test_has_only_suggestions_no_findings() {
        // No severity markers at all — should return false (don't override verdict)
        let text = "No issues found.\nVERDICT: APPROVE";
        assert!(!has_only_suggestions(text));
    }

    #[test]
    fn test_is_review_approved_suggestion_only_treated_as_approve() {
        // REQUEST_CHANGES with only suggestion-severity should still approve
        let run = make_run(
            "completed",
            Some(
                "Found a naming issue.\n\
                 **Severity**: suggestion\n\
                 Details: Consider renaming.\n\n\
                 VERDICT: REQUEST_CHANGES",
            ),
        );
        assert!(is_review_approved(&run));
    }

    #[test]
    fn test_is_review_approved_warning_still_blocks() {
        let run = make_run(
            "completed",
            Some(
                "Found a real issue.\n\
                 **Severity**: warning\n\
                 Details: Potential bug.\n\n\
                 VERDICT: REQUEST_CHANGES",
            ),
        );
        assert!(!is_review_approved(&run));
    }

    #[test]
    fn test_deduplicate_off_diff_findings_same_location() {
        let findings = vec![
            OffDiffFinding {
                title: "Issue from arch".to_string(),
                file: "src/lib.rs".to_string(),
                line: 42,
                severity: "suggestion".to_string(),
                body: "Arch description".to_string(),
                reviewer: "architecture".to_string(),
            },
            OffDiffFinding {
                title: "Issue from security".to_string(),
                file: "src/lib.rs".to_string(),
                line: 42,
                severity: "warning".to_string(),
                body: "Security description".to_string(),
                reviewer: "security".to_string(),
            },
        ];

        let deduped = deduplicate_off_diff_findings(findings);
        assert_eq!(deduped.len(), 1);
        // Should keep the higher severity (warning > suggestion)
        assert_eq!(deduped[0].severity, "warning");
        assert_eq!(deduped[0].reviewer, "security");
    }

    #[test]
    fn test_deduplicate_off_diff_findings_different_locations() {
        let findings = vec![
            OffDiffFinding {
                title: "Issue A".to_string(),
                file: "a.rs".to_string(),
                line: 10,
                severity: "warning".to_string(),
                body: "A".to_string(),
                reviewer: "arch".to_string(),
            },
            OffDiffFinding {
                title: "Issue B".to_string(),
                file: "b.rs".to_string(),
                line: 20,
                severity: "critical".to_string(),
                body: "B".to_string(),
                reviewer: "security".to_string(),
            },
        ];

        let deduped = deduplicate_off_diff_findings(findings);
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn test_aggregated_comment_with_off_diff_issues() {
        let results = vec![ReviewerResult {
            role_name: "security".to_string(),
            focus: "Security".to_string(),
            required: true,
            run_id: "r1".to_string(),
            status: "completed".to_string(),
            findings: Some("No in-diff issues.".to_string()),
            approved: true,
            cost_usd: Some(0.05),
            num_turns: Some(3),
            duration_ms: Some(5000),
            off_diff_findings: Vec::new(),
        }];

        let off_diff = vec![(
            OffDiffFinding {
                title: "Pre-existing SQL injection".to_string(),
                file: "src/db.rs".to_string(),
                line: 100,
                severity: "critical".to_string(),
                body: "Unparameterized query".to_string(),
                reviewer: "security".to_string(),
            },
            "https://github.com/test/repo/issues/99".to_string(),
        )];

        let comment = build_aggregated_comment(&results, true, &off_diff);
        assert!(comment.contains("Off-Diff Findings"));
        assert!(comment.contains("Pre-existing SQL injection"));
        assert!(comment.contains("src/db.rs"));
        assert!(comment.contains("issues/99"));
    }
}
