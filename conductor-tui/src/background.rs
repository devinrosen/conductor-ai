use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
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
            let wf_mgr = conductor_core::workflow::WorkflowManager::new(&conn);
            match wf_mgr.recover_stuck_steps() {
                Ok(n) if n > 0 => tracing::debug!("Recovered {n} stuck workflow step(s)"),
                Ok(_) => {}
                Err(e) => tracing::warn!("recover_stuck_steps failed: {e}"),
            }
            match wf_mgr.reap_orphaned_workflow_runs() {
                Ok(n) if n > 0 => tracing::debug!("Reaped {n} orphaned workflow run(s)"),
                Ok(_) => {}
                Err(e) => tracing::warn!("reap_orphaned_workflow_runs failed: {e}"),
            }
        }
    }

    let repos = repo_mgr.list().ok()?;
    let worktrees = wt_mgr.list(None, true).ok()?;
    let tickets = ticket_syncer.list(None).ok()?;
    let ticket_labels = ticket_syncer.get_all_labels().unwrap_or_default();
    let latest_agent_runs = agent_mgr.latest_runs_by_worktree().unwrap_or_default();
    let ticket_agent_totals = agent_mgr.totals_by_ticket_all().unwrap_or_default();

    use conductor_core::workflow::{WorkflowManager, WorkflowRunStatus};
    let wf_mgr = WorkflowManager::new(&conn);
    // Build a per-worktree map of the most recent *root* run for inline indicators.
    // Using list_root_workflow_runs ensures the parent run wins the per-worktree slot
    // rather than a concurrently-active child sub-workflow run.
    // Fetch recent runs sorted DESC; the first entry per worktree_id wins.
    let mut latest_workflow_runs_by_worktree = std::collections::HashMap::new();
    for run in wf_mgr.list_root_workflow_runs(100).unwrap_or_default() {
        // Skip ephemeral runs (no registered worktree) — they have no worktree
        // entry to display inline indicators for.
        if let Some(ref wt_id) = run.worktree_id {
            latest_workflow_runs_by_worktree
                .entry(wt_id.clone())
                .or_insert(run);
        }
    }

    // Fetch active non-worktree workflow runs (repo/ticket-targeted).
    let active_non_worktree_workflow_runs = wf_mgr
        .list_active_non_worktree_workflow_runs(50)
        .unwrap_or_default();

    // Collect IDs of active runs to fetch current step summaries in a single batch query.
    let active_run_ids: Vec<String> = latest_workflow_runs_by_worktree
        .values()
        .filter(|r| {
            matches!(
                r.status,
                WorkflowRunStatus::Running | WorkflowRunStatus::Waiting
            )
        })
        .map(|r| r.id.clone())
        .chain(
            active_non_worktree_workflow_runs
                .iter()
                .map(|r| r.id.clone()),
        )
        .collect();
    let active_run_id_refs: Vec<&str> = active_run_ids.iter().map(|s| s.as_str()).collect();
    let workflow_step_summaries = wf_mgr
        .get_step_summaries_for_runs(&active_run_id_refs)
        .unwrap_or_default();

    Some(Action::DataRefreshed(Box::new(DataRefreshedPayload {
        repos,
        worktrees,
        tickets,
        ticket_labels,
        latest_agent_runs,
        ticket_agent_totals,
        latest_workflow_runs_by_worktree,
        workflow_step_summaries,
        active_non_worktree_workflow_runs,
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
#[allow(dead_code, clippy::too_many_arguments)]
pub fn spawn_workflow_poller(
    tx: BackgroundSender,
    interval: Duration,
    worktree_id: Option<String>,
    worktree_path: Option<String>,
    repo_path: Option<String>,
    repo_id: Option<String>,
    selected_run_id: Option<String>,
    selected_step_child_run_id: Option<String>,
) {
    thread::spawn(move || loop {
        thread::sleep(interval);
        if let Some(action) = poll_workflow_data(
            worktree_id.as_deref(),
            worktree_path.as_deref(),
            repo_path.as_deref(),
            repo_id.as_deref(),
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
    repo_id: Option<&str>,
    selected_run_id: Option<&str>,
    selected_step_child_run_id: Option<&str>,
) -> Option<Action> {
    use conductor_core::workflow::{WorkflowDef, WorkflowManager, WorkflowWarning};

    let db = db_path();
    let conn = open_database(&db).ok()?;

    // Skip FS scan when a run is selected — defs don't change during a run.
    let (defs, def_slugs, parse_warnings): (
        Option<Vec<_>>,
        Option<Vec<String>>,
        Vec<WorkflowWarning>,
    ) = if selected_run_id.is_some() {
        (None, None, Vec::new())
    } else if let Some(wt_path) = worktree_path {
        // Worktree-scoped: load defs from this worktree's filesystem path.
        let (defs, warnings) =
            WorkflowManager::list_defs(wt_path, repo_path.unwrap_or("")).unwrap_or_default();
        (Some(defs), Some(Vec::new()), warnings)
    } else if let Some(rid) = repo_id {
        // Repo-scoped: scan all active worktrees of this repo, deduplicate by name.
        let mut all_defs: Vec<WorkflowDef> = Vec::new();
        let mut all_warnings = Vec::new();
        if let Ok(config) = conductor_core::config::load_config() {
            let wt_mgr = conductor_core::worktree::WorktreeManager::new(&conn, &config);
            let repo_mgr = conductor_core::repo::RepoManager::new(&conn, &config);
            let rp = repo_mgr
                .list()
                .unwrap_or_default()
                .into_iter()
                .find(|r| r.id == rid)
                .map(|r| r.local_path)
                .unwrap_or_default();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for wt in wt_mgr.list(None, true).unwrap_or_default() {
                if wt.repo_id != rid {
                    continue;
                }
                let (mut wt_defs, warnings) =
                    WorkflowManager::list_defs(&wt.path, &rp).unwrap_or_default();
                all_warnings.extend(warnings);
                wt_defs.retain(|d| seen.insert(d.name.clone()));
                all_defs.extend(wt_defs);
            }
        }
        // def_slugs empty: all defs belong to the same repo, no slug labels needed.
        (Some(all_defs), Some(Vec::new()), all_warnings)
    } else {
        // Global mode: scan every registered worktree for workflow definitions.
        let mut all_defs = Vec::new();
        let mut all_slugs = Vec::new();
        let mut all_warnings = Vec::new();
        if let Ok(config) = conductor_core::config::load_config() {
            let wt_mgr = conductor_core::worktree::WorktreeManager::new(&conn, &config);
            let repo_mgr = conductor_core::repo::RepoManager::new(&conn, &config);
            let repos: std::collections::HashMap<String, (String, String)> = repo_mgr
                .list()
                .unwrap_or_default()
                .into_iter()
                .map(|r| (r.id, (r.slug, r.local_path)))
                .collect();
            let mut seen: std::collections::HashSet<(String, String)> =
                std::collections::HashSet::new();
            let mut tagged: Vec<(String, String, WorkflowDef)> = Vec::new();
            for wt in wt_mgr.list(None, true).unwrap_or_default() {
                let (repo_slug, rp) = repos
                    .get(&wt.repo_id)
                    .map(|(s, p)| (s.as_str(), p.as_str()))
                    .unwrap_or(("?", ""));
                let (mut wt_defs, warnings) =
                    WorkflowManager::list_defs(&wt.path, rp).unwrap_or_default();
                all_warnings.extend(warnings);
                // Deduplicate by (repo_id, workflow_name): each worktree has its own
                // filesystem copy of .conductor/workflows/, so source_path differs per
                // worktree even for the same logical workflow.
                wt_defs.retain(|d| seen.insert((wt.repo_id.clone(), d.name.clone())));
                for d in wt_defs {
                    tagged.push((wt.repo_id.clone(), repo_slug.to_string(), d));
                }
            }
            // Sort by repo_id so defs are contiguous per repo for grouping in the renderer.
            tagged.sort_by(|a, b| a.0.cmp(&b.0));
            for (_, slug, d) in tagged {
                all_slugs.push(slug);
                all_defs.push(d);
            }
        }
        (Some(all_defs), Some(all_slugs), all_warnings)
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let runs = if let Some(wt_id) = worktree_id {
        wf_mgr.list_workflow_runs(wt_id).unwrap_or_default()
    } else if let Some(rid) = repo_id {
        wf_mgr
            .list_workflow_runs_for_repo(rid, 50)
            .unwrap_or_default()
    } else {
        wf_mgr.list_all_workflow_runs(50).unwrap_or_default()
    };
    let steps = if let Some(run_id) = selected_run_id {
        wf_mgr.get_workflow_steps(run_id).unwrap_or_default()
    } else {
        Vec::new()
    };

    // Batch-fetch steps for all leaf runs (runs with no children in the current batch).
    // Build the set of run IDs that appear as someone's parent — these are non-leaf.
    let runs_with_children: std::collections::HashSet<&str> = runs
        .iter()
        .filter_map(|r| r.parent_workflow_run_id.as_deref())
        .collect();
    let leaf_run_ids: Vec<&str> = runs
        .iter()
        .filter(|r| !runs_with_children.contains(r.id.as_str()))
        .map(|r| r.id.as_str())
        .collect();
    let all_run_steps = wf_mgr.get_steps_for_runs(&leaf_run_ids).unwrap_or_default();

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
            workflow_def_slugs: def_slugs,
            workflow_runs: runs,
            workflow_steps: steps,
            step_agent_events,
            step_agent_run,
            workflow_parse_warnings: parse_warnings,
            all_run_steps,
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
    repo_id: Option<String>,
    selected_run_id: Option<String>,
    selected_step_child_run_id: Option<String>,
) {
    thread::spawn(move || {
        if let Some(action) = poll_workflow_data(
            worktree_id.as_deref(),
            worktree_path.as_deref(),
            repo_path.as_deref(),
            repo_id.as_deref(),
            selected_run_id.as_deref(),
            selected_step_child_run_id.as_deref(),
        ) {
            let _ = tx.send(action);
        }
    });
}

/// Like [`spawn_workflow_poll_once`] but clears an `AtomicBool` guard when done,
/// so the caller can prevent concurrent polls.
#[allow(clippy::too_many_arguments)]
pub fn spawn_workflow_poll_once_guarded(
    tx: BackgroundSender,
    worktree_id: Option<String>,
    worktree_path: Option<String>,
    repo_path: Option<String>,
    repo_id: Option<String>,
    selected_run_id: Option<String>,
    selected_step_child_run_id: Option<String>,
    in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    thread::spawn(move || {
        let result = poll_workflow_data(
            worktree_id.as_deref(),
            worktree_path.as_deref(),
            repo_path.as_deref(),
            repo_id.as_deref(),
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

/// Module-level flag: true while a PR fetch thread is running.
/// Declared at module level so `PrFetchGuard` can reset it on drop (panic-safe).
static PR_FETCH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// RAII guard that clears `PR_FETCH_IN_FLIGHT` on drop, even if the thread panics.
struct PrFetchGuard;

impl Drop for PrFetchGuard {
    fn drop(&mut self) {
        PR_FETCH_IN_FLIGHT.store(false, Ordering::SeqCst);
    }
}

/// Spawn a one-shot PR fetch for a single repo. Sends `Action::PrsRefreshed`
/// with the results (or an empty list if `gh` is unavailable).
///
/// A static in-flight guard prevents concurrent `gh` subprocesses when the
/// user navigates quickly between repos (same pattern as the `LAST_REAP` guard
/// used for orphan reaping above). The guard is RAII so a thread panic cannot
/// leave the flag stuck `true`.
pub fn spawn_pr_fetch_once(tx: BackgroundSender, remote_url: String, repo_id: String) {
    if PR_FETCH_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        // A fetch is already running; skip to avoid redundant `gh` subprocesses.
        return;
    }
    thread::spawn(move || {
        let _guard = PrFetchGuard;
        let prs = conductor_core::github::list_open_prs(&remote_url).unwrap_or_default();
        let _ = tx.send(Action::PrsRefreshed { repo_id, prs });
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
