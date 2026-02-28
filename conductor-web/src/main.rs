use std::sync::Arc;

use anyhow::Result;
use conductor_core::config::{db_path, ensure_dirs, load_config};
use conductor_core::db::open_database;
use tokio::sync::Mutex;
use tower_http::cors::{Any, CorsLayer};

use conductor_web::assets::static_handler;
use conductor_web::routes::api_router;
use conductor_web::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = load_config()?;
    ensure_dirs(&config)?;
    let conn = open_database(&db_path())?;

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        config: Arc::new(config),
    };

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
