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
    if let Ok(n) = agent_mgr.reap_orphaned_runs() {
        if n > 0 {
            tracing::info!("Reaped {n} orphaned agent run(s) on startup");
        }
    }

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        config: Arc::new(RwLock::new(config)),
        events: EventBus::new(64),
    };

    // Spawn a background task that periodically reaps orphaned runs.
    let reaper_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let conn = reaper_state.db.lock().await;
            let mgr = AgentManager::new(&conn);
            let _ = mgr.reap_orphaned_runs();
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
