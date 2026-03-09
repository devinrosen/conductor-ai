use std::sync::atomic::{AtomicI64, Ordering};
use std::thread;
use std::time::Duration;

use conductor_core::agent::AgentManager;
use conductor_core::config::{db_path, load_config};
use conductor_core::db::open_database;
use conductor_core::error::ConductorError;
use conductor_core::github;
use conductor_core::github_app;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager, JiraConfig};
use conductor_core::jira_acli;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::{TicketInput, TicketSyncer};
use conductor_core::worktree::WorktreeManager;

use crate::action::{Action, DataRefreshedPayload, WorkflowDataPayload};
use crate::event::BackgroundSender;

/// Spawn the DB poller thread. Polls every `interval` and sends DataRefreshed events.
pub fn spawn_db_poller(tx: BackgroundSender, interval: Duration) {
    thread::spawn(move || loop {
        thread::sleep(interval);
        if let Some(action) = poll_data() {
            if !tx.send(action) {
                break;
            }
        }
    });
}

/// Poll all data from the database. Returns a DataRefreshed action if successful.
pub fn poll_data() -> Option<Action> {
    let db = db_path();
    let conn = open_database(&db).ok()?;
    let config = load_config().ok()?;

    let repo_mgr = RepoManager::new(&conn, &config);
    let wt_mgr = WorktreeManager::new(&conn, &config);
    let ticket_syncer = TicketSyncer::new(&conn);
    let agent_mgr = AgentManager::new(&conn);

    // Reap orphaned runs whose tmux windows have disappeared and clean up
    // stale worktrees whose artifacts persist on disk after merge/abandon.
    // Throttle to at most once every 30 seconds to avoid spawning tmux
    // subprocesses on every poll tick.
    {
        static LAST_REAP: AtomicI64 = AtomicI64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        if now - LAST_REAP.load(Ordering::Relaxed) >= 30 {
            LAST_REAP.store(now, Ordering::Relaxed);
            let _ = agent_mgr.reap_orphaned_runs();
            let _ = wt_mgr.reap_stale_worktrees();
        }
    }

    let repos = repo_mgr.list().ok()?;
    let worktrees = wt_mgr.list(None, true).ok()?;
    let tickets = ticket_syncer.list(None).ok()?;
    let latest_agent_runs = agent_mgr.latest_runs_by_worktree().unwrap_or_default();
    let ticket_agent_totals = agent_mgr.totals_by_ticket_all().unwrap_or_default();

    use conductor_core::workflow::WorkflowManager;
    let wf_mgr = WorkflowManager::new(&conn);
    // Build a per-worktree map of the most recent run (any status) for inline indicators.
    // Fetch recent runs sorted DESC; the first entry per worktree_id wins.
    let mut latest_workflow_runs_by_worktree = std::collections::HashMap::new();
    for run in wf_mgr.list_all_workflow_runs(100).unwrap_or_default() {
        latest_workflow_runs_by_worktree
            .entry(run.worktree_id.clone())
            .or_insert(run);
    }

    Some(Action::DataRefreshed(Box::new(DataRefreshedPayload {
        repos,
        worktrees,
        tickets,
        latest_agent_runs,
        ticket_agent_totals,
        latest_workflow_runs_by_worktree,
    })))
}

/// Spawn the ticket sync timer. Syncs all repos every `interval`.
pub fn spawn_ticket_sync(tx: BackgroundSender, interval: Duration) {
    thread::spawn(move || loop {
        thread::sleep(interval);
        sync_all_tickets(&tx);
    });
}

/// Spawn a one-shot ticket sync for all repos. Sends per-repo
/// `TicketSyncComplete`/`TicketSyncFailed` actions followed by a final
/// `TicketSyncDone` when all repos have been processed.
pub fn spawn_ticket_sync_once(tx: BackgroundSender) {
    thread::spawn(move || {
        sync_all_tickets(&tx);
        if !tx.send(Action::TicketSyncDone) {
            eprintln!("failed to send TicketSyncDone: channel closed");
        }
    });
}

fn sync_all_tickets(tx: &BackgroundSender) {
    let db = db_path();
    let Ok(conn) = open_database(&db) else { return };
    let Ok(config) = load_config() else { return };

    let repo_mgr = RepoManager::new(&conn, &config);
    let Ok(repos) = repo_mgr.list() else { return };

    let syncer = TicketSyncer::new(&conn);
    let source_mgr = IssueSourceManager::new(&conn);
    let token_res = github_app::resolve_app_token(&config, "github-issues-sync");
    let token = token_res.token();

    for repo in repos {
        let sources = source_mgr.list(&repo.id).unwrap_or_default();

        if sources.is_empty() {
            // Backward compat: auto-detect GitHub from remote_url
            if let Some((owner, name)) = github::parse_github_remote(&repo.remote_url) {
                let action = sync_repo(&syncer, &repo.id, &repo.slug, "github", || {
                    github::sync_github_issues(&owner, &name, token)
                });
                if !tx.send(action) {
                    return;
                }
            }
        } else {
            for source in sources {
                match source.source_type.as_str() {
                    "github" => {
                        let action = match serde_json::from_str::<GitHubConfig>(&source.config_json)
                        {
                            Ok(cfg) => sync_repo(&syncer, &repo.id, &repo.slug, "github", || {
                                github::sync_github_issues(&cfg.owner, &cfg.repo, token)
                            }),
                            Err(e) => Action::TicketSyncFailed {
                                repo_slug: repo.slug.clone(),
                                error: format!("invalid github config: {e}"),
                            },
                        };
                        if !tx.send(action) {
                            return;
                        }
                    }
                    "jira" => {
                        let action = match serde_json::from_str::<JiraConfig>(&source.config_json) {
                            Ok(cfg) => sync_repo(&syncer, &repo.id, &repo.slug, "jira", || {
                                jira_acli::sync_jira_issues_acli(&cfg.jql, &cfg.url)
                            }),
                            Err(e) => Action::TicketSyncFailed {
                                repo_slug: repo.slug.clone(),
                                error: format!("invalid jira config: {e}"),
                            },
                        };
                        if !tx.send(action) {
                            return;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Sync issues for a single repo using the given fetch closure, returning the appropriate Action.
fn sync_repo(
    syncer: &TicketSyncer,
    repo_id: &str,
    repo_slug: &str,
    source_type: &str,
    fetch: impl FnOnce() -> Result<Vec<TicketInput>, ConductorError>,
) -> Action {
    match fetch() {
        Ok(tickets) => {
            let synced_ids: Vec<&str> = tickets.iter().map(|t| t.source_id.as_str()).collect();
            match syncer.upsert_tickets(repo_id, &tickets) {
                Ok(count) => {
                    if let Err(e) = syncer.close_missing_tickets(repo_id, source_type, &synced_ids)
                    {
                        eprintln!("warn: close_missing_tickets failed for {repo_slug}: {e}");
                    }
                    if let Err(e) = syncer.mark_worktrees_for_closed_tickets(repo_id) {
                        eprintln!(
                            "warn: mark_worktrees_for_closed_tickets failed for {repo_slug}: {e}"
                        );
                    }
                    Action::TicketSyncComplete {
                        repo_slug: repo_slug.to_string(),
                        count,
                    }
                }
                Err(e) => Action::TicketSyncFailed {
                    repo_slug: repo_slug.to_string(),
                    error: e.to_string(),
                },
            }
        }
        Err(e) => Action::TicketSyncFailed {
            repo_slug: repo_slug.to_string(),
            error: e.to_string(),
        },
    }
}

/// Spawn the workflow data poller. Polls workflow runs/steps for the given
/// worktree and run IDs every `interval` and sends WorkflowDataRefreshed events.
#[allow(dead_code)]
pub fn spawn_workflow_poller(
    tx: BackgroundSender,
    interval: Duration,
    worktree_id: Option<String>,
    worktree_path: Option<String>,
    repo_path: Option<String>,
    selected_run_id: Option<String>,
    selected_step_child_run_id: Option<String>,
) {
    thread::spawn(move || loop {
        thread::sleep(interval);
        if let Some(action) = poll_workflow_data(
            worktree_id.as_deref(),
            worktree_path.as_deref(),
            repo_path.as_deref(),
            selected_run_id.as_deref(),
            selected_step_child_run_id.as_deref(),
        ) {
            if !tx.send(action) {
                break;
            }
        }
    });
}

fn poll_workflow_data(
    worktree_id: Option<&str>,
    worktree_path: Option<&str>,
    repo_path: Option<&str>,
    selected_run_id: Option<&str>,
    selected_step_child_run_id: Option<&str>,
) -> Option<Action> {
    use conductor_core::workflow::WorkflowManager;

    let db = db_path();
    let conn = open_database(&db).ok()?;

    // Load defs: scoped to one worktree, or aggregated across all worktrees in global mode.
    let defs = if let Some(wt_path) = worktree_path {
        WorkflowManager::list_defs(wt_path, repo_path.unwrap_or("")).unwrap_or_default()
    } else {
        // Global mode: scan every registered worktree for workflow definitions.
        let mut all_defs = Vec::new();
        if let Ok(config) = conductor_core::config::load_config() {
            let wt_mgr = conductor_core::worktree::WorktreeManager::new(&conn, &config);
            let repo_mgr = conductor_core::repo::RepoManager::new(&conn, &config);
            let repo_paths: std::collections::HashMap<String, String> = repo_mgr
                .list()
                .unwrap_or_default()
                .into_iter()
                .map(|r| (r.id, r.local_path))
                .collect();
            for wt in wt_mgr.list(None, true).unwrap_or_default() {
                let rp = repo_paths
                    .get(&wt.repo_id)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let mut wt_defs = WorkflowManager::list_defs(&wt.path, rp).unwrap_or_default();
                all_defs.append(&mut wt_defs);
            }
        }
        all_defs
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let runs = wf_mgr
        .list_workflow_runs_for_scope(worktree_id, 50)
        .unwrap_or_default();
    let steps = if let Some(run_id) = selected_run_id {
        wf_mgr.get_workflow_steps(run_id).unwrap_or_default()
    } else {
        Vec::new()
    };

    // Load agent events for the selected step's child run
    let agent_mgr = AgentManager::new(&conn);
    let (step_agent_events, step_agent_run) = if let Some(child_run_id) = selected_step_child_run_id
    {
        let events = agent_mgr
            .list_events_for_run(child_run_id)
            .unwrap_or_default();
        let run = agent_mgr.get_run(child_run_id).ok().flatten();
        (events, run)
    } else {
        (Vec::new(), None)
    };

    Some(Action::WorkflowDataRefreshed(Box::new(
        WorkflowDataPayload {
            workflow_defs: defs,
            workflow_runs: runs,
            workflow_steps: steps,
            step_agent_events,
            step_agent_run,
        },
    )))
}

/// One-shot async workflow data poll. Spawns a thread that loads defs, runs,
/// and steps and sends a `WorkflowDataRefreshed` action back.
#[allow(dead_code)]
pub fn spawn_workflow_poll_once(
    tx: BackgroundSender,
    worktree_id: Option<String>,
    worktree_path: Option<String>,
    repo_path: Option<String>,
    selected_run_id: Option<String>,
    selected_step_child_run_id: Option<String>,
) {
    thread::spawn(move || {
        if let Some(action) = poll_workflow_data(
            worktree_id.as_deref(),
            worktree_path.as_deref(),
            repo_path.as_deref(),
            selected_run_id.as_deref(),
            selected_step_child_run_id.as_deref(),
        ) {
            let _ = tx.send(action);
        }
    });
}

/// Like [`spawn_workflow_poll_once`] but clears an `AtomicBool` guard when done,
/// so the caller can prevent concurrent polls.
pub fn spawn_workflow_poll_once_guarded(
    tx: BackgroundSender,
    worktree_id: Option<String>,
    worktree_path: Option<String>,
    repo_path: Option<String>,
    selected_run_id: Option<String>,
    selected_step_child_run_id: Option<String>,
    in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    thread::spawn(move || {
        let result = poll_workflow_data(
            worktree_id.as_deref(),
            worktree_path.as_deref(),
            repo_path.as_deref(),
            selected_run_id.as_deref(),
            selected_step_child_run_id.as_deref(),
        );
        // Clear the guard before sending so the next tick can enqueue a new poll.
        in_flight.store(false, std::sync::atomic::Ordering::SeqCst);
        if let Some(action) = result {
            let _ = tx.send(action);
        }
    });
}

/// Spawn a one-shot background operation for blocking tasks.
#[allow(dead_code)]
pub fn spawn_blocking(tx: BackgroundSender, f: impl FnOnce() -> Action + Send + 'static) {
    thread::spawn(move || {
        let action = f();
        let _ = tx.send(action);
    });
}
