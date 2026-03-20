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

    /// Lock both `db` and `config` mutexes, returning guards or a string error.
    pub fn lock_both(
        &self,
    ) -> Result<
        (
            std::sync::MutexGuard<'_, Connection>,
            std::sync::MutexGuard<'_, Config>,
        ),
        String,
    > {
        let db = self.db.lock().map_err(|e| e.to_string())?;
        let config = self.config.lock().map_err(|e| e.to_string())?;
        Ok((db, config))
    }
}
