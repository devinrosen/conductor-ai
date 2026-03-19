use std::path::PathBuf;
use std::sync::Mutex;

use conductor_core::config::Config;
use rusqlite::Connection;

#[allow(dead_code)]
/// Shared application state for the desktop app.
///
/// In Tauri, this is managed via `tauri::Manager::manage()` and accessed
/// in command handlers via `tauri::State<'_, AppState>`.
pub struct AppState {
    pub db: Mutex<Connection>,
    pub config: Mutex<Config>,
    pub db_path: PathBuf,
}

impl AppState {
    pub fn new(db_path: PathBuf, conn: Connection, config: Config) -> Self {
        Self {
            db: Mutex::new(conn),
            config: Mutex::new(config),
            db_path,
        }
    }
}
