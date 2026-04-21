use anyhow::Result;
use rusqlite::Connection;

use conductor_core::agent::AgentManager;
use conductor_core::config::Config;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::{build_agent_prompt, TicketSyncer};
use conductor_core::worktree::{WorktreeCreateOptions, WorktreeManager};

use crate::commands::WorktreeCommands;
use crate::handlers::agent::run_agent;

pub fn handle_worktree(
    command: WorktreeCommands,
    conn: &Connection,
    config: &Config,
) -> Result<()> {
    // Reap stale worktrees before handling any worktree command.
    {
        let wt_mgr = WorktreeManager::new(conn, config);
        if let Err(e) = wt_mgr.reap_stale_worktrees() {
            eprintln!("Warning: reap_stale_worktrees failed: {e}");
        }
    }
    match command {
        WorktreeCommands::Create {
            repo,
            name,
            from,
            from_pr,
            ticket,
            auto_agent,
            force,
        } => {
            let effective_from = from;

            let mgr = WorktreeManager::new(conn, config);

            // Run health check before creation (skip for --from-pr paths since
            // staleness is irrelevant; dirty check still applies).
            let (force_dirty, pre_health) = if force {
                (true, None)
            } else if from_pr.is_none() {
                let health = mgr.check_main_health(&repo, effective_from.as_deref())?;
                if health.is_dirty {
                    eprintln!("Warning: base branch has uncommitted changes:");
                    for f in &health.dirty_files {
                        eprintln!("  {f}");
                    }
                    eprint!("Proceed anyway? [y/N] ");
                    use std::io::BufRead;
                    let mut input = String::new();
                    std::io::stdin().lock().read_line(&mut input)?;
                    if !input.trim().eq_ignore_ascii_case("y") {
                        eprintln!("Aborted.");
                        return Ok(());
                    }
                    (true, None)
                } else {
                    if health.commits_behind > 0 {
                        eprintln!(
                            "Info: base branch is {} commit(s) behind origin (will fast-forward)",
                            health.commits_behind
                        );
                    }
                    (false, Some(health))
                }
            } else {
                (false, None)
            };

            let (wt, warnings) = mgr.create(
                &repo,
                &name,
                WorktreeCreateOptions {
                    from_branch: effective_from,
                    ticket_id: ticket.clone(),
                    from_pr,
                    force_dirty,
                    pre_health,
                },
            )?;

            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
            println!("Created worktree: {} ({})", wt.slug, wt.branch);
            println!("  Path: {}", wt.path);

            if auto_agent {
                if let Some(ref tid) = ticket {
                    let syncer = TicketSyncer::new(conn);
                    match syncer.get_by_id(tid) {
                        Ok(t) => {
                            let prompt = build_agent_prompt(&t);
                            println!("Starting agent...");
                            // Resolve model: per-worktree → per-repo config → global config
                            let repo_mgr = RepoManager::new(conn, config);
                            let repo_model = repo_mgr.get_by_slug(&repo).ok().and_then(|r| r.model);
                            let resolved_model = conductor_core::models::resolve_model(
                                wt.model.as_deref(),
                                repo_model.as_deref(),
                                config.general.model.as_deref(),
                            );
                            let model = resolved_model.as_deref();
                            let agent_mgr = AgentManager::new(conn);
                            let run = agent_mgr.create_run(Some(&wt.id), &prompt, None, model)?;
                            run_agent(
                                conn,
                                &run.id,
                                &wt.path,
                                &prompt,
                                None,
                                model,
                                None,
                                None,
                                &[],
                            )?;
                        }
                        Err(e) => {
                            eprintln!("Warning: could not load ticket for agent prompt: {e}");
                        }
                    }
                } else {
                    eprintln!("Warning: --auto-agent requires --ticket to be set");
                }
            }
        }
        WorktreeCommands::List { repo } => {
            let mgr = WorktreeManager::new(conn, config);
            let worktrees = mgr.list(repo.as_deref(), false)?;
            if worktrees.is_empty() {
                println!("No worktrees.");
            } else {
                for wt in worktrees {
                    println!("  {}  {}  [{}]", wt.slug, wt.branch, wt.status);
                }
            }
        }
        WorktreeCommands::Delete { repo, name } => {
            let mgr = WorktreeManager::new(conn, config);
            let wt = mgr.delete(&repo, &name)?;
            println!("Worktree {name} marked as {} ✓", wt.status);
        }
        WorktreeCommands::Purge { repo, name } => {
            let mgr = WorktreeManager::new(conn, config);
            let count = mgr.purge(&repo, name.as_deref())?;
            if count == 0 {
                println!("No completed worktrees to purge.");
            } else {
                println!("Purged {count} completed worktree record(s).");
            }
        }
        WorktreeCommands::Push { repo, name } => {
            let mgr = WorktreeManager::new(conn, config);
            let msg = mgr.push(&repo, &name)?;
            println!("{msg}");
        }
        WorktreeCommands::Pr { repo, name, draft } => {
            let mgr = WorktreeManager::new(conn, config);
            let url = mgr.create_pr(&repo, &name, draft)?;
            println!("PR created: {url}");
        }
        WorktreeCommands::SetModel { repo, name, model } => {
            let mgr = WorktreeManager::new(conn, config);
            mgr.set_model(&repo, &name, model.as_deref())?;
            match model {
                Some(m) => println!("Set model for {name} to: {m}"),
                None => {
                    println!("Cleared model override for {name} (will use global default)")
                }
            }
        }
        WorktreeCommands::Cleanup { repo } => {
            let mgr = WorktreeManager::new(conn, config);
            let count = mgr.cleanup_merged_worktrees(repo.as_deref())?;
            if count == 0 {
                println!("No merged worktrees found to clean up.");
            } else {
                println!("Cleaned up {count} merged worktree(s).");
            }
        }
        WorktreeCommands::SetBaseBranch {
            repo,
            name,
            base_branch,
            rebase,
        } => {
            let mgr = WorktreeManager::new(conn, config);
            mgr.set_base_branch(&repo, &name, base_branch.as_deref(), rebase)?;
            match base_branch {
                Some(b) => println!("Base branch for {name} set to: {b}"),
                None => println!("Base branch for {name} cleared (will use repo default)"),
            }
        }
        WorktreeCommands::CreateStack {
            repo,
            root_branch,
            tickets,
        } => {
            if tickets.is_empty() {
                return Err(anyhow::anyhow!(
                    "--tickets must specify at least one ticket ID."
                ));
            }
            let mgr = WorktreeManager::new(conn, config);
            let results = mgr.create_from_dep_graph(&repo, &root_branch, &tickets)?;
            for (wt, warnings) in &results {
                for warning in warnings {
                    eprintln!("warning: {warning}");
                }
                println!("Created worktree: {} ({})", wt.slug, wt.branch);
                println!("  Path: {}", wt.path);
            }
            println!("Stack of {} worktree(s) created.", results.len());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use conductor_core::worktree::WorktreeManager;

    #[test]
    fn test_set_base_branch_clear_succeeds() {
        let conn = conductor_core::test_helpers::setup_db();
        let config = conductor_core::config::Config::default();
        let mgr = WorktreeManager::new(&conn, &config);
        // Clearing (None) requires no git ops — always succeeds on a DB-only path.
        let result = mgr.set_base_branch("test-repo", "feat-test", None, false);
        assert!(result.is_ok(), "clear should succeed: {result:?}");
    }

    #[test]
    fn test_set_base_branch_rejects_dash_branch() {
        let conn = conductor_core::test_helpers::setup_db();
        let config = conductor_core::config::Config::default();
        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.set_base_branch("test-repo", "feat-test", Some("--bad"), false);
        assert!(
            result.is_err(),
            "dash-prefixed branch name should be rejected: {result:?}"
        );
    }

    #[test]
    fn test_set_base_branch_rebase_flag_forwarded() {
        let conn = conductor_core::test_helpers::setup_db();
        let config = conductor_core::config::Config::default();
        let mgr = WorktreeManager::new(&conn, &config);
        // With rebase=true and a non-existent ref the ancestry check will error — the
        // important thing is the rebase path is reached (error is NOT the "pass rebase=true" hint).
        let result = mgr.set_base_branch("test-repo", "feat-test", Some("release/v1"), true);
        assert!(result.is_err(), "expected error for non-existent ref: {result:?}");
        let msg = result.unwrap_err().to_string();
        assert!(
            !msg.contains("Pass rebase=true"),
            "rebase flag should be forwarded, not prompt user to set it: {msg}"
        );
    }
}
