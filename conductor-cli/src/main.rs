use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

use anyhow::Result;
use clap::{Parser, Subcommand};

use conductor_core::agent::{
    build_startup_context, parse_events_from_line, AgentManager, PlanStep,
};
use conductor_core::config::{ensure_dirs, load_config};
use conductor_core::db::open_database;
use conductor_core::github;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager, JiraConfig};
use conductor_core::jira_acli;
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
use conductor_core::tickets::{build_agent_prompt, TicketSyncer};
use conductor_core::worktree::WorktreeManager;

/// Environment variable name used to pass the current agent run ID to subprocesses.
const CONDUCTOR_RUN_ID_ENV: &str = "CONDUCTOR_RUN_ID";

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
        /// Model to use (e.g. "sonnet", "claude-opus-4-6"). Overrides per-worktree and global defaults.
        #[arg(long)]
        model: Option<String>,
    },
    /// Create a new GitHub issue (called by agents during a run)
    CreateIssue {
        /// Issue title
        #[arg(long)]
        title: String,
        /// Issue body
        #[arg(long)]
        body: String,
        /// Agent run ID (defaults to $CONDUCTOR_RUN_ID env var)
        #[arg(long)]
        run_id: Option<String>,
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
    /// Discover repos from your GitHub account or an org (requires gh CLI).
    /// Omit <owner> to list orgs; pass an org name to list its repos.
    Discover {
        /// GitHub org login, or omit to list available orgs
        owner: Option<String>,
    },
    /// Remove a repository
    Remove {
        /// Repo slug
        slug: String,
    },
    /// Set (or clear) the per-repo default model for agent runs
    SetModel {
        /// Repo slug
        slug: String,
        /// Model alias or ID (e.g. "sonnet", "claude-opus-4-6"). Omit to clear.
        model: Option<String>,
    },
    /// Manage issue sources for a repository
    Sources {
        #[command(subcommand)]
        command: SourceCommands,
    },
    /// Allow or disallow agents to create issues for a repository
    AllowAgentIssues {
        /// Repo slug
        slug: String,
        /// Set to true to allow, false to disallow
        #[arg(long, default_value = "true")]
        allow: bool,
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
        /// Auto-start an agent after creation (requires --ticket)
        #[arg(long)]
        auto_agent: bool,
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
    /// Push worktree branch to origin
    Push {
        /// Repo slug
        repo: String,
        /// Worktree slug
        name: String,
    },
    /// Create a pull request for the worktree branch
    Pr {
        /// Repo slug
        repo: String,
        /// Worktree slug
        name: String,
        /// Create as draft PR
        #[arg(long)]
        draft: bool,
    },
    /// Set (or clear) the per-worktree default model for agent runs
    SetModel {
        /// Repo slug
        repo: String,
        /// Worktree slug
        name: String,
        /// Model alias or full ID (e.g. "sonnet", "claude-opus-4-6"). Omit to clear.
        model: Option<String>,
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
    /// Link a ticket to a worktree
    Link {
        /// Ticket source ID (e.g., GitHub issue number)
        ticket: String,
        /// Repo slug
        repo: String,
        /// Worktree slug
        worktree: String,
    },
    /// Show aggregate agent cost/turns/time per ticket
    Stats {
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
            RepoCommands::Discover { owner } => {
                if let Some(ref owner_str) = owner {
                    // List repos for a specific owner (org or personal via "")
                    let owner_opt = if owner_str.is_empty() {
                        None
                    } else {
                        Some(owner_str.as_str())
                    };
                    let discovered = github::discover_github_repos(owner_opt)?;
                    if discovered.is_empty() {
                        println!("No repos found for {}.", owner_str);
                    } else {
                        let mgr = RepoManager::new(&conn, &config);
                        let registered = mgr.list()?;
                        for repo in &discovered {
                            let is_registered = registered.iter().any(|r| {
                                r.remote_url == repo.clone_url || r.remote_url == repo.ssh_url
                            });
                            let marker = if is_registered { " [registered]" } else { "" };
                            let privacy = if repo.private { " (private)" } else { "" };
                            println!("  {}{}{}", repo.full_name, privacy, marker);
                            if !repo.description.is_empty() {
                                println!("    {}", repo.description);
                            }
                        }
                        let unregistered = discovered
                            .iter()
                            .filter(|r| {
                                !registered.iter().any(|reg| {
                                    reg.remote_url == r.clone_url || reg.remote_url == r.ssh_url
                                })
                            })
                            .count();
                        println!(
                            "\n{} repo(s) found, {} not yet registered.",
                            discovered.len(),
                            unregistered
                        );
                        println!("Use `conductor repo add <url>` to register a repo.");
                    }
                } else {
                    // No owner given: list orgs (+ personal)
                    let orgs = github::list_github_orgs()?;
                    println!("  Personal (your repos)  →  conductor repo discover \"\"");
                    for org in &orgs {
                        println!("  {org}  →  conductor repo discover {org}");
                    }
                    if orgs.is_empty() {
                        println!("  (no organizations found)");
                    }
                }
            }
            RepoCommands::Remove { slug } => {
                let mgr = RepoManager::new(&conn, &config);
                mgr.remove(&slug)?;
                println!("Removed repo: {slug}");
            }
            RepoCommands::SetModel { slug, model } => {
                let mgr = RepoManager::new(&conn, &config);
                mgr.set_model(&slug, model.as_deref())?;
                match model {
                    Some(m) => println!("Set model for {slug} to: {m}"),
                    None => println!("Cleared model override for {slug} (will use global default)"),
                }
            }
            RepoCommands::AllowAgentIssues { slug, allow } => {
                let mgr = RepoManager::new(&conn, &config);
                let repo = mgr.get_by_slug(&slug)?;
                mgr.set_allow_agent_issue_creation(&repo.id, allow)?;
                if allow {
                    println!("Enabled agent issue creation for {slug}");
                } else {
                    println!("Disabled agent issue creation for {slug}");
                }
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
                auto_agent,
            } => {
                let mgr = WorktreeManager::new(&conn, &config);
                let (wt, warnings) =
                    mgr.create(&repo, &name, from.as_deref(), ticket.as_deref())?;
                for warning in &warnings {
                    eprintln!("warning: {warning}");
                }
                println!("Created worktree: {} ({})", wt.slug, wt.branch);
                println!("  Path: {}", wt.path);

                if auto_agent {
                    if let Some(ref tid) = ticket {
                        let syncer = TicketSyncer::new(&conn);
                        match syncer.get_by_id(tid) {
                            Ok(t) => {
                                let prompt = build_agent_prompt(&t);
                                println!("Starting agent...");
                                // Resolve model: per-worktree → per-repo → global config
                                let repo_mgr = RepoManager::new(&conn, &config);
                                let repo_model =
                                    repo_mgr.get_by_slug(&repo).ok().and_then(|r| r.model);
                                let model = wt
                                    .model
                                    .as_deref()
                                    .or(repo_model.as_deref())
                                    .or(config.general.model.as_deref());
                                let agent_mgr = AgentManager::new(&conn);
                                let run =
                                    agent_mgr.create_run(&wt.id, &prompt, Some(&wt.slug), model)?;
                                run_agent(&conn, &run.id, &wt.path, &prompt, None, model)?;
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
            WorktreeCommands::Push { repo, name } => {
                let mgr = WorktreeManager::new(&conn, &config);
                let msg = mgr.push(&repo, &name)?;
                println!("{msg}");
            }
            WorktreeCommands::Pr { repo, name, draft } => {
                let mgr = WorktreeManager::new(&conn, &config);
                let url = mgr.create_pr(&repo, &name, draft)?;
                println!("PR created: {url}");
            }
            WorktreeCommands::SetModel { repo, name, model } => {
                let mgr = WorktreeManager::new(&conn, &config);
                mgr.set_model(&repo, &name, model.as_deref())?;
                match model {
                    Some(m) => println!("Set model for {name} to: {m}"),
                    None => println!("Cleared model override for {name} (will use global default)"),
                }
            }
        },
        Commands::Agent { command } => match command {
            AgentCommands::Run {
                run_id,
                worktree_path,
                prompt,
                resume,
                model,
            } => {
                run_agent(
                    &conn,
                    &run_id,
                    &worktree_path,
                    &prompt,
                    resume.as_deref(),
                    model.as_deref(),
                )?;
            }
            AgentCommands::CreateIssue {
                title,
                body,
                run_id,
            } => {
                let run_id = run_id
                    .or_else(|| std::env::var(CONDUCTOR_RUN_ID_ENV).ok())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "No run ID provided. Use --run-id or set ${CONDUCTOR_RUN_ID_ENV}."
                        )
                    })?;

                let agent_mgr = AgentManager::new(&conn);

                // Look up run → worktree → repo
                let run = agent_mgr
                    .get_run(&run_id)?
                    .ok_or_else(|| anyhow::anyhow!("Agent run not found: {run_id}"))?;

                let (repo_id, remote_url): (String, String) = conn
                    .query_row(
                        "SELECT r.id, r.remote_url \
                         FROM worktrees w \
                         JOIN repos r ON w.repo_id = r.id \
                         WHERE w.id = ?1",
                        rusqlite::params![run.worktree_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|_| {
                        anyhow::anyhow!("Could not find repo for worktree {}", run.worktree_id)
                    })?;

                // Check per-repo opt-in
                let allow: bool = conn
                    .query_row(
                        "SELECT allow_agent_issue_creation FROM repos WHERE id = ?1",
                        rusqlite::params![repo_id],
                        |row| row.get::<_, i64>(0).map(|v| v != 0),
                    )
                    .unwrap_or(false);

                if !allow {
                    anyhow::bail!(
                        "Agent issue creation is disabled for this repo. \
                         Enable it via `conductor repo allow-agent-issues <repo-slug>`."
                    );
                }

                // Determine GitHub owner/repo from remote URL
                let (owner, repo_name) =
                    github::parse_github_remote(&remote_url).ok_or_else(|| {
                        anyhow::anyhow!(
                            "Cannot determine GitHub repo from remote URL: {remote_url}"
                        )
                    })?;

                // Create the GitHub issue
                let (source_id, url) =
                    github::create_github_issue(&owner, &repo_name, &title, &body)?;

                // Record in DB
                agent_mgr
                    .record_created_issue(&run_id, &repo_id, "github", &source_id, &title, &url)?;

                println!("Created issue #{source_id}: {url}");
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
            TicketCommands::Link {
                ticket,
                repo,
                worktree,
            } => {
                let repo_mgr = RepoManager::new(&conn, &config);
                let r = repo_mgr.get_by_slug(&repo)?;

                let ticket_id: String = conn
                    .query_row(
                        "SELECT id FROM tickets WHERE repo_id = ?1 AND source_id = ?2",
                        rusqlite::params![r.id, ticket],
                        |row| row.get(0),
                    )
                    .map_err(|_| anyhow::anyhow!("Ticket not found: #{ticket}"))?;

                let (worktree_id, existing_ticket): (String, Option<String>) = conn
                    .query_row(
                        "SELECT id, ticket_id FROM worktrees WHERE repo_id = ?1 AND slug = ?2",
                        rusqlite::params![r.id, worktree],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|_| anyhow::anyhow!("Worktree not found: {worktree}"))?;

                if existing_ticket.is_some() {
                    anyhow::bail!("Worktree '{worktree}' already has a linked ticket");
                }

                let syncer = TicketSyncer::new(&conn);
                syncer.link_to_worktree(&ticket_id, &worktree_id)?;
                println!("Linked ticket #{ticket} to worktree '{worktree}'");
            }
            TicketCommands::Stats { repo } => {
                let repo_mgr = RepoManager::new(&conn, &config);
                let repo_id = if let Some(slug) = &repo {
                    Some(repo_mgr.get_by_slug(slug)?.id)
                } else {
                    None
                };

                let syncer = TicketSyncer::new(&conn);
                let tickets = syncer.list(repo_id.as_deref())?;
                let agent_mgr = AgentManager::new(&conn);
                let totals = agent_mgr.totals_by_ticket_all()?;

                let mut found = false;
                for t in &tickets {
                    if let Some(stats) = totals.get(&t.id) {
                        found = true;
                        let dur_secs = stats.total_duration_ms as f64 / 1000.0;
                        let mins = (dur_secs / 60.0) as i64;
                        let secs = (dur_secs % 60.0) as i64;
                        println!(
                            "  #{:<6} {:<40} ${:.4}  {} turns  {}m{:02}s  ({} runs)",
                            t.source_id,
                            truncate_str(&t.title, 40),
                            stats.total_cost,
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
        },
    }

    Ok(())
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

/// Generate an implementation plan by asking Claude to produce a JSON step list.
///
/// Returns `None` (non-fatal) if plan generation fails or produces no steps.
fn generate_plan(worktree_path: &str, prompt: &str) -> Option<Vec<PlanStep>> {
    let plan_prompt = format!(
        "You are planning a software development task. \
         Generate a concise implementation plan as a JSON array.\n\n\
         Task:\n{prompt}\n\n\
         Respond with ONLY a JSON array of step objects. \
         Each object must have a \"description\" string field. \
         No markdown, no backticks, no explanation — just the raw JSON array. \
         Example: [{{\"description\":\"Read the relevant files\"}},{{\"description\":\"Write tests\"}}]\n\
         Aim for 3-8 concrete, actionable steps."
    );

    let output = Command::new("claude")
        .arg("-p")
        .arg(&plan_prompt)
        .arg("--output-format")
        .arg("json")
        .arg("--dangerously-skip-permissions")
        .current_dir(worktree_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let response: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let result_text = response.get("result")?.as_str()?;

    // Strip optional markdown code fences that Claude sometimes adds
    let trimmed = result_text.trim();
    let json_text = if let Some(inner) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
    {
        inner.trim_end_matches("```").trim()
    } else {
        trimmed
    };

    let raw: Vec<serde_json::Value> = serde_json::from_str(json_text).ok()?;
    let steps: Vec<PlanStep> = raw
        .into_iter()
        .filter_map(|v| {
            let description = v.get("description")?.as_str()?.to_string();
            Some(PlanStep {
                description,
                done: false,
            })
        })
        .collect();

    if steps.is_empty() {
        None
    } else {
        Some(steps)
    }
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
    model: Option<&str>,
) -> Result<()> {
    let mgr = AgentManager::new(conn);

    // Verify the run exists
    let run = mgr.get_run(run_id)?;
    let run = match run {
        Some(r) => r,
        None => anyhow::bail!("agent run not found: {run_id}"),
    };

    // Build effective prompt with optional startup context
    let config = load_config().unwrap_or_default();
    let effective_prompt = if config.general.inject_startup_context {
        match build_startup_context(conn, &run.worktree_id, run_id, worktree_path) {
            Some(context) => {
                eprintln!("[conductor] Injecting session context into prompt");
                format!("{context}\n\n---\n\n{prompt}")
            }
            None => prompt.to_string(),
        }
    } else {
        prompt.to_string()
    };

    // Phase 1: Plan generation (only for new runs, not resumes)
    if resume_session_id.is_none() {
        eprintln!("[conductor] Phase 1: Generating plan...");
        match generate_plan(worktree_path, &effective_prompt) {
            Some(steps) => {
                eprintln!("[conductor] Plan ({} steps):", steps.len());
                for (i, step) in steps.iter().enumerate() {
                    eprintln!("  {}. {}", i + 1, step.description);
                }
                if let Err(e) = mgr.update_run_plan(run_id, &steps) {
                    eprintln!("[conductor] Warning: could not save plan to DB: {e}");
                }
            }
            None => {
                eprintln!("[conductor] Plan generation skipped (no steps returned)");
            }
        }
        eprintln!("[conductor] Phase 2: Executing...");
    }

    // Build the claude command in print mode with stream-json output.
    // stdout: stream-json events (piped, parsed for result metadata)
    // stderr: verbose turn-by-turn output (inherited, visible in tmux)
    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg(&effective_prompt)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--dangerously-skip-permissions")
        .env(CONDUCTOR_RUN_ID_ENV, run_id)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .current_dir(worktree_path);

    if let Some(session_id) = resume_session_id {
        cmd.arg("--resume").arg(session_id);
    }

    if let Some(m) = model {
        cmd.arg("--model").arg(m);
    }

    eprintln!(
        "[conductor] Running agent for run_id={} in {}",
        run_id, worktree_path
    );

    // Emit the user's prompt as the first event so it appears in the activity log
    {
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(e) = mgr.create_event(run_id, "prompt", prompt, &now, None) {
            eprintln!("[conductor] Warning: could not persist prompt event: {e}");
        }
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let error_msg = format!("Failed to spawn claude: {e}");
            mgr.update_run_failed(run_id, &error_msg)?;
            eprintln!("[conductor] {}", error_msg);
            return Ok(());
        }
    };

    // Set up log file to capture stream-json events as they arrive.
    let log_dir = conductor_core::config::conductor_dir().join("agent-logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!("{run_id}.log"));
    let mut log_file = std::fs::File::create(&log_path).ok();

    // Store log file path in DB immediately so the TUI can read streaming events.
    if log_file.is_some() {
        let path_str = log_path.to_string_lossy().to_string();
        if let Err(e) = mgr.update_run_log_file(run_id, &path_str) {
            eprintln!("[conductor] Warning: could not save log path to DB: {e}");
        }
    }

    // Parse stream-json events from stdout to extract result metadata.
    let mut session_id_parsed: Option<String> = None;
    let mut result_text: Option<String> = None;
    let mut cost_usd: Option<f64> = None;
    let mut num_turns: Option<i64> = None;
    let mut duration_ms: Option<i64> = None;
    let mut is_error = false;

    // Track the last persisted event span so we can fill its ended_at
    let mut last_event_id: Option<String> = None;

    if let Some(stdout) = child.stdout.take() {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { continue };

            // Write every line to the log file
            if let Some(ref mut f) = log_file {
                let _ = writeln!(f, "{line}");
            }

            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };

            // Display human-readable activity in the tmux terminal
            print_event_summary(&event);

            // Capture session_id from init message
            if let Some(sid) = event.get("session_id").and_then(|v| v.as_str()) {
                session_id_parsed = Some(sid.to_string());
            }

            // Capture result from final message
            if event.get("result").is_some() {
                result_text = event
                    .get("result")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                cost_usd = event.get("total_cost_usd").and_then(|v| v.as_f64());
                num_turns = event.get("num_turns").and_then(|v| v.as_i64());
                duration_ms = event.get("duration_ms").and_then(|v| v.as_i64());
                is_error = event
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            }

            // Persist parsed events to DB as spans
            let parsed = parse_events_from_line(&line);
            if !parsed.is_empty() {
                let now = chrono::Utc::now().to_rfc3339();
                // Close the previous span
                if let Some(ref prev_id) = last_event_id {
                    let _ = mgr.update_event_ended_at(prev_id, &now);
                }
                // Create a new span for each parsed event; only the last one stays open
                for ev in &parsed {
                    match mgr.create_event(run_id, &ev.kind, &ev.summary, &now, None) {
                        Ok(db_ev) => last_event_id = Some(db_ev.id),
                        Err(e) => eprintln!("[conductor] Warning: could not persist event: {e}"),
                    }
                }
            }
        }
    }

    let status = child.wait();

    let end_time = chrono::Utc::now().to_rfc3339();

    // Close the last open event span
    if let Some(ref prev_id) = last_event_id {
        let _ = mgr.update_event_ended_at(prev_id, &end_time);
    }

    match status {
        Ok(s) if s.success() && !is_error => {
            mgr.update_run_completed(
                run_id,
                session_id_parsed.as_deref(),
                result_text.as_deref(),
                cost_usd,
                num_turns,
                duration_ms,
            )?;
            // Mark all plan steps done now that the run succeeded
            if let Err(e) = mgr.mark_plan_done(run_id) {
                eprintln!("[conductor] Warning: could not mark plan done: {e}");
            }
            eprintln!("[conductor] Agent completed successfully");
            if let Some(cost) = cost_usd {
                eprintln!(
                    "[conductor] Cost: ${:.4}  Turns: {}  Duration: {:.1}s",
                    cost,
                    num_turns.unwrap_or(0),
                    duration_ms.map(|ms| ms as f64 / 1000.0).unwrap_or(0.0)
                );
            }
        }
        Ok(_) if is_error => {
            let error_msg = result_text.as_deref().unwrap_or("Claude reported an error");
            mgr.update_run_failed(run_id, error_msg)?;
            eprintln!("[conductor] Agent failed: {}", error_msg);
        }
        Ok(s) => {
            let error_msg = format!("Claude exited with status: {}", s);
            mgr.update_run_failed(run_id, &error_msg)?;
            eprintln!("[conductor] Agent failed: {}", error_msg);
        }
        Err(e) => {
            let error_msg = format!("Error waiting for claude: {e}");
            mgr.update_run_failed(run_id, &error_msg)?;
            eprintln!("[conductor] {}", error_msg);
        }
    }

    eprintln!(
        "[conductor] Agent log saved to {}",
        log_path.to_string_lossy()
    );

    Ok(())
}

/// Print a human-readable summary of a stream-json event to stderr (visible in tmux).
fn print_event_summary(event: &serde_json::Value) {
    let msg_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        // Assistant text message
        "assistant" => {
            if let Some(content) = event.get("message").and_then(|m| m.get("content")) {
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                eprintln!("{text}");
                            }
                        }
                        if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                            let tool = block.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            eprintln!("[tool: {tool}]");
                        }
                    }
                }
            }
            // Also check top-level content (varies by event shape)
            if let Some(content) = event.get("content") {
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                eprintln!("{text}");
                            }
                        }
                        if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                            let tool = block.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            eprintln!("[tool: {tool}]");
                        }
                    }
                }
            }
        }
        // Result message
        "result" => {
            if let Some(text) = event.get("result").and_then(|v| v.as_str()) {
                let truncated = if text.len() > 200 { &text[..200] } else { text };
                eprintln!("[result] {truncated}");
            }
        }
        _ => {}
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
