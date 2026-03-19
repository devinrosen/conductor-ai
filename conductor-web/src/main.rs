use std::sync::Arc;

use anyhow::Result;
use conductor_core::agent::AgentManager;
use conductor_core::config::{db_path, ensure_dirs, load_config};
use conductor_core::db::open_database;
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::{Any, CorsLayer};

use conductor_web::assets::static_handler;
use conductor_web::events::EventBus;
use conductor_web::routes::api_router;
use conductor_web::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = load_config()?;
    ensure_dirs(&config)?;
    let conn = open_database(&db_path())?;

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
        loop {
            interval.tick().await;
            let db = reaper_state.db.clone();
            let cfg = reaper_config.clone();
            let mut seen = seen_agent_statuses.clone();
            let mut init = agent_initialized;
            let result = tokio::task::spawn_blocking(move || {
                let conn = db.blocking_lock();
                let mgr = AgentManager::new(&conn);
                mgr.reap_orphaned_runs()?;
                let cfg = cfg.blocking_read();
                let wt_mgr = conductor_core::worktree::WorktreeManager::new(&conn, &cfg);
                wt_mgr.reap_stale_worktrees()?;
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

                Ok::<_, conductor_core::error::ConductorError>((seen, init))
            })
            .await;
            match result {
                Ok(Ok((new_seen, new_init))) => {
                    seen_agent_statuses = new_seen;
                    agent_initialized = new_init;
                }
                Ok(Err(e)) => tracing::warn!("periodic reaper failed: {e}"),
                Err(join_err) => tracing::warn!("periodic reaper panicked: {join_err}"),
            }
        }
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = api_router()
        .fallback(static_handler)
        .layer(cors)
        .with_state(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));
    tracing::info!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
