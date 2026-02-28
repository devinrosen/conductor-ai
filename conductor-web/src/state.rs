use std::sync::Arc;

use conductor_core::config::Config;
use rusqlite::Connection;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub config: Arc<Config>,
}
