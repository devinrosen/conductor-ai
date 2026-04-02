use std::sync::Arc;

use conductor_core::config::Config;
use tokio::sync::{Mutex, RwLock};

use crate::events::EventBus;
use crate::state::AppState;

/// AppState backed by a fresh in-memory DB with migrations applied. No seed data.
pub fn empty_state() -> AppState {
    let conn = conductor_core::test_helpers::create_test_conn();
    AppState {
        db: Arc::new(Mutex::new(conn)),
        config: Arc::new(RwLock::new(Config::default())),
        events: EventBus::new(1),
        workflow_done_notify: None,
    }
}

/// AppState with repo `r1` + worktree `w1` pre-seeded.
pub fn seeded_state() -> AppState {
    let conn = conductor_core::test_helpers::setup_db();
    AppState {
        db: Arc::new(Mutex::new(conn)),
        config: Arc::new(RwLock::new(Config::default())),
        events: EventBus::new(1),
        workflow_done_notify: None,
    }
}

/// AppState with repo `r1`, worktree `w1`, and agent_run `ar1` pre-seeded.
pub fn seeded_state_with_agent_run() -> AppState {
    let conn = conductor_core::test_helpers::setup_db_with_agent_run();
    AppState {
        db: Arc::new(Mutex::new(conn)),
        config: Arc::new(RwLock::new(Config::default())),
        events: EventBus::new(1),
        workflow_done_notify: None,
    }
}
