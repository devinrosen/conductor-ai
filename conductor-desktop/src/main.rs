//! Conductor Desktop — Tauri v2 desktop application.
//!
//! Embeds the full conductor-web axum HTTP server on a random localhost port.
//! The React frontend detects desktop mode and queries `get_api_port` to learn
//! where to send `fetch()` and `EventSource` requests.
//!
//! ## Running
//!
//! ```bash
//! # Build frontend first
//! cd conductor-web/frontend && bun install && bun run build && cd ../..
//!
//! # Dev mode (hot-reload frontend)
//! cargo tauri dev --manifest-path conductor-desktop/Cargo.toml
//!
//! # Production build
//! cargo tauri build --manifest-path conductor-desktop/Cargo.toml
//! ```

mod commands;
mod state;

use std::sync::Arc;

use conductor_core::agent::AgentManager;
use conductor_core::config::{db_path, load_config};
use conductor_core::db::open_database;
use conductor_web::events::EventBus;
use conductor_web::routes::api_router;
use tower_http::cors::{Any, CorsLayer};

fn main() {
    tracing_subscriber::fmt::init();

    // macOS GUI apps don't inherit shell PATH — fix it early.
    commands::fixup_macos_path();

    tauri::Builder::default()
        .setup(|app| {
            use tauri::Manager;

            let db_path_val = db_path();
            let conn = open_database(&db_path_val).expect("Failed to open conductor database");
            let config = load_config().expect("Failed to load conductor config");

            // Reap orphaned agent runs on startup.
            let agent_mgr = AgentManager::new(&conn);
            if let Ok(n) = agent_mgr.reap_orphaned_runs() {
                if n > 0 {
                    tracing::info!("Reaped {n} orphaned agent run(s) on startup");
                }
            }

            // Reap stale worktrees on startup.
            {
                use conductor_core::worktree::WorktreeManager;
                let wt_mgr = WorktreeManager::new(&conn, &config);
                if let Ok(n) = wt_mgr.reap_stale_worktrees() {
                    if n > 0 {
                        tracing::info!("Reaped {n} stale worktree(s) on startup");
                    }
                }
            }

            // Reap orphaned workflow runs on startup.
            {
                use conductor_core::workflow::WorkflowManager;
                let wf_mgr = WorkflowManager::new(&conn);
                if let Ok(n) = wf_mgr.reap_orphaned_workflow_runs() {
                    if n > 0 {
                        tracing::info!("Reaped {n} orphaned workflow run(s) on startup");
                    }
                }
            }

            // Build the conductor-web AppState for the embedded HTTP server.
            let web_state = conductor_web::state::AppState {
                db: Arc::new(tokio::sync::Mutex::new(conn)),
                config: Arc::new(tokio::sync::RwLock::new(config)),
                events: EventBus::new(64),
                workflow_done_notify: None,
            };

            // Bind to a random available port.
            let listener = std::net::TcpListener::bind("127.0.0.1:0")
                .expect("Failed to bind to a local port for embedded API server");
            let port = listener.local_addr().unwrap().port();
            tracing::info!("Embedded API server on http://127.0.0.1:{port}");

            // Store the port so the frontend can query it via `get_api_port`.
            app.manage(state::ApiPort(port));

            // Spawn the axum server on a background thread with its own tokio runtime.
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new()
                    .expect("Failed to create tokio runtime for embedded server");
                rt.block_on(async move {
                    let cors = CorsLayer::new()
                        .allow_origin(Any)
                        .allow_methods(Any)
                        .allow_headers(Any);

                    let router = api_router().layer(cors).with_state(web_state);

                    let tokio_listener = tokio::net::TcpListener::from_std(listener)
                        .expect("Failed to convert std listener to tokio");
                    axum::serve(tokio_listener, router)
                        .await
                        .expect("Embedded API server exited unexpectedly");
                });
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![commands::get_api_port,])
        .run(tauri::generate_context!())
        .expect("Tauri runtime failed to start");
}
