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

use axum::http::HeaderValue;
use conductor_core::agent::AgentManager;
use conductor_core::config::{conductor_dir, load_config};
use conductor_core::db::open_database;
use conductor_web::routes::api_router_with_cors;

/// Log the result of a startup reap operation.
fn log_reap(label: &str, result: conductor_core::error::Result<usize>) {
    match result {
        Ok(n) if n > 0 => tracing::info!("Reaped {n} {label}(s) on startup"),
        Ok(_) => {}
        Err(e) => tracing::warn!("Failed to reap {label}: {e}"),
    }
}

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

            // Reap stale resources on startup.
            log_reap(
                "orphaned agent run",
                AgentManager::new(&conn).reap_orphaned_runs(),
            );
            log_reap("stale worktree", {
                use conductor_core::worktree::WorktreeManager;
                WorktreeManager::new(&conn, &config).reap_stale_worktrees()
            });
            log_reap("orphaned workflow run", {
                use conductor_core::workflow::WorkflowManager;
                WorkflowManager::new(&conn).reap_orphaned_workflow_runs()
            });

            // Build the conductor-web AppState for the embedded HTTP server.
            let web_state = conductor_web::state::AppState::new(conn, config, db_path_val, 64);

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

                    // Restrict to Tauri webview origins only.
                    // - tauri://localhost  → macOS / Linux (Tauri custom protocol)
                    // - http://tauri.localhost → Windows (localhost-mapped protocol)
                    // In debug builds, also allow the Vite dev server origin.
                    // The server only binds on 127.0.0.1, but a browser tab on any
                    // origin could still reach it without this restriction.
                    #[allow(unused_mut)]
                    let mut allowed_origins = vec![
                        HeaderValue::from_static("tauri://localhost"),
                        HeaderValue::from_static("http://tauri.localhost"),
                    ];
                    #[cfg(debug_assertions)]
                    allowed_origins.push(HeaderValue::from_static("http://localhost:8675"));
                    let router =
                        api_router_with_cors(allowed_origins).with_state(web_state.clone());

                    // Spawn periodic background reaper (same as conductor-web main.rs)
                    let reaper_db = web_state.db.clone();
                    let reaper_config = web_state.config.clone();
                    tokio::spawn(async move {
                        let mut interval = tokio::time::interval_at(
                            tokio::time::Instant::now() + std::time::Duration::from_secs(30),
                            std::time::Duration::from_secs(30),
                        );
                        loop {
                            interval.tick().await;
                            let db = reaper_db.clone();
                            let cfg = reaper_config.clone();
                            if let Err(e) = tokio::task::spawn_blocking(move || {
                                let conn = db.blocking_lock();
                                let mgr = conductor_core::agent::AgentManager::new(&conn);
                                if let Err(e) = mgr.reap_orphaned_runs() {
                                    tracing::warn!("reap_orphaned_runs failed: {e}");
                                }
                                if let Err(e) = mgr.dismiss_expired_feedback_requests() {
                                    tracing::warn!("dismiss_expired_feedback_requests failed: {e}");
                                }
                                let cfg = cfg.blocking_read();
                                let wt_mgr =
                                    conductor_core::worktree::WorktreeManager::new(&conn, &cfg);
                                if let Err(e) = wt_mgr.reap_stale_worktrees() {
                                    tracing::warn!("reap_stale_worktrees failed: {e}");
                                }
                            })
                            .await
                            {
                                tracing::warn!("reaper task panicked: {e}");
                            }
                        }
                    });

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
