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

use conductor_core::agent::AgentManager;
use conductor_core::config::{conductor_dir, load_config};
use conductor_core::db::open_database;
use conductor_web::routes::api_router;
use tower_http::cors::{Any, CorsLayer};

fn main() {
    tracing_subscriber::fmt::init();

    // macOS GUI apps don't inherit shell PATH — fix it early.
    commands::fixup_macos_path();

    tauri::Builder::default()
        .setup(|app| {
            use tauri::Manager;

            // Always use the global database — the desktop app manages all
            // repos, so worktree-local DB detection must be bypassed.
            let db_path_val = conductor_dir().join("conductor.db");
            let conn = open_database(&db_path_val).expect("Failed to open conductor database");
            let config = load_config().expect("Failed to load conductor config");

            // Reap orphaned agent runs on startup.
            let agent_mgr = AgentManager::new(&conn);
            match agent_mgr.reap_orphaned_runs() {
                Ok(n) if n > 0 => tracing::info!("Reaped {n} orphaned agent run(s) on startup"),
                Ok(_) => {}
                Err(e) => tracing::warn!("Failed to reap orphaned agent runs: {e}"),
            }

            // Reap stale worktrees on startup.
            {
                use conductor_core::worktree::WorktreeManager;
                let wt_mgr = WorktreeManager::new(&conn, &config);
                match wt_mgr.reap_stale_worktrees() {
                    Ok(n) if n > 0 => tracing::info!("Reaped {n} stale worktree(s) on startup"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("Failed to reap stale worktrees: {e}"),
                }
            }

            // Reap orphaned workflow runs on startup.
            {
                use conductor_core::workflow::WorkflowManager;
                let wf_mgr = WorkflowManager::new(&conn);
                match wf_mgr.reap_orphaned_workflow_runs() {
                    Ok(n) if n > 0 => {
                        tracing::info!("Reaped {n} orphaned workflow run(s) on startup")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("Failed to reap orphaned workflow runs: {e}"),
                }
            }

            // Build the conductor-web AppState for the embedded HTTP server.
            let web_state = conductor_web::state::AppState::new(conn, config, 64);

            // Channel to receive the bound port (or error) from the server thread.
            let (port_tx, port_rx) = std::sync::mpsc::channel::<Result<u16, String>>();

            // Spawn the axum server on a background thread with its own tokio runtime.
            // Binding happens inside the tokio runtime to avoid the from_std() issue
            // with blocking file descriptors (tokio #7172).
            std::thread::spawn(move || {
                let rt = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = port_tx.send(Err(format!("Failed to create tokio runtime: {e}")));
                        return;
                    }
                };
                rt.block_on(async move {
                    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
                        Ok(l) => l,
                        Err(e) => {
                            let _ = port_tx
                                .send(Err(format!("Failed to bind embedded API server: {e}")));
                            return;
                        }
                    };
                    let port = listener
                        .local_addr()
                        .expect("Failed to get local address from bound listener")
                        .port();
                    let _ = port_tx.send(Ok(port));

                    // Allow any origin — the server only listens on 127.0.0.1 so
                    // only local processes can reach it. The Tauri webview origin
                    // varies by platform and isn't worth enumerating.
                    let cors = CorsLayer::new()
                        .allow_origin(Any)
                        .allow_methods(Any)
                        .allow_headers(Any);

                    let router = api_router().layer(cors).with_state(web_state);

                    if let Err(e) = axum::serve(listener, router).await {
                        eprintln!(
                            "[conductor-desktop] Embedded API server exited unexpectedly: {e}"
                        );
                        std::process::exit(1);
                    }
                });
            });

            // Wait for the server to bind and report its port.
            let port = port_rx
                .recv()
                .map_err(|_| "Server thread exited before binding".to_string())??;
            tracing::info!("Embedded API server on http://127.0.0.1:{port}");
            app.manage(state::ApiPort(port));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![commands::get_api_port,])
        .run(tauri::generate_context!())
        .expect("Tauri runtime failed to start");
}
