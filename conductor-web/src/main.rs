use std::sync::Arc;

use anyhow::Result;
use axum::http::HeaderValue;
use conductor_core::agent::AgentManager;
use conductor_core::config::{conductor_dir, ensure_dirs, load_config, save_config};
use conductor_core::db::open_database;
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use conductor_web::assets::static_handler;
use conductor_web::events::{ConductorEvent, EventBus};
use conductor_web::push::{PushPayload, PushSubscriptionManager};
use conductor_web::routes::api_router;
use conductor_web::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

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
    }

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        config: Arc::new(RwLock::new(config)),
        events: EventBus::new(64),
        workflow_done_notify: None,
    };

    // Spawn a background task that periodically reaps orphaned runs,
    // stale worktrees, and detects agent run terminal transitions for
    // notifications. Uses spawn_blocking to avoid blocking the tokio
    // runtime with synchronous DB queries and subprocess calls.
    let reaper_state = state.clone();
    let reaper_config = state.config.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
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
                let transitions = conductor_core::notify::detect_agent_terminal_transitions(
                    runs_iter, &mut seen, &mut init,
                );
                for t in &transitions {
                    conductor_core::notify::fire_agent_run_notification(
                        &conn,
                        &cfg.notifications,
                        &t.run_id,
                        t.worktree_slug.as_deref(),
                        t.succeeded,
                        t.error_msg.as_deref(),
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
                let wf_transitions = conductor_core::notify::detect_workflow_terminal_transitions(
                    workflow_runs.iter(),
                    &mut wf_seen,
                    &mut wf_init,
                );
                for t in &wf_transitions {
                    conductor_core::notify::fire_workflow_notification(
                        &conn,
                        &cfg.notifications,
                        &t.run_id,
                        &t.workflow_name,
                        t.target_label.as_deref(),
                        t.succeeded,
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
