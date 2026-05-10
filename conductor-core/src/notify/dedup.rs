use std::path::PathBuf;

use runkon_notify::{DedupStore, NotifyError};

use crate::config::db_path;

/// SQLite-backed dedup store for notification events.
///
/// Opens a fresh `rusqlite::Connection` per `try_claim` call to avoid threading
/// `Arc<Mutex<Connection>>` through all `fire_*` function signatures.
pub struct SqliteDedupStore {
    path: PathBuf,
}

impl SqliteDedupStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Create a store targeting the default conductor database path (`~/.conductor/conductor.db`).
    pub fn default_db() -> Self {
        Self { path: db_path() }
    }
}

impl DedupStore for SqliteDedupStore {
    fn try_claim(&self, entity_id: &str, event_type: &str) -> runkon_notify::Result<bool> {
        let conn = rusqlite::Connection::open(&self.path)
            .map_err(|e| NotifyError::Dispatch(format!("dedup DB open: {e}")))?;

        let now = chrono::Utc::now().to_rfc3339();
        match conn.execute(
            "INSERT OR IGNORE INTO notification_log \
             (entity_id, event_type, fired_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![entity_id, event_type, now],
        ) {
            Ok(rows) => Ok(rows == 1),
            Err(e) => {
                tracing::warn!(entity_id, event_type, "SqliteDedupStore error: {e}");
                Err(NotifyError::Dispatch(format!("dedup DB error: {e}")))
            }
        }
    }
}
