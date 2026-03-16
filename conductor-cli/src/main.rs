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
use conductor_core::orchestrator::{self, OrchestratorConfig};
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
use conductor_core::schema_config;
use conductor_core::tickets::{build_agent_prompt, TicketInput, TicketSyncer};
use conductor_core::workflow::{
    collect_agent_names, detect_workflow_cycles, validate_script_steps,
    validate_workflow_semantics, WorkflowExecConfig, WorkflowManager,
};
use conductor_core::workflow_config;
use conductor_core::worktree::WorktreeManager;

mod mcp;
mod statusline;

/// Environment variable name used to pass the current agent run ID to subprocesses.
const CONDUCTOR_RUN_ID_ENV: &str = "CONDUCTOR_RUN_ID";

#[derive(Parser)]
#[command(name = "conductor", about = "Multi-repo orchestration tool", version)]
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
    /// Multi-step workflow engine
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommands,
    },
    /// Manage Claude Code status line integration
    Statusline {
        #[command(subcommand)]
        command: StatuslineCommands,
    },
    /// Model Context Protocol server (stdio transport for Claude Code integration)
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },
}

#[derive(Subcommand)]
enum McpCommands {
    /// Start the conductor MCP server on stdio
    Serve,
}

#[derive(Subcommand)]
enum StatuslineCommands {
    /// Install the conductor status line into Claude Code
    Install,
    /// Uninstall the conductor status line from Claude Code
    Uninstall,
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
        /// Named GitHub App bot identity to use (matches [github.apps.<name>] in config).
        #[arg(long)]
        bot_name: Option<String>,
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
        /// Repo slug (required unless --path is given)
        #[arg(required_unless_present = "path")]
        repo: Option<String>,
        /// Worktree slug (required unless --path is given)
        #[arg(required_unless_present = "path")]
        worktree: Option<String>,
        /// Path to a repo root directory; skips DB lookup
        #[arg(long, conflicts_with_all = &["repo", "worktree"])]
        path: Option<String>,
    },
    /// Run a workflow
    #[command(
        after_help = "Examples:\n  conductor workflow run ticket-to-pr my-repo my-worktree\n  conductor workflow run ticket-to-pr my-repo my-worktree --input key=value\n  conductor workflow run draft-release-notes --pr https://github.com/org/repo/pull/42\n  conductor workflow run publish-docs --pr org/repo#42 --input force=true\n  conductor workflow run draft-release-notes --repo my-repo\n  conductor workflow run workflow-postmortem --workflow-run 01ABC123\n  conductor workflow run ticket-to-pr --ticket 01KKFYDVE7F0X5KPR9Q7CX6SJ3"
    )]
    Run {
        /// Workflow name (must match a .conductor/workflows/<name>.wf file)
        name: String,
        /// Repo slug (required unless --pr, --repo, --workflow-run, or --ticket is used)
        #[arg(required_unless_present_any = &["pr", "repo_flag", "workflow_run", "ticket"])]
        repo: Option<String>,
        /// Worktree slug (required unless --pr, --repo, --workflow-run, or --ticket is used)
        #[arg(required_unless_present_any = &["pr", "repo_flag", "workflow_run", "ticket"])]
        worktree: Option<String>,
        /// Run the workflow against a GitHub PR URL or reference (e.g. https://github.com/owner/repo/pull/123)
        #[arg(long, conflicts_with_all = &["repo", "worktree", "repo_flag", "workflow_run", "ticket"])]
        pr: Option<String>,
        /// Run a repo-targeted workflow without a worktree (conflicts with positional repo/worktree and --pr)
        #[arg(long = "repo", conflicts_with_all = &["repo", "worktree", "pr", "workflow_run", "ticket"])]
        repo_flag: Option<String>,
        /// Run the workflow targeting a prior workflow run (e.g. for postmortem workflows)
        #[arg(long, conflicts_with_all = &["repo", "worktree", "pr", "repo_flag", "ticket"])]
        workflow_run: Option<String>,
        /// Run the workflow against a ticket (ULID from `conductor ticket list`)
        #[arg(long, conflicts_with_all = &["repo", "worktree", "pr", "repo_flag", "workflow_run"])]
        ticket: Option<String>,
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
    #[command(name = "run-show", alias = "show")]
    RunShow {
        /// Workflow run ID
        id: String,
    },
    /// Validate a workflow definition (check all agents exist)
    Validate {
        /// Workflow name
        name: String,
        /// Repo slug (optional; auto-detected from CWD when omitted)
        repo: Option<String>,
        /// Worktree slug (optional; auto-detected from CWD when omitted)
        worktree: Option<String>,
        /// Path to a repo root directory; skips DB lookup
        #[arg(long, conflicts_with_all = &["repo", "worktree"])]
        path: Option<String>,
    },
    /// Resume a failed or stalled workflow run
    #[command(
        after_help = "Examples:\n  conductor workflow resume 01ABC123 --restart\n  conductor workflow resume 01ABC123 --from-step run-tests\n  # Find run IDs: conductor workflow runs my-repo"
    )]
    Resume {
        /// Workflow run ID
        id: String,
        /// Resume from a specific step name (re-runs from that step onward)
        #[arg(long)]
        from_step: Option<String>,
        /// Model override for agent steps
        #[arg(long)]
        model: Option<String>,
        /// Restart from the beginning (reuse same run record)
        #[arg(long)]
        restart: bool,
    },
    /// Cancel a running or waiting workflow
    Cancel {
        /// Workflow run ID
        id: String,
    },
    /// Approve a pending human gate
    #[command(
        after_help = "A gate pauses the workflow at a checkpoint waiting for human approval.\nFind the RUN_ID with: conductor workflow runs <repo>"
    )]
    GateApprove {
        /// Workflow run ID — find it with: conductor workflow runs <repo>
        #[arg(value_name = "RUN_ID")]
        run_id: String,
    },
    /// Reject a pending human gate (fails the workflow)
    #[command(
        after_help = "A gate pauses the workflow at a checkpoint waiting for human approval.\nFind the RUN_ID with: conductor workflow runs <repo>"
    )]
    GateReject {
        /// Workflow run ID — find it with: conductor workflow runs <repo>
        #[arg(value_name = "RUN_ID")]
        run_id: String,
    },
    /// Provide feedback and approve a pending human gate
    #[command(
        after_help = "A gate pauses the workflow at a checkpoint waiting for human approval.\nFind the RUN_ID with: conductor workflow runs <repo>"
    )]
    GateFeedback {
        /// Workflow run ID — find it with: conductor workflow runs <repo>
        #[arg(value_name = "RUN_ID")]
        run_id: String,
        /// Feedback text
        feedback: String,
    },
    /// Delete completed, failed, and cancelled workflow runs
    Purge {
        /// Only purge runs for this repo slug
        #[arg(long)]
        repo: Option<String>,
        /// Filter by status: completed, failed, cancelled, all (default: all terminal)
        #[arg(long)]
        status: Option<String>,
        /// Print what would be deleted without deleting
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum RepoCommands {
    /// Register a repository
    Register {
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
    /// Unregister a repository
    Unregister {
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
    #[command(
        after_help = "Examples:\n  conductor worktree create my-repo --ticket PROJ-42\n  conductor worktree create my-repo --from main\n  conductor worktree create my-repo --ticket PROJ-42 --auto-agent\n  conductor worktree create my-repo pr-42-fix --from-pr 42"
    )]
    Create {
        /// Repo slug
        repo: String,
        /// Worktree name (e.g., smart-playlists, fix-scan-crash)
        name: String,
        /// Base branch
        #[arg(long, short, conflicts_with = "from_pr")]
        from: Option<String>,
        /// Checkout an existing PR branch by PR number
        #[arg(long, conflicts_with = "from")]
        from_pr: Option<u32>,
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

fn check_prerequisites() {
    let mut missing = Vec::new();
    if Command::new("gh").arg("--version").output().is_err() {
        missing.push("  - gh (GitHub CLI): https://cli.github.com");
    }
    if Command::new("tmux").arg("-V").output().is_err() {
        missing.push("  - tmux: https://github.com/tmux/tmux");
    }
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        missing.push("  - ANTHROPIC_API_KEY (get a key at https://console.anthropic.com)");
    }
    if !missing.is_empty() {
        eprintln!("conductor: missing prerequisites:\n{}", missing.join("\n"));
        eprintln!("Some commands may not work until these are resolved.\n");
    }
}

fn report_workflow_result(result: conductor_core::workflow::WorkflowResult) {
    println!(
        "\nTotal: {} turns, {:.1}s",
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

fn main() -> Result<()> {
    // Initialize tracing subscriber so workflow engine log events appear on
    // stderr for CLI users.  Respects RUST_LOG; defaults to `info`.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let config = load_config()?;
    ensure_dirs(&config)?;

    let db_path = conductor_core::config::db_path();
    let conn = open_database(&db_path)?;

    check_prerequisites();

    match cli.command {
        Commands::Repo { command } => match command {
            RepoCommands::Register {
                remote_url,
                slug,
                local_path,
                workspace,
            } => {
                let slug = slug.unwrap_or_else(|| derive_slug_from_url(&remote_url));

                let local = local_path.unwrap_or_else(|| derive_local_path(&config, &slug));

                let mgr = RepoManager::new(&conn, &config);
                let repo = mgr.register(&slug, &local, &remote_url, workspace.as_deref())?;
                println!("Registered repo: {} ({})", repo.slug, repo.remote_url);
            }
            RepoCommands::List => {
                let mgr = RepoManager::new(&conn, &config);
                let repos = mgr.list()?;
                if repos.is_empty() {
                    println!("No repos registered. Use `conductor repo register` to register one.");
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
                        println!("Use `conductor repo register <url>` to register a repo.");
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
            RepoCommands::Unregister { slug } => {
                let mgr = RepoManager::new(&conn, &config);
                mgr.unregister(&slug)?;
                println!("Unregistered repo: {slug}");
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
                    from_pr,
                    ticket,
                    auto_agent,
                } => {
                    let mgr = WorktreeManager::new(&conn, &config);
                    let (wt, warnings) =
                        mgr.create(&repo, &name, from.as_deref(), ticket.as_deref(), from_pr)?;
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
                                        Some(&wt.id),
                                        &prompt,
                                        Some(&wt.slug),
                                        model,
                                    )?;
                                    run_agent(
                                        &conn, &run.id, &wt.path, &prompt, None, model, None,
                                    )?;
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
                if let Err(e) = agent_mgr.reap_orphaned_runs() {
                    eprintln!("Warning: reap_orphaned_runs failed: {e}");
                }
                let wf_mgr = WorkflowManager::new(&conn);
                if let Err(e) = wf_mgr.reap_orphaned_workflow_runs() {
                    eprintln!("Warning: reap_orphaned_workflow_runs failed: {e}");
                }
            }

            match command {
                AgentCommands::Run {
                    run_id,
                    worktree_path,
                    prompt,
                    prompt_file,
                    resume,
                    model,
                    bot_name,
                } => {
                    let resolved_prompt = match (prompt, prompt_file) {
                        (Some(p), _) => p,
                        (None, Some(path)) => read_and_maybe_cleanup_prompt_file(&path)?,
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
                        bot_name.as_deref(),
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

                    let worktree_id = run.worktree_id.as_deref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Cannot create issues from ephemeral workflow runs \
                             (run {run_id} has no registered worktree)"
                        )
                    })?;

                    let (repo_id, remote_url): (String, String) = conn
                        .query_row(
                            "SELECT r.id, r.remote_url \
                         FROM worktrees w \
                         JOIN repos r ON w.repo_id = r.id \
                         WHERE w.id = ?1",
                            rusqlite::params![worktree_id],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .map_err(|_| {
                            anyhow::anyhow!("Could not find repo for worktree {worktree_id}")
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
        },
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
            WorkflowCommands::List {
                repo,
                worktree,
                path,
            } => {
                let (wt_path, repo_path) = if let Some(ref dir) = path {
                    (dir.clone(), dir.clone())
                } else {
                    let repo_mgr = RepoManager::new(&conn, &config);
                    let r = repo_mgr.get_by_slug(repo.as_deref().unwrap())?;
                    let wt_mgr = WorktreeManager::new(&conn, &config);
                    let wt = wt_mgr.get_by_slug(&r.id, worktree.as_deref().unwrap())?;
                    (wt.path, r.local_path)
                };

                // Try new .wf files first, fall back to legacy .md
                let (wf_defs, wf_warnings) = WorkflowManager::list_defs(&wt_path, &repo_path)?;
                for w in &wf_warnings {
                    eprintln!("warning: Failed to parse {}: {}", w.file, w.message);
                }
                if !wf_defs.is_empty() {
                    for def in &wf_defs {
                        let node_count = def.total_nodes();
                        println!(
                            "  {:<20} {:<40} [{}, {} nodes]",
                            def.name, def.description, def.trigger, node_count
                        );
                    }
                } else {
                    let defs = workflow_config::load_workflow_defs(&wt_path, &repo_path)?;
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
                pr,
                repo_flag,
                workflow_run,
                ticket,
                model,
                dry_run,
                no_fail_fast,
                step_timeout_secs,
                inputs,
            } => {
                // Parse input key=value pairs (shared by both paths)
                let mut input_map = std::collections::HashMap::new();
                for input_str in &inputs {
                    if let Some((key, value)) = input_str.split_once('=') {
                        input_map.insert(key.to_string(), value.to_string());
                    } else {
                        anyhow::bail!("Invalid input format: '{}'. Use key=value.", input_str);
                    }
                }

                let exec_config = WorkflowExecConfig {
                    step_timeout: std::time::Duration::from_secs(step_timeout_secs),
                    fail_fast: !no_fail_fast,
                    dry_run,
                    ..Default::default()
                };

                if dry_run {
                    println!("DRY RUN: Actor steps will show intended changes without committing.");
                }

                if let Some(pr_url) = pr {
                    // Ephemeral PR run
                    let pr_ref = conductor_core::workflow_ephemeral::parse_pr_ref(&pr_url)?;

                    println!(
                        "Running workflow '{}' against PR #{} ({})...",
                        name,
                        pr_ref.number,
                        pr_ref.repo_slug()
                    );

                    match conductor_core::workflow_ephemeral::run_workflow_on_pr(
                        &conn,
                        &config,
                        &pr_ref,
                        &name,
                        model.as_deref(),
                        exec_config,
                        input_map,
                        dry_run,
                    ) {
                        Ok(result) => report_workflow_result(result),
                        Err(e) => {
                            eprintln!("Workflow execution failed: {e}");
                            std::process::exit(1);
                        }
                    }
                } else if let Some(repo_slug) = repo_flag {
                    // Repo-targeted workflow run (no worktree)
                    let repo_mgr = RepoManager::new(&conn, &config);
                    let r = repo_mgr.get_by_slug(&repo_slug)?;

                    let workflow =
                        WorkflowManager::load_def_by_name(&r.local_path, &r.local_path, &name)?;

                    if !workflow.targets.contains(&"repo".to_string()) {
                        eprintln!(
                            "Warning: workflow '{}' targets {:?}, not 'repo'. Proceeding anyway.",
                            name, workflow.targets
                        );
                    }

                    conductor_core::workflow::apply_workflow_input_defaults(
                        &workflow,
                        &mut input_map,
                    )?;

                    let node_count = workflow.total_nodes();
                    println!(
                        "Running workflow '{}' ({} nodes) on repo '{}'...",
                        workflow.name, node_count, repo_slug
                    );

                    match conductor_core::workflow::execute_workflow(
                        &conductor_core::workflow::WorkflowExecInput {
                            conn: &conn,
                            config: &config,
                            workflow: &workflow,
                            worktree_id: None,
                            working_dir: &r.local_path,
                            repo_path: &r.local_path,
                            ticket_id: None,
                            repo_id: Some(&r.id),
                            model: model.as_deref(),
                            exec_config: &exec_config,
                            inputs: input_map,
                            depth: 0,
                            parent_workflow_run_id: None,
                            target_label: Some(r.slug.as_str()),
                            default_bot_name: None,
                            run_id_notify: None,
                        },
                    ) {
                        Ok(result) => report_workflow_result(result),
                        Err(e) => {
                            eprintln!("Workflow execution failed: {e}");
                            std::process::exit(1);
                        }
                    }
                } else if let Some(run_id) = workflow_run {
                    // Workflow-run targeted run (e.g. postmortem workflows)
                    let wf_mgr = WorkflowManager::new(&conn);
                    let ctx = wf_mgr.resolve_run_context(&run_id, &config)?;

                    // Auto-inject the workflow_run_id input (user --input flags merge after)
                    input_map
                        .entry("workflow_run_id".to_string())
                        .or_insert_with(|| run_id.clone());

                    let workflow =
                        WorkflowManager::load_def_by_name(&ctx.working_dir, &ctx.repo_path, &name)?;

                    conductor_core::workflow::apply_workflow_input_defaults(
                        &workflow,
                        &mut input_map,
                    )?;

                    let node_count = workflow.total_nodes();
                    println!(
                        "Running workflow '{}' ({} nodes) on workflow run {}...",
                        workflow.name, node_count, run_id
                    );

                    match conductor_core::workflow::execute_workflow(
                        &conductor_core::workflow::WorkflowExecInput {
                            conn: &conn,
                            config: &config,
                            workflow: &workflow,
                            worktree_id: ctx.worktree_id.as_deref(),
                            working_dir: &ctx.working_dir,
                            repo_path: &ctx.repo_path,
                            ticket_id: None,
                            repo_id: ctx.repo_id.as_deref(),
                            model: model.as_deref(),
                            exec_config: &exec_config,
                            inputs: input_map,
                            depth: 0,
                            parent_workflow_run_id: None,
                            target_label: Some(run_id.as_str()),
                            default_bot_name: None,
                            run_id_notify: None,
                        },
                    ) {
                        Ok(result) => report_workflow_result(result),
                        Err(e) => {
                            eprintln!("Workflow execution failed: {e}");
                            std::process::exit(1);
                        }
                    }
                } else if let Some(ticket_id) = ticket {
                    let syncer = TicketSyncer::new(&conn);
                    let ticket = syncer.get_by_id(&ticket_id)?;
                    let repo_mgr = RepoManager::new(&conn, &config);
                    let repo = repo_mgr.get_by_id(&ticket.repo_id)?;

                    let workflow = WorkflowManager::load_def_by_name(
                        &repo.local_path,
                        &repo.local_path,
                        &name,
                    )?;

                    conductor_core::workflow::apply_workflow_input_defaults(
                        &workflow,
                        &mut input_map,
                    )?;

                    println!(
                        "Running workflow '{}' ({} nodes) on ticket {}...",
                        workflow.name,
                        workflow.total_nodes(),
                        ticket_id
                    );

                    match conductor_core::workflow::execute_workflow(
                        &conductor_core::workflow::WorkflowExecInput {
                            conn: &conn,
                            config: &config,
                            workflow: &workflow,
                            worktree_id: None,
                            working_dir: &repo.local_path,
                            repo_path: &repo.local_path,
                            ticket_id: Some(&ticket_id),
                            repo_id: Some(&ticket.repo_id),
                            model: model.as_deref(),
                            exec_config: &exec_config,
                            inputs: input_map,
                            depth: 0,
                            parent_workflow_run_id: None,
                            target_label: Some(repo.slug.as_str()),
                            default_bot_name: None,
                            run_id_notify: None,
                        },
                    ) {
                        Ok(result) => report_workflow_result(result),
                        Err(e) => {
                            eprintln!("Workflow execution failed: {e}");
                            std::process::exit(1);
                        }
                    }
                } else {
                    // Normal registered repo/worktree run
                    let repo_slug = repo.expect("repo is required when --pr is not used");
                    let worktree_slug =
                        worktree.expect("worktree is required when --pr is not used");

                    let repo_mgr = RepoManager::new(&conn, &config);
                    let r = repo_mgr.get_by_slug(&repo_slug)?;
                    let wt_mgr = WorktreeManager::new(&conn, &config);
                    let wt = wt_mgr.get_by_slug(&r.id, &worktree_slug)?;

                    let workflow =
                        WorkflowManager::load_def_by_name(&wt.path, &r.local_path, &name)?;

                    // Validate required inputs and apply defaults
                    conductor_core::workflow::apply_workflow_input_defaults(
                        &workflow,
                        &mut input_map,
                    )?;

                    let node_count = workflow.total_nodes();
                    println!(
                        "Running workflow '{}' ({} nodes) on {}/{}...",
                        workflow.name, node_count, repo_slug, worktree_slug
                    );

                    let wt_label = format!("{repo_slug}/{worktree_slug}");
                    match conductor_core::workflow::execute_workflow(
                        &conductor_core::workflow::WorkflowExecInput {
                            conn: &conn,
                            config: &config,
                            workflow: &workflow,
                            worktree_id: Some(&wt.id),
                            working_dir: &wt.path,
                            repo_path: &r.local_path,
                            ticket_id: wt.ticket_id.as_deref(),
                            repo_id: None,
                            model: model.as_deref(),
                            exec_config: &exec_config,
                            inputs: input_map,
                            depth: 0,
                            parent_workflow_run_id: None,
                            target_label: Some(&wt_label),
                            default_bot_name: None,
                            run_id_notify: None,
                        },
                    ) {
                        Ok(result) => report_workflow_result(result),
                        Err(e) => {
                            eprintln!("Workflow execution failed: {e}");
                            std::process::exit(1);
                        }
                    }
                }
            }
            WorkflowCommands::RunShow { id } => {
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
                        if !run.inputs.is_empty() {
                            println!("  Inputs:");
                            let mut sorted_inputs: Vec<_> = run.inputs.iter().collect();
                            sorted_inputs.sort_by_key(|(k, _)| k.as_str());
                            for (k, v) in sorted_inputs {
                                println!("    {k}: {v}");
                            }
                        }
                        if let Some(ref snapshot) = run.definition_snapshot {
                            println!("  Definition snapshot:");
                            for line in snapshot.lines() {
                                println!("    {line}");
                            }
                        }
                        if let Some(ref summary) = run.result_summary {
                            println!("\n{summary}");
                        }

                        let steps = wf_mgr.get_workflow_steps(&run.id)?;
                        if !steps.is_empty() {
                            println!("\nSteps:");
                            for step in &steps {
                                let marker = step.status.short_label();
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
                                if let Some(ref started) = step.started_at {
                                    print!("        started: {started}");
                                    if let Some(ref ended) = step.ended_at {
                                        print!("  ended: {ended}");
                                    }
                                    println!();
                                }
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
                                if let Some(ref expr) = step.condition_expr {
                                    let met = step
                                        .condition_met
                                        .map(|b| if b { "true" } else { "false" })
                                        .unwrap_or("(unevaluated)");
                                    println!("        condition: {expr} => {met}");
                                }
                                if let Some(ref markers) = step.markers_out {
                                    println!("        markers: {markers}");
                                }
                                if let Some(ref ctx) = step.context_out {
                                    if !ctx.is_empty() {
                                        println!("        context: {ctx}");
                                    }
                                }
                                if let Some(ref result) = step.result_text {
                                    if !result.is_empty() {
                                        println!("        result: {result}");
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
                path,
            } => {
                let (wt_path, repo_path) = if let Some(ref dir) = path {
                    (dir.clone(), dir.clone())
                } else if repo.is_some() && worktree.is_some() {
                    let repo_mgr = RepoManager::new(&conn, &config);
                    let r = repo_mgr.get_by_slug(repo.as_deref().unwrap())?;
                    let wt_mgr = WorktreeManager::new(&conn, &config);
                    let wt = wt_mgr.get_by_slug(&r.id, worktree.as_deref().unwrap())?;
                    (wt.path, r.local_path)
                } else if repo.is_some() || worktree.is_some() {
                    anyhow::bail!(
                        "--repo and --worktree must be supplied together; \
                         got only {}. Pass both or omit both to auto-detect from CWD.",
                        if repo.is_some() {
                            "--repo"
                        } else {
                            "--worktree"
                        }
                    );
                } else {
                    let cwd = std::env::current_dir()?;
                    let wt_mgr = WorktreeManager::new(&conn, &config);
                    let wt = wt_mgr.find_by_cwd(&cwd)?.ok_or_else(|| {
                        anyhow::anyhow!(
                            "Could not detect repo/worktree from current directory. \
                             Run from inside a conductor-managed worktree, or pass <repo> <worktree> explicitly."
                        )
                    })?;
                    let repo_mgr = RepoManager::new(&conn, &config);
                    let r = repo_mgr.get_by_id(&wt.repo_id)?;
                    (wt.path, r.local_path)
                };

                let workflow = WorkflowManager::load_def_by_name(&wt_path, &repo_path, &name)?;

                let mut all_refs = collect_agent_names(&workflow.body);
                all_refs.extend(collect_agent_names(&workflow.always));

                // Deduplicate
                all_refs.sort();
                all_refs.dedup();

                let specs: Vec<AgentSpec> = all_refs.iter().map(AgentSpec::from).collect();
                let missing = conductor_core::agent_config::find_missing_agents(
                    &wt_path,
                    &repo_path,
                    &specs,
                    Some(&name),
                );

                println!("Workflow: {}", workflow.name);
                println!("  Description: {}", workflow.description);
                println!("  Trigger: {}", workflow.trigger);
                let node_count = workflow.total_nodes();
                println!("  Nodes: {node_count}");
                println!("  Agents referenced: {}", all_refs.len());

                // Collect and validate prompt snippets
                let all_snippets = workflow.collect_all_snippet_refs();

                let missing_snippets = conductor_core::prompt_config::find_missing_snippets(
                    &wt_path,
                    &repo_path,
                    &all_snippets,
                    Some(&name),
                );

                let mut has_errors = false;

                if missing.is_empty() {
                    println!("  All agents found.");
                } else {
                    println!("\n  MISSING agents ({}/{}):", missing.len(), all_refs.len());
                    for agent in &missing {
                        println!("    - {agent}");
                    }
                    has_errors = true;
                }

                if !all_snippets.is_empty() {
                    println!("  Prompt snippets referenced: {}", all_snippets.len());
                    if missing_snippets.is_empty() {
                        println!("  All prompt snippets found.");
                    } else {
                        println!(
                            "\n  MISSING prompt snippets ({}/{}):",
                            missing_snippets.len(),
                            all_snippets.len()
                        );
                        for snippet in &missing_snippets {
                            println!("    - {snippet}");
                        }
                        has_errors = true;
                    }
                }

                // Collect and validate output schemas.
                let all_schemas = workflow.collect_all_schema_refs();
                let schema_issues =
                    schema_config::check_schemas(&wt_path, &repo_path, &all_schemas, Some(&name));
                if !all_schemas.is_empty() {
                    println!("  Schemas referenced: {}", all_schemas.len());
                    if schema_issues.is_empty() {
                        println!("  All schemas found and valid.");
                    } else {
                        for issue in &schema_issues {
                            match issue {
                                schema_config::SchemaIssue::Missing(s) => {
                                    println!("\n  MISSING schema: {s}");
                                }
                                schema_config::SchemaIssue::Invalid { name: s, error } => {
                                    println!("\n  INVALID schema: {s}\n    {error}");
                                }
                            }
                        }
                        has_errors = true;
                    }
                }

                // Check bot names against config.
                let all_bots = workflow.collect_all_bot_names();
                let unknown_bots: Vec<_> = all_bots
                    .iter()
                    .filter(|b| !config.github.apps.contains_key(b.as_str()))
                    .cloned()
                    .collect();
                if !all_bots.is_empty() {
                    println!("  Bot names referenced: {}", all_bots.len());
                    if unknown_bots.is_empty() {
                        println!("  All bot names found in config.");
                    } else {
                        println!(
                            "\n  WARNING: unknown bot names ({}/{}):",
                            unknown_bots.len(),
                            all_bots.len()
                        );
                        for b in &unknown_bots {
                            println!("    ~ {b} (not in [github.apps])");
                        }
                        // does NOT set has_errors = true — bot config is per-environment
                    }
                }

                // Build a loader closure for cycle detection and semantic validation.
                let wt_path = wt_path.clone();
                let repo_path = repo_path.clone();
                let loader = |wf_name: &str| {
                    WorkflowManager::load_def_by_name(&wt_path, &repo_path, wf_name)
                        .map_err(|e| e.to_string())
                };

                // Cycle detection.
                if let Err(cycle_msg) = detect_workflow_cycles(&workflow.name, &loader) {
                    println!("\n  CYCLE DETECTED: {cycle_msg}");
                    has_errors = true;
                }

                // Semantic validation (dataflow + required inputs).
                let report = validate_workflow_semantics(&workflow, &loader);
                if !report.is_ok() {
                    println!("\n  SEMANTIC ERRORS ({}):", report.errors.len());
                    for err in &report.errors {
                        println!("    \u{2717} {}", err.message);
                        if let Some(hint) = &err.hint {
                            println!("      hint: {hint}");
                        }
                    }
                    has_errors = true;
                }

                // Script step validation (existence + executable bit).
                let script_errors = validate_script_steps(&workflow, &wt_path, &repo_path);
                if !script_errors.is_empty() {
                    println!("\n  SCRIPT STEP ERRORS ({}):", script_errors.len());
                    for err in &script_errors {
                        println!("    \u{2717} {}", err.message);
                        if let Some(hint) = &err.hint {
                            println!("      hint: {hint}");
                        }
                    }
                    has_errors = true;
                }

                if has_errors {
                    std::process::exit(1);
                }
            }
            WorkflowCommands::Resume {
                id,
                from_step,
                model,
                restart,
            } => {
                let resume_input = conductor_core::workflow::WorkflowResumeInput {
                    conn: &conn,
                    config: &config,
                    workflow_run_id: &id,
                    model: model.as_deref(),
                    from_step: from_step.as_deref(),
                    restart,
                };

                if restart {
                    println!("Restarting workflow run {id} from the beginning...");
                } else if let Some(ref step) = from_step {
                    println!("Resuming workflow run {id} from step '{step}'...");
                } else {
                    println!("Resuming workflow run {id}...");
                }

                match conductor_core::workflow::resume_workflow(&resume_input) {
                    Ok(result) => {
                        println!(
                            "\nTotal: ${:.4}, {} turns, {:.1}s",
                            result.total_cost,
                            result.total_turns,
                            result.total_duration_ms as f64 / 1000.0
                        );
                        if result.all_succeeded {
                            println!("Workflow resumed and completed successfully.");
                        } else {
                            eprintln!("Workflow resumed but finished with failures.");
                            std::process::exit(1);
                        }
                    }
                    Err(e) => {
                        eprintln!("Workflow resume failed: {e}");
                        std::process::exit(1);
                    }
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
                        wf_mgr.reject_gate(&step.id, &user, None)?;
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
            WorkflowCommands::Purge {
                repo,
                status,
                dry_run,
            } => {
                const ALLOWED: &[&str] = &["completed", "failed", "cancelled"];
                let status_val = status.as_deref().unwrap_or("all");
                let statuses: Vec<&str> = if status_val == "all" {
                    ALLOWED.to_vec()
                } else if ALLOWED.contains(&status_val) {
                    vec![status_val]
                } else {
                    anyhow::bail!(
                        "Unknown status '{status_val}'. Allowed values: completed, failed, cancelled, all"
                    );
                };

                let repo_id: Option<String> = if let Some(slug) = &repo {
                    let repo_mgr = RepoManager::new(&conn, &config);
                    let r = repo_mgr.get_by_slug(slug)?;
                    Some(r.id)
                } else {
                    None
                };

                let wf_mgr = WorkflowManager::new(&conn);
                if dry_run {
                    let count = wf_mgr.purge_count(repo_id.as_deref(), &statuses)?;
                    println!("Would purge {count} workflow run(s) (dry run).");
                } else {
                    let count = wf_mgr.purge(repo_id.as_deref(), &statuses)?;
                    println!("Purged {count} workflow run(s).");
                }
            }
        },
        Commands::Statusline { command } => match command {
            StatuslineCommands::Install => statusline::install()?,
            StatuslineCommands::Uninstall => statusline::uninstall()?,
        },
        Commands::Mcp { command } => match command {
            McpCommands::Serve => {
                let rt = tokio::runtime::Runtime::new()
                    .context("failed to create tokio runtime for MCP server")?;
                rt.block_on(mcp::serve())?;
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
    bot_name: Option<&str>,
) -> Result<()> {
    let mgr = AgentManager::new(conn);

    // Verify the run exists
    let run = mgr.get_run(run_id)?;
    let run = match run {
        Some(r) => r,
        None => anyhow::bail!("agent run not found: {run_id}"),
    };

    // Build effective prompt with optional startup context
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[conductor] Warning: failed to load config, GitHub App auth will be skipped: {e}"
            );
            conductor_core::config::Config::default()
        }
    };
    let effective_prompt = if config.general.inject_startup_context {
        let context =
            build_startup_context(conn, run.worktree_id.as_deref(), run_id, worktree_path);
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
        if let Some(wt_id) = run.worktree_id.as_deref() {
            if let Ok(prev_runs) = mgr.list_for_worktree(wt_id) {
                let prev_run = prev_runs.iter().find(|r| {
                    r.claude_session_id.as_deref() == resume_session_id && r.id != run_id
                });
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
        }
        eprintln!("[conductor] Resuming session...");
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

    // Set up log file path once; created on turn 0, appended on feedback resume turns.
    let log_dir = conductor_core::config::agent_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = conductor_core::config::agent_log_path(run_id);

    // session_id persists across turns so feedback resumes can use --resume <sid>
    let mut session_id_parsed: Option<String> = None;

    // Accumulated cost/token stats across all turns
    let mut acc_cost_usd: f64 = 0.0;
    let mut acc_input_tokens: i64 = 0;
    let mut acc_output_tokens: i64 = 0;
    let mut acc_cache_read_tokens: i64 = 0;
    let mut acc_cache_creation_tokens: i64 = 0;
    let mut acc_num_turns: i64 = 0;
    let mut acc_duration_ms: i64 = 0;

    // When Some, the next loop iteration is a feedback resume turn
    let mut feedback_response_for_resume: Option<String> = None;
    // True once at least one feedback resume turn has completed; used to decide
    // whether to override the eager DB update with accumulated totals at the end.
    let mut had_feedback_resume = false;

    loop {
        // ── per-turn mutable state ────────────────────────────────────────────
        let mut pending_feedback_id: Option<String> = None;
        let mut result_text: Option<String> = None;
        let mut cost_usd: Option<f64> = None;
        let mut num_turns: Option<i64> = None;
        let mut duration_ms: Option<i64> = None;
        let mut is_error = false;
        let mut input_tokens: Option<i64> = None;
        let mut output_tokens: Option<i64> = None;
        let mut cache_read_input_tokens: Option<i64> = None;
        let mut cache_creation_input_tokens: Option<i64> = None;
        let mut db_updated_eagerly = false;
        let mut last_event_id: Option<String> = None;

        // ── build command for this turn ───────────────────────────────────────
        // stdout: stream-json events (piped, parsed for result metadata)
        // stderr: verbose turn-by-turn output (inherited, visible in tmux)
        let mut cmd = Command::new("claude");
        if let Some(ref feedback) = feedback_response_for_resume {
            // Feedback resume turn: deliver the human response as the next message
            let sid = session_id_parsed
                .as_deref()
                .expect("session_id always captured before a feedback resume");
            cmd.arg("-p").arg(feedback).arg("--resume").arg(sid);
        } else {
            cmd.arg("-p").arg(&effective_prompt);
            if let Some(ref sid) = resume_session_id {
                cmd.arg("--resume").arg(sid);
            }
        }
        cmd.arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--dangerously-skip-permissions")
            .env(CONDUCTOR_RUN_ID_ENV, run_id)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .current_dir(worktree_path);

        // Inject GH_TOKEN from the GitHub App installation token so all `gh` calls
        // made by the agent (including `gh pr create`) use the bot identity rather
        // than the human `gh` CLI user. Fall back gracefully when not configured.
        match github_app::resolve_named_app_token(&config, bot_name, "agent-run") {
            github_app::TokenResolution::AppToken(token) => {
                if let Some(name) = bot_name {
                    eprintln!("[conductor] Using GitHub App token for bot identity: {name}");
                }
                cmd.env("GH_TOKEN", token);
            }
            github_app::TokenResolution::Fallback { reason } => {
                eprintln!(
                    "[conductor] Warning: GitHub App token failed, agents will use gh user identity: {reason}"
                );
            }
            github_app::TokenResolution::NotConfigured => {
                if let Some(name) = bot_name {
                    eprintln!(
                        "[conductor] Warning: bot name '{name}' specified but no matching GitHub App found in config — agents will use gh user identity"
                    );
                }
            }
        }

        if let Some(m) = model {
            cmd.arg("--model").arg(m);
        }

        // ── open log file (create on turn 0, append on feedback resume turns) ─
        let mut log_file = if feedback_response_for_resume.is_some() {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .ok()
        } else {
            let f = std::fs::File::create(&log_path).ok();
            // Store log file path in DB so the TUI can read streaming events.
            if f.is_some() {
                let path_str = log_path.to_string_lossy().to_string();
                if let Err(e) = mgr.update_run_log_file(run_id, &path_str) {
                    eprintln!("[conductor] Warning: could not save log path to DB: {e}");
                }
            }
            f
        };

        // ── spawn ─────────────────────────────────────────────────────────────
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                let error_msg = format!("Failed to spawn claude: {e}");
                mgr.update_run_failed(run_id, &error_msg)?;
                eprintln!("[conductor] {}", error_msg);
                return Ok(());
            }
        };

        // ── drain stdout ──────────────────────────────────────────────────────
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
                // Skip the eager update when feedback is pending — we must stay in
                // waiting_for_feedback until the human responds.
                if event.get("result").is_some() {
                    let parsed = conductor_core::agent::parse_result_event(&event);
                    result_text = parsed.result_text;
                    cost_usd = parsed.cost_usd;
                    num_turns = parsed.num_turns;
                    duration_ms = parsed.duration_ms;
                    is_error = parsed.is_error;
                    input_tokens = parsed.input_tokens;
                    output_tokens = parsed.output_tokens;
                    cache_read_input_tokens = parsed.cache_read_input_tokens;
                    cache_creation_input_tokens = parsed.cache_creation_input_tokens;

                    // Only eagerly mark complete/failed when no feedback is pending.
                    // If feedback was requested, the run must stay in waiting_for_feedback
                    // until the human responds; resume_run_after_feedback() transitions it
                    // back to running when the TUI submits the response.
                    if pending_feedback_id.is_none() {
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
                            input_tokens,
                            output_tokens,
                            cache_read_input_tokens,
                            cache_creation_input_tokens,
                        ) {
                            eprintln!("[conductor] Warning: eager DB update failed: {e}");
                        }
                        db_updated_eagerly = true;
                    }
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
                            Err(e) => {
                                eprintln!("[conductor] Warning: could not persist event: {e}")
                            }
                        }

                        // Detect feedback request markers in agent text output.
                        // Record the pending feedback id but do NOT block here — blocking
                        // would fill the OS pipe buffer (64 KB) if Claude keeps writing,
                        // causing a deadlock. We wait after child.wait() instead.
                        if ev.kind == "text" {
                            if let Some(feedback_prompt) =
                                conductor_core::agent::parse_feedback_marker(&ev.summary)
                            {
                                eprintln!(
                                    "[conductor] Agent requesting feedback: {feedback_prompt}"
                                );
                                match mgr.request_feedback(run_id, feedback_prompt) {
                                    Ok(fb) => {
                                        eprintln!(
                                            "[conductor] Feedback requested (id: {}), will wait after turn completes",
                                            fb.id
                                        );
                                        pending_feedback_id = Some(fb.id);
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

        // ── wait for child to exit ────────────────────────────────────────────
        let status = child.wait();

        let end_time = chrono::Utc::now().to_rfc3339();

        // Close the last open event span
        if let Some(ref prev_id) = last_event_id {
            let _ = mgr.update_event_ended_at(prev_id, &end_time);
        }

        // ── accumulate stats ──────────────────────────────────────────────────
        acc_cost_usd += cost_usd.unwrap_or(0.0);
        acc_input_tokens += input_tokens.unwrap_or(0);
        acc_output_tokens += output_tokens.unwrap_or(0);
        acc_cache_read_tokens += cache_read_input_tokens.unwrap_or(0);
        acc_cache_creation_tokens += cache_creation_input_tokens.unwrap_or(0);
        acc_num_turns += num_turns.unwrap_or(0);
        acc_duration_ms += duration_ms.unwrap_or(0);

        // ── deliver feedback and loop, or fall through to completion ──────────
        // Now that stdout is at EOF, it is safe to block waiting for the human.
        if let Some(ref feedback_id) = pending_feedback_id {
            if !is_error {
                eprintln!("[conductor] Waiting for human feedback (id: {feedback_id})...");
                if let Some(response) = wait_for_feedback_response(&mgr, feedback_id) {
                    eprintln!("[conductor] Feedback received, spawning resume turn...");
                    let fb_now = chrono::Utc::now().to_rfc3339();
                    let _ = mgr.create_event(
                        run_id,
                        "feedback",
                        &format!("Human feedback: {response}"),
                        &fb_now,
                        None,
                    );
                    if session_id_parsed.is_some() {
                        feedback_response_for_resume = Some(response);
                        had_feedback_resume = true;
                        continue; // spawn the resume turn
                    } else {
                        let msg =
                            "Cannot deliver feedback: no session_id captured from Claude output";
                        eprintln!("[conductor] Error: {msg}");
                        mgr.update_run_failed(run_id, msg)?;
                        return Ok(());
                    }
                }
                // Feedback dismissed or timed out — fall through to normal completion
                eprintln!("[conductor] Feedback dismissed or timed out, completing run");
            }
        }

        // ── final completion handling ─────────────────────────────────────────
        // When we had feedback resume turns, the eager update used per-turn values
        // for the last turn only. Override with accumulated totals for accuracy.
        let final_cost = if acc_cost_usd > 0.0 {
            Some(acc_cost_usd)
        } else {
            cost_usd
        };
        let final_num_turns = if acc_num_turns > 0 {
            Some(acc_num_turns)
        } else {
            num_turns
        };
        let final_duration_ms = if acc_duration_ms > 0 {
            Some(acc_duration_ms)
        } else {
            duration_ms
        };
        let final_input_tokens = if acc_input_tokens > 0 {
            Some(acc_input_tokens)
        } else {
            input_tokens
        };
        let final_output_tokens = if acc_output_tokens > 0 {
            Some(acc_output_tokens)
        } else {
            output_tokens
        };
        let final_cache_read = if acc_cache_read_tokens > 0 {
            Some(acc_cache_read_tokens)
        } else {
            cache_read_input_tokens
        };
        let final_cache_creation = if acc_cache_creation_tokens > 0 {
            Some(acc_cache_creation_tokens)
        } else {
            cache_creation_input_tokens
        };

        match status {
            Ok(s) if s.success() && !is_error => {
                if !db_updated_eagerly || had_feedback_resume {
                    mgr.update_run_completed(
                        run_id,
                        session_id_parsed.as_deref(),
                        result_text.as_deref(),
                        final_cost,
                        final_num_turns,
                        final_duration_ms,
                        final_input_tokens,
                        final_output_tokens,
                        final_cache_read,
                        final_cache_creation,
                    )?;
                }
                // Mark all plan steps done now that the run succeeded
                if let Err(e) = mgr.mark_plan_done(run_id) {
                    eprintln!("[conductor] Warning: could not mark plan done: {e}");
                }
                eprintln!("[conductor] Agent completed successfully");
                {
                    let fmt_k = |n: i64| -> String {
                        if n >= 1000 {
                            format!("{:.1}k", n as f64 / 1000.0)
                        } else {
                            n.to_string()
                        }
                    };
                    let in_str = fmt_k(final_input_tokens.unwrap_or(0));
                    let out_str = final_output_tokens.unwrap_or(0).to_string();
                    let cache_r_str = fmt_k(final_cache_read.unwrap_or(0));
                    let cache_w_str = fmt_k(final_cache_creation.unwrap_or(0));
                    let turns = final_num_turns.unwrap_or(0);
                    let dur = final_duration_ms
                        .map(|ms| ms as f64 / 1000.0)
                        .unwrap_or(0.0);
                    eprintln!(
                        "[conductor] in: {in_str}  out: {out_str}  cache_r: {cache_r_str}  cache_w: {cache_w_str}  turns: {turns}  duration: {dur:.1}s"
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
                mgr.update_run_failed_with_session(
                    run_id,
                    &error_msg,
                    session_id_parsed.as_deref(),
                )?;
                eprintln!("[conductor] Agent failed: {}", error_msg);
            }
            Err(e) => {
                let error_msg = format!("Error waiting for claude: {e}");
                mgr.update_run_failed_with_session(
                    run_id,
                    &error_msg,
                    session_id_parsed.as_deref(),
                )?;
                eprintln!("[conductor] {}", error_msg);
            }
        }

        break;
    } // end multi-turn loop

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
        let context =
            build_startup_context(conn, run.worktree_id.as_deref(), run_id, worktree_path);
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

/// Read `path` and, if it is an internal conductor temp file
/// (`.conductor-prompt-*.txt`), delete it afterwards.
///
/// User-supplied files passed via `--prompt-file` are left untouched.
fn read_and_maybe_cleanup_prompt_file(path: &str) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read prompt file: {path}"))?;
    if let Some(filename) = std::path::Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
    {
        if filename.starts_with(".conductor-prompt-") && filename.ends_with(".txt") {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::read_and_maybe_cleanup_prompt_file;

    #[test]
    fn internal_temp_file_is_deleted_after_read() {
        let tmp = std::env::temp_dir();
        let path = tmp.join(".conductor-prompt-run-abc123.txt");
        std::fs::write(&path, "hello").unwrap();
        let content = read_and_maybe_cleanup_prompt_file(path.to_str().unwrap()).unwrap();
        assert_eq!(content, "hello");
        assert!(
            !path.exists(),
            "internal temp file should have been deleted"
        );
    }

    #[test]
    fn user_prompt_file_is_not_deleted_after_read() {
        let tmp = std::env::temp_dir();
        let path = tmp.join("my-custom-prompt.txt");
        std::fs::write(&path, "custom prompt").unwrap();
        let content = read_and_maybe_cleanup_prompt_file(path.to_str().unwrap()).unwrap();
        assert_eq!(content, "custom prompt");
        assert!(
            path.exists(),
            "user-provided prompt file should not be deleted"
        );
        let _ = std::fs::remove_file(&path);
    }
}
