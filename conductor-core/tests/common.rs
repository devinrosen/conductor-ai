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
    RuntimeRequest {
        run_id: run_id.to_string(),
        agent_def: make_agent_def(runtime),
        prompt: prompt.to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        extra_cli_args: vec![],
        plugin_dirs: vec![],
        resume_session_id: None,
        tracker,
        event_sink,
    }
}
