use anyhow::Result;
use rusqlite::Connection;

use conductor_core::agent::AgentManager;
use conductor_core::config::Config;
use conductor_core::github;
use conductor_core::github_app;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager, JiraConfig};
use conductor_core::jira_acli;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::WorktreeManager;

use crate::commands::TicketCommands;
use crate::helpers::{sync_repo, truncate_str};

pub fn handle_tickets(command: TicketCommands, conn: &Connection, config: &Config) -> Result<()> {
    match command {
        TicketCommands::Sync { repo } => {
            let repo_mgr = RepoManager::new(conn, config);
            let repos = if let Some(slug) = repo {
                vec![repo_mgr.get_by_slug(&slug)?]
            } else {
                repo_mgr.list()?
            };

            let syncer = TicketSyncer::new(conn);
            let source_mgr = IssueSourceManager::new(conn);
            let token_res = github_app::resolve_app_token(config, "github-issues-sync");
            let token = token_res.token();

            for r in repos {
                let sources = source_mgr.list(&r.id)?;

                if sources.is_empty() {
                    // Backward compat: auto-detect GitHub from remote_url
                    if let Some((owner, name)) = github::parse_github_remote(&r.remote_url) {
                        sync_repo(&syncer, &r.id, &r.slug, "github", "GitHub issues", || {
                            github::sync_github_issues(&owner, &name, token)
                        });
                    }
                } else {
                    for source in sources {
                        match source.source_type.as_str() {
                            "github" => {
                                match serde_json::from_str::<GitHubConfig>(&source.config_json) {
                                    Ok(cfg) => {
                                        sync_repo(
                                            &syncer,
                                            &r.id,
                                            &r.slug,
                                            "github",
                                            "GitHub issues",
                                            || {
                                                github::sync_github_issues(
                                                    &cfg.owner, &cfg.repo, token,
                                                )
                                            },
                                        );
                                    }
                                    Err(e) => {
                                        eprintln!("  {} — invalid github config: {e}", r.slug);
                                    }
                                }
                            }
                            "jira" => {
                                match serde_json::from_str::<JiraConfig>(&source.config_json) {
                                    Ok(cfg) => {
                                        sync_repo(
                                            &syncer,
                                            &r.id,
                                            &r.slug,
                                            "jira",
                                            "Jira issues",
                                            || jira_acli::sync_jira_issues_acli(&cfg.jql, &cfg.url),
                                        );
                                    }
                                    Err(e) => {
                                        eprintln!("  {} — invalid jira config: {e}", r.slug);
                                    }
                                }
                            }
                            other => {
                                eprintln!("  {} — unknown source type: {other}", r.slug);
                            }
                        }
                    }
                }
            }
        }
        TicketCommands::List { repo } => {
            let repo_mgr = RepoManager::new(conn, config);
            let repo_id = if let Some(slug) = &repo {
                Some(repo_mgr.get_by_slug(slug)?.id)
            } else {
                None
            };

            let syncer = TicketSyncer::new(conn);
            let tickets = syncer.list(repo_id.as_deref())?;
            if tickets.is_empty() {
                println!("No tickets. Run `conductor tickets sync` first.");
            } else {
                for t in tickets {
                    println!(
                        "  {} #{} — {} [{}]",
                        t.source_type, t.source_id, t.title, t.state
                    );
                }
            }
        }
        TicketCommands::Link {
            ticket,
            repo,
            worktree,
        } => {
            let repo_mgr = RepoManager::new(conn, config);
            let r = repo_mgr.get_by_slug(&repo)?;

            let syncer = TicketSyncer::new(conn);
            let t = syncer
                .get_by_source_id(&r.id, &ticket)
                .map_err(|e| anyhow::anyhow!("Ticket not found: #{ticket}: {e}"))?;
            let ticket_id = t.id;

            let wt_mgr = WorktreeManager::new(conn, config);
            let wt = wt_mgr
                .get_by_slug(&r.id, &worktree)
                .map_err(|e| anyhow::anyhow!("Worktree not found: {worktree}: {e}"))?;

            if wt.ticket_id.is_some() {
                anyhow::bail!("Worktree '{worktree}' already has a linked ticket");
            }

            let worktree_id = wt.id;

            let syncer = TicketSyncer::new(conn);
            syncer.link_to_worktree(&ticket_id, &worktree_id)?;
            println!("Linked ticket #{ticket} to worktree '{worktree}'");
        }
        TicketCommands::Delete {
            repo,
            source_type,
            source_id,
        } => {
            let repo_obj = RepoManager::new(conn, config).get_by_slug(&repo)?;
            let syncer = TicketSyncer::new(conn);
            syncer.delete_ticket(&repo_obj.id, &source_type, &source_id)?;
            println!(
                "Deleted ticket {}#{} from {}.",
                source_type, source_id, repo
            );
        }
        TicketCommands::Upsert {
            repo,
            source_type,
            source_id,
            title,
            state,
            body,
            url,
            labels,
            assignee,
            priority,
        } => {
            let repo_obj = RepoManager::new(conn, config).get_by_slug(&repo)?;

            let labels_vec: Vec<String> = labels
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            let ticket_input = conductor_core::tickets::TicketInput {
                source_type: source_type.clone(),
                source_id: source_id.clone(),
                title,
                body,
                state,
                labels: labels_vec,
                label_details: vec![],
                assignee,
                priority,
                url,
                raw_json: "{}".to_string(),
            };

            let syncer = TicketSyncer::new(conn);
            syncer.upsert_tickets(&repo_obj.id, &[ticket_input])?;
            println!(
                "Upserted ticket {}#{} into {}.",
                source_type, source_id, repo
            );
        }
        TicketCommands::Stats { repo } => {
            let repo_mgr = RepoManager::new(conn, config);
            let repo_id = if let Some(slug) = &repo {
                Some(repo_mgr.get_by_slug(slug)?.id)
            } else {
                None
            };

            let syncer = TicketSyncer::new(conn);
            let tickets = syncer.list(repo_id.as_deref())?;
            let agent_mgr = AgentManager::new(conn);
            let totals = agent_mgr.totals_by_ticket_all()?;

            let mut found = false;
            for t in &tickets {
                if let Some(stats) = totals.get(&t.id) {
                    found = true;
                    let dur_secs = stats.total_duration_ms as f64 / 1000.0;
                    let mins = (dur_secs / 60.0) as i64;
                    let secs = (dur_secs % 60.0) as i64;
                    println!(
                        "  #{:<6} {:<40} {} turns  {}m{:02}s  ({} runs)",
                        t.source_id,
                        truncate_str(&t.title, 40),
                        stats.total_turns,
                        mins,
                        secs,
                        stats.total_runs,
                    );
                }
            }
            if !found {
                println!("No agent stats. Run agents on ticket-linked worktrees first.");
            }
        }
    }
    Ok(())
}
