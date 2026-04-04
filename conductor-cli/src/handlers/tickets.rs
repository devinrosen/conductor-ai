use anyhow::Result;
use rusqlite::Connection;

use conductor_core::agent::AgentManager;
use conductor_core::config::Config;
use conductor_core::github;
use conductor_core::github_app;
use conductor_core::issue_source::IssueSourceManager;
use conductor_core::repo::RepoManager;
use conductor_core::ticket_source::TicketSource;
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
                        match TicketSource::from_issue_source(&source) {
                            Ok(ts) => {
                                let ts = ts.with_repo_slug(&r.slug);
                                let label = match ts.source_type_str() {
                                    "github" => "GitHub issues",
                                    "jira" => "Jira issues",
                                    "vantage" => "Vantage deliverables",
                                    other => other,
                                };
                                sync_repo(
                                    &syncer,
                                    &r.id,
                                    &r.slug,
                                    ts.source_type_str(),
                                    label,
                                    || ts.sync(token),
                                );
                            }
                            Err(e) => {
                                eprintln!("  {} — {e}", r.slug);
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
        TicketCommands::Get { id, json, format } => {
            let syncer = TicketSyncer::new(conn);
            let ticket = syncer.get_by_id(&id).or_else(|_| {
                // Fall back to searching by source_id across all repos
                let all = syncer.list(None)?;
                all.into_iter()
                    .find(|t| t.source_id == id)
                    .ok_or_else(|| anyhow::anyhow!("Ticket not found: {id}"))
            })?;

            if json || format == "json" {
                println!("{}", serde_json::to_string_pretty(&ticket)?);
            } else {
                println!("ID:         {}", ticket.id);
                println!("Source:     {} #{}", ticket.source_type, ticket.source_id);
                println!("Title:      {}", ticket.title);
                println!("State:      {}", ticket.state);
                if !ticket.body.is_empty() {
                    println!("Body:       {}", truncate_str(&ticket.body, 200));
                }
                if !ticket.url.is_empty() {
                    println!("URL:        {}", ticket.url);
                }
                if let Some(ref a) = ticket.assignee {
                    println!("Assignee:   {a}");
                }
                if let Some(ref p) = ticket.priority {
                    println!("Priority:   {p}");
                }
                if !ticket.labels.is_empty() {
                    println!("Labels:     {}", ticket.labels);
                }
                if let Some(ref wf) = ticket.workflow {
                    println!("Workflow:   {wf}");
                }
                if let Some(ref am) = ticket.agent_map {
                    println!("Agent Map:  {am}");
                }
            }
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
            workflow,
            agent_map,
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

            // Apply workflow/agent_map routing if specified
            if workflow.is_some() || agent_map.is_some() {
                let ticket = syncer.get_by_source_id(&repo_obj.id, &source_id)?;
                syncer.update_ticket(
                    &ticket.id,
                    None,
                    workflow.as_deref(),
                    agent_map.as_deref(),
                )?;
            }
            println!(
                "Upserted ticket {}#{} into {}.",
                source_type, source_id, repo
            );
        }
        TicketCommands::Update {
            id,
            state,
            workflow,
            agent_map,
        } => {
            let syncer = TicketSyncer::new(conn);
            let _ticket = syncer.get_by_id(&id)?;

            syncer.update_ticket(
                &id,
                state.as_deref(),
                workflow.as_deref(),
                agent_map.as_deref(),
            )?;

            if let Some(ref new_state) = state {
                println!("Updated ticket {} state to '{}'.", id, new_state);
            }
            if let Some(ref w) = workflow {
                if w.is_empty() {
                    println!("Cleared ticket {} workflow.", id);
                } else {
                    println!("Set ticket {} workflow to '{}'.", id, w);
                }
            }
            if let Some(ref a) = agent_map {
                if a.is_empty() {
                    println!("Cleared ticket {} agent_map.", id);
                } else {
                    println!("Set ticket {} agent_map.", id);
                }
            }
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
