use std::process::Command;

use rusqlite::{params, Connection};

use super::manager::AgentManager;
use super::status::{AgentRunStatus, FeedbackStatus};

/// Prefix used for the parent run prompt when launching a PR review swarm.
pub const PR_REVIEW_SWARM_PROMPT_PREFIX: &str = "PR review swarm";

/// Standard feedback protocol appended to every agent prompt.
///
/// Defined once here so both the ephemeral and worktree code paths use the
/// exact same wording — a single edit propagates everywhere.
const FEEDBACK_PROTOCOL: &str = "**Feedback protocol:** If you need human input to continue \
     (e.g. a decision, clarification, or approval), output \
     `[NEEDS_FEEDBACK] <your question>` as a standalone line. \
     The conductor will pause your run and surface the question to \
     the user. When they respond, your run will resume with their answer.";

/// Run `git log --oneline -10` in `worktree_path` and return the commit lines.
///
/// Returns an empty `Vec` when git is unavailable, the directory is not a
/// repository, or the output contains invalid UTF-8.  Uses `from_utf8_lossy`
/// so partial output is never silently discarded.
fn git_recent_commits(worktree_path: &str) -> Vec<String> {
    Command::new("git")
        .args(["log", "--oneline", "-10"])
        .current_dir(worktree_path)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Build a startup context block to prepend to the agent prompt.
///
/// Pulls worktree info, linked ticket, prior run plans, recent commits,
/// and prior run summaries from the database. Always includes the feedback
/// protocol so agents know how to request human input mid-run.
pub fn build_startup_context(
    conn: &Connection,
    worktree_id: Option<&str>,
    current_run_id: &str,
    worktree_path: &str,
) -> String {
    let mut sections = Vec::new();

    // For ephemeral runs (no worktree), skip worktree-specific context
    let Some(wt_id) = worktree_id else {
        // Still include commits + feedback protocol below
        let commit_lines = git_recent_commits(worktree_path);
        if !commit_lines.is_empty() {
            let formatted: Vec<String> = commit_lines.iter().map(|l| format!("- {l}")).collect();
            sections.push(format!("**Recent commits:**\n{}", formatted.join("\n")));
        }
        sections.push(FEEDBACK_PROTOCOL.to_string());
        return sections.join("\n\n---\n\n");
    };

    // 1. Worktree branch
    let branch: Option<String> = conn
        .query_row(
            "SELECT branch FROM worktrees WHERE id = ?1",
            params![wt_id],
            |row| row.get(0),
        )
        .ok();

    if let Some(ref branch) = branch {
        sections.push(format!("**Worktree:** {branch}"));
    }

    // 2. Linked ticket
    let ticket_info: Option<(String, String)> = conn
        .query_row(
            "SELECT t.source_id, t.title FROM tickets t \
             JOIN worktrees w ON w.ticket_id = t.id \
             WHERE w.id = ?1",
            params![wt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();

    if let Some((source_id, title)) = ticket_info {
        sections.push(format!("**Ticket:** #{source_id} — {title}"));
    }

    // 3. Prior runs (excluding the current run being started)
    let mgr = AgentManager::new(conn);
    if let Ok(runs) = mgr.list_for_worktree(wt_id) {
        let prior_runs: Vec<_> = runs.iter().filter(|r| r.id != current_run_id).collect();

        // Plan steps from the most recent run that has a plan
        if let Some(run_with_plan) = prior_runs.iter().find(|r| r.plan.is_some()) {
            if let Some(ref plan) = run_with_plan.plan {
                let plan_lines: Vec<String> = plan
                    .iter()
                    .enumerate()
                    .map(|(i, step)| {
                        let marker = if step.done { "✅" } else { "⏳" };
                        format!("{}. {} {}", i + 1, marker, step.description)
                    })
                    .collect();
                if !plan_lines.is_empty() {
                    sections.push(format!(
                        "**Plan steps (from prior run):**\n{}",
                        plan_lines.join("\n")
                    ));
                }
            }
        }

        // Prior run summary (from last completed or failed run)
        if let Some(last_run) = prior_runs
            .iter()
            .find(|r| r.status == AgentRunStatus::Completed || r.status == AgentRunStatus::Failed)
        {
            if let Some(ref result) = last_run.result_text {
                let truncated = crate::text_util::cap_with_suffix(result, 500, "…");
                sections.push(format!(
                    "**Prior run outcome ({}):** {}",
                    last_run.status, truncated
                ));
            }

            // Include feedback Q&A from the prior run
            if let Ok(feedback_list) = mgr.list_feedback_for_run(&last_run.id) {
                let responded: Vec<_> = feedback_list
                    .iter()
                    .filter(|f| f.status == FeedbackStatus::Responded && f.response.is_some())
                    .collect();
                if !responded.is_empty() {
                    let lines: Vec<String> = responded
                        .iter()
                        .map(|f| {
                            format!(
                                "- **Q:** {}\n  **A:** {}",
                                f.prompt,
                                f.response.as_deref().unwrap_or("")
                            )
                        })
                        .collect();
                    sections.push(format!(
                        "**Feedback from prior run:**\n{}",
                        lines.join("\n")
                    ));
                }
            }
        }
    }

    // 4. Recent commits via git log
    let commit_lines = git_recent_commits(worktree_path);
    if !commit_lines.is_empty() {
        let formatted: Vec<String> = commit_lines.iter().map(|l| format!("- {l}")).collect();
        sections.push(format!(
            "**Recent commits in this worktree:**\n{}",
            formatted.join("\n")
        ));
    }

    // Always include the feedback protocol so agents know how to request input.
    sections.push(FEEDBACK_PROTOCOL.to_string());

    format!("## Session Context\n\n{}", sections.join("\n\n"))
}
