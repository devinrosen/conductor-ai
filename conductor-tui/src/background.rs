use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread;
use std::time::Duration;

use conductor_core::agent::AgentManager;
use conductor_core::config::{db_path, load_config};
use conductor_core::db::open_database;
use conductor_core::error::ConductorError;
use conductor_core::feature::FeatureManager;
use conductor_core::github;
use conductor_core::github_app;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager, JiraConfig};
use conductor_core::jira_acli;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::{TicketInput, TicketSyncer};
use conductor_core::worktree::WorktreeManager;

use crate::action::{Action, DataRefreshedPayload, WorkflowDataPayload};
use crate::event::BackgroundSender;

pub(crate) struct PollResult {
    pub action: Action,
    pub config: conductor_core::config::Config,
    pub conn: rusqlite::Connection,
}

// Workflow terminal transition detection is now in conductor_core::notify::detect_workflow_terminal_transitions.

/// Spawn the DB poller thread. Polls every `interval` and sends DataRefreshed events.
pub fn spawn_db_poller(tx: BackgroundSender, interval: Duration) {
    use std::collections::{HashMap, HashSet};

    thread::spawn(move || {
        let mut seen: HashMap<String, conductor_core::workflow::WorkflowRunStatus> = HashMap::new();
        // On the first poll `seen` is empty, so every pre-existing terminal run would
        // look like a fresh transition. Skip notifications until the map is seeded.
        let mut initialized = false;
        // Track IDs that have already been notified this session so we skip redundant
        // INSERT OR IGNORE attempts on every subsequent tick.
        let mut notified_feedback_ids: HashSet<String> = HashSet::new();
        let mut notified_gate_ids: HashSet<String> = HashSet::new();
        let mut notified_grouped_run_ids: HashSet<String> = HashSet::new();
        // Agent run terminal transition tracking (similar to workflow transitions).
        let mut seen_agent_statuses: HashMap<String, conductor_core::agent::AgentRunStatus> =
            HashMap::new();
        let mut agent_initialized = false;
        // Incremental turn-counting state: run_id → (byte_offset, turn_count).
        // Keyed by run ID (not worktree ID) so that a new run on the same
        // worktree starts with a fresh offset instead of inheriting a stale one.
        let mut turn_state: HashMap<String, (u64, i64)> = HashMap::new();
        loop {
            thread::sleep(interval);
            if let Some(PollResult {
                mut action,
                config,
                conn,
            }) = poll_data()
            {
                // Compute live turn counts incrementally, reusing byte offsets from
                // the previous tick so only newly-appended log bytes are parsed.
                if let Action::DataRefreshed(ref mut payload) = action {
                    use conductor_core::agent::{count_turns_incremental, AgentRunStatus};

                    let mut live_turns = HashMap::new();
                    let mut live_run_ids = HashSet::new();
                    for (wt_id, run) in &payload.latest_agent_runs {
                        if run.status == AgentRunStatus::Running {
                            if let Some(ref path) = run.log_file {
                                let (prev_offset, prev_count) =
                                    turn_state.get(&run.id).copied().unwrap_or((0, 0));
                                let (new_offset, new_count) =
                                    count_turns_incremental(path, prev_offset, prev_count);
                                turn_state.insert(run.id.clone(), (new_offset, new_count));
                                live_turns.insert(wt_id.clone(), new_count);
                                live_run_ids.insert(run.id.clone());
                            }
                        }
                    }
                    // Prune entries for runs that are no longer active.
                    turn_state.retain(|run_id, _| live_run_ids.contains(run_id));

                    payload.live_turns_by_worktree = live_turns;

                    // Reuse the connection returned by poll_data() — no need to open a
                    // second connection just for notification claims.
                    let claim_conn = if config.notifications.enabled {
                        Some(conn)
                    } else {
                        None
                    };

                    let all_runs = payload
                        .latest_workflow_runs_by_worktree
                        .values()
                        .chain(payload.active_non_worktree_workflow_runs.iter());
                    let transitions = conductor_core::notify::detect_workflow_terminal_transitions(
                        all_runs,
                        &mut seen,
                        &mut initialized,
                    );
                    if let Some(ref conn) = claim_conn {
                        for t in transitions {
                            crate::notify::fire_workflow_notification(
                                conn,
                                &config.notifications,
                                &t.run_id,
                                &t.workflow_name,
                                t.target_label.as_deref(),
                                t.succeeded,
                            );
                        }

                        // Fire feedback-requested notifications, skipping IDs already notified
                        // this session to avoid a redundant INSERT OR IGNORE on every tick.
                        for req in &payload.pending_feedback_requests {
                            if notified_feedback_ids.insert(req.id.clone()) {
                                crate::notify::fire_feedback_notification(
                                    conn,
                                    &config.notifications,
                                    &req.id,
                                    &req.prompt,
                                );
                            }
                        }

                        // Fire gate-waiting notifications, grouping by workflow_run_id.
                        // For runs with >1 waiting gate, fire a single grouped notification
                        // instead of one per gate.
                        {
                            // Group un-notified steps by workflow_run_id
                            type GateEntry = (
                                conductor_core::workflow::WorkflowRunStep,
                                String,
                                Option<String>,
                            );
                            let mut by_run: HashMap<String, Vec<&GateEntry>> = HashMap::new();
                            for entry in &payload.waiting_gate_steps {
                                let (step, _, _) = entry;
                                if !notified_gate_ids.contains(&step.id) {
                                    by_run
                                        .entry(step.workflow_run_id.clone())
                                        .or_default()
                                        .push(entry);
                                }
                            }

                            for (run_id, steps) in &by_run {
                                if steps.len() == 1 {
                                    // Single gate: fire individual notification (no behavior change)
                                    let (step, workflow_name, target_label) = steps[0];
                                    notified_gate_ids.insert(step.id.clone());
                                    crate::notify::fire_gate_notification(
                                        conn,
                                        &config.notifications,
                                        &crate::notify::GateNotificationParams {
                                            step_id: &step.id,
                                            step_name: &step.step_name,
                                            workflow_name,
                                            target_label: target_label.as_deref(),
                                            gate_type: step.gate_type.as_ref(),
                                            gate_prompt: step.gate_prompt.as_deref(),
                                        },
                                    );
                                } else if !notified_grouped_run_ids.contains(run_id) {
                                    // Multiple gates: fire a single grouped notification
                                    let (_, workflow_name, target_label) = steps[0];
                                    let gate_types: Vec<
                                        Option<&conductor_core::workflow::GateType>,
                                    > = steps
                                        .iter()
                                        .map(|(s, _, _)| s.gate_type.as_ref())
                                        .collect();
                                    crate::notify::fire_grouped_gate_notification(
                                        conn,
                                        &config.notifications,
                                        &crate::notify::GroupedGateNotificationParams {
                                            run_id,
                                            workflow_name,
                                            target_label: target_label.as_deref(),
                                            gate_types,
                                            count: steps.len(),
                                        },
                                    );
                                    notified_grouped_run_ids.insert(run_id.clone());
                                    // Mark all individual step IDs to prevent re-processing
                                    for (step, _, _) in steps {
                                        notified_gate_ids.insert(step.id.clone());
                                    }
                                }
                            }
                        }

                        // Detect agent run terminal transitions and fire notifications.
                        {
                            // Build worktree_id → slug lookup for notification text.
                            let wt_slugs: HashMap<&str, &str> = payload
                                .worktrees
                                .iter()
                                .map(|wt| (wt.id.as_str(), wt.slug.as_str()))
                                .collect();

                            let runs_iter = payload.latest_agent_runs.iter().map(|(wt_id, run)| {
                                let slug = wt_slugs.get(wt_id.as_str()).copied();
                                (slug, run)
                            });
                            let transitions =
                                conductor_core::notify::detect_agent_terminal_transitions(
                                    runs_iter,
                                    &mut seen_agent_statuses,
                                    &mut agent_initialized,
                                );
                            for t in transitions {
                                crate::notify::fire_agent_run_notification(
                                    conn,
                                    &config.notifications,
                                    &t.run_id,
                                    t.worktree_slug.as_deref(),
                                    t.succeeded,
                                    t.error_msg.as_deref(),
                                );
                            }
                        }

                        // Prune resolved feedback requests to prevent unbounded growth.
                        notified_feedback_ids.retain(|id| {
                            payload
                                .pending_feedback_requests
                                .iter()
                                .any(|r| &r.id == id)
                        });

                        // Prune resolved gate steps to prevent unbounded growth.
                        notified_gate_ids.retain(|id| {
                            payload
                                .waiting_gate_steps
                                .iter()
                                .any(|(step, _, _)| &step.id == id)
                        });

                        // Prune grouped run IDs when all gates for that run are resolved.
                        notified_grouped_run_ids.retain(|run_id| {
                            payload
                                .waiting_gate_steps
                                .iter()
                                .any(|(step, _, _)| &step.workflow_run_id == run_id)
                        });
                    }
                }
                if !tx.send(action) {
                    break;
                }
            }
        }
    });
}

/// Run `f` only when `enabled` is true; return an empty `Vec` otherwise.
fn query_if_enabled<T>(enabled: bool, f: impl FnOnce() -> Vec<T>) -> Vec<T> {
    if enabled {
        f()
    } else {
        vec![]
    }
}

/// Build fallback `AgentRunEvent`s by parsing log files for runs that lack DB event records.
/// Called on the background thread so file I/O never blocks the TUI main thread.
fn build_fallback_events(
    runs: &[conductor_core::agent::AgentRun],
) -> Vec<conductor_core::agent::AgentRunEvent> {
    use conductor_core::agent::{parse_agent_log, AgentRunEvent};

    let mut fallback = Vec::new();
    for run in runs {
        if let Some(ref path) = run.log_file {
            let events = parse_agent_log(path);
            for ev in events {
                fallback.push(AgentRunEvent {
                    id: conductor_core::new_id(),
                    run_id: run.id.clone(),
                    kind: ev.kind,
                    summary: ev.summary,
                    started_at: run.started_at.clone(),
                    ended_at: None,
                    metadata: None,
                });
            }
        }
    }
    fallback
}

/// Poll all data from the database. Returns a DataRefreshed action, the loaded config, and the
/// open DB connection so the caller can reuse it (e.g. for notification claims) without opening
/// a second connection on the same tick.
pub fn poll_data() -> Option<PollResult> {
    let db = db_path();
    let conn = open_database(&db).ok()?;
    let config = load_config().unwrap_or_else(|e| {
        tracing::warn!("config parse error (using defaults): {e}");
        conductor_core::config::Config::default()
    });

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
            let _ = agent_mgr.dismiss_expired_feedback_requests();
            let _ = wt_mgr.reap_stale_worktrees();
            if config.general.auto_cleanup_merged_branches {
                match wt_mgr.cleanup_merged_worktrees(None) {
                    Ok(n) if n > 0 => tracing::info!("Auto-cleaned {n} merged worktree(s)"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("cleanup_merged_worktrees failed: {e}"),
                }
            }
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
            match wf_mgr.detect_stuck_workflow_run_ids(60) {
                Ok(ids) if !ids.is_empty() => {
                    let n = ids.len();
                    tracing::info!("Auto-resuming {n} stuck workflow run(s)");
                    let conductor_bin_dir =
                        conductor_core::workflow::resolve_conductor_bin_dir();
                    for run_id in ids {
                        let config_clone = config.clone();
                        let bin_dir = conductor_bin_dir.clone();
                        std::thread::spawn(move || {
                            let params = conductor_core::workflow::WorkflowResumeStandalone {
                                config: config_clone,
                                workflow_run_id: run_id.clone(),
                                model: None,
                                from_step: None,
                                restart: false,
                                db_path: None,
                                conductor_bin_dir: bin_dir,
                            };
                            if let Err(e) =
                                conductor_core::workflow::resume_workflow_standalone(&params)
                            {
                                tracing::warn!(
                                    run_id = %run_id,
                                    "Auto-resume of stuck workflow run failed: {e}"
                                );
                            }
                        });
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("detect_stuck_workflow_run_ids failed: {e}"),
            }
        }
    }

    let repos = repo_mgr.list().ok()?;
    let worktrees = wt_mgr.list(None, true).ok()?;
    let tickets = ticket_syncer.list(None).ok()?;
    let ticket_labels = ticket_syncer.get_all_labels().unwrap_or_default();
    let latest_agent_runs = agent_mgr.latest_runs_by_worktree().unwrap_or_default();
    let latest_repo_agent_runs = agent_mgr.latest_repo_scoped_runs_all().unwrap_or_default();
    let ticket_agent_totals = agent_mgr.totals_by_ticket_all().unwrap_or_default();

    // Fetch all worktree-scoped agent events in a single batch query; fall back to log-file
    // parsing for worktrees whose runs pre-date DB-backed event storage.
    let mut worktree_agent_events = agent_mgr.list_all_events_by_worktree().unwrap_or_default();
    for wt_id in latest_agent_runs.keys() {
        if worktree_agent_events
            .get(wt_id)
            .is_none_or(|v| v.is_empty())
        {
            let mut runs = agent_mgr.list_for_worktree(wt_id).unwrap_or_default();
            runs.reverse();
            let fallback = build_fallback_events(&runs);
            if !fallback.is_empty() {
                worktree_agent_events.insert(wt_id.clone(), fallback);
            }
        }
    }

    // Same pattern for repo-scoped events.
    let mut repo_agent_events = agent_mgr.list_all_repo_events_by_repo().unwrap_or_default();
    for repo_id in latest_repo_agent_runs.keys() {
        if repo_agent_events.get(repo_id).is_none_or(|v| v.is_empty()) {
            let mut runs = agent_mgr.list_repo_scoped(repo_id).unwrap_or_default();
            runs.reverse();
            let fallback = build_fallback_events(&runs);
            if !fallback.is_empty() {
                repo_agent_events.insert(repo_id.clone(), fallback);
            }
        }
    }

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

    // Only run notification-specific queries when notifications are enabled.
    let pending_feedback_requests = query_if_enabled(config.notifications.enabled, || {
        agent_mgr
            .list_all_pending_feedback_requests()
            .unwrap_or_else(|e| {
                tracing::warn!("list_all_pending_feedback_requests failed: {e}");
                vec![]
            })
    });
    let waiting_gate_steps = query_if_enabled(config.notifications.enabled, || {
        wf_mgr.list_all_waiting_gate_steps().unwrap_or_else(|e| {
            tracing::warn!("list_all_waiting_gate_steps failed: {e}");
            vec![]
        })
    });

    // Live turn counts are computed incrementally by the background loop caller.
    // Return an empty map here; the loop merges in the incremental state.
    let live_turns_by_worktree = std::collections::HashMap::new();

    // Fetch unread notification count for the footer indicator.
    let unread_notification_count = {
        use conductor_core::notification_manager::NotificationManager;
        NotificationManager::new(&conn).unread_count().unwrap_or(0)
    };

    // Load active features for all repos in a single query.
    let feat_mgr = FeatureManager::new(&conn, &config);

    // Refresh last_commit_at cache at most once per 60 seconds.
    {
        static LAST_REFRESH: AtomicI64 = AtomicI64::new(0);
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        if now_secs - LAST_REFRESH.load(Ordering::Relaxed) >= 60 {
            LAST_REFRESH.store(now_secs, Ordering::Relaxed);
            let repos_for_refresh = repo_mgr.list().unwrap_or_default();
            for repo in &repos_for_refresh {
                if let Err(e) = feat_mgr.refresh_last_commit_all(&repo.slug) {
                    tracing::warn!("refresh_last_commit_all for {}: {e}", repo.slug);
                }
            }
        }
    }

    let features_by_repo = feat_mgr.list_all_active().unwrap_or_else(|e| {
        tracing::warn!("list_all_active features failed: {e}");
        std::collections::HashMap::new()
    });

    let action = Action::DataRefreshed(Box::new(DataRefreshedPayload {
        repos,
        worktrees,
        tickets,
        ticket_labels,
        latest_agent_runs,
        ticket_agent_totals,
        latest_workflow_runs_by_worktree,
        workflow_step_summaries,
        active_non_worktree_workflow_runs,
        pending_feedback_requests,
        waiting_gate_steps,
        live_turns_by_worktree,
        features_by_repo,
        unread_notification_count,
        latest_repo_agent_runs,
        worktree_agent_events,
        repo_agent_events,
    }));
    Some(PollResult {
        action,
        config,
        conn,
    })
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
        if !sync_sources_for_repo(
            tx,
            &syncer,
            &source_mgr,
            &repo.id,
            &repo.slug,
            &repo.remote_url,
            token,
        ) {
            return;
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

/// Staleness threshold for auto-sync: skip sync if tickets were synced within this duration.
pub const TICKET_SYNC_STALE_SECS: i64 = 300; // 5 minutes

/// Sync sources for a single repo, sending per-source actions to `tx`.
/// Returns `false` if the channel is closed (caller should stop).
fn sync_sources_for_repo(
    tx: &BackgroundSender,
    syncer: &TicketSyncer,
    source_mgr: &IssueSourceManager,
    repo_id: &str,
    repo_slug: &str,
    remote_url: &str,
    token: Option<&str>,
) -> bool {
    let sources = source_mgr.list(repo_id).unwrap_or_default();

    if sources.is_empty() {
        // Backward compat: auto-detect GitHub from remote_url
        if let Some((owner, name)) = github::parse_github_remote(remote_url) {
            let action = sync_repo(syncer, repo_id, repo_slug, "github", || {
                github::sync_github_issues(&owner, &name, token)
            });
            if !tx.send(action) {
                return false;
            }
        }
    } else {
        for source in sources {
            match source.source_type.as_str() {
                "github" => {
                    let action = match serde_json::from_str::<GitHubConfig>(&source.config_json) {
                        Ok(cfg) => sync_repo(syncer, repo_id, repo_slug, "github", || {
                            github::sync_github_issues(&cfg.owner, &cfg.repo, token)
                        }),
                        Err(e) => Action::TicketSyncFailed {
                            repo_slug: repo_slug.to_string(),
                            error: format!("invalid github config: {e}"),
                        },
                    };
                    if !tx.send(action) {
                        return false;
                    }
                }
                "jira" => {
                    let action = match serde_json::from_str::<JiraConfig>(&source.config_json) {
                        Ok(cfg) => sync_repo(syncer, repo_id, repo_slug, "jira", || {
                            jira_acli::sync_jira_issues_acli(&cfg.jql, &cfg.url)
                        }),
                        Err(e) => Action::TicketSyncFailed {
                            repo_slug: repo_slug.to_string(),
                            error: format!("invalid jira config: {e}"),
                        },
                    };
                    if !tx.send(action) {
                        return false;
                    }
                }
                _ => {}
            }
        }
    }
    true
}

/// Spawn a one-shot ticket sync for a single repo. Checks staleness inside the
/// background thread, then sends per-source `TicketSyncComplete`/`TicketSyncFailed`
/// actions followed by `TicketSyncDone`.
pub fn spawn_ticket_sync_for_repo(
    tx: BackgroundSender,
    repo_id: String,
    repo_slug: String,
    remote_url: String,
) {
    thread::spawn(move || {
        let db = db_path();
        let conn = match open_database(&db) {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Action::TicketSyncFailed {
                    repo_slug: repo_slug.clone(),
                    error: format!("failed to open database: {e}"),
                });
                let _ = tx.send(Action::TicketSyncDone);
                return;
            }
        };
        let config = match load_config() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Action::TicketSyncFailed {
                    repo_slug: repo_slug.clone(),
                    error: format!("failed to load config: {e}"),
                });
                let _ = tx.send(Action::TicketSyncDone);
                return;
            }
        };

        // Check staleness: skip sync if tickets were synced recently.
        let syncer = TicketSyncer::new(&conn);
        let is_stale = match syncer.latest_synced_at(&repo_id) {
            Ok(Some(ts)) => chrono::DateTime::parse_from_rfc3339(&ts)
                .map(|dt| {
                    chrono::Utc::now().signed_duration_since(dt).num_seconds()
                        > TICKET_SYNC_STALE_SECS
                })
                .unwrap_or(true),
            Ok(None) => true,
            Err(_) => false,
        };

        if !is_stale {
            let _ = tx.send(Action::TicketSyncDone);
            return;
        }

        let source_mgr = IssueSourceManager::new(&conn);
        let token_res = github_app::resolve_app_token(&config, "github-issues-sync");
        let token = token_res.token();

        sync_sources_for_repo(
            &tx,
            &syncer,
            &source_mgr,
            &repo_id,
            &repo_slug,
            &remote_url,
            token,
        );

        let _ = tx.send(Action::TicketSyncDone);
    });
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
        let (mut defs, warnings) =
            WorkflowManager::list_defs(wt_path, repo_path.unwrap_or("")).unwrap_or_default();
        defs.sort_by(|a, b| {
            let ka = (
                if a.group.is_none() { 1u8 } else { 0u8 },
                a.group.as_deref().unwrap_or(""),
                a.name.as_str(),
            );
            let kb = (
                if b.group.is_none() { 1u8 } else { 0u8 },
                b.group.as_deref().unwrap_or(""),
                b.name.as_str(),
            );
            ka.cmp(&kb)
        });
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
            // Fallback: no active worktrees → load from repo root
            if all_defs.is_empty() && !rp.is_empty() {
                let (repo_defs, warnings) =
                    WorkflowManager::list_defs(&rp, &rp).unwrap_or_default();
                all_warnings.extend(warnings);
                all_defs.extend(repo_defs);
            }
        }
        all_defs.sort_by(|a, b| {
            let ka = (
                if a.group.is_none() { 1u8 } else { 0u8 },
                a.group.as_deref().unwrap_or(""),
                a.name.as_str(),
            );
            let kb = (
                if b.group.is_none() { 1u8 } else { 0u8 },
                b.group.as_deref().unwrap_or(""),
                b.name.as_str(),
            );
            ka.cmp(&kb)
        });
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
            // Fallback per repo: if no worktree-sourced defs were seen, load from repo root
            for (repo_id, (repo_slug, repo_path)) in &repos {
                if seen.iter().any(|(rid, _)| rid == repo_id) {
                    continue; // at least one def was found from a worktree
                }
                if repo_path.is_empty() {
                    continue;
                }
                let (mut repo_defs, warnings) =
                    WorkflowManager::list_defs(repo_path, repo_path).unwrap_or_default();
                all_warnings.extend(warnings);
                repo_defs.retain(|d| seen.insert((repo_id.clone(), d.name.clone())));
                for d in repo_defs {
                    tagged.push((repo_id.clone(), repo_slug.clone(), d));
                }
            }
            // Sort by repo_id, then group (named first, ungrouped last), then name.
            tagged.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then_with(|| {
                        let ga = a.2.group.as_deref().unwrap_or("");
                        let gb = b.2.group.as_deref().unwrap_or("");
                        let ka: (u8, &str) = (if a.2.group.is_none() { 1 } else { 0 }, ga);
                        let kb: (u8, &str) = (if b.2.group.is_none() { 1 } else { 0 }, gb);
                        ka.cmp(&kb)
                    })
                    .then_with(|| a.2.name.cmp(&b.2.name))
            });
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

    // Batch-fetch steps for ALL runs (not just leaves) so that:
    // 1. Direct-call steps on non-leaf parents are available for interleaving
    // 2. Step detail panel works for non-leaf parents
    let all_run_ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    let mut all_run_steps = match wf_mgr.get_steps_for_runs(&all_run_ids) {
        Ok(steps) => steps,
        Err(e) => {
            tracing::warn!("get_steps_for_runs failed for runs {:?}: {e}", all_run_ids);
            Default::default()
        }
    };

    // Ancestor fetch: collect parent_workflow_run_id values that aren't in the
    // current run set and fetch their steps too. This covers global/repo 50-run-limit
    // mode where the parent may be outside the paginated window.
    let known_ids: std::collections::HashSet<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    let ancestor_ids: Vec<String> = runs
        .iter()
        .filter_map(|r| r.parent_workflow_run_id.as_deref())
        .filter(|pid| !known_ids.contains(pid))
        .map(|s| s.to_string())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    if !ancestor_ids.is_empty() {
        let ancestor_refs: Vec<&str> = ancestor_ids.iter().map(|s| s.as_str()).collect();
        match wf_mgr.get_steps_for_runs(&ancestor_refs) {
            Ok(ancestor_steps) => all_run_steps.extend(ancestor_steps),
            Err(e) => {
                tracing::warn!(
                    "get_steps_for_runs failed for ancestor runs {:?}: {e}",
                    ancestor_ids
                );
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    // ── query_if_enabled ──────────────────────────────────────────────

    #[test]
    fn query_if_enabled_returns_result_when_true() {
        let result = query_if_enabled(true, || vec![1, 2, 3]);
        assert_eq!(result, vec![1, 2, 3]);
    }

    #[test]
    fn query_if_enabled_returns_empty_when_false() {
        let result: Vec<i32> = query_if_enabled(false, || vec![1, 2, 3]);
        assert!(result.is_empty());
    }

    #[test]
    fn query_if_enabled_closure_not_called_when_false() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();
        let _: Vec<i32> = query_if_enabled(false, move || {
            called_clone.store(true, Ordering::SeqCst);
            vec![1]
        });
        assert!(!called.load(Ordering::SeqCst));
    }

    // ── sync_repo ─────────────────────────────────────────────────────

    #[test]
    fn sync_repo_returns_ticket_sync_complete_on_success() {
        let conn = conductor_core::test_helpers::setup_db();
        let syncer = TicketSyncer::new(&conn);

        let ticket = TicketInput {
            source_type: "github".into(),
            source_id: "42".into(),
            title: "Test issue".into(),
            body: "".into(),
            state: "open".into(),
            labels: vec![],
            assignee: None,
            priority: None,
            url: "https://example.com".into(),
            raw_json: "{}".into(),
            label_details: vec![],
        };

        let action = sync_repo(&syncer, "r1", "test-repo", "github", || Ok(vec![ticket]));
        match action {
            Action::TicketSyncComplete { repo_slug, count } => {
                assert_eq!(repo_slug, "test-repo");
                assert_eq!(count, 1);
            }
            other => panic!("expected TicketSyncComplete, got {other:?}"),
        }
    }

    #[test]
    fn sync_repo_returns_ticket_sync_failed_on_fetch_error() {
        let conn = conductor_core::test_helpers::setup_db();
        let syncer = TicketSyncer::new(&conn);

        let action = sync_repo(&syncer, "r1", "test-repo", "github", || {
            Err(ConductorError::TicketSync("fetch failed".into()))
        });
        match action {
            Action::TicketSyncFailed { repo_slug, error } => {
                assert_eq!(repo_slug, "test-repo");
                assert!(error.contains("fetch failed"));
            }
            other => panic!("expected TicketSyncFailed, got {other:?}"),
        }
    }

    #[test]
    fn sync_repo_returns_ticket_sync_failed_on_upsert_error() {
        // Use a connection with no repo registered — foreign key constraint will fail
        let conn = conductor_core::test_helpers::create_test_conn();
        let syncer = TicketSyncer::new(&conn);

        let ticket = TicketInput {
            source_type: "github".into(),
            source_id: "1".into(),
            title: "Test".into(),
            body: "".into(),
            state: "open".into(),
            labels: vec![],
            assignee: None,
            priority: None,
            url: "https://example.com".into(),
            raw_json: "{}".into(),
            label_details: vec![],
        };

        let action = sync_repo(&syncer, "nonexistent-repo", "test-repo", "github", || {
            Ok(vec![ticket])
        });
        match action {
            Action::TicketSyncFailed { repo_slug, .. } => {
                assert_eq!(repo_slug, "test-repo");
            }
            other => panic!("expected TicketSyncFailed, got {other:?}"),
        }
    }

    // ── TICKET_SYNC_STALE_SECS ────────────────────────────────────────

    #[test]
    fn ticket_sync_stale_secs_is_five_minutes() {
        assert_eq!(TICKET_SYNC_STALE_SECS, 300);
    }

    // ── PR_FETCH_IN_FLIGHT guard ──────────────────────────────────────

    #[test]
    fn pr_fetch_guard_drop_clears_flag() {
        PR_FETCH_IN_FLIGHT.store(true, Ordering::SeqCst);
        {
            let _guard = PrFetchGuard;
            assert!(PR_FETCH_IN_FLIGHT.load(Ordering::SeqCst));
        }
        // After guard is dropped, flag should be cleared
        assert!(!PR_FETCH_IN_FLIGHT.load(Ordering::SeqCst));
    }

    #[test]
    fn pr_fetch_in_flight_swap_prevents_concurrent() {
        // Reset first
        PR_FETCH_IN_FLIGHT.store(false, Ordering::SeqCst);

        // First swap returns false (was not in flight)
        let was_in_flight = PR_FETCH_IN_FLIGHT.swap(true, Ordering::SeqCst);
        assert!(!was_in_flight, "first swap should return false");

        // Second swap returns true (already in flight → skip)
        let was_in_flight = PR_FETCH_IN_FLIGHT.swap(true, Ordering::SeqCst);
        assert!(was_in_flight, "second swap should return true");

        // Clean up
        PR_FETCH_IN_FLIGHT.store(false, Ordering::SeqCst);
    }
}
