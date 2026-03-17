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
    /// Signalled by `run_workflow`'s background task when `execute_workflow` returns.
    /// `None` in production. Populated in tests that need deterministic synchronization.
    pub workflow_done_notify: Option<Arc<Notify>>,
}
