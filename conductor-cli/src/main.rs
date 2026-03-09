use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use conductor_core::agent::{
    build_startup_context, parse_events_from_line, AgentManager, PlanStep,
};
use conductor_core::agent_config::AgentSpec;
use conductor_core::config::{ensure_dirs, load_config};
use conductor_core::db::open_database;
use conductor_core::error::ConductorError;
use conductor_core::github;
use conductor_core::github_app;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager, JiraConfig};
use conductor_core::jira_acli;
use conductor_core::merge_queue::MergeQueueManager;
use conductor_core::orchestrator::{self, OrchestratorConfig};
use conductor_core::post_run::{self, PostRunInput};
use conductor_core::pr_review::{self, ReviewSwarmConfig, ReviewSwarmInput};
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
use conductor_core::tickets::{build_agent_prompt, TicketInput, TicketSyncer};
use conductor_core::workflow::{collect_agent_names, WorkflowExecConfig, WorkflowManager};
use conductor_core::workflow_config;
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
    /// Manage the merge queue for serializing parallel agent merges
    #[command(name = "merge-queue")]
    MergeQueue {
        #[command(subcommand)]
        command: MergeQueueCommands,
    },
    /// Multi-step workflow engine
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommands,
    },
    /// Approve and merge a PR that is awaiting manual approval
    Approve {
        /// Repo slug
        repo: String,
        /// Worktree slug
        worktree: String,
    },
}

#[derive(Subcommand)]
enum MergeQueueCommands {
    /// Add a worktree to the merge queue
    Enqueue {
        /// Repo slug
        repo: String,
        /// Worktree slug
        worktree: String,
        /// Target branch (defaults to worktree's base branch, or main)
        #[arg(long)]
        target: Option<String>,
    },
    /// List merge queue entries for a repo
    List {
        /// Repo slug
        repo: String,
        /// Show only pending entries (queued/processing)
        #[arg(long)]
        pending: bool,
    },
    /// Show details of a single merge queue entry
    Show {
        /// Entry ID
        id: String,
    },
    /// Pop the next queued entry and mark it as processing
    Pop {
        /// Repo slug
        repo: String,
    },
    /// Mark an entry as merged
    Merged {
        /// Entry ID
        id: String,
    },
    /// Mark an entry as failed
    Failed {
        /// Entry ID
        id: String,
    },
    /// Remove an entry from the queue
    Remove {
        /// Entry ID
        id: String,
    },
    /// Show queue statistics for a repo
    Stats {
        /// Repo slug
        repo: String,
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
        /// Prompt for Claude (use --prompt-file for large prompts)
        #[arg(long)]
        prompt: Option<String>,
        /// Read prompt from a file (avoids OS arg length limits for large diffs)
        #[arg(long)]
        prompt_file: Option<String>,
        /// Resume a previous Claude session
        #[arg(long)]
        resume: Option<String>,
        /// Model to use (e.g. "sonnet", "claude-opus-4-6"). Overrides per-worktree and global defaults.
        #[arg(long)]
        model: Option<String>,
    },
    /// Orchestrate child agents: spawn a child run for each plan step
    Orchestrate {
        /// Parent agent run ID (must have plan steps)
        #[arg(long)]
        run_id: String,
        /// Path to the worktree directory
        #[arg(long)]
        worktree_path: String,
        /// Model to use for child agents
        #[arg(long)]
        model: Option<String>,
        /// Stop on first child failure
        #[arg(long)]
        fail_fast: bool,
        /// Child run timeout in seconds (default: 1800 = 30 min)
        #[arg(long, default_value = "1800")]
        child_timeout_secs: u64,
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
    /// Run the automated post-agent lifecycle (commit, PR, review loop, merge)
    PostRun {
        /// Repo slug
        repo: String,
        /// Worktree slug
        worktree: String,
    },
    /// Run a multi-agent PR review swarm on a worktree
    Review {
        /// Repo slug
        repo: String,
        /// Worktree slug
        worktree: String,
        /// PR number (auto-detected from branch if omitted)
        #[arg(long)]
        pr_number: Option<i64>,
        /// Model to use for reviewer agents
        #[arg(long)]
        model: Option<String>,
        /// Reviewer timeout in seconds (default: 900 = 15 min)
        #[arg(long, default_value = "900")]
        reviewer_timeout_secs: u64,
    },
}

#[derive(Subcommand)]
enum WorkflowCommands {
    /// List workflow run history for a repository
    Runs {
        /// Repository slug
        repo: String,
        /// Filter by worktree slug
        worktree: Option<String>,
    },
    /// List available workflow definitions for a repo/worktree
    List {
        /// Repo slug
        repo: String,
        /// Worktree slug
        worktree: String,
    },
    /// Run a workflow
    Run {
        /// Repo slug
        repo: String,
        /// Worktree slug
        worktree: String,
        /// Workflow name (must match a .conductor/workflows/<name>.wf file)
        name: String,
        /// Model to use for agent steps
        #[arg(long)]
        model: Option<String>,
        /// Dry-run mode: show what would be committed without doing it
        #[arg(long)]
        dry_run: bool,
        /// Continue past step failures
        #[arg(long)]
        no_fail_fast: bool,
        /// Step timeout in seconds (default: 1800 = 30 min)
        #[arg(long, default_value = "1800")]
        step_timeout_secs: u64,
        /// Input variables (key=value pairs)
        #[arg(long = "input", value_name = "KEY=VALUE")]
        inputs: Vec<String>,
    },
    /// Show details of a workflow run
    Show {
        /// Workflow run ID
        id: String,
    },
    /// Validate a workflow definition (check all agents exist)
    Validate {
        /// Repo slug
        repo: String,
        /// Worktree slug
        worktree: String,
        /// Workflow name
        name: String,
    },
    /// Cancel a running or waiting workflow
    Cancel {
        /// Workflow run ID
        id: String,
    },
    /// Approve a pending human gate
    GateApprove {
        /// Workflow run ID
        #[arg(value_name = "RUN_ID")]
        run_id: String,
    },
    /// Reject a pending human gate (fails the workflow)
    GateReject {
        /// Workflow run ID
        #[arg(value_name = "RUN_ID")]
        run_id: String,
    },
    /// Provide feedback and approve a pending human gate
    GateFeedback {
        /// Workflow run ID
        #[arg(value_name = "RUN_ID")]
        run_id: String,
        /// Feedback text
        feedback: String,
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
        Commands::Worktree { command } => {
            // Reap stale worktrees before handling any worktree command.
            {
                let wt_mgr = WorktreeManager::new(&conn, &config);
                let _ = wt_mgr.reap_stale_worktrees();
            }
            match command {
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
                                    let run = agent_mgr.create_run(
                                        &wt.id,
                                        &prompt,
                                        Some(&wt.slug),
                                        model,
                                    )?;
                                    run_agent(&conn, &run.id, &wt.path, &prompt, None, model)?;
                                }
                                Err(e) => {
                                    eprintln!(
                                        "Warning: could not load ticket for agent prompt: {e}"
                                    );
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
                        None => {
                            println!("Cleared model override for {name} (will use global default)")
                        }
                    }
                }
            }
        }
        Commands::Agent { command } => {
            // Reap orphaned runs before handling any agent command.
            {
                let agent_mgr = AgentManager::new(&conn);
                let _ = agent_mgr.reap_orphaned_runs();
            }

            match command {
                AgentCommands::Run {
                    run_id,
                    worktree_path,
                    prompt,
                    prompt_file,
                    resume,
                    model,
                } => {
                    let resolved_prompt = match (prompt, prompt_file) {
                        (Some(p), _) => p,
                        (None, Some(path)) => std::fs::read_to_string(&path)
                            .with_context(|| format!("Failed to read prompt file: {path}"))?,
                        (None, None) => {
                            anyhow::bail!("Either --prompt or --prompt-file is required")
                        }
                    };
                    run_agent(
                        &conn,
                        &run_id,
                        &worktree_path,
                        &resolved_prompt,
                        resume.as_deref(),
                        model.as_deref(),
                    )?;
                }
                AgentCommands::Orchestrate {
                    run_id,
                    worktree_path,
                    model,
                    fail_fast,
                    child_timeout_secs,
                } => {
                    run_orchestrate(
                        &conn,
                        &config,
                        &run_id,
                        &worktree_path,
                        model.as_deref(),
                        fail_fast,
                        child_timeout_secs,
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
                        github::create_github_issue(&owner, &repo_name, &title, &body, &[], None)?;

                    // Record in DB
                    agent_mgr.record_created_issue(
                        &run_id, &repo_id, "github", &source_id, &title, &url,
                    )?;

                    println!("Created issue #{source_id}: {url}");
                }
                AgentCommands::PostRun { repo, worktree } => {
                    match post_run::run_post_lifecycle(&PostRunInput {
                        conn: &conn,
                        config: &config,
                        repo_slug: &repo,
                        worktree_slug: &worktree,
                    }) {
                        Ok(result) => {
                            println!("{}", result.summary);
                            if let Some(ref url) = result.pr_url {
                                println!("PR: {url}");
                            }
                        }
                        Err(e) => {
                            eprintln!("Post-run lifecycle failed: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                AgentCommands::Review {
                    repo,
                    worktree,
                    pr_number,
                    model,
                    reviewer_timeout_secs,
                } => {
                    let repo_mgr = RepoManager::new(&conn, &config);
                    let r = repo_mgr.get_by_slug(&repo)?;
                    let wt_mgr = WorktreeManager::new(&conn, &config);
                    let wt = wt_mgr.get_by_slug(&r.id, &worktree)?;

                    // Auto-detect PR number from branch if not provided
                    let pr_num = match pr_number {
                        Some(n) => Some(n),
                        None => github::detect_pr_number(&r.remote_url, &wt.branch),
                    };

                    let swarm_config = ReviewSwarmConfig {
                        reviewer_timeout: std::time::Duration::from_secs(reviewer_timeout_secs),
                        ..Default::default()
                    };

                    println!(
                        "Starting PR review swarm for {}/{} (branch: {})...",
                        repo, worktree, wt.branch
                    );
                    if let Some(n) = pr_num {
                        println!("PR #{n}");
                    }

                    let token_resolution = github_app::resolve_app_token(&config, "review");

                    match pr_review::run_review_swarm(&ReviewSwarmInput {
                        conn: &conn,
                        config: &config,
                        repo_id: &r.id,
                        worktree_id: &wt.id,
                        pr_branch: &wt.branch,
                        pr_number: pr_num,
                        model: model.as_deref(),
                        swarm_config: &swarm_config,
                        app_token: token_resolution.token(),
                    }) {
                        Ok(result) => {
                            println!("\n{}", result.aggregated_comment);
                            if result.all_required_approved {
                                println!("All required reviewers approved — added to merge queue.");
                            } else {
                                let blocking: Vec<_> = result
                                    .reviewer_results
                                    .iter()
                                    .filter(|r| r.required && !r.approved)
                                    .map(|r| r.role_name.as_str())
                                    .collect();
                                println!("Blocking reviewers: {}", blocking.join(", "));
                            }
                            println!(
                                "Total: ${:.4}, {} turns, {:.1}s",
                                result.total_cost,
                                result.total_turns,
                                result.total_duration_ms as f64 / 1000.0
                            );
                        }
                        Err(e) => {
                            eprintln!("Review swarm failed: {e}");
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
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
                let token_res = github_app::resolve_app_token(&config, "github-issues-sync");
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
                                    match serde_json::from_str::<GitHubConfig>(&source.config_json)
                                    {
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
                                                || {
                                                    jira_acli::sync_jira_issues_acli(
                                                        &cfg.jql, &cfg.url,
                                                    )
                                                },
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
        Commands::MergeQueue { command } => {
            let mqm = MergeQueueManager::new(&conn);
            match command {
                MergeQueueCommands::Enqueue {
                    repo,
                    worktree,
                    target,
                } => {
                    let repo_mgr = RepoManager::new(&conn, &config);
                    let repo_obj = repo_mgr.get_by_slug(&repo)?;
                    let wt_mgr = WorktreeManager::new(&conn, &config);
                    let wt = wt_mgr.get_by_slug(&repo_obj.id, &worktree)?;
                    let effective_target = target
                        .as_deref()
                        .unwrap_or_else(|| wt.effective_base(&repo_obj.default_branch));
                    let entry = mqm.enqueue(&repo_obj.id, &wt.id, None, Some(effective_target))?;
                    println!(
                        "Enqueued {} at position {} (id: {})",
                        worktree, entry.position, entry.id
                    );
                }
                MergeQueueCommands::List { repo, pending } => {
                    let repo_mgr = RepoManager::new(&conn, &config);
                    let repo_obj = repo_mgr.get_by_slug(&repo)?;
                    let entries = if pending {
                        mqm.list_pending(&repo_obj.id)?
                    } else {
                        mqm.list_for_repo(&repo_obj.id)?
                    };
                    if entries.is_empty() {
                        println!("Merge queue is empty.");
                    } else {
                        println!("{:<4} {:<10} {:<26} Queued At", "Pos", "Status", "ID");
                        for e in &entries {
                            println!(
                                "{:<4} {:<10} {:<26} {}",
                                e.position, e.status, e.id, e.queued_at
                            );
                        }
                    }
                }
                MergeQueueCommands::Show { id } => {
                    if let Some(e) = mqm.get(&id)? {
                        println!("ID:            {}", e.id);
                        println!("Worktree:      {}", e.worktree_id);
                        println!("Target branch: {}", e.target_branch);
                        println!("Position:      {}", e.position);
                        println!("Status:        {}", e.status);
                        println!("Queued at:     {}", e.queued_at);
                        if let Some(ref s) = e.started_at {
                            println!("Started at:    {}", s);
                        }
                        if let Some(ref c) = e.completed_at {
                            println!("Completed at:  {}", c);
                        }
                    } else {
                        println!("Entry not found: {}", id);
                    }
                }
                MergeQueueCommands::Pop { repo } => {
                    let repo_mgr = RepoManager::new(&conn, &config);
                    let repo_obj = repo_mgr.get_by_slug(&repo)?;
                    if let Some(entry) = mqm.pop_next(&repo_obj.id)? {
                        println!(
                            "Processing entry {} (worktree: {})",
                            entry.id, entry.worktree_id
                        );
                    } else {
                        println!("Nothing to process (queue empty or entry already processing).");
                    }
                }
                MergeQueueCommands::Merged { id } => {
                    let wt_mgr = WorktreeManager::new(&conn, &config);
                    match mqm.mark_merged_and_cleanup(&id, &wt_mgr)? {
                        (_, Ok(wt)) => println!(
                            "Marked {} as merged and cleaned up worktree '{}'.",
                            id, wt.slug
                        ),
                        (entry, Err(e)) => {
                            println!("Marked {} as merged.", id);
                            eprintln!(
                                "Warning: could not clean up worktree {}: {e}",
                                entry.worktree_id
                            );
                        }
                    }
                }
                MergeQueueCommands::Failed { id } => {
                    mqm.mark_failed(&id)?;
                    println!("Marked {} as failed.", id);
                }
                MergeQueueCommands::Remove { id } => {
                    mqm.remove(&id)?;
                    println!("Removed {} from merge queue.", id);
                }
                MergeQueueCommands::Stats { repo } => {
                    let repo_mgr = RepoManager::new(&conn, &config);
                    let repo_obj = repo_mgr.get_by_slug(&repo)?;
                    let stats = mqm.queue_stats(&repo_obj.id)?;
                    println!("Queued:     {}", stats.queued);
                    println!("Processing: {}", stats.processing);
                    println!("Merged:     {}", stats.merged);
                    println!("Failed:     {}", stats.failed);
                }
            }
        }
        Commands::Workflow { command } => match command {
            WorkflowCommands::Runs { repo, worktree } => {
                let repo_mgr = RepoManager::new(&conn, &config);
                let r = repo_mgr.get_by_slug(&repo)?;

                let agent_mgr = AgentManager::new(&conn);
                let runs = if let Some(wt_slug) = worktree {
                    let wt_mgr = WorktreeManager::new(&conn, &config);
                    let wt = wt_mgr.get_by_slug(&r.id, &wt_slug)?;
                    agent_mgr.list_for_worktree(&wt.id)?
                } else {
                    agent_mgr.list_for_repo(&r.id)?
                };

                if runs.is_empty() {
                    println!("No workflow runs found.");
                } else {
                    println!(
                        "  {:<10}  {:<40}  {:<20}  STARTED AT",
                        "RUN ID", "WORKFLOW", "STATUS"
                    );
                    for run in &runs {
                        println!(
                            "  {:<10}  {:<40}  {:<20}  {}",
                            &run.id[..8.min(run.id.len())],
                            truncate_str(&run.prompt, 40),
                            run.status,
                            &run.started_at[..16.min(run.started_at.len())],
                        );
                    }
                }
            }
            WorkflowCommands::List { repo, worktree } => {
                let repo_mgr = RepoManager::new(&conn, &config);
                let r = repo_mgr.get_by_slug(&repo)?;
                let wt_mgr = WorktreeManager::new(&conn, &config);
                let wt = wt_mgr.get_by_slug(&r.id, &worktree)?;

                // Try new .wf files first, fall back to legacy .md
                let wf_defs = WorkflowManager::list_defs(&wt.path, &r.local_path)?;
                if !wf_defs.is_empty() {
                    for def in &wf_defs {
                        let node_count = def.total_nodes();
                        println!(
                            "  {:<20} {:<40} [{}, {} nodes]",
                            def.name, def.description, def.trigger, node_count
                        );
                    }
                } else {
                    let defs = workflow_config::load_workflow_defs(&wt.path, &r.local_path)?;
                    if defs.is_empty() {
                        println!(
                            "No workflows found. Create .conductor/workflows/<name>.wf in your repo."
                        );
                    } else {
                        for def in &defs {
                            println!(
                                "  {:<20} {:<40} [{}, {} steps]",
                                def.name,
                                def.description,
                                def.trigger,
                                def.steps.len()
                            );
                        }
                    }
                }
            }
            WorkflowCommands::Run {
                repo,
                worktree,
                name,
                model,
                dry_run,
                no_fail_fast,
                step_timeout_secs,
                inputs,
            } => {
                let repo_mgr = RepoManager::new(&conn, &config);
                let r = repo_mgr.get_by_slug(&repo)?;
                let wt_mgr = WorktreeManager::new(&conn, &config);
                let wt = wt_mgr.get_by_slug(&r.id, &worktree)?;

                let workflow = WorkflowManager::load_def_by_name(&wt.path, &r.local_path, &name)?;

                // Parse input key=value pairs
                let mut input_map = std::collections::HashMap::new();
                for input in &inputs {
                    if let Some((key, value)) = input.split_once('=') {
                        input_map.insert(key.to_string(), value.to_string());
                    } else {
                        anyhow::bail!("Invalid input format: '{}'. Use key=value.", input);
                    }
                }

                // Validate required inputs
                for input_decl in &workflow.inputs {
                    if input_decl.required && !input_map.contains_key(&input_decl.name) {
                        anyhow::bail!(
                            "Missing required input: '{}'. Use --input {}=<value>.",
                            input_decl.name,
                            input_decl.name
                        );
                    }
                    // Set defaults for missing optional inputs
                    if let Some(ref default) = input_decl.default {
                        input_map
                            .entry(input_decl.name.clone())
                            .or_insert_with(|| default.clone());
                    }
                }

                if dry_run {
                    println!("DRY RUN: Actor steps will show intended changes without committing.");
                }

                let exec_config = WorkflowExecConfig {
                    step_timeout: std::time::Duration::from_secs(step_timeout_secs),
                    fail_fast: !no_fail_fast,
                    dry_run,
                    ..Default::default()
                };

                let node_count = workflow.total_nodes();
                println!(
                    "Running workflow '{}' ({} nodes) on {}/{}...",
                    workflow.name, node_count, repo, worktree
                );

                match conductor_core::workflow::execute_workflow(
                    &conductor_core::workflow::WorkflowExecInput {
                        conn: &conn,
                        config: &config,
                        workflow: &workflow,
                        worktree_id: &wt.id,
                        worktree_path: &wt.path,
                        repo_path: &r.local_path,
                        model: model.as_deref(),
                        exec_config: &exec_config,
                        inputs: input_map,
                        depth: 0,
                    },
                ) {
                    Ok(result) => {
                        println!(
                            "\nTotal: ${:.4}, {} turns, {:.1}s",
                            result.total_cost,
                            result.total_turns,
                            result.total_duration_ms as f64 / 1000.0
                        );
                        if result.all_succeeded {
                            println!("Workflow completed successfully.");
                        } else {
                            eprintln!("Workflow finished with failures.");
                            std::process::exit(1);
                        }
                    }
                    Err(e) => {
                        eprintln!("Workflow execution failed: {e}");
                        std::process::exit(1);
                    }
                }
            }
            WorkflowCommands::Show { id } => {
                let wf_mgr = WorkflowManager::new(&conn);
                match wf_mgr.get_workflow_run(&id)? {
                    Some(run) => {
                        println!("Workflow Run: {}", run.id);
                        println!("  Name:    {}", run.workflow_name);
                        println!("  Status:  {}", run.status);
                        println!("  Trigger: {}", run.trigger);
                        println!("  Dry run: {}", run.dry_run);
                        println!("  Started: {}", run.started_at);
                        if let Some(ref ended) = run.ended_at {
                            println!("  Ended:   {ended}");
                        }
                        if let Some(ref summary) = run.result_summary {
                            println!("\n{summary}");
                        }

                        let steps = wf_mgr.get_workflow_steps(&run.id)?;
                        if !steps.is_empty() {
                            println!("\nSteps:");
                            for step in &steps {
                                let marker = match step.status {
                                    conductor_core::workflow::WorkflowStepStatus::Completed => "ok",
                                    conductor_core::workflow::WorkflowStepStatus::Failed => "FAIL",
                                    conductor_core::workflow::WorkflowStepStatus::Skipped => "skip",
                                    conductor_core::workflow::WorkflowStepStatus::Running => "...",
                                    conductor_core::workflow::WorkflowStepStatus::Pending => "-",
                                    conductor_core::workflow::WorkflowStepStatus::Waiting => "wait",
                                    conductor_core::workflow::WorkflowStepStatus::TimedOut => {
                                        "tout"
                                    }
                                };
                                let commit_flag = if step.can_commit { " [commit]" } else { "" };
                                let iter_label = if step.iteration > 0 {
                                    format!(" iter={}", step.iteration)
                                } else {
                                    String::new()
                                };
                                println!(
                                    "  [{marker}] {} ({}{}){iter_label}",
                                    step.step_name, step.role, commit_flag
                                );
                                if let Some(ref gate_type) = step.gate_type {
                                    print!("        gate: {gate_type}");
                                    if let Some(ref approved_at) = step.gate_approved_at {
                                        print!(" (approved {approved_at})");
                                    }
                                    println!();
                                }
                                if step.retry_count > 0 {
                                    println!("        retries: {}", step.retry_count);
                                }
                                if let Some(ref markers) = step.markers_out {
                                    println!("        markers: {markers}");
                                }
                                if let Some(ref ctx) = step.context_out {
                                    if !ctx.is_empty() {
                                        println!("        context: {ctx}");
                                    }
                                }
                                if let Some(ref child) = step.child_run_id {
                                    println!("        child run: {child}");
                                }
                            }
                        }
                    }
                    None => {
                        println!("Workflow run not found: {id}");
                    }
                }
            }
            WorkflowCommands::Validate {
                repo,
                worktree,
                name,
            } => {
                let repo_mgr = RepoManager::new(&conn, &config);
                let r = repo_mgr.get_by_slug(&repo)?;
                let wt_mgr = WorktreeManager::new(&conn, &config);
                let wt = wt_mgr.get_by_slug(&r.id, &worktree)?;

                let workflow = WorkflowManager::load_def_by_name(&wt.path, &r.local_path, &name)?;

                let mut all_refs = collect_agent_names(&workflow.body);
                all_refs.extend(collect_agent_names(&workflow.always));

                // Deduplicate
                all_refs.sort();
                all_refs.dedup();

                let specs: Vec<AgentSpec> = all_refs.iter().map(AgentSpec::from).collect();
                let missing = conductor_core::agent_config::find_missing_agents(
                    &wt.path,
                    &r.local_path,
                    &specs,
                    Some(&name),
                );

                println!("Workflow: {}", workflow.name);
                println!("  Description: {}", workflow.description);
                println!("  Trigger: {}", workflow.trigger);
                let node_count = workflow.total_nodes();
                println!("  Nodes: {node_count}");
                println!("  Agents referenced: {}", all_refs.len());

                if missing.is_empty() {
                    println!("  All agents found.");
                } else {
                    println!("\n  MISSING agents ({}/{}):", missing.len(), all_refs.len());
                    for agent in &missing {
                        println!("    - {agent}");
                    }
                    std::process::exit(1);
                }
            }
            WorkflowCommands::Cancel { id } => {
                let wf_mgr = WorkflowManager::new(&conn);
                match wf_mgr.get_workflow_run(&id)? {
                    Some(run) => {
                        if matches!(
                            run.status,
                            conductor_core::workflow::WorkflowRunStatus::Completed
                                | conductor_core::workflow::WorkflowRunStatus::Failed
                                | conductor_core::workflow::WorkflowRunStatus::Cancelled
                        ) {
                            println!(
                                "Workflow run {} is already in terminal state: {}",
                                id, run.status
                            );
                        } else {
                            wf_mgr.update_workflow_status(
                                &id,
                                conductor_core::workflow::WorkflowRunStatus::Cancelled,
                                Some("Cancelled by user"),
                            )?;
                            println!("Workflow run {} cancelled.", id);
                        }
                    }
                    None => {
                        println!("Workflow run not found: {id}");
                    }
                }
            }
            WorkflowCommands::GateApprove { run_id } => {
                let wf_mgr = WorkflowManager::new(&conn);
                match wf_mgr.find_waiting_gate(&run_id)? {
                    Some(step) => {
                        let user = std::env::var("USER").unwrap_or_else(|_| "cli".to_string());
                        wf_mgr.approve_gate(&step.id, &user, None)?;
                        println!("Gate '{}' approved by {user}.", step.step_name);
                    }
                    None => {
                        println!("No waiting gate found for workflow run: {run_id}");
                    }
                }
            }
            WorkflowCommands::GateReject { run_id } => {
                let wf_mgr = WorkflowManager::new(&conn);
                match wf_mgr.find_waiting_gate(&run_id)? {
                    Some(step) => {
                        let user = std::env::var("USER").unwrap_or_else(|_| "cli".to_string());
                        wf_mgr.reject_gate(&step.id, &user)?;
                        wf_mgr.update_workflow_status(
                            &run_id,
                            conductor_core::workflow::WorkflowRunStatus::Failed,
                            Some(&format!("Gate '{}' rejected by {user}", step.step_name)),
                        )?;
                        println!("Gate '{}' rejected by {user}.", step.step_name);
                    }
                    None => {
                        println!("No waiting gate found for workflow run: {run_id}");
                    }
                }
            }
            WorkflowCommands::GateFeedback { run_id, feedback } => {
                let wf_mgr = WorkflowManager::new(&conn);
                match wf_mgr.find_waiting_gate(&run_id)? {
                    Some(step) => {
                        let user = std::env::var("USER").unwrap_or_else(|_| "cli".to_string());
                        wf_mgr.approve_gate(&step.id, &user, Some(&feedback))?;
                        println!(
                            "Gate '{}' approved with feedback by {user}.",
                            step.step_name
                        );
                    }
                    None => {
                        println!("No waiting gate found for workflow run: {run_id}");
                    }
                }
            }
        },
        Commands::Approve { repo, worktree } => {
            match post_run::approve_and_merge(&PostRunInput {
                conn: &conn,
                config: &config,
                repo_slug: &repo,
                worktree_slug: &worktree,
            }) {
                Ok(result) => {
                    println!("{}", result.summary);
                }
                Err(e) => {
                    eprintln!("Approve failed: {e}");
                    std::process::exit(1);
                }
            }
        }
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
                ..Default::default()
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
        let context = build_startup_context(conn, &run.worktree_id, run_id, worktree_path);
        eprintln!("[conductor] Injecting session context into prompt");
        format!("{context}\n\n---\n\n{prompt}")
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
    } else {
        // Resuming: carry forward the plan from the previous run
        // Look up the previous run that owns this session_id
        if let Ok(prev_runs) = mgr.list_for_worktree(&run.worktree_id) {
            let prev_run = prev_runs
                .iter()
                .find(|r| r.claude_session_id.as_deref() == resume_session_id && r.id != run_id);
            if let Some(prev) = prev_run {
                if prev.has_incomplete_plan_steps() {
                    let incomplete: Vec<&PlanStep> = prev.incomplete_plan_steps();
                    eprintln!(
                        "[conductor] Resuming with {} incomplete plan steps:",
                        incomplete.len()
                    );
                    for (i, step) in incomplete.iter().enumerate() {
                        eprintln!("  {}. {}", i + 1, step.description);
                    }
                    // Carry forward the full plan to the new run
                    if let Some(ref plan) = prev.plan {
                        if let Err(e) = mgr.update_run_plan(run_id, plan) {
                            eprintln!("[conductor] Warning: could not carry forward plan: {e}");
                        }
                    }
                }
            }
        }
        eprintln!("[conductor] Resuming session...");
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
    let log_dir = conductor_core::config::agent_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = conductor_core::config::agent_log_path(run_id);
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
    let mut db_updated_eagerly = false;

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

            // Capture session_id from init message and save immediately for resume
            if let Some(sid) = event.get("session_id").and_then(|v| v.as_str()) {
                session_id_parsed = Some(sid.to_string());
                if let Err(e) = mgr.update_run_session_id(run_id, sid) {
                    eprintln!("[conductor] Warning: could not save session_id: {e}");
                }
            }

            // Capture result from final message and eagerly update DB to
            // narrow the race window if the process is killed before child.wait().
            if event.get("result").is_some() {
                let parsed = conductor_core::agent::parse_result_event(&event);
                result_text = parsed.result_text;
                cost_usd = parsed.cost_usd;
                num_turns = parsed.num_turns;
                duration_ms = parsed.duration_ms;
                is_error = parsed.is_error;

                // Eagerly persist completion/failure so the consumer sees it
                // even if this process is killed before child.wait() returns.
                if is_error {
                    let error_msg = result_text
                        .as_deref()
                        .unwrap_or(conductor_core::agent::DEFAULT_AGENT_ERROR_MSG);
                    if let Err(e) = mgr.update_run_failed_with_session(
                        run_id,
                        error_msg,
                        session_id_parsed.as_deref(),
                    ) {
                        eprintln!("[conductor] Warning: eager DB update failed: {e}");
                    }
                } else if let Err(e) = mgr.update_run_completed(
                    run_id,
                    session_id_parsed.as_deref(),
                    result_text.as_deref(),
                    cost_usd,
                    num_turns,
                    duration_ms,
                ) {
                    eprintln!("[conductor] Warning: eager DB update failed: {e}");
                }
                db_updated_eagerly = true;
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

                    // Detect feedback request markers in agent text output.
                    if ev.kind == "text" {
                        if let Some(feedback_prompt) =
                            conductor_core::agent::parse_feedback_marker(&ev.summary)
                        {
                            eprintln!("[conductor] Agent requesting feedback: {feedback_prompt}");
                            match mgr.request_feedback(run_id, feedback_prompt) {
                                Ok(fb) => {
                                    eprintln!(
                                        "[conductor] Waiting for human feedback (id: {})...",
                                        fb.id
                                    );
                                    // Poll for feedback response
                                    if let Some(response) = wait_for_feedback_response(&mgr, &fb.id)
                                    {
                                        eprintln!("[conductor] Feedback received: {response}");
                                        // Inject feedback as a user event
                                        let fb_now = chrono::Utc::now().to_rfc3339();
                                        let _ = mgr.create_event(
                                            run_id,
                                            "feedback",
                                            &format!("Human feedback: {response}"),
                                            &fb_now,
                                            None,
                                        );
                                    } else {
                                        eprintln!("[conductor] Feedback dismissed, continuing...");
                                    }
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[conductor] Warning: could not create feedback request: {e}"
                                    );
                                }
                            }
                        }
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
            if !db_updated_eagerly {
                mgr.update_run_completed(
                    run_id,
                    session_id_parsed.as_deref(),
                    result_text.as_deref(),
                    cost_usd,
                    num_turns,
                    duration_ms,
                )?;
            }
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
            let error_msg = result_text
                .as_deref()
                .unwrap_or(conductor_core::agent::DEFAULT_AGENT_ERROR_MSG);
            if !db_updated_eagerly {
                mgr.update_run_failed_with_session(
                    run_id,
                    error_msg,
                    session_id_parsed.as_deref(),
                )?;
            }
            eprintln!("[conductor] Agent failed: {}", error_msg);
        }
        Ok(s) => {
            // Non-zero exit without is_error — override any eager update
            let error_msg = format!("Claude exited with status: {}", s);
            mgr.update_run_failed_with_session(run_id, &error_msg, session_id_parsed.as_deref())?;
            eprintln!("[conductor] Agent failed: {}", error_msg);
        }
        Err(e) => {
            let error_msg = format!("Error waiting for claude: {e}");
            mgr.update_run_failed_with_session(run_id, &error_msg, session_id_parsed.as_deref())?;
            eprintln!("[conductor] {}", error_msg);
        }
    }

    eprintln!(
        "[conductor] Agent log saved to {}",
        log_path.to_string_lossy()
    );

    Ok(())
}

/// Poll the database for a feedback response. Returns the response text if responded,
/// or None if dismissed. Polls every 2 seconds for up to 1 hour.
fn wait_for_feedback_response(mgr: &AgentManager, feedback_id: &str) -> Option<String> {
    let max_polls = 1800; // 2s * 1800 = 1 hour
    for _ in 0..max_polls {
        std::thread::sleep(std::time::Duration::from_secs(2));

        match mgr.get_feedback(feedback_id) {
            Ok(Some(fb)) => match fb.status {
                conductor_core::agent::FeedbackStatus::Responded => return fb.response,
                conductor_core::agent::FeedbackStatus::Dismissed => return None,
                _ => continue, // still pending
            },
            Ok(None) => return None, // feedback request deleted
            Err(e) => {
                eprintln!("[conductor] Warning: error polling feedback: {e}");
                continue;
            }
        }
    }

    eprintln!("[conductor] Feedback request timed out after 1 hour");
    None
}

/// Run the orchestration: generate a plan, then spawn child agents for each step.
/// Note: feedback detection (`[NEEDS_FEEDBACK]`) is intentionally omitted here.
/// The orchestrator manages a plan and spawns child `run_agent` invocations;
/// each child has its own event loop that handles feedback markers.
fn run_orchestrate(
    conn: &rusqlite::Connection,
    config: &conductor_core::config::Config,
    run_id: &str,
    worktree_path: &str,
    model: Option<&str>,
    fail_fast: bool,
    child_timeout_secs: u64,
) -> Result<()> {
    let mgr = AgentManager::new(conn);

    // Verify the run exists
    let run = mgr.get_run(run_id)?;
    let run = match run {
        Some(r) => r,
        None => anyhow::bail!("agent run not found: {run_id}"),
    };

    // Build effective prompt with startup context
    let effective_prompt = if config.general.inject_startup_context {
        let context = build_startup_context(conn, &run.worktree_id, run_id, worktree_path);
        eprintln!("[orchestrator] Injecting session context into prompt");
        format!("{context}\n\n---\n\n{}", run.prompt)
    } else {
        run.prompt.clone()
    };

    // Phase 1: Generate plan
    eprintln!("[orchestrator] Generating plan...");
    let steps = generate_plan(worktree_path, &effective_prompt);
    match steps {
        Some(ref plan_steps) => {
            eprintln!("[orchestrator] Plan ({} steps):", plan_steps.len());
            for (i, step) in plan_steps.iter().enumerate() {
                eprintln!("  {}. {}", i + 1, step.description);
            }
            if let Err(e) = mgr.update_run_plan(run_id, plan_steps) {
                eprintln!("[orchestrator] Warning: could not save plan to DB: {e}");
            }
        }
        None => {
            let msg = "Plan generation returned no steps — cannot orchestrate";
            eprintln!("[orchestrator] {msg}");
            mgr.update_run_failed(run_id, msg)?;
            return Ok(());
        }
    }

    // Emit orchestration start event
    {
        let now = chrono::Utc::now().to_rfc3339();
        let _ = mgr.create_event(
            run_id,
            "system",
            &format!("Orchestrating {} plan steps", steps.as_ref().unwrap().len()),
            &now,
            None,
        );
    }

    // Phase 2: Orchestrate child runs
    eprintln!("[orchestrator] Starting child orchestration...");

    let orch_config = OrchestratorConfig {
        fail_fast,
        child_timeout: std::time::Duration::from_secs(child_timeout_secs),
        ..Default::default()
    };

    match orchestrator::orchestrate_run(conn, config, run_id, worktree_path, model, &orch_config) {
        Ok(result) => {
            if result.all_succeeded {
                eprintln!("[orchestrator] All steps completed successfully");
            } else {
                eprintln!("[orchestrator] Orchestration completed with failures");
            }
        }
        Err(e) => {
            eprintln!("[orchestrator] Orchestration failed: {e}");
            let _ = mgr.update_run_failed(run_id, &format!("Orchestration error: {e}"));
        }
    }

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

/// Sync issues for a single repo using the given fetch closure, printing results.
fn sync_repo(
    syncer: &TicketSyncer,
    repo_id: &str,
    repo_slug: &str,
    source_type: &str,
    label: &str,
    fetch: impl FnOnce() -> Result<Vec<TicketInput>, ConductorError>,
) {
    match fetch() {
        Ok(tickets) => {
            let synced_ids: Vec<&str> = tickets.iter().map(|t| t.source_id.as_str()).collect();
            match syncer.upsert_tickets(repo_id, &tickets) {
                Ok(count) => {
                    let closed = syncer
                        .close_missing_tickets(repo_id, source_type, &synced_ids)
                        .unwrap_or_else(|e| {
                            eprintln!("  {repo_slug} — warning: close_missing_tickets failed: {e}");
                            0
                        });
                    let merged = syncer
                        .mark_worktrees_for_closed_tickets(repo_id)
                        .unwrap_or_else(|e| {
                            eprintln!(
                                "  {repo_slug} — warning: mark_worktrees_for_closed_tickets failed: {e}"
                            );
                            0
                        });
                    print!("  {} — synced {count} {label}", repo_slug);
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
