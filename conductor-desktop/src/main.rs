//! Conductor Desktop — Tauri v2 desktop application.
//!
//! Shares the React frontend with conductor-web via a transport adapter.
//! Calls conductor-core managers directly (no daemon, no HTTP).
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

use std::path::PathBuf;

fn main() {
    tracing_subscriber::fmt::init();

    // macOS GUI apps don't inherit shell PATH — fix it early.
    commands::fixup_macos_path();

    let db_path = conductor_core::config::db_path();
    let conn =
        conductor_core::db::open_database(&db_path).expect("Failed to open conductor database");
    let config = conductor_core::config::load_config().expect("Failed to load conductor config");

    let app_state = state::AppState::new(PathBuf::from(&db_path), conn, config);

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::list_repos,
            commands::list_worktrees,
            commands::list_milestones,
            commands::create_milestone,
            commands::milestone_progress,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Conductor desktop");
}
