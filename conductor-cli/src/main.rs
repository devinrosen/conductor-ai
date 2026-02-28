use std::io::Read;
use std::process::{Command, Stdio};

use anyhow::Result;
use clap::{Parser, Subcommand};

use conductor_core::agent::{AgentManager, ClaudeJsonResult};
use conductor_core::config::{ensure_dirs, load_config};
use conductor_core::db::open_database;
use conductor_core::github;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager, JiraConfig};
use conductor_core::jira_acli;
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
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
    /// Run Claude agent (used internally by TUI tmux windows)
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },
}

#[derive(Subcommand)]
enum AgentCommands {
    /// Run a Claude agent for a worktree (spawned by TUI in tmux)
    Run {
        /// Agent run ID (from agent_runs table)
        #[arg(long)]
        run_id: String,
        /// Path to the worktree directory
        #[arg(long)]
        worktree_path: String,
        /// Prompt for Claude
        #[arg(long)]
        prompt: String,
        /// Resume a previous Claude session
        #[arg(long)]
        resume: Option<String>,
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
                let worktrees = mgr.list(repo.as_deref())?;
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
                mgr.delete(&repo, &name)?;
                println!("Deleted worktree: {name}");
            }
        },
        Commands::Agent { command } => match command {
            AgentCommands::Run {
                run_id,
                worktree_path,
                prompt,
                resume,
            } => {
                run_agent(&conn, &run_id, &worktree_path, &prompt, resume.as_deref())?;
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

/// Run a Claude agent for a worktree. Called inside a tmux window by the TUI.
///
/// Uses `--output-format json` (single JSON result) since the tmux terminal IS the display.
/// Claude's interactive output goes directly to the terminal; we only parse the final JSON result.
fn run_agent(
    conn: &rusqlite::Connection,
    run_id: &str,
    worktree_path: &str,
    prompt: &str,
    resume_session_id: Option<&str>,
) -> Result<()> {
    let mgr = AgentManager::new(conn);

    // Verify the run exists
    let run = mgr.get_run(run_id)?;
    if run.is_none() {
        anyhow::bail!("agent run not found: {run_id}");
    }

    // Build the claude command.
    // stdout is piped (captures JSON result), stderr is inherited (visible in tmux terminal).
    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg(prompt)
        .arg("--output-format")
        .arg("json")
        .arg("--verbose")
        .arg("--permission-mode")
        .arg("acceptEdits")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .current_dir(worktree_path);

    if let Some(session_id) = resume_session_id {
        cmd.arg("--resume").arg(session_id);
    }

    eprintln!(
        "[conductor] Running agent for run_id={} in {}",
        run_id, worktree_path
    );

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let error_msg = format!("Failed to spawn claude: {e}");
            mgr.update_run_failed(run_id, &error_msg)?;
            eprintln!("[conductor] {}", error_msg);
            return Ok(());
        }
    };

    // Read all of stdout (the JSON result blob)
    let mut stdout_buf = String::new();
    if let Some(ref mut stdout) = child.stdout {
        let _ = stdout.read_to_string(&mut stdout_buf);
    }

    let status = child.wait();

    match status {
        Ok(s) => {
            // Try to parse the JSON result from stdout
            if let Ok(result) = serde_json::from_str::<ClaudeJsonResult>(&stdout_buf) {
                let is_error = result.is_error.unwrap_or(false);
                if is_error {
                    let error_msg = result
                        .result
                        .as_deref()
                        .unwrap_or("Claude reported an error");
                    mgr.update_run_failed(run_id, error_msg)?;
                    eprintln!("[conductor] Agent failed: {}", error_msg);
                } else {
                    mgr.update_run_completed(
                        run_id,
                        result.session_id.as_deref(),
                        result.result.as_deref(),
                        result.cost_usd,
                        result.num_turns,
                        result.duration_ms,
                    )?;
                    eprintln!("[conductor] Agent completed successfully");
                    if let Some(cost) = result.cost_usd {
                        eprintln!(
                            "[conductor] Cost: ${:.4}  Turns: {}  Duration: {:.1}s",
                            cost,
                            result.num_turns.unwrap_or(0),
                            result
                                .duration_ms
                                .map(|ms| ms as f64 / 1000.0)
                                .unwrap_or(0.0)
                        );
                    }
                }
            } else if s.success() {
                mgr.update_run_completed(run_id, None, Some(stdout_buf.trim()), None, None, None)?;
                eprintln!("[conductor] Agent completed (no JSON result parsed)");
            } else {
                let error_msg = format!("Claude exited with status: {}", s);
                mgr.update_run_failed(run_id, &error_msg)?;
                eprintln!("[conductor] Agent failed: {}", error_msg);
            }
        }
        Err(e) => {
            let error_msg = format!("Error waiting for claude: {e}");
            mgr.update_run_failed(run_id, &error_msg)?;
            eprintln!("[conductor] {}", error_msg);
        }
    }

    Ok(())
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
