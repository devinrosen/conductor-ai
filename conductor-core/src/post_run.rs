//! Automated post-agent lifecycle: commit, PR, review loop, and merge.
//!
//! After an agent run finishes and passes validation, this module automates:
//! 1. Commit with AI-generated message
//! 2. Push + PR creation with AI-generated description
//! 3. Review swarm → fix loop (configurable iterations)
//! 4. Auto-merge or pause for manual approval based on issue labels

use std::process::Command;

use rusqlite::Connection;

use crate::agent::{AgentManager, CostPhase};
use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::github;
use crate::pr_review::{self, ReviewSwarmConfig, ReviewSwarmInput};
use crate::repo::RepoManager;
use crate::text_util::truncate_str;
use crate::tickets::{Ticket, TicketSyncer};
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

    let ctx = resolve_context(input)?;
    let ResolvedContext {
        ref wt,
        ref wt_mgr,
        ref owner,
        ref repo_name,
        ..
    } = ctx;

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

    let diff = stage_all_and_get_diff(&wt.path)?;
    let ticket = fetch_linked_ticket(conn, wt);
    let ticket_context = ticket
        .as_ref()
        .map(format_ticket_context)
        .unwrap_or_default();
    let commit_msg = generate_commit_message(
        &wt.path,
        &diff,
        &ticket_context,
        &post_cfg.commit_style,
        &post_cfg.commit_model,
    )?;
    let pr_description = generate_pr_description(
        &wt.path,
        &diff,
        &ticket_context,
        &commit_msg,
        &post_cfg.commit_model,
    )?;

    stage_and_commit(&wt.path, &commit_msg)?;
    eprintln!("[post-run] Committed: {}", first_line(&commit_msg));

    // ── Phase 2: Push + PR ───────────────────────────────────────────
    eprintln!("[post-run] Phase 2: Push and create PR");

    push_branch(&wt.path, &wt.branch)?;

    // Check for existing PR first
    let (pr_number, pr_url) = match github::detect_pr(owner, repo_name, &wt.branch)? {
        Some((num, url)) => {
            eprintln!("[post-run] Existing PR found: #{num}");
            (num, url)
        }
        None => {
            let pr_title = first_line(&commit_msg);
            let url = github::create_pr_with_body(
                owner,
                repo_name,
                &wt.branch,
                pr_title,
                &pr_description,
            )?;
            let num = github::parse_pr_number_from_url(&url).unwrap_or(0);
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
            repo_id: &ctx.repo.id,
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
            let fix_result = run_fix_review(&wt.path, post_cfg.dangerous_skip_permissions);
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

    // Post cost summary (best-effort, non-blocking)
    post_cost_summary(conn, owner, repo_name, pr_number, &wt.id);

    if !all_approved {
        // Exhaustion: label PR and notify
        eprintln!(
            "[post-run] Review loop exhausted after {iteration} iterations — marking needs-human"
        );
        let _ = github::add_pr_label(owner, repo_name, pr_number, "needs-human");

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
    let issue_labels = ticket
        .as_ref()
        .map(|t| parse_labels(&t.labels))
        .unwrap_or_default();
    let should_auto_merge = classify_merge_behavior(&issue_labels, post_cfg);

    if should_auto_merge {
        eprintln!("[post-run] Phase 4a: Auto-merge");

        merge_and_cleanup(
            owner,
            repo_name,
            pr_number,
            ticket.as_ref(),
            wt,
            wt_mgr,
            "[post-run]",
        )?;

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
    let ctx = resolve_context(input)?;
    let ResolvedContext {
        repo: _,
        ref wt,
        ref wt_mgr,
        ref owner,
        ref repo_name,
    } = ctx;

    let (pr_number, pr_url) = github::detect_pr(owner, repo_name, &wt.branch)?
        .ok_or_else(|| ConductorError::Agent(format!("No PR found for branch {}", wt.branch)))?;

    let ticket = fetch_linked_ticket(input.conn, wt);
    merge_and_cleanup(
        owner,
        repo_name,
        pr_number,
        ticket.as_ref(),
        wt,
        wt_mgr,
        "[approve]",
    )?;

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

/// Resolved context shared by both `run_post_lifecycle` and `approve_and_merge`.
struct ResolvedContext<'a> {
    repo: crate::repo::Repo,
    wt: crate::worktree::Worktree,
    wt_mgr: WorktreeManager<'a>,
    owner: String,
    repo_name: String,
}

fn resolve_context<'a>(input: &'a PostRunInput<'a>) -> Result<ResolvedContext<'a>> {
    let repo_mgr = RepoManager::new(input.conn, input.config);
    let repo = repo_mgr.get_by_slug(input.repo_slug)?;
    let wt_mgr = WorktreeManager::new(input.conn, input.config);
    let wt = wt_mgr.get_by_slug(&repo.id, input.worktree_slug)?;

    let (owner, repo_name) = github::parse_github_remote(&repo.remote_url).ok_or_else(|| {
        ConductorError::Agent(format!(
            "Cannot determine GitHub repo from remote URL: {}",
            repo.remote_url
        ))
    })?;

    Ok(ResolvedContext {
        repo,
        wt,
        wt_mgr,
        owner,
        repo_name,
    })
}

fn has_changes(worktree_path: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree_path)
        .output()?;
    Ok(!output.stdout.is_empty())
}

fn stage_all_and_get_diff(worktree_path: &str) -> Result<String> {
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

/// Fetch the linked ticket for a worktree, if any.
fn fetch_linked_ticket(conn: &Connection, wt: &crate::worktree::Worktree) -> Option<Ticket> {
    let ticket_id = wt.ticket_id.as_ref()?;
    TicketSyncer::new(conn).get_by_id(ticket_id).ok()
}

/// Format a ticket into context text for AI prompts.
fn format_ticket_context(ticket: &Ticket) -> String {
    format!(
        "Ticket: {} #{} — {}\n\n{}",
        ticket.source_type, ticket.source_id, ticket.title, ticket.body
    )
}

/// Parse labels JSON string into a Vec<String>.
fn parse_labels(labels_json: &str) -> Vec<String> {
    serde_json::from_str(labels_json).unwrap_or_default()
}

/// Truncate diff to `MAX_DIFF_BYTES` on a char boundary.
const MAX_DIFF_BYTES: usize = 10_000;

fn truncate_diff(diff: &str) -> &str {
    truncate_str(diff, MAX_DIFF_BYTES)
}

/// Call `claude -p` with a prompt and return the text result.
/// Uses `--allowedTools ""` to prevent tool use (text generation only).
fn call_claude(prompt: &str, worktree_path: &str, model: &str) -> Result<Option<String>> {
    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg(prompt)
        .arg("--output-format")
        .arg("json")
        .arg("--allowedTools")
        .arg("")
        .current_dir(worktree_path);
    if !model.is_empty() {
        cmd.arg("--model").arg(model);
    }
    let output = cmd
        .output()
        .map_err(|e| ConductorError::Agent(format!("failed to run claude: {e}")))?;

    if !output.status.success() {
        return Ok(None);
    }

    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
    Ok(response
        .get("result")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string()))
}

fn generate_commit_message(
    worktree_path: &str,
    diff: &str,
    ticket_context: &str,
    commit_style: &str,
    model: &str,
) -> Result<String> {
    let style_instruction = if commit_style == "conventional" {
        "Use Conventional Commits format (e.g. 'feat:', 'fix:', 'refactor:', etc.)."
    } else {
        "Write a clear, descriptive commit message in free-form style."
    };

    let truncated_diff = truncate_diff(diff);

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

    call_claude(&prompt, worktree_path, model)?.ok_or_else(|| {
        ConductorError::Agent("Claude failed to generate commit message".to_string())
    })
}

fn generate_pr_description(
    worktree_path: &str,
    diff: &str,
    ticket_context: &str,
    commit_msg: &str,
    model: &str,
) -> Result<String> {
    let truncated_diff = truncate_diff(diff);

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

    Ok(call_claude(&prompt, worktree_path, model)?
        .unwrap_or_else(|| format!("## Summary\n\n{commit_msg}")))
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

fn run_fix_review(
    worktree_path: &str,
    dangerous_skip_permissions: bool,
) -> std::result::Result<(), String> {
    // Run claude with the rebase-and-fix-review skill
    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg("/rebase-and-fix-review")
        .current_dir(worktree_path);
    if dangerous_skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn fix agent: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("fix agent failed: {stderr}"))
    }
}

fn classify_merge_behavior(
    issue_labels: &[String],
    post_cfg: &crate::config::PostRunConfig,
) -> bool {
    // Pre-lowercase config labels once to avoid repeated allocations
    let manual_lower: Vec<String> = post_cfg
        .manual_merge_labels
        .iter()
        .map(|l| l.to_lowercase())
        .collect();
    let auto_lower: Vec<String> = post_cfg
        .auto_merge_labels
        .iter()
        .map(|l| l.to_lowercase())
        .collect();

    // If any label matches manual_merge_labels → manual
    for label in issue_labels {
        let lower = label.to_lowercase();
        if manual_lower.contains(&lower) {
            return false;
        }
    }
    // If any label matches auto_merge_labels → auto
    for label in issue_labels {
        let lower = label.to_lowercase();
        if auto_lower.contains(&lower) {
            return true;
        }
    }
    // Default: no labels → auto-merge (bugs and chores are the common case)
    true
}

/// Squash-merge a PR, close the linked GitHub issue (if any), and delete the local worktree.
fn merge_and_cleanup(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    ticket: Option<&Ticket>,
    wt: &crate::worktree::Worktree,
    wt_mgr: &WorktreeManager,
    log_prefix: &str,
) -> Result<()> {
    github::squash_merge_pr(owner, repo_name, pr_number)?;
    eprintln!("{log_prefix} PR #{pr_number} squash-merged");

    // Close linked GitHub issue if applicable
    if let Some(t) = ticket {
        if t.source_type == "github" {
            let _ = github::close_github_issue(owner, repo_name, &t.source_id);
            eprintln!("{log_prefix} Closed issue #{}", t.source_id);
        }
    }

    let _ = wt_mgr.delete_by_id(&wt.id);
    eprintln!("{log_prefix} Cleaned up worktree '{}'", wt.slug);
    Ok(())
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

/// Format a duration in milliseconds as a human-readable string (e.g. "8m 12s").
fn format_duration(ms: i64) -> String {
    let total_secs = ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    if mins > 0 {
        format!("{mins}m {secs:02}s")
    } else {
        format!("{secs}s")
    }
}

/// Build the markdown body for the sticky cost-summary PR comment.
pub fn build_cost_comment(phases: &[CostPhase]) -> String {
    let marker = github::COST_COMMENT_MARKER;
    let mut comment = format!("{marker}\n## Implementation Cost\n\n");

    if phases.is_empty() {
        comment.push_str("*No cost data recorded yet.*\n");
    } else {
        comment.push_str("| Phase | Model | Cost | Duration |\n");
        comment.push_str("|---|---|---|---|\n");

        let mut total_cost = 0.0f64;
        let mut total_duration = 0i64;

        for phase in phases {
            let model = phase.model.as_deref().unwrap_or("default");
            total_cost += phase.cost_usd;
            total_duration += phase.duration_ms;
            comment.push_str(&format!(
                "| {} | {} | ${:.4} | {} |\n",
                phase.label,
                model,
                phase.cost_usd,
                format_duration(phase.duration_ms),
            ));
        }

        if phases.len() > 1 {
            comment.push_str(&format!(
                "| **Total** | | **${:.4}** | **{}** |\n",
                total_cost,
                format_duration(total_duration),
            ));
        }
    }

    comment.push_str("\n*Generated with [Claude Code](https://claude.ai/code) via conductor-ai*\n");
    comment
}

/// Post (or update) the sticky cost-summary comment on a PR.
///
/// Reads the per-phase cost breakdown from the DB and upserts the comment.
/// Errors are logged but do not fail the lifecycle.
fn post_cost_summary(
    conn: &Connection,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    worktree_id: &str,
) {
    let mgr = AgentManager::new(conn);
    let phases = match mgr.worktree_cost_phases(worktree_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[post-run] Failed to fetch cost phases: {e}");
            return;
        }
    };
    let body = build_cost_comment(&phases);
    if let Err(e) = github::upsert_sticky_comment(owner, repo_name, pr_number, &body) {
        eprintln!("[post-run] Failed to post cost summary: {e}");
    }
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

    #[test]
    fn test_format_duration_seconds_only() {
        assert_eq!(format_duration(45_000), "45s");
    }

    #[test]
    fn test_format_duration_minutes_and_seconds() {
        assert_eq!(format_duration(492_000), "8m 12s");
    }

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration(0), "0s");
    }

    #[test]
    fn test_build_cost_comment_empty() {
        let comment = build_cost_comment(&[]);
        assert!(comment.contains("No cost data recorded yet."));
        assert!(comment.contains("<!-- conductor-cost-summary -->"));
    }

    #[test]
    fn test_build_cost_comment_single_phase() {
        let phases = vec![CostPhase {
            label: "Initial run".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
            cost_usd: 0.031,
            duration_ms: 492_000,
        }];
        let comment = build_cost_comment(&phases);
        assert!(comment.contains("<!-- conductor-cost-summary -->"));
        assert!(comment.contains("Initial run"));
        assert!(comment.contains("claude-sonnet-4-6"));
        assert!(comment.contains("$0.0310"));
        assert!(comment.contains("8m 12s"));
        // Single phase: no total row
        assert!(!comment.contains("**Total**"));
    }

    #[test]
    fn test_build_cost_comment_multiple_phases_has_total() {
        let phases = vec![
            CostPhase {
                label: "Initial run".to_string(),
                model: Some("claude-sonnet-4-6".to_string()),
                cost_usd: 0.031,
                duration_ms: 492_000,
            },
            CostPhase {
                label: "Review #1".to_string(),
                model: Some("claude-haiku-4-5".to_string()),
                cost_usd: 0.002,
                duration_ms: 138_000,
            },
        ];
        let comment = build_cost_comment(&phases);
        assert!(comment.contains("**Total**"));
        assert!(comment.contains("**$0.0330**"));
        assert!(comment.contains("**10m 30s**"));
    }

    #[test]
    fn test_build_cost_comment_default_model() {
        let phases = vec![CostPhase {
            label: "Initial run".to_string(),
            model: None,
            cost_usd: 0.01,
            duration_ms: 60_000,
        }];
        let comment = build_cost_comment(&phases);
        assert!(comment.contains("| default |"));
    }
}
