use anyhow::Result;
use clap::{Parser, Subcommand};

use conductor_core::config::{ensure_dirs, load_config};
use conductor_core::db::open_database;
use conductor_core::github;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager, JiraConfig};
use conductor_core::jira_acli;
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
use conductor_core::session::SessionTracker;
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::WorktreeManager;

#[derive(Parser)]
#[command(name = "conductor", about = "Multi-repo orchestration tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage repositories
    Repo {
        #[command(subcommand)]
        command: RepoCommands,
    },
    /// Manage worktrees
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommands,
    },
    /// Manage tickets
    Tickets {
        #[command(subcommand)]
        command: TicketCommands,
    },
    /// Manage sessions
    Session {
        #[command(subcommand)]
        command: SessionCommands,
    },
}

#[derive(Subcommand)]
enum RepoCommands {
    /// Add a repository
    Add {
        /// Git remote URL
        remote_url: String,
        /// Short slug for the repo
        #[arg(long)]
        slug: Option<String>,
        /// Local path to existing checkout (skips clone)
        #[arg(long)]
        local_path: Option<String>,
        /// Workspace directory for worktrees
        #[arg(long)]
        workspace: Option<String>,
    },
    /// List all repositories
    List,
    /// Remove a repository
    Remove {
        /// Repo slug
        slug: String,
    },
    /// Manage issue sources for a repository
    Sources {
        #[command(subcommand)]
        command: SourceCommands,
    },
}

#[derive(Subcommand)]
enum SourceCommands {
    /// Add an issue source
    Add {
        /// Repo slug
        slug: String,
        /// Source type (github or jira)
        #[arg(long = "type")]
        source_type: String,
        /// JSON config (auto-inferred for github from remote URL if omitted)
        #[arg(long)]
        config: Option<String>,
    },
    /// List issue sources for a repo
    List {
        /// Repo slug
        slug: String,
    },
    /// Remove an issue source
    Remove {
        /// Repo slug
        slug: String,
        /// Source type to remove (github or jira)
        #[arg(long = "type")]
        source_type: String,
    },
}

#[derive(Subcommand)]
enum WorktreeCommands {
    /// Create a new worktree
    Create {
        /// Repo slug
        repo: String,
        /// Worktree name (e.g., smart-playlists, fix-scan-crash)
        name: String,
        /// Base branch
        #[arg(long, short)]
        from: Option<String>,
        /// Link to a ticket ID
        #[arg(long)]
        ticket: Option<String>,
    },
    /// List worktrees
    List {
        /// Filter by repo slug
        repo: Option<String>,
    },
    /// Delete a worktree (soft-delete: marks as merged or abandoned)
    Delete {
        /// Repo slug
        repo: String,
        /// Worktree slug
        name: String,
    },
    /// Permanently remove completed worktree records
    Purge {
        /// Repo slug
        repo: String,
        /// Specific worktree slug (purges all completed if omitted)
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum TicketCommands {
    /// Sync tickets from configured sources
    Sync {
        /// Repo slug (syncs all if omitted)
        repo: Option<String>,
    },
    /// List cached tickets
    List {
        /// Filter by repo slug
        repo: Option<String>,
    },
}

#[derive(Subcommand)]
enum SessionCommands {
    /// Start a new session
    Start,
    /// End the current session
    End {
        /// Postmortem notes
        #[arg(long)]
        notes: Option<String>,
    },
    /// Attach a worktree to the current session
    Attach {
        /// Worktree slug
        worktree: String,
    },
    /// Show the current active session
    Current,
    /// List past sessions
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = load_config()?;
    ensure_dirs(&config)?;

    let db_path = conductor_core::config::db_path();
    let conn = open_database(&db_path)?;

    match cli.command {
        Commands::Repo { command } => match command {
            RepoCommands::Add {
                remote_url,
                slug,
                local_path,
                workspace,
            } => {
                let slug = slug.unwrap_or_else(|| derive_slug_from_url(&remote_url));

                let local = local_path.unwrap_or_else(|| derive_local_path(&config, &slug));

                let mgr = RepoManager::new(&conn, &config);
                let repo = mgr.add(&slug, &local, &remote_url, workspace.as_deref())?;
                println!("Added repo: {} ({})", repo.slug, repo.remote_url);
            }
            RepoCommands::List => {
                let mgr = RepoManager::new(&conn, &config);
                let repos = mgr.list()?;
                if repos.is_empty() {
                    println!("No repos registered. Use `conductor repo add` to add one.");
                } else {
                    for repo in repos {
                        println!("  {}  {}", repo.slug, repo.remote_url);
                    }
                }
            }
            RepoCommands::Remove { slug } => {
                let mgr = RepoManager::new(&conn, &config);
                mgr.remove(&slug)?;
                println!("Removed repo: {slug}");
            }
            RepoCommands::Sources { command } => {
                let repo_mgr = RepoManager::new(&conn, &config);
                let source_mgr = IssueSourceManager::new(&conn);

                match command {
                    SourceCommands::Add {
                        slug,
                        source_type,
                        config: config_json,
                    } => {
                        let repo = repo_mgr.get_by_slug(&slug)?;

                        let config_str = match (source_type.as_str(), config_json) {
                            ("github", Some(json)) => {
                                // Validate it's valid JSON
                                let _: serde_json::Value = serde_json::from_str(&json)
                                    .map_err(|e| anyhow::anyhow!("invalid JSON config: {e}"))?;
                                json
                            }
                            ("github", None) => {
                                // Auto-infer from remote URL
                                let (owner, name) =
                                    github::parse_github_remote(&repo.remote_url).ok_or_else(
                                        || {
                                            anyhow::anyhow!(
                                            "cannot infer GitHub config from remote URL: {}. Use --config to specify manually.",
                                            repo.remote_url
                                        )
                                        },
                                    )?;
                                serde_json::to_string(&GitHubConfig { owner, repo: name })?
                            }
                            ("jira", Some(json)) => {
                                let _: serde_json::Value = serde_json::from_str(&json)
                                    .map_err(|e| anyhow::anyhow!("invalid JSON config: {e}"))?;
                                json
                            }
                            ("jira", None) => {
                                anyhow::bail!(
                                    "--config is required for jira sources (e.g. --config '{{\"jql\":\"project = KEY AND status != Done\",\"url\":\"https://...\"}}')");
                            }
                            _ => {
                                anyhow::bail!(
                                    "unsupported source type: '{}'. Use 'github' or 'jira'.",
                                    source_type
                                );
                            }
                        };

                        let source = source_mgr.add(&repo.id, &source_type, &config_str, &slug)?;
                        println!(
                            "Added {} source for {}: {}",
                            source.source_type, slug, source.config_json
                        );
                    }
                    SourceCommands::List { slug } => {
                        let repo = repo_mgr.get_by_slug(&slug)?;
                        let sources = source_mgr.list(&repo.id)?;
                        if sources.is_empty() {
                            println!("No issue sources configured for {slug}.");
                        } else {
                            for s in sources {
                                println!("  {} — {}", s.source_type, s.config_json);
                            }
                        }
                    }
                    SourceCommands::Remove { slug, source_type } => {
                        let repo = repo_mgr.get_by_slug(&slug)?;
                        let removed = source_mgr.remove_by_type(&repo.id, &source_type)?;
                        if removed {
                            println!("Removed {source_type} source for {slug}");
                        } else {
                            println!("No {source_type} source found for {slug}");
                        }
                    }
                }
            }
        },
        Commands::Worktree { command } => match command {
            WorktreeCommands::Create {
                repo,
                name,
                from,
                ticket,
            } => {
                let mgr = WorktreeManager::new(&conn, &config);
                let wt = mgr.create(&repo, &name, from.as_deref(), ticket.as_deref())?;
                println!("Created worktree: {} ({})", wt.slug, wt.branch);
                println!("  Path: {}", wt.path);
            }
            WorktreeCommands::List { repo } => {
                let mgr = WorktreeManager::new(&conn, &config);
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
                let mgr = WorktreeManager::new(&conn, &config);
                let wt = mgr.delete(&repo, &name)?;
                println!("Worktree {name} marked as {} ✓", wt.status);
            }
            WorktreeCommands::Purge { repo, name } => {
                let mgr = WorktreeManager::new(&conn, &config);
                let count = mgr.purge(&repo, name.as_deref())?;
                if count == 0 {
                    println!("No completed worktrees to purge.");
                } else {
                    println!("Purged {count} completed worktree record(s).");
                }
            }
        },
        Commands::Session { command } => match command {
            SessionCommands::Start => {
                let tracker = SessionTracker::new(&conn);
                if let Some(existing) = tracker.current()? {
                    anyhow::bail!(
                        "Session already active: {} (started {})",
                        existing.id,
                        existing.started_at
                    );
                }
                let session = tracker.start()?;
                println!("Started session: {}", session.id);
            }
            SessionCommands::End { notes } => {
                let tracker = SessionTracker::new(&conn);
                let session = tracker
                    .current()?
                    .ok_or_else(|| anyhow::anyhow!("No active session"))?;
                tracker.end(&session.id, notes.as_deref())?;
                println!("Ended session: {}", session.id);
                if let Some(n) = &notes {
                    println!("  Notes: {n}");
                }
            }
            SessionCommands::Attach { worktree } => {
                let tracker = SessionTracker::new(&conn);
                let session = tracker
                    .current()?
                    .ok_or_else(|| anyhow::anyhow!("No active session"))?;

                // Look up the worktree by slug
                let wt_id: String = conn
                    .query_row(
                        "SELECT id FROM worktrees WHERE slug = ?1",
                        rusqlite::params![worktree],
                        |row| row.get(0),
                    )
                    .map_err(|_| anyhow::anyhow!("Worktree not found: {worktree}"))?;

                tracker.add_worktree(&session.id, &wt_id)?;
                println!("Attached worktree '{worktree}' to session {}", session.id);
            }
            SessionCommands::Current => {
                let tracker = SessionTracker::new(&conn);
                match tracker.current()? {
                    Some(session) => {
                        println!("Session: {}", session.id);
                        println!("  Started: {}", session.started_at);
                        let worktrees = tracker.get_worktrees(&session.id)?;
                        if worktrees.is_empty() {
                            println!("  Worktrees: (none)");
                        } else {
                            println!("  Worktrees:");
                            for wt in worktrees {
                                println!("    {} [{}]", wt.slug, wt.branch);
                            }
                        }
                    }
                    None => {
                        println!("No active session.");
                    }
                }
            }
            SessionCommands::List => {
                let tracker = SessionTracker::new(&conn);
                let sessions = tracker.list()?;
                if sessions.is_empty() {
                    println!("No sessions.");
                } else {
                    for s in sessions {
                        let status = if s.ended_at.is_some() {
                            "ended"
                        } else {
                            "active"
                        };
                        let worktrees = tracker.get_worktrees(&s.id)?;
                        let duration = match &s.ended_at {
                            Some(end) => {
                                if let (Ok(start_dt), Ok(end_dt)) = (
                                    chrono::DateTime::parse_from_rfc3339(&s.started_at),
                                    chrono::DateTime::parse_from_rfc3339(end),
                                ) {
                                    let dur = end_dt - start_dt;
                                    format_duration(dur)
                                } else {
                                    "?".to_string()
                                }
                            }
                            None => {
                                if let Ok(start_dt) =
                                    chrono::DateTime::parse_from_rfc3339(&s.started_at)
                                {
                                    let dur =
                                        chrono::Utc::now() - start_dt.with_timezone(&chrono::Utc);
                                    format!("{} (ongoing)", format_duration(dur))
                                } else {
                                    "?".to_string()
                                }
                            }
                        };
                        print!(
                            "  {}  [{status}]  {}  worktrees: {}",
                            &s.id[..13],
                            duration,
                            worktrees.len(),
                        );
                        if let Some(notes) = &s.notes {
                            print!("  — {notes}");
                        }
                        println!();
                    }
                }
            }
        },
        Commands::Tickets { command } => match command {
            TicketCommands::Sync { repo } => {
                let repo_mgr = RepoManager::new(&conn, &config);
                let repos = if let Some(slug) = repo {
                    vec![repo_mgr.get_by_slug(&slug)?]
                } else {
                    repo_mgr.list()?
                };

                let syncer = TicketSyncer::new(&conn);
                let source_mgr = IssueSourceManager::new(&conn);

                for r in repos {
                    let sources = source_mgr.list(&r.id)?;

                    if sources.is_empty() {
                        // Backward compat: auto-detect GitHub from remote_url
                        if let Some((owner, name)) = github::parse_github_remote(&r.remote_url) {
                            sync_github(&syncer, &r.id, &r.slug, &owner, &name);
                        }
                    } else {
                        for source in sources {
                            match source.source_type.as_str() {
                                "github" => {
                                    match serde_json::from_str::<GitHubConfig>(&source.config_json)
                                    {
                                        Ok(cfg) => {
                                            sync_github(
                                                &syncer, &r.id, &r.slug, &cfg.owner, &cfg.repo,
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
                                            sync_jira(&syncer, &r.id, &r.slug, &cfg.jql, &cfg.url);
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
                let repo_mgr = RepoManager::new(&conn, &config);
                let repo_id = if let Some(slug) = &repo {
                    Some(repo_mgr.get_by_slug(slug)?.id)
                } else {
                    None
                };

                let syncer = TicketSyncer::new(&conn);
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
        },
    }

    Ok(())
}

fn format_duration(dur: chrono::Duration) -> String {
    let total_secs = dur.num_seconds();
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

/// Sync Jira issues for a single repo, printing results.
fn sync_jira(syncer: &TicketSyncer, repo_id: &str, repo_slug: &str, jql: &str, base_url: &str) {
    match jira_acli::sync_jira_issues_acli(jql, base_url) {
        Ok(tickets) => {
            let synced_ids: Vec<&str> = tickets.iter().map(|t| t.source_id.as_str()).collect();
            match syncer.upsert_tickets(repo_id, &tickets) {
                Ok(count) => {
                    let closed = syncer
                        .close_missing_tickets(repo_id, "jira", &synced_ids)
                        .unwrap_or(0);
                    let merged = syncer
                        .mark_worktrees_for_closed_tickets(repo_id)
                        .unwrap_or(0);
                    print!("  {} — synced {count} Jira issues", repo_slug);
                    if closed > 0 {
                        print!(", {closed} marked closed");
                    }
                    if merged > 0 {
                        print!(", {merged} worktrees merged");
                    }
                    println!();
                }
                Err(e) => {
                    eprintln!("  {} — sync failed: {e}", repo_slug);
                }
            }
        }
        Err(e) => {
            eprintln!("  {} — sync failed: {e}", repo_slug);
        }
    }
}

/// Sync GitHub issues for a single repo, printing results.
fn sync_github(syncer: &TicketSyncer, repo_id: &str, repo_slug: &str, owner: &str, name: &str) {
    match github::sync_github_issues(owner, name) {
        Ok(tickets) => {
            let synced_ids: Vec<&str> = tickets.iter().map(|t| t.source_id.as_str()).collect();
            match syncer.upsert_tickets(repo_id, &tickets) {
                Ok(count) => {
                    let closed = syncer
                        .close_missing_tickets(repo_id, "github", &synced_ids)
                        .unwrap_or(0);
                    let merged = syncer
                        .mark_worktrees_for_closed_tickets(repo_id)
                        .unwrap_or(0);
                    print!("  {} — synced {count} GitHub issues", repo_slug);
                    if closed > 0 {
                        print!(", {closed} marked closed");
                    }
                    if merged > 0 {
                        print!(", {merged} worktrees merged");
                    }
                    println!();
                }
                Err(e) => {
                    eprintln!("  {} — sync failed: {e}", repo_slug);
                }
            }
        }
        Err(e) => {
            eprintln!("  {} — sync failed: {e}", repo_slug);
        }
    }
}
