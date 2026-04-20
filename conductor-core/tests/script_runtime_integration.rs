//! Integration tests for ScriptRuntime.
//!
//! Uses /bin/sh commands only for CI portability (no Python dependency).

use std::sync::Mutex;
use std::time::Duration;

// Serialize tests that mutate CONDUCTOR_DB_PATH.
static DB_PATH_LOCK: Mutex<()> = Mutex::new(());

use conductor_core::agent_config::{AgentDef, AgentRole};
use conductor_core::config::{AgentPermissionMode, RuntimeConfig};
use conductor_core::runtime::script::ScriptRuntime;
use conductor_core::runtime::{AgentRuntime, RuntimeRequest};

fn make_runtime(command: Option<&str>) -> ScriptRuntime {
    ScriptRuntime::new(RuntimeConfig {
        command: command.map(|s| s.to_string()),
        ..RuntimeConfig::default()
    })
}

fn make_agent_def() -> AgentDef {
    AgentDef {
        name: "test".to_string(),
        role: AgentRole::Actor,
        can_commit: false,
        model: None,
        runtime: "script".to_string(),
        prompt: "test prompt".to_string(),
    }
}

fn setup_test_db(run_id: &str) -> tempfile::NamedTempFile {
    let tmp = tempfile::NamedTempFile::new().expect("temp db file");
    let path = tmp.path().to_string_lossy().to_string();

    std::env::set_var("CONDUCTOR_DB_PATH", &path);

    let conn = conductor_core::db::open_database(tmp.path()).expect("open test db");
    conn.execute(
        "INSERT INTO agent_runs (id, prompt, status, started_at, runtime) \
         VALUES (?1, 'test', 'running', '2024-01-01T00:00:00Z', 'script')",
        rusqlite::params![run_id],
    )
    .expect("insert run");

    tmp
}

fn make_request(run_id: &str, prompt: &str) -> RuntimeRequest {
    RuntimeRequest {
        run_id: run_id.to_string(),
        agent_def: make_agent_def(),
        prompt: prompt.to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        permission_mode: AgentPermissionMode::SkipPermissions,
        config_dir: None,
        bot_name: None,
        plugin_dirs: vec![],
    }
}

#[test]
fn test_script_runtime_success() {
    let _lock = DB_PATH_LOCK.lock().unwrap();
    let run_id = format!("test-script-{}", ulid::Ulid::new());
    let _db_guard = setup_test_db(&run_id);

    let runtime = make_runtime(Some("echo hello"));
    let req = make_request(&run_id, "test prompt");

    runtime.spawn(&req).expect("spawn must succeed");

    let result = runtime
        .poll(&run_id, None, Duration::from_secs(5))
        .expect("poll must succeed");

    assert_eq!(
        result.status,
        conductor_core::agent::AgentRunStatus::Completed
    );
    let text = result.result_text.expect("result_text must be set");
    assert!(
        text.contains("hello"),
        "expected 'hello' in output, got: {text}"
    );
}

#[test]
fn test_script_runtime_captures_conductor_prompt() {
    let _lock = DB_PATH_LOCK.lock().unwrap();
    let run_id = format!("test-script-prompt-{}", ulid::Ulid::new());
    let _db_guard = setup_test_db(&run_id);

    let runtime = make_runtime(Some("echo $CONDUCTOR_PROMPT"));
    let req = make_request(&run_id, "my-unique-prompt-string");

    runtime.spawn(&req).expect("spawn must succeed");

    let result = runtime
        .poll(&run_id, None, Duration::from_secs(5))
        .expect("poll must succeed");

    assert_eq!(
        result.status,
        conductor_core::agent::AgentRunStatus::Completed
    );
    let text = result.result_text.expect("result_text must be set");
    assert!(
        text.contains("my-unique-prompt-string"),
        "expected CONDUCTOR_PROMPT in output, got: {text}"
    );
}

#[test]
fn test_script_runtime_missing_command_errors() {
    let _lock = DB_PATH_LOCK.lock().unwrap();
    let run_id = format!("test-script-nocmd-{}", ulid::Ulid::new());
    let _db_guard = setup_test_db(&run_id);

    let runtime = make_runtime(None);
    let req = make_request(&run_id, "prompt");

    let err = runtime
        .spawn(&req)
        .expect_err("spawn must fail without command");
    assert!(
        err.to_string().contains("command"),
        "error must mention 'command', got: {err}"
    );
}

#[test]
fn test_script_runtime_nonzero_exit_is_failed() {
    let _lock = DB_PATH_LOCK.lock().unwrap();
    let run_id = format!("test-script-fail-{}", ulid::Ulid::new());
    let _db_guard = setup_test_db(&run_id);

    let runtime = make_runtime(Some("exit 1"));
    let req = make_request(&run_id, "prompt");

    runtime
        .spawn(&req)
        .expect("spawn must succeed even for non-zero exit");

    let result = runtime.poll(&run_id, None, Duration::from_secs(5));
    assert!(
        matches!(result, Err(conductor_core::runtime::PollError::Failed(_))),
        "non-zero exit must map to PollError::Failed, got: {result:?}"
    );
}

#[test]
fn test_script_runtime_resolve_via_config() {
    use conductor_core::config::{Config, RuntimeConfig};
    use std::collections::HashMap;

    let mut runtimes = HashMap::new();
    runtimes.insert(
        "my-script".to_string(),
        RuntimeConfig {
            runtime_type: Some("script".to_string()),
            command: Some("echo hi".to_string()),
            ..RuntimeConfig::default()
        },
    );
    let config = Config {
        runtimes,
        ..Config::default()
    };

    let runtime = conductor_core::runtime::resolve_runtime("my-script", &config);
    assert!(
        runtime.is_ok(),
        "resolve_runtime must return Ok for type=script"
    );
}
