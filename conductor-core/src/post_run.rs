//! Automated post-agent lifecycle: commit, PR, review loop, and merge.
//!
//! After an agent run finishes and passes validation, this module automates:
//! 1. Commit with AI-generated message
//! 2. Push + PR creation with AI-generated description
//! 3. Review swarm → fix loop (configurable iterations)
//! 4. Auto-merge or pause for manual approval based on issue labels

use std::process::Command;

use rusqlite::Connection;

use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::github;
use crate::pr_review::{self, ReviewSwarmConfig, ReviewSwarmInput};
use crate::repo::RepoManager;
use crate::worktree::WorktreeManager;

/// Outcome of the post-run lifecycle.
#[derive(Debug, Clone)]
pub struct PostRunResult {
    /// What phase the lifecycle reached.
    pub phase: PostRunPhase,
    /// PR URL if one was created.
    pub pr_url: Option<String>,
    /// PR number if one was created.
    pub pr_number: Option<i64>,
    /// Whether the PR was merged.
    pub merged: bool,
    /// Number of review-fix iterations executed.
    pub review_iterations: u32,
    /// Human-readable summary.
    pub summary: String,
}

/// The phase the lifecycle completed or stopped at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostRunPhase {
    /// Nothing to commit (clean worktree).
    NothingToCommit,
    /// Committed and pushed, but PR creation failed.
    PushFailed,
    /// PR created, review loop exhausted — needs human.
    NeedsHuman,
    /// PR created, awaiting manual approval (feature/enhancement).
    AwaitingApproval,
    /// PR merged automatically.
    Merged,
}

/// Input parameters for the post-run lifecycle.
pub struct PostRunInput<'a> {
    pub conn: &'a Connection,
    pub config: &'a Config,
    pub repo_slug: &'a str,
    pub worktree_slug: &'a str,
    pub conductor_bin: &'a str,
}

/// Run the full post-agent lifecycle.
pub fn run_post_lifecycle(input: &PostRunInput<'_>) -> Result<PostRunResult> {
    let conn = input.conn;
    let config = input.config;
    let post_cfg = &config.post_run;

    let repo_mgr = RepoManager::new(conn, config);
    let repo = repo_mgr.get_by_slug(input.repo_slug)?;
    let wt_mgr = WorktreeManager::new(conn, config);
    let wt = wt_mgr.get_by_slug(&repo.id, input.worktree_slug)?;

    let (owner, repo_name) = github::parse_github_remote(&repo.remote_url).ok_or_else(|| {
        ConductorError::Agent(format!(
            "Cannot determine GitHub repo from remote URL: {}",
            repo.remote_url
        ))
    })?;

    // ── Phase 1: Commit ──────────────────────────────────────────────
    eprintln!("[post-run] Phase 1: Commit");

    if !has_changes(&wt.path)? {
        eprintln!("[post-run] No changes to commit — nothing to do");
        return Ok(PostRunResult {
            phase: PostRunPhase::NothingToCommit,
            pr_url: None,
            pr_number: None,
            merged: false,
            review_iterations: 0,
            summary: "No changes to commit".to_string(),
        });
    }

    let diff = get_staged_or_all_diff(&wt.path)?;
    let ticket_context = get_ticket_context(conn, &wt);
    let commit_msg =
        generate_commit_message(&wt.path, &diff, &ticket_context, &post_cfg.commit_style)?;
    let pr_description = generate_pr_description(&wt.path, &diff, &ticket_context, &commit_msg)?;

    stage_and_commit(&wt.path, &commit_msg)?;
    eprintln!("[post-run] Committed: {}", first_line(&commit_msg));

    // ── Phase 2: Push + PR ───────────────────────────────────────────
    eprintln!("[post-run] Phase 2: Push and create PR");

    push_branch(&wt.path, &wt.branch)?;

    // Check for existing PR first
    let (pr_number, pr_url) = match github::detect_pr(&owner, &repo_name, &wt.branch)? {
        Some((num, url)) => {
            eprintln!("[post-run] Existing PR found: #{num}");
            (num, url)
        }
        None => {
            let pr_title = first_line(&commit_msg);
            let url = github::create_pr_with_body(
                &owner,
                &repo_name,
                &wt.branch,
                pr_title,
                &pr_description,
            )?;
            let num = github::detect_pr(&owner, &repo_name, &wt.branch)?
                .map(|(n, _)| n)
                .unwrap_or(0);
            eprintln!("[post-run] Created PR #{num}: {url}");
            (num, url)
        }
    };

    // ── Phase 3: Review → Fix Loop ───────────────────────────────────
    eprintln!(
        "[post-run] Phase 3: Review loop (max {} iterations)",
        post_cfg.review_loop_max
    );

    let swarm_config = ReviewSwarmConfig::default();
    let mut iteration = 0u32;
    let mut all_approved = false;

    while iteration < post_cfg.review_loop_max {
        iteration += 1;
        eprintln!(
            "[post-run] Review iteration {iteration}/{}",
            post_cfg.review_loop_max
        );

        let swarm_result = pr_review::run_review_swarm(&ReviewSwarmInput {
            conn,
            config,
            repo_id: &repo.id,
            worktree_id: &wt.id,
            pr_branch: &wt.branch,
            pr_number: Some(pr_number),
            model: None,
            conductor_bin: input.conductor_bin,
            swarm_config: &swarm_config,
        })?;

        if swarm_result.all_required_approved {
            eprintln!("[post-run] All required reviewers approved on iteration {iteration}");
            all_approved = true;
            break;
        }

        // Not approved — run fix agent if we have iterations left
        if iteration < post_cfg.review_loop_max {
            eprintln!("[post-run] Running rebase-and-fix-review...");
            let fix_result = run_fix_review(input.conductor_bin, &wt.path);
            match fix_result {
                Ok(()) => {
                    // Push the fixes
                    let push = Command::new("git")
                        .args(["push", "--force-with-lease"])
                        .current_dir(&wt.path)
                        .output()?;
                    if !push.status.success() {
                        eprintln!(
                            "[post-run] Push after fix failed: {}",
                            String::from_utf8_lossy(&push.stderr)
                        );
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("[post-run] Fix agent failed: {e}");
                    break;
                }
            }
        }
    }

    if !all_approved {
        // Exhaustion: label PR and notify
        eprintln!(
            "[post-run] Review loop exhausted after {iteration} iterations — marking needs-human"
        );
        let _ = github::add_pr_label(&owner, &repo_name, pr_number, "needs-human");

        return Ok(PostRunResult {
            phase: PostRunPhase::NeedsHuman,
            pr_url: Some(pr_url),
            pr_number: Some(pr_number),
            merged: false,
            review_iterations: iteration,
            summary: format!(
                "Review loop exhausted after {iteration} iterations. PR #{pr_number} labeled needs-human."
            ),
        });
    }

    // ── Phase 4: Merge decision ──────────────────────────────────────
    let issue_labels = get_issue_labels(conn, &wt);
    let should_auto_merge = classify_merge_behavior(&issue_labels, post_cfg);

    if should_auto_merge {
        eprintln!("[post-run] Phase 4a: Auto-merge");

        github::squash_merge_pr(&owner, &repo_name, pr_number)?;
        eprintln!("[post-run] PR #{pr_number} squash-merged");

        // Close linked issue if applicable
        if let Some(issue_number) = get_linked_issue_number(conn, &wt) {
            let _ = github::close_github_issue(&owner, &repo_name, &issue_number);
            eprintln!("[post-run] Closed issue #{issue_number}");
        }

        // Clean up local worktree
        let _ = wt_mgr.delete_by_id(&wt.id);
        eprintln!("[post-run] Cleaned up worktree '{}'", wt.slug);

        Ok(PostRunResult {
            phase: PostRunPhase::Merged,
            pr_url: Some(pr_url),
            pr_number: Some(pr_number),
            merged: true,
            review_iterations: iteration,
            summary: format!("PR #{pr_number} squash-merged and cleaned up."),
        })
    } else {
        eprintln!("[post-run] Phase 4b: Awaiting manual approval");
        eprintln!(
            "[post-run] PR #{pr_number} is ready for review. \
             Run `conductor approve {repo_slug} {worktree_slug}` to merge.",
            repo_slug = input.repo_slug,
            worktree_slug = input.worktree_slug,
        );

        Ok(PostRunResult {
            phase: PostRunPhase::AwaitingApproval,
            pr_url: Some(pr_url),
            pr_number: Some(pr_number),
            merged: false,
            review_iterations: iteration,
            summary: format!(
                "PR #{pr_number} approved, awaiting manual merge (feature/enhancement)."
            ),
        })
    }
}

/// Approve and merge a PR that was waiting for manual approval.
pub fn approve_and_merge(input: &PostRunInput<'_>) -> Result<PostRunResult> {
    let conn = input.conn;
    let config = input.config;

    let repo_mgr = RepoManager::new(conn, config);
    let repo = repo_mgr.get_by_slug(input.repo_slug)?;
    let wt_mgr = WorktreeManager::new(conn, config);
    let wt = wt_mgr.get_by_slug(&repo.id, input.worktree_slug)?;

    let (owner, repo_name) = github::parse_github_remote(&repo.remote_url).ok_or_else(|| {
        ConductorError::Agent(format!(
            "Cannot determine GitHub repo from remote URL: {}",
            repo.remote_url
        ))
    })?;

    let (pr_number, pr_url) = github::detect_pr(&owner, &repo_name, &wt.branch)?
        .ok_or_else(|| ConductorError::Agent(format!("No PR found for branch {}", wt.branch)))?;

    github::squash_merge_pr(&owner, &repo_name, pr_number)?;
    eprintln!("[approve] PR #{pr_number} squash-merged");

    if let Some(issue_number) = get_linked_issue_number(conn, &wt) {
        let _ = github::close_github_issue(&owner, &repo_name, &issue_number);
        eprintln!("[approve] Closed issue #{issue_number}");
    }

    let _ = wt_mgr.delete_by_id(&wt.id);
    eprintln!("[approve] Cleaned up worktree '{}'", wt.slug);

    Ok(PostRunResult {
        phase: PostRunPhase::Merged,
        pr_url: Some(pr_url),
        pr_number: Some(pr_number),
        merged: true,
        review_iterations: 0,
        summary: format!("PR #{pr_number} approved and squash-merged."),
    })
}

// ── Internal helpers ─────────────────────────────────────────────────

fn has_changes(worktree_path: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree_path)
        .output()?;
    Ok(!output.stdout.is_empty())
}

fn get_staged_or_all_diff(worktree_path: &str) -> Result<String> {
    // Stage everything first so we can get a complete diff
    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(worktree_path)
        .output()?;

    let output = Command::new("git")
        .args(["diff", "--cached"])
        .current_dir(worktree_path)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn get_ticket_context(conn: &Connection, wt: &crate::worktree::Worktree) -> String {
    let ticket_id = match wt.ticket_id {
        Some(ref id) => id,
        None => return String::new(),
    };

    conn.query_row(
        "SELECT source_type, source_id, title, body FROM tickets WHERE id = ?1",
        rusqlite::params![ticket_id],
        |row| {
            let source_type: String = row.get(0)?;
            let source_id: String = row.get(1)?;
            let title: String = row.get(2)?;
            let body: String = row.get(3)?;
            Ok(format!(
                "Ticket: {source_type} #{source_id} — {title}\n\n{body}"
            ))
        },
    )
    .unwrap_or_default()
}

fn generate_commit_message(
    worktree_path: &str,
    diff: &str,
    ticket_context: &str,
    commit_style: &str,
) -> Result<String> {
    let style_instruction = if commit_style == "conventional" {
        "Use Conventional Commits format (e.g. 'feat:', 'fix:', 'refactor:', etc.)."
    } else {
        "Write a clear, descriptive commit message in free-form style."
    };

    // Truncate diff for the prompt to stay within limits
    let max_diff = 10_000;
    let truncated_diff = if diff.len() > max_diff {
        &diff[..max_diff]
    } else {
        diff
    };

    let prompt = format!(
        "Generate a git commit message for the following changes. {style_instruction}\n\
         Write ONLY the commit message, nothing else. First line should be a short summary \
         (max 72 chars), optionally followed by a blank line and body.\n\n\
         {ticket_ctx}\n\n\
         Diff:\n```\n{diff}\n```",
        ticket_ctx = if ticket_context.is_empty() {
            String::new()
        } else {
            format!("Context:\n{ticket_context}\n")
        },
        diff = truncated_diff,
    );

    let output = Command::new("claude")
        .arg("-p")
        .arg(&prompt)
        .arg("--output-format")
        .arg("json")
        .arg("--dangerously-skip-permissions")
        .current_dir(worktree_path)
        .output()
        .map_err(|e| ConductorError::Agent(format!("failed to run claude for commit msg: {e}")))?;

    if !output.status.success() {
        return Err(ConductorError::Agent(
            "Claude failed to generate commit message".to_string(),
        ));
    }

    let response: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| ConductorError::Agent(format!("failed to parse claude output: {e}")))?;

    let result = response
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or("chore: automated changes")
        .trim()
        .to_string();

    Ok(result)
}

fn generate_pr_description(
    worktree_path: &str,
    diff: &str,
    ticket_context: &str,
    commit_msg: &str,
) -> Result<String> {
    let max_diff = 10_000;
    let truncated_diff = if diff.len() > max_diff {
        &diff[..max_diff]
    } else {
        diff
    };

    let prompt = format!(
        "Generate a concise PR description in markdown for GitHub. \
         Include a ## Summary section with 1-3 bullet points.\n\n\
         Commit message: {commit_msg}\n\n\
         {ticket_ctx}\n\n\
         Diff:\n```\n{diff}\n```\n\n\
         Write ONLY the PR description body, no title.",
        ticket_ctx = if ticket_context.is_empty() {
            String::new()
        } else {
            format!("Context:\n{ticket_context}\n")
        },
        diff = truncated_diff,
    );

    let output = Command::new("claude")
        .arg("-p")
        .arg(&prompt)
        .arg("--output-format")
        .arg("json")
        .arg("--dangerously-skip-permissions")
        .current_dir(worktree_path)
        .output()
        .map_err(|e| ConductorError::Agent(format!("failed to run claude for PR desc: {e}")))?;

    if !output.status.success() {
        return Ok(format!("## Summary\n\n{commit_msg}"));
    }

    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_default();

    let result = response
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or(commit_msg)
        .trim()
        .to_string();

    Ok(result)
}

fn stage_and_commit(worktree_path: &str, message: &str) -> Result<()> {
    // Stage all changes
    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(worktree_path)
        .output()?;
    if !add.status.success() {
        return Err(ConductorError::Git(
            String::from_utf8_lossy(&add.stderr).to_string(),
        ));
    }

    // Commit
    let commit = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(worktree_path)
        .output()?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr).to_string();
        // "nothing to commit" is not an error
        if stderr.contains("nothing to commit") {
            return Ok(());
        }
        return Err(ConductorError::Git(stderr));
    }

    Ok(())
}

fn push_branch(worktree_path: &str, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["push", "-u", "origin", branch])
        .current_dir(worktree_path)
        .output()?;
    if !output.status.success() {
        return Err(ConductorError::Git(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(())
}

fn run_fix_review(_conductor_bin: &str, worktree_path: &str) -> std::result::Result<(), String> {
    // Run claude with the rebase-and-fix-review skill
    let output = Command::new("claude")
        .arg("-p")
        .arg("/rebase-and-fix-review")
        .arg("--dangerously-skip-permissions")
        .current_dir(worktree_path)
        .output()
        .map_err(|e| format!("failed to spawn fix agent: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("fix agent failed: {stderr}"))
    }
}

fn get_issue_labels(conn: &Connection, wt: &crate::worktree::Worktree) -> Vec<String> {
    let ticket_id = match wt.ticket_id {
        Some(ref id) => id,
        None => return Vec::new(),
    };

    conn.query_row(
        "SELECT labels FROM tickets WHERE id = ?1",
        rusqlite::params![ticket_id],
        |row| {
            let labels_json: String = row.get(0)?;
            let labels: Vec<String> = serde_json::from_str(&labels_json).unwrap_or_default();
            Ok(labels)
        },
    )
    .unwrap_or_default()
}

fn classify_merge_behavior(
    issue_labels: &[String],
    post_cfg: &crate::config::PostRunConfig,
) -> bool {
    // If any label matches manual_merge_labels → manual
    for label in issue_labels {
        let lower = label.to_lowercase();
        if post_cfg
            .manual_merge_labels
            .iter()
            .any(|m| m.to_lowercase() == lower)
        {
            return false;
        }
    }
    // If any label matches auto_merge_labels → auto
    for label in issue_labels {
        let lower = label.to_lowercase();
        if post_cfg
            .auto_merge_labels
            .iter()
            .any(|a| a.to_lowercase() == lower)
        {
            return true;
        }
    }
    // Default: no labels → auto-merge (bugs and chores are the common case)
    true
}

fn get_linked_issue_number(conn: &Connection, wt: &crate::worktree::Worktree) -> Option<String> {
    let ticket_id = wt.ticket_id.as_ref()?;
    conn.query_row(
        "SELECT source_id FROM tickets WHERE id = ?1 AND source_type = 'github'",
        rusqlite::params![ticket_id],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PostRunConfig;

    #[test]
    fn test_classify_merge_auto_with_bug_label() {
        let cfg = PostRunConfig::default();
        let labels = vec!["bug".to_string()];
        assert!(classify_merge_behavior(&labels, &cfg));
    }

    #[test]
    fn test_classify_merge_manual_with_feature_label() {
        let cfg = PostRunConfig::default();
        let labels = vec!["enhancement".to_string()];
        assert!(!classify_merge_behavior(&labels, &cfg));
    }

    #[test]
    fn test_classify_merge_manual_takes_precedence() {
        let cfg = PostRunConfig::default();
        // Both auto and manual labels present — manual wins
        let labels = vec!["bug".to_string(), "enhancement".to_string()];
        assert!(!classify_merge_behavior(&labels, &cfg));
    }

    #[test]
    fn test_classify_merge_no_labels_defaults_to_auto() {
        let cfg = PostRunConfig::default();
        let labels: Vec<String> = vec![];
        assert!(classify_merge_behavior(&labels, &cfg));
    }

    #[test]
    fn test_classify_merge_case_insensitive() {
        let cfg = PostRunConfig::default();
        let labels = vec!["Enhancement".to_string()];
        assert!(!classify_merge_behavior(&labels, &cfg));
    }

    #[test]
    fn test_classify_merge_chore_label() {
        let cfg = PostRunConfig::default();
        let labels = vec!["chore".to_string()];
        assert!(classify_merge_behavior(&labels, &cfg));
    }

    #[test]
    fn test_first_line() {
        assert_eq!(first_line("hello\nworld"), "hello");
        assert_eq!(first_line("single"), "single");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn test_post_run_phase_eq() {
        assert_eq!(PostRunPhase::Merged, PostRunPhase::Merged);
        assert_ne!(PostRunPhase::Merged, PostRunPhase::NeedsHuman);
    }
}
