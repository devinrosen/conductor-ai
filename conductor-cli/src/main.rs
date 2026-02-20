use anyhow::Result;
use clap::{Parser, Subcommand};

use conductor_core::config::{ensure_dirs, load_config};
use conductor_core::db::open_database;
use conductor_core::github;
use conductor_core::repo::RepoManager;
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
    /// Delete a worktree
    Delete {
        /// Repo slug
        repo: String,
        /// Worktree slug
        name: String,
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
                let slug = slug.unwrap_or_else(|| {
                    // Derive slug from remote URL
                    remote_url
                        .rsplit('/')
                        .next()
                        .unwrap_or("repo")
                        .strip_suffix(".git")
                        .unwrap_or("repo")
                        .to_string()
                });

                let local = local_path.unwrap_or_else(|| {
                    config
                        .general
                        .workspace_root
                        .join(&slug)
                        .join("main")
                        .to_string_lossy()
                        .to_string()
                });

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
                let worktrees = mgr.list(repo.as_deref())?;
                if worktrees.is_empty() {
                    println!("No worktrees.");
                } else {
                    for wt in worktrees {
                        println!(
                            "  {}  {}  [{}]",
                            wt.slug, wt.branch, wt.status
                        );
                    }
                }
            }
            WorktreeCommands::Delete { repo, name } => {
                let mgr = WorktreeManager::new(&conn, &config);
                mgr.delete(&repo, &name)?;
                println!("Deleted worktree: {name}");
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
                for r in repos {
                    if let Some((owner, name)) = github::parse_github_remote(&r.remote_url) {
                        match github::sync_github_issues(&owner, &name) {
                            Ok(tickets) => {
                                let count = syncer.upsert_tickets(&r.id, &tickets)?;
                                println!("  {} — synced {count} GitHub issues", r.slug);
                            }
                            Err(e) => {
                                eprintln!("  {} — sync failed: {e}", r.slug);
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
