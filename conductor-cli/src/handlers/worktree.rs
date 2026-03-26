use anyhow::Result;
use rusqlite::Connection;

use conductor_core::agent::AgentManager;
use conductor_core::config::Config;
use conductor_core::feature::FeatureManager;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::{build_agent_prompt, TicketSyncer};
use conductor_core::worktree::WorktreeManager;

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
        let _ = wt_mgr.reap_stale_worktrees();
    }
    match command {
        WorktreeCommands::Create {
            repo,
            name,
            from,
            from_pr,
            feature,
            ticket,
            auto_agent,
        } => {
            // --feature resolves to --from <feature-branch>
            let effective_from = if let Some(ref feat_name) = feature {
                let feat_mgr = FeatureManager::new(conn, config);
                let f = feat_mgr.get_by_name(&repo, feat_name)?;
                Some(f.branch)
            } else {
                from
            };

            let mgr = WorktreeManager::new(conn, config);
            let (wt, warnings) = mgr.create(
                &repo,
                &name,
                effective_from.as_deref(),
                ticket.as_deref(),
                from_pr,
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
                            let run = agent_mgr.create_run(
                                Some(&wt.id),
                                &prompt,
                                Some(&wt.slug),
                                model,
                            )?;
                            run_agent(conn, &run.id, &wt.path, &prompt, None, model, None, None)?;
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
    }
    Ok(())
}
