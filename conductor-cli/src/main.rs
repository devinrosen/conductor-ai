use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

use anyhow::Result;
use clap::{Parser, Subcommand};

use conductor_core::agent::AgentManager;
use conductor_core::config::{ensure_dirs, load_config};
use conductor_core::db::open_database;
use conductor_core::github;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager, JiraConfig};
use conductor_core::jira_acli;
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
use conductor_core::session::SessionTracker;
use conductor_core::tickets::{build_agent_prompt, TicketSyncer};
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
                auto_agent,
            } => {
                let mgr = WorktreeManager::new(&conn, &config);
                let wt = mgr.create(&repo, &name, from.as_deref(), ticket.as_deref())?;
                println!("Created worktree: {} ({})", wt.slug, wt.branch);
                println!("  Path: {}", wt.path);

                if auto_agent {
                    if let Some(ref tid) = ticket {
                        let syncer = TicketSyncer::new(&conn);
                        match syncer.get_by_id(tid) {
                            Ok(t) => {
                                let prompt = build_agent_prompt(&t);
                                println!("Starting agent...");
                                let agent_mgr = AgentManager::new(&conn);
                                let run = agent_mgr.create_run(&wt.id, &prompt, Some(&wt.slug))?;
                                run_agent(&conn, &run.id, &wt.path, &prompt, None)?;
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

                let worktree_id: String = conn
                    .query_row(
                        "SELECT id FROM worktrees WHERE repo_id = ?1 AND slug = ?2",
                        rusqlite::params![r.id, worktree],
                        |row| row.get(0),
                    )
                    .map_err(|_| anyhow::anyhow!("Worktree not found: {worktree}"))?;

                let syncer = TicketSyncer::new(&conn);
                syncer.link_to_worktree(&ticket_id, &worktree_id)?;
                println!("Linked ticket #{ticket} to worktree '{worktree}'");
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

    // Build the claude command in print mode with stream-json output.
    // stdout: stream-json events (piped, parsed for result metadata)
    // stderr: verbose turn-by-turn output (inherited, visible in tmux)
    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg(prompt)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--dangerously-skip-permissions")
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
        }
    }

    let status = child.wait();

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
        Ok(s) if is_error => {
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
