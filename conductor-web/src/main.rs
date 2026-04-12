use std::sync::Arc;

use anyhow::Result;
use axum::http::HeaderValue;
use conductor_core::agent::AgentManager;
use conductor_core::config::{conductor_dir, db_path, ensure_dirs, load_config, save_config};
use conductor_core::db::open_database;
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use conductor_web::assets::static_handler;
use conductor_web::events::{ConductorEvent, EventBus};
use conductor_web::openapi::ApiDoc;
use conductor_web::push::{PushPayload, PushSubscriptionManager};
use conductor_web::routes::api_router;
use conductor_web::state::AppState;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut config = load_config()?;
    ensure_dirs(&config)?;

    // Generate or load VAPID keys for push notifications.
    // The placeholder check detects zero-filled keys written by older versions.
    fn is_placeholder_key(key: &str) -> bool {
        key == "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
    }
    let needs_keygen = config.web_push.vapid_public_key.is_none()
        || config.web_push.vapid_private_key.is_none()
        || config
            .web_push
            .vapid_private_key
            .as_deref()
            .map(is_placeholder_key)
            .unwrap_or(false)
        || config
            .web_push
            .vapid_public_key
            .as_deref()
            .map(is_placeholder_key)
            .unwrap_or(false);
    if needs_keygen {
        tracing::info!("Generating VAPID keys for push notifications");

        use p256::ecdsa::SigningKey;
        let signing_key = SigningKey::random(&mut rand_core::OsRng);
        let private_key_bytes = signing_key.to_bytes();
        let public_key_bytes = signing_key
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();

        let private_key = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            private_key_bytes,
        );
        let public_key = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            public_key_bytes,
        );

        config.web_push.vapid_private_key = Some(private_key);
        config.web_push.vapid_public_key = Some(public_key);
        config.web_push.vapid_subject = Some("mailto:notifications@conductor.local".to_string());

        if let Err(e) = save_config(&config) {
            tracing::warn!("Failed to save VAPID keys to config: {e}");
        } else {
            tracing::info!("VAPID keys saved to config");
        }
    }
    // Always use the global database — the web server manages all repos,
    // so worktree-local DB detection must be bypassed.
    let conn = open_database(&conductor_dir().join("conductor.db"))?;

    // Reap orphaned agent runs on startup.
    let agent_mgr = AgentManager::new(&conn);
    match agent_mgr.reap_orphaned_runs() {
        Ok(n) if n > 0 => tracing::info!("Reaped {n} orphaned agent run(s) on startup"),
        Ok(_) => {}
        Err(e) => tracing::warn!("reap_orphaned_runs failed on startup: {e}"),
    }

    // Reap stale worktrees on startup.
    {
        use conductor_core::worktree::WorktreeManager;
        let wt_mgr = WorktreeManager::new(&conn, &config);
        match wt_mgr.reap_stale_worktrees() {
            Ok(n) if n > 0 => tracing::info!("Reaped {n} stale worktree(s) on startup"),
            Ok(_) => {}
            Err(e) => tracing::warn!("reap_stale_worktrees failed on startup: {e}"),
        }
        if config.general.auto_cleanup_merged_branches {
            match wt_mgr.cleanup_merged_worktrees(None) {
                Ok(n) if n > 0 => {
                    tracing::info!("Auto-cleaned {n} merged worktree(s) on startup")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("cleanup_merged_worktrees failed on startup: {e}"),
            }
        }
    }

    // Reap orphaned workflow runs on startup.
    {
        use conductor_core::workflow::WorkflowManager;
        let wf_mgr = WorkflowManager::new(&conn);
        match wf_mgr.reap_orphaned_workflow_runs() {
            Ok(n) if n > 0 => tracing::info!("Reaped {n} orphaned workflow run(s) on startup"),
            Ok(_) => {}
            Err(e) => tracing::warn!("reap_orphaned_workflow_runs failed on startup: {e}"),
        }
        match wf_mgr.reap_orphaned_script_steps() {
            Ok(n) if n > 0 => {
                tracing::info!("Reaped {n} orphaned script step(s) on startup")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("reap_orphaned_script_steps failed on startup: {e}"),
        }
        match wf_mgr.reap_finalization_stuck_workflow_runs(60) {
            Ok(n) if n > 0 => {
                tracing::info!("Reaper finalized {n} stuck workflow run(s) on startup")
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("reap_finalization_stuck_workflow_runs failed on startup: {e}")
            }
        }
        {
            let conductor_bin_dir = conductor_core::workflow::resolve_conductor_bin_dir();
            match wf_mgr.reap_heartbeat_stuck_runs(&config, 60, conductor_bin_dir) {
                Ok(n) if n > 0 => {
                    tracing::info!("Auto-resuming {n} stuck workflow run(s) on startup")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("reap_heartbeat_stuck_runs failed on startup: {e}"),
            }
        }
    }

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        config: Arc::new(RwLock::new(config)),
        events: EventBus::new(64),
        db_path: db_path(),
        workflow_done_notify: None,
    };

    // Spawn a background task that periodically reaps orphaned runs,
    // stale worktrees, and detects agent run terminal transitions for
    // notifications. Uses spawn_blocking to avoid blocking the tokio
    // runtime with synchronous DB queries and subprocess calls.
    let reaper_state = state.clone();
    let reaper_config = state.config.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now() + std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(30),
        );
        let mut seen_agent_statuses: std::collections::HashMap<
            String,
            conductor_core::agent::AgentRunStatus,
        > = std::collections::HashMap::new();
        let mut agent_initialized = false;
        let mut seen_workflow_statuses: std::collections::HashMap<
            String,
            conductor_core::workflow::WorkflowRunStatus,
        > = std::collections::HashMap::new();
        let mut workflow_initialized = false;
        loop {
            interval.tick().await;
            let db = reaper_state.db.clone();
            let cfg = reaper_config.clone();
            let mut seen = std::mem::take(&mut seen_agent_statuses);
            let mut init = agent_initialized;
            let mut wf_seen = std::mem::take(&mut seen_workflow_statuses);
            let mut wf_init = workflow_initialized;
            let result = tokio::task::spawn_blocking(move || {
                let conn = db.blocking_lock();
                let mgr = AgentManager::new(&conn);
                mgr.reap_orphaned_runs()?;
                mgr.dismiss_expired_feedback_requests()?;
                let cfg = cfg.blocking_read();
                let wt_mgr = conductor_core::worktree::WorktreeManager::new(&conn, &cfg);
                wt_mgr.reap_stale_worktrees()?;
                if cfg.general.auto_cleanup_merged_branches {
                    match wt_mgr.cleanup_merged_worktrees(None) {
                        Ok(n) if n > 0 => {
                            tracing::info!("Auto-cleaned {n} merged worktree(s)")
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!("cleanup_merged_worktrees failed: {e}"),
                    }
                }
                let wf_mgr = conductor_core::workflow::WorkflowManager::new(&conn);
                wf_mgr.reap_orphaned_workflow_runs()?;
                wf_mgr.reap_orphaned_script_steps()?;
                match wf_mgr.reap_finalization_stuck_workflow_runs(60) {
                    Ok(n) if n > 0 => {
                        tracing::info!("Reaper finalized {n} stuck workflow run(s)")
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("reap_finalization_stuck_workflow_runs failed: {e}")
                    }
                }
                {
                    let conductor_bin_dir = conductor_core::workflow::resolve_conductor_bin_dir();
                    match wf_mgr.reap_heartbeat_stuck_runs(&cfg, 60, conductor_bin_dir) {
                        Ok(n) if n > 0 => {
                            tracing::info!("Auto-resuming {n} stuck workflow run(s)")
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!("reap_heartbeat_stuck_runs failed: {e}"),
                    }
                }

                // Detect agent run terminal transitions and fire notifications.
                let latest_runs = mgr.latest_runs_by_worktree()?;
                let worktrees = wt_mgr.list(None, false)?;
                let wt_slugs: std::collections::HashMap<&str, &str> = worktrees
                    .iter()
                    .map(|wt| (wt.id.as_str(), wt.slug.as_str()))
                    .collect();
                let runs_iter = latest_runs.iter().map(|(wt_id, run)| {
                    let slug = wt_slugs.get(wt_id.as_str()).copied();
                    (slug, run)
                });
                let transitions = conductor_web::notify::detect_agent_terminal_transitions(
                    runs_iter, &mut seen, &mut init,
                );
                for t in &transitions {
                    conductor_web::notify::fire_agent_run_notification(
                        &conn,
                        &cfg.notifications,
                        &cfg.notify.hooks,
                        &conductor_web::notify::AgentRunNotificationArgs {
                            run_id: &t.run_id,
                            worktree_slug: t.worktree_slug.as_deref(),
                            succeeded: t.succeeded,
                            error_msg: t.error_msg.as_deref(),
                            repo_slug: &t.repo_slug,
                            branch: &t.branch,
                            duration_ms: t.duration_ms,
                            ticket_url: None,
                        },
                    );
                }

                // Send push notifications for agent run transitions
                if !transitions.is_empty() {
                    for t in &transitions {
                        let payload = PushPayload {
                            title: if t.succeeded {
                                "Agent Run Completed"
                            } else {
                                "Agent Run Failed"
                            }
                            .to_string(),
                            body: format!(
                                "Agent run {} for worktree {}",
                                if t.succeeded {
                                    "completed successfully"
                                } else {
                                    "failed"
                                },
                                t.worktree_slug.as_deref().unwrap_or("unknown")
                            ),
                            tag: Some(format!("agent-run-{}", t.run_id)),
                            url: t
                                .worktree_slug
                                .as_ref()
                                .map(|slug| format!("/worktrees/{}", slug)),
                        };

                        if let (Some(private_key), Some(public_key), Some(subject)) = (
                            &cfg.web_push.vapid_private_key,
                            &cfg.web_push.vapid_public_key,
                            &cfg.web_push.vapid_subject,
                        ) {
                            let push_mgr = PushSubscriptionManager::new(
                                &conn,
                                private_key.clone(),
                                public_key.clone(),
                                subject.clone(),
                            );
                            let runtime = tokio::runtime::Handle::current();
                            if let Err(e) = runtime.block_on(push_mgr.send_all(&payload)) {
                                tracing::warn!(
                                    "Failed to send push notification for agent run: {e}"
                                );
                            }
                        }
                    }
                }

                // Detect workflow run terminal transitions and fire notifications.
                let workflow_runs = wf_mgr.list_all_workflow_runs(200)?;
                let wf_transitions = conductor_web::notify::detect_workflow_terminal_transitions(
                    workflow_runs.iter(),
                    &mut wf_seen,
                    &mut wf_init,
                );
                for t in &wf_transitions {
                    conductor_web::notify::fire_workflow_notification(
                        &conn,
                        &cfg.notifications,
                        &cfg.notify.hooks,
                        &conductor_web::notify::WorkflowNotificationArgs {
                            run_id: &t.run_id,
                            workflow_name: &t.workflow_name,
                            target_label: t.target_label.as_deref(),
                            succeeded: t.succeeded,
                            parent_workflow_run_id: t.parent_workflow_run_id.as_deref(),
                            repo_slug: &t.repo_slug,
                            branch: &t.branch,
                            duration_ms: t.duration_ms,
                            ticket_url: None,
                            error: t.error.as_deref(),
                            repo_id: t.repo_id.as_deref(),
                            worktree_id: t.worktree_id.as_deref(),
                        },
                    );
                }

                // Send push notifications for workflow run transitions
                if !wf_transitions.is_empty() {
                    for t in &wf_transitions {
                        let payload = PushPayload {
                            title: if t.succeeded {
                                "Workflow Completed"
                            } else {
                                "Workflow Failed"
                            }
                            .to_string(),
                            body: format!(
                                "Workflow '{}' {}",
                                t.workflow_name,
                                if t.succeeded {
                                    "completed successfully"
                                } else {
                                    "failed"
                                }
                            ),
                            tag: Some(format!("workflow-run-{}", t.run_id)),
                            url: Some(format!("/workflows/runs/{}", t.run_id)),
                        };

                        if let (Some(private_key), Some(public_key), Some(subject)) = (
                            &cfg.web_push.vapid_private_key,
                            &cfg.web_push.vapid_public_key,
                            &cfg.web_push.vapid_subject,
                        ) {
                            let push_mgr = PushSubscriptionManager::new(
                                &conn,
                                private_key.clone(),
                                public_key.clone(),
                                subject.clone(),
                            );
                            let runtime = tokio::runtime::Handle::current();
                            if let Err(e) = runtime.block_on(push_mgr.send_all(&payload)) {
                                tracing::warn!(
                                    "Failed to send push notification for workflow: {e}"
                                );
                            }
                        }
                    }
                }

                Ok::<_, conductor_core::error::ConductorError>((seen, init, wf_seen, wf_init))
            })
            .await;
            match result {
                Ok(Ok((new_seen, new_init, new_wf_seen, new_wf_init))) => {
                    seen_agent_statuses = new_seen;
                    agent_initialized = new_init;
                    seen_workflow_statuses = new_wf_seen;
                    workflow_initialized = new_wf_init;
                }
                Ok(Err(e)) => tracing::warn!("periodic reaper failed: {e}"),
                Err(join_err) => tracing::warn!("periodic reaper panicked: {join_err}"),
            }
        }
    });

    // Spawn a 2-second background poller that emits AgentStep SSE events for
    // each new agent_run_events row written by running CLI agents.
    //
    // Circuit-breaker: after POLLER_FAIL_THRESHOLD consecutive failures the
    // poll interval backs off to POLLER_BACKOFF_SECS and logs at ERROR level.
    // The interval and log level reset to normal on the next success.
    const POLLER_FAIL_THRESHOLD: u32 = 5;
    const POLLER_NORMAL_SECS: u64 = 2;
    const POLLER_BACKOFF_SECS: u64 = 30;
    let step_poller_state = state.clone();
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(POLLER_NORMAL_SECS));
        let mut tracker: StepTracker = StepTracker::new();
        let mut consecutive_failures: u32 = 0;
        loop {
            interval.tick().await;
            let db = step_poller_state.db.clone();
            let events_bus = step_poller_state.events.clone();
            // Clone the tracker so the original is preserved if spawn_blocking fails,
            // preventing duplicate SSE emissions on the next tick.
            let tracker_snapshot = tracker.clone();
            let result = tokio::task::spawn_blocking(move || {
                let conn = db.blocking_lock();
                let mgr = AgentManager::new(&conn);
                let running_runs = mgr.list_agent_runs(
                    None,
                    None,
                    Some(&conductor_core::agent::AgentRunStatus::Running),
                    100,
                    0,
                )?;
                let new_events_by_run: std::collections::HashMap<String, Vec<_>> = running_runs
                    .iter()
                    .map(|run| {
                        let (last_id, _) = tracker_snapshot
                            .get(&run.id)
                            .cloned()
                            .unwrap_or_else(|| (String::new(), 0));
                        let evs = mgr.list_step_events_for_run_since(&run.id, &last_id)?;
                        Ok::<_, conductor_core::error::ConductorError>((run.id.clone(), evs))
                    })
                    .collect::<Result<_, _>>()?;
                let (new_tracker, to_emit) =
                    compute_step_events(&running_runs, tracker_snapshot, &new_events_by_run);
                for (run_id, description, step_index) in to_emit {
                    events_bus.emit(ConductorEvent::AgentStep {
                        agent_run_id: run_id,
                        description,
                        step_index: Some(step_index),
                    });
                }
                Ok::<_, conductor_core::error::ConductorError>(new_tracker)
            })
            .await;
            match result {
                Ok(Ok(new_tracker)) => {
                    if consecutive_failures >= POLLER_FAIL_THRESHOLD {
                        tracing::info!("step-event poller recovered after {consecutive_failures} consecutive failures");
                        interval = tokio::time::interval(std::time::Duration::from_secs(
                            POLLER_NORMAL_SECS,
                        ));
                    }
                    consecutive_failures = 0;
                    tracker = new_tracker;
                }
                Ok(Err(e)) => {
                    consecutive_failures += 1;
                    if consecutive_failures >= POLLER_FAIL_THRESHOLD {
                        tracing::error!(
                            consecutive_failures,
                            "step-event poller failing repeatedly: {e} — backing off to {POLLER_BACKOFF_SECS}s interval"
                        );
                        interval = tokio::time::interval(std::time::Duration::from_secs(
                            POLLER_BACKOFF_SECS,
                        ));
                    } else {
                        tracing::warn!("step-event poller failed: {e}");
                    }
                }
                Err(join_err) => {
                    consecutive_failures += 1;
                    if consecutive_failures >= POLLER_FAIL_THRESHOLD {
                        tracing::error!(
                            consecutive_failures,
                            "step-event poller panicking repeatedly: {join_err} — backing off to {POLLER_BACKOFF_SECS}s interval"
                        );
                        interval = tokio::time::interval(std::time::Duration::from_secs(
                            POLLER_BACKOFF_SECS,
                        ));
                    } else {
                        tracing::warn!("step-event poller panicked: {join_err}");
                    }
                }
            }
        }
    });

    // Spawn a task that subscribes to the EventBus and sends push notifications
    // for high-urgency gate-waiting and feedback-requested events in real time.
    let gate_state = state.clone();
    tokio::spawn(async move {
        let mut rx = gate_state.events.subscribe();
        loop {
            match rx.recv().await {
                Ok(ConductorEvent::WorkflowGateWaiting { run_id, .. }) => {
                    let db = gate_state.db.clone();
                    let cfg = gate_state.config.clone();
                    let run_id = run_id.clone();
                    tokio::task::spawn_blocking(move || {
                        let conn = db.blocking_lock();
                        let cfg = cfg.blocking_read();
                        if let (Some(priv_k), Some(pub_k), Some(sub)) = (
                            &cfg.web_push.vapid_private_key,
                            &cfg.web_push.vapid_public_key,
                            &cfg.web_push.vapid_subject,
                        ) {
                            let push_mgr = PushSubscriptionManager::new(
                                &conn,
                                priv_k.clone(),
                                pub_k.clone(),
                                sub.clone(),
                            );
                            let payload = PushPayload {
                                title: "Workflow paused — your review is needed".into(),
                                body: "Gate waiting for approval".into(),
                                tag: Some(format!("gate-{run_id}")),
                                url: Some(format!("/workflows/runs/{run_id}")),
                            };
                            let rt = tokio::runtime::Handle::current();
                            if let Err(e) = rt.block_on(push_mgr.send_all(&payload)) {
                                tracing::warn!("gate push failed for run {run_id}: {e}");
                            }
                        }
                    });
                }
                Ok(ConductorEvent::FeedbackRequested {
                    run_id,
                    worktree_id,
                    ..
                }) => {
                    let db = gate_state.db.clone();
                    let cfg = gate_state.config.clone();
                    let run_id = run_id.clone();
                    let worktree_id = worktree_id.clone();
                    tokio::task::spawn_blocking(move || {
                        let conn = db.blocking_lock();
                        let cfg = cfg.blocking_read();
                        if let (Some(priv_k), Some(pub_k), Some(sub)) = (
                            &cfg.web_push.vapid_private_key,
                            &cfg.web_push.vapid_public_key,
                            &cfg.web_push.vapid_subject,
                        ) {
                            let push_mgr = PushSubscriptionManager::new(
                                &conn,
                                priv_k.clone(),
                                pub_k.clone(),
                                sub.clone(),
                            );
                            let payload = PushPayload {
                                title: "Agent needs your input".into(),
                                body: format!("Feedback requested for run {run_id}"),
                                tag: Some(format!("feedback-{run_id}")),
                                url: Some(format!("/worktrees/{worktree_id}")),
                            };
                            let rt = tokio::runtime::Handle::current();
                            if let Err(e) = rt.block_on(push_mgr.send_all(&payload)) {
                                tracing::warn!("feedback push failed for run {run_id}: {e}");
                            }
                        }
                    });
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("gate-push subscriber lagged by {n} events");
                }
                Err(_) => break,
            }
        }
    });

    let host: std::net::IpAddr = std::env::var("CONDUCTOR_HOST")
        .unwrap_or_else(|_| "127.0.0.1".to_string())
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid CONDUCTOR_HOST: {e}"))?;
    let port: u16 = std::env::var("CONDUCTOR_PORT")
        .unwrap_or_else(|_| "3000".to_string())
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid CONDUCTOR_PORT: {e}"))?;

    let mut origins: Vec<HeaderValue> = vec![
        format!("http://localhost:{port}").parse().unwrap(),
        format!("http://127.0.0.1:{port}").parse().unwrap(),
    ];
    if let Ok(v) = "http://localhost:5173".parse() {
        origins.push(v);
    }
    if let Ok(v) = "http://127.0.0.1:5173".parse() {
        origins.push(v);
    }

    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = api_router()
        .merge(SwaggerUi::new("/api/docs").url("/api/openapi.json", ApiDoc::openapi()))
        .fallback(static_handler)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state);
    let addr = std::net::SocketAddr::from((host, port));
    tracing::info!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Tracks the last-seen event ID and step count per active run.
type StepTracker = std::collections::HashMap<String, (String, i64)>;

/// Pure step-event computation for the background poller.
///
/// Given the list of currently-running agent runs, the previous tracker state
/// (`run_id → (last_event_id, step_count)`), and a pre-fetched map of new
/// step events per run, returns:
/// - the updated tracker (pruned to active runs)
/// - a list of `(run_id, description, step_index)` tuples to emit as SSE events
///
/// Keeping this logic pure (no I/O) makes it straightforward to unit-test.
fn compute_step_events(
    running_runs: &[conductor_core::agent::AgentRun],
    mut tracker: StepTracker,
    new_events_by_run: &std::collections::HashMap<
        String,
        Vec<conductor_core::agent::AgentRunEvent>,
    >,
) -> (StepTracker, Vec<(String, String, i64)>) {
    let mut to_emit: Vec<(String, String, i64)> = Vec::new();
    for run in running_runs {
        let (last_id, step_count) = tracker
            .get(&run.id)
            .cloned()
            .unwrap_or_else(|| (String::new(), 0));
        let mut current_last_id = last_id;
        let mut current_step_count = step_count;
        if let Some(events) = new_events_by_run.get(&run.id) {
            for ev in events {
                to_emit.push((run.id.clone(), ev.summary.clone(), current_step_count));
                current_step_count += 1;
                current_last_id = ev.id.clone();
            }
        }
        tracker.insert(run.id.clone(), (current_last_id, current_step_count));
    }
    // Prune stale entries for runs that are no longer active.
    let active_ids: std::collections::HashSet<&str> =
        running_runs.iter().map(|r| r.id.as_str()).collect();
    tracker.retain(|id, _| active_ids.contains(id.as_str()));
    (tracker, to_emit)
}

#[cfg(test)]
mod tests {
    use super::compute_step_events;
    use conductor_core::agent::{AgentRun, AgentRunEvent, AgentRunStatus};

    fn make_run(id: &str) -> AgentRun {
        AgentRun {
            id: id.to_string(),
            worktree_id: None,
            repo_id: None,
            claude_session_id: None,
            prompt: String::new(),
            status: AgentRunStatus::Running,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: String::new(),
            ended_at: None,
            tmux_window: None,
            log_file: None,
            model: None,
            plan: None,
            parent_run_id: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            bot_name: None,
            conversation_id: None,
            subprocess_pid: None,
        }
    }

    fn make_event(id: &str, run_id: &str, summary: &str) -> AgentRunEvent {
        AgentRunEvent {
            id: id.to_string(),
            run_id: run_id.to_string(),
            kind: "tool".to_string(),
            summary: summary.to_string(),
            started_at: String::new(),
            ended_at: None,
            metadata: None,
        }
    }

    #[test]
    fn test_compute_step_events_advances_tracker() {
        let run = make_run("run1");
        let ev1 = make_event("ev1", "run1", "Step 1");
        let ev2 = make_event("ev2", "run1", "Step 2");

        let tracker = std::collections::HashMap::new();
        let mut events_map = std::collections::HashMap::new();
        events_map.insert("run1".to_string(), vec![ev1.clone(), ev2.clone()]);

        let (new_tracker, to_emit) = compute_step_events(&[run], tracker, &events_map);

        assert_eq!(to_emit.len(), 2);
        assert_eq!(to_emit[0], ("run1".to_string(), "Step 1".to_string(), 0));
        assert_eq!(to_emit[1], ("run1".to_string(), "Step 2".to_string(), 1));
        assert_eq!(new_tracker["run1"], ("ev2".to_string(), 2));
    }

    #[test]
    fn test_compute_step_events_resumes_from_tracker() {
        let run = make_run("run1");
        let ev3 = make_event("ev3", "run1", "Step 3");

        let mut tracker = std::collections::HashMap::new();
        tracker.insert("run1".to_string(), ("ev2".to_string(), 2_i64));

        let mut events_map = std::collections::HashMap::new();
        events_map.insert("run1".to_string(), vec![ev3.clone()]);

        let (new_tracker, to_emit) = compute_step_events(&[run], tracker, &events_map);

        assert_eq!(to_emit.len(), 1);
        assert_eq!(to_emit[0], ("run1".to_string(), "Step 3".to_string(), 2));
        assert_eq!(new_tracker["run1"], ("ev3".to_string(), 3));
    }

    #[test]
    fn test_compute_step_events_prunes_stale_runs() {
        let run_a = make_run("runA");
        let ev = make_event("ev1", "runA", "Step A");

        // Tracker has a stale entry for runB which is no longer running
        let mut tracker = std::collections::HashMap::new();
        tracker.insert("runB".to_string(), ("ev_old".to_string(), 5_i64));

        let mut events_map = std::collections::HashMap::new();
        events_map.insert("runA".to_string(), vec![ev]);

        let (new_tracker, _) = compute_step_events(&[run_a], tracker, &events_map);

        assert!(new_tracker.contains_key("runA"));
        assert!(
            !new_tracker.contains_key("runB"),
            "stale run must be pruned"
        );
    }

    #[test]
    fn test_compute_step_events_no_new_events() {
        let run = make_run("run1");
        let mut tracker = std::collections::HashMap::new();
        tracker.insert("run1".to_string(), ("ev1".to_string(), 1_i64));

        let events_map: std::collections::HashMap<String, Vec<AgentRunEvent>> =
            std::collections::HashMap::new();

        let (new_tracker, to_emit) = compute_step_events(&[run], tracker, &events_map);

        assert!(to_emit.is_empty());
        assert_eq!(new_tracker["run1"], ("ev1".to_string(), 1));
    }
}
