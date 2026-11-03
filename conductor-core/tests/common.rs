#![allow(dead_code)]

use std::sync::Arc;

use conductor_core::agent_config::{AgentDef, AgentRole};
use conductor_core::runtime::adapter::SqliteHostAdapter;
use conductor_core::runtime::RuntimeRequest;

pub fn make_agent_def(runtime: &str) -> AgentDef {
    AgentDef {
        name: "test".to_string(),
        role: AgentRole::Actor,
        can_commit: false,
        model: None,
        runtime: runtime.to_string(),
        prompt: "test prompt".to_string(),
    }
}

pub fn setup_test_db(run_id: &str, runtime: &str) -> tempfile::NamedTempFile {
    let tmp = tempfile::NamedTempFile::new().expect("temp db file");

    let conn = conductor_core::db::open_database(tmp.path()).expect("open test db");
    conn.execute(
        "INSERT INTO agent_runs (id, prompt, status, started_at, runtime) \
         VALUES (?1, 'test', 'running', '2024-01-01T00:00:00Z', ?2)",
        rusqlite::params![run_id, runtime],
    )
    .expect("insert run");

    tmp
}

/// Build a [`RuntimeRequest`] with a [`SqliteHostAdapter`] tracker/sink backed by `db_path`.
pub fn make_request(
    run_id: &str,
    prompt: &str,
    db_path: std::path::PathBuf,
    runtime: &str,
) -> RuntimeRequest {
    let tracker = Arc::new(SqliteHostAdapter::new(db_path.clone()).unwrap());
    let event_sink = tracker.clone();
    let agent_def = make_agent_def(runtime);
    let effective_runtime = agent_def.runtime.clone();
    RuntimeRequest {
        run_id: run_id.to_string(),
        agent_def,
        prompt: prompt.to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        extra_cli_args: vec![],
        plugin_dirs: vec![],
        resume_session_id: None,
        effective_runtime,
        tracker,
        event_sink,
    }
}

// Integration tests link the lib without `test_helpers`, so isolate here (#3172).
// `mod common;` pulls this into every integration-test binary.
static ISOLATED_HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

#[ctor::ctor]
fn isolate_conductor_home() {
    if std::env::var_os("CONDUCTOR_HOME").is_some_and(|h| !h.is_empty()) {
        return;
    }
    let dir = tempfile::Builder::new()
        .prefix("conductor-test-home-")
        .tempdir()
        .expect("create isolated CONDUCTOR_HOME")
        .keep();
    std::env::set_var("CONDUCTOR_HOME", &dir);
    let _ = ISOLATED_HOME.set(dir);
}

#[ctor::dtor]
fn cleanup_conductor_home() {
    if let Some(dir) = ISOLATED_HOME.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}
