#![allow(dead_code)]

use conductor_core::agent_config::{AgentDef, AgentRole};

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
