use std::path::PathBuf;
use std::sync::Arc;

use conductor_core::config::Config;
use rusqlite::Connection;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::events::EventBus;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub config: Arc<RwLock<Config>>,
    pub events: EventBus,
    /// Path to the SQLite database file. Used by `spawn_blocking` closures that
    /// need their own `rusqlite::Connection` (which is not `Send`).
    pub db_path: PathBuf,
    /// Signalled by `run_workflow`'s background task when `execute_workflow` returns.
    /// `None` in production. Populated in tests that need deterministic synchronization.
    pub workflow_done_notify: Option<Arc<Notify>>,
}

impl AppState {
    /// Construct a production `AppState` with the given connection, config, and event bus capacity.
    pub fn new(conn: Connection, config: Config, db_path: PathBuf, event_capacity: usize) -> Self {
        Self {
            db: Arc::new(Mutex::new(conn)),
            config: Arc::new(RwLock::new(config)),
            events: EventBus::new(event_capacity),
            db_path,
            workflow_done_notify: None,
        }
    }
}
