use std::process::Command;

use rusqlite::Connection;

use super::manager::AgentManager;
use super::status::{AgentRunStatus, FeedbackStatus};
use crate::config::Config;
use crate::tickets::TicketSyncer;
use crate::worktree::WorktreeManager;

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
    config: &Config,
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
    let wt_mgr = WorktreeManager::new(conn, config);
    let worktree = wt_mgr.get_by_id(wt_id).ok();
    let branch = worktree.as_ref().map(|w| w.branch.clone());

    if let Some(ref branch) = branch {
        sections.push(format!("**Worktree:** {branch}"));
    }

    // 2. Linked ticket
    let ticket_info = worktree
        .as_ref()
        .and_then(|w| w.ticket_id.as_deref())
        .and_then(|tid| TicketSyncer::new(conn).get_by_id(tid).ok())
        .map(|t| (t.source_id, t.title));

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::manager::AgentManager;
    use crate::agent::status::{AgentRunStatus, StepStatus};
    use crate::agent::types::PlanStep;
    use crate::config::Config;

    fn setup_conn() -> rusqlite::Connection {
        crate::agent::manager::setup_db()
    }

    #[test]
    fn test_build_startup_context_ephemeral_no_commits() {
        let conn = setup_conn();
        // Ephemeral run (no worktree_id). Use a non-git path so git_recent_commits
        // returns empty — only the feedback protocol section should be present.
        let ctx = build_startup_context(&conn, &Config::default(), None, "run-1", "/tmp");
        assert!(
            ctx.contains("[NEEDS_FEEDBACK]"),
            "feedback protocol must be included"
        );
        // No worktree/ticket sections for ephemeral runs.
        assert!(!ctx.contains("**Worktree:**"));
        assert!(!ctx.contains("**Ticket:**"));
    }

    #[test]
    fn test_build_startup_context_with_worktree_no_ticket() {
        let conn = setup_conn();
        let mgr = AgentManager::new(&conn);
        let run = mgr.create_run(Some("w1"), "initial prompt", None).unwrap();

        // "w1" is the test worktree with branch "feat/test" (from setup_db).
        let ctx = build_startup_context(&conn, &Config::default(), Some("w1"), &run.id, "/tmp");
        assert!(ctx.contains("**Worktree:** feat/test"));
        assert!(ctx.contains("[NEEDS_FEEDBACK]"));
        // No ticket linked in the test fixture.
        assert!(!ctx.contains("**Ticket:**"));
    }

    #[test]
    fn test_build_startup_context_includes_prior_run_outcome() {
        let conn = setup_conn();
        let mgr = AgentManager::new(&conn);

        // Create and complete a prior run.
        let prior = mgr.create_run(Some("w1"), "do the thing", None).unwrap();
        mgr.update_run_completed(
            &prior.id,
            None,
            Some("Prior run finished successfully."),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        // Create a new (current) run.
        let current = mgr.create_run(Some("w1"), "follow-up", None).unwrap();
        assert_eq!(current.status, AgentRunStatus::Running);

        let ctx = build_startup_context(&conn, &Config::default(), Some("w1"), &current.id, "/tmp");
        assert!(
            ctx.contains("Prior run finished successfully."),
            "context must include prior run result"
        );
    }

    #[test]
    fn test_build_startup_context_excludes_current_run_from_prior() {
        let conn = setup_conn();
        let mgr = AgentManager::new(&conn);

        // Only one run exists — the current one. No prior run section should appear.
        let run = mgr.create_run(Some("w1"), "only run", None).unwrap();
        let ctx = build_startup_context(&conn, &Config::default(), Some("w1"), &run.id, "/tmp");
        assert!(
            !ctx.contains("**Prior run outcome"),
            "current run must not appear as prior"
        );
    }

    #[test]
    fn test_startup_context_includes_ticket() {
        let conn = setup_conn();

        // Insert a ticket and link it to worktree w1
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'github', '42', 'Fix payment bug', 'Description', 'open', '[]', '', '2024-01-01T00:00:00Z', '{}')",
            [],
        ).unwrap();
        conn.execute("UPDATE worktrees SET ticket_id = 't1' WHERE id = 'w1'", [])
            .unwrap();

        let mgr = AgentManager::new(&conn);
        let current = mgr.create_run(Some("w1"), "Fix it", None).unwrap();

        let ctx = build_startup_context(&conn, &Config::default(), Some("w1"), &current.id, "/tmp");
        assert!(ctx.contains("**Ticket:** #42 — Fix payment bug"));
    }

    #[test]
    fn test_startup_context_includes_prior_plan_steps() {
        let conn = setup_conn();
        let mgr = AgentManager::new(&conn);

        // Create a prior completed run with a plan
        let prior = mgr.create_run(Some("w1"), "Prior work", None).unwrap();
        let steps = vec![
            PlanStep {
                description: "Read the code".to_string(),
                done: true,
                status: StepStatus::Completed,
                ..Default::default()
            },
            PlanStep {
                description: "Write tests".to_string(),
                done: true,
                status: StepStatus::Completed,
                ..Default::default()
            },
            PlanStep {
                description: "Implement feature".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&prior.id, &steps).unwrap();
        mgr.update_run_completed(
            &prior.id,
            None,
            Some("All done"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        // Create current run
        let current = mgr.create_run(Some("w1"), "Continue work", None).unwrap();

        let ctx = build_startup_context(&conn, &Config::default(), Some("w1"), &current.id, "/tmp");
        assert!(ctx.contains("**Plan steps (from prior run):**"));
        assert!(ctx.contains("1. ✅ Read the code"));
        assert!(ctx.contains("2. ✅ Write tests"));
        assert!(ctx.contains("3. ⏳ Implement feature"));
    }

    #[test]
    fn test_startup_context_truncates_long_result() {
        let conn = setup_conn();
        let mgr = AgentManager::new(&conn);

        // Create a prior run with a very long result
        let prior = mgr.create_run(Some("w1"), "Prior task", None).unwrap();
        let long_result = "x".repeat(1000);
        mgr.update_run_completed(
            &prior.id,
            None,
            Some(&long_result),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let current = mgr.create_run(Some("w1"), "Next", None).unwrap();

        let ctx = build_startup_context(&conn, &Config::default(), Some("w1"), &current.id, "/tmp");
        assert!(ctx.contains("**Prior run outcome (completed):**"));
        // Should be truncated to 500 chars + ellipsis
        assert!(ctx.contains(&"x".repeat(500)));
        assert!(ctx.contains('…'));
        assert!(!ctx.contains(&"x".repeat(501)));
    }
}
