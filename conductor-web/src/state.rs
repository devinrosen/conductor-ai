use std::sync::Arc;

use conductor_core::config::Config;
use rusqlite::Connection;
use tokio::sync::{Mutex, RwLock};

use crate::events::EventBus;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub config: Arc<RwLock<Config>>,
    pub events: EventBus,
}
