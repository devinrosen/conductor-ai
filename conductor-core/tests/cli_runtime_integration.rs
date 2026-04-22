//! Integration tests for CliRuntime.
//!
//! Uses a mock shell script to simulate a CLI agent. No tmux dependency.

#[path = "common.rs"]
mod common;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::{atomic::AtomicBool, Arc, Mutex};
use std::time::Duration;

use conductor_core::config::RuntimeConfig;
use conductor_core::runtime::cli::CliRuntime;
use conductor_core::runtime::{AgentRuntime, RuntimeRequest};

// Serializes tests that mutate CONDUCTOR_DB_PATH so they don't race.
static DB_PATH_LOCK: Mutex<()> = Mutex::new(());

fn make_mock_script(json_body: &str, exit_code: i32) -> (tempfile::NamedTempFile, String) {
    let mut f = tempfile::Builder::new()
        .suffix(".sh")
        .tempfile()
        .expect("temp script");
    let escaped = json_body.replace('\'', "'\\''");
    writeln!(
        f,
        "#!/bin/sh\nprintf '%s' '{}'\nexit {}",
        escaped, exit_code
    )
    .unwrap();
    let path = f.path().to_string_lossy().to_string();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (f, path)
}

fn make_runtime(script_path: &str, result_field: &str, token_fields: Option<&str>) -> CliRuntime {
    CliRuntime::new(RuntimeConfig {
        runtime_type: Some("cli".to_string()),
        binary: Some("sh".to_string()),
        args: Some(vec![script_path.to_string()]),
        prompt_via: None,
        default_model: None,
        result_field: Some(result_field.to_string()),
        token_fields: token_fields.map(|s| s.to_string()),
        api_key_env: None,
        command: None,
    })
}

/// Open (or create) the integration test DB at a temp file, run migrations,
/// insert an agent_run row for `run_id`, and return the temp file guard
/// (must stay alive for the duration of the test).
///
/// The DB path is set via `CONDUCTOR_DB_PATH` so `CliRuntime::spawn/poll`
/// use the same file.
fn assert_run_cancelled(db_guard: &tempfile::NamedTempFile, run_id: &str) {
    let conn =
        conductor_core::db::open_database(db_guard.path()).expect("open test db for cancel check");
    let agent_mgr = conductor_core::agent::AgentManager::new(&conn);
    let updated = agent_mgr
        .get_run(run_id)
        .expect("get_run must succeed after cancel")
        .expect("run must still exist in DB after cancel");
    assert_eq!(
        updated.status,
        conductor_core::agent::AgentRunStatus::Cancelled,
        "status must be Cancelled after cancel()"
    );
    assert!(
        updated.ended_at.is_some(),
        "ended_at must be set after cancel()"
    );
}

fn setup_test_db(run_id: &str) -> (tempfile::NamedTempFile, std::sync::MutexGuard<'static, ()>) {
    let lock = DB_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = common::setup_test_db(run_id, "claude");
    let path = tmp.path().to_string_lossy().to_string();
    std::env::set_var("CONDUCTOR_DB_PATH", &path);
    (tmp, lock)
}

#[test]
fn test_cli_runtime_success() {
    let json_body = r#"{"response":"hello world","stats":{"total_tokens":42}}"#;
    let (_guard, script_path) = make_mock_script(json_body, 0);
    let runtime = make_runtime(&script_path, "response", Some("stats.total_tokens"));

    let run_id = format!("test-cli-{}", ulid::Ulid::new());
    let (_db_guard, _lock) = setup_test_db(&run_id);

    let req = RuntimeRequest {
        run_id: run_id.clone(),
        agent_def: common::make_agent_def("cli"),
        prompt: "test prompt".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        bot_name: None,
        plugin_dirs: vec![],
        db_path: _db_guard.path().to_path_buf(),
    };

    runtime.spawn_validated(&req).expect("spawn must succeed");

    let result = runtime
        .poll(&run_id, None, Duration::from_secs(10), _db_guard.path())
        .expect("poll must succeed");

    assert_eq!(result.runtime.as_str(), "cli");
    assert_eq!(
        result.status,
        conductor_core::agent::AgentRunStatus::Completed
    );
    assert!(
        result.subprocess_pid.is_some(),
        "subprocess_pid must be persisted so is_alive() and orphan reaper can track the run"
    );
    assert_eq!(
        result.input_tokens,
        Some(42),
        "token_fields extraction must persist stats.total_tokens into input_tokens"
    );
}

/// Assert that a non-zero exit code causes the run to be marked `Failed`.
fn assert_nonzero_exit_maps_to_failed(exit_code: i32, run_id_prefix: &str) {
    let json_body = r#"{"response":"error"}"#;
    let (_guard, script_path) = make_mock_script(json_body, exit_code);
    let runtime = make_runtime(&script_path, "response", None);

    let run_id = format!("{}-{}", run_id_prefix, ulid::Ulid::new());
    let (_db_guard, _lock) = setup_test_db(&run_id);

    let req = RuntimeRequest {
        run_id: run_id.clone(),
        agent_def: common::make_agent_def("cli"),
        prompt: "bad prompt".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        bot_name: None,
        plugin_dirs: vec![],
        db_path: _db_guard.path().to_path_buf(),
    };

    runtime.spawn_validated(&req).expect("spawn must succeed");

    let result = runtime
        .poll(&run_id, None, Duration::from_secs(10), _db_guard.path())
        .expect("poll must succeed — even failed runs are returned as Ok(AgentRun)");

    assert_eq!(
        result.status,
        conductor_core::agent::AgentRunStatus::Failed,
        "exit code {exit_code} must map to a failed run"
    );
}

#[test]
fn test_cli_runtime_exit_code_1_is_error() {
    assert_nonzero_exit_maps_to_failed(1, "test-cli1");
}

#[test]
fn test_cli_runtime_exit_code_42_is_error() {
    assert_nonzero_exit_maps_to_failed(42, "test-cli42");
}

#[test]
fn test_cli_runtime_exit_code_53_is_error() {
    assert_nonzero_exit_maps_to_failed(53, "test-cli53");
}

#[test]
fn test_cli_runtime_stdin_mode() {
    // Script that reads stdin and echoes it as JSON output.
    let mut f = tempfile::Builder::new()
        .suffix(".sh")
        .tempfile()
        .expect("temp script");
    writeln!(
        f,
        r#"#!/bin/sh
read line
printf '{{"response":"%s"}}' "$line"
exit 0"#
    )
    .unwrap();
    let path = f.path().to_string_lossy().to_string();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let runtime = CliRuntime::new(RuntimeConfig {
        runtime_type: Some("cli".to_string()),
        binary: Some("sh".to_string()),
        args: Some(vec![path.clone()]),
        prompt_via: Some("stdin".to_string()),
        result_field: Some("response".to_string()),
        ..RuntimeConfig::default()
    });

    let run_id = format!("test-stdin-{}", ulid::Ulid::new());
    let (_db_guard, _lock) = setup_test_db(&run_id);

    let req = RuntimeRequest {
        run_id: run_id.clone(),
        agent_def: common::make_agent_def("cli"),
        prompt: "hello from stdin".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        bot_name: None,
        plugin_dirs: vec![],
        db_path: _db_guard.path().to_path_buf(),
    };

    runtime
        .spawn_validated(&req)
        .expect("stdin spawn must succeed");

    let result = runtime
        .poll(&run_id, None, Duration::from_secs(10), _db_guard.path())
        .expect("stdin poll must succeed");

    assert_eq!(
        result.status,
        conductor_core::agent::AgentRunStatus::Completed
    );
    let _ = f; // keep tempfile alive
}

/// Spawn a slow-sleeping script via CliRuntime and return the handles needed
/// to call `poll()` in the test.  Returns `(script_guard, db_guard, runtime,
/// run_id, lock)` — callers must keep all guards alive for the duration of the test.
fn spawn_slow_script(
    id_prefix: &str,
) -> (
    tempfile::NamedTempFile,
    tempfile::NamedTempFile,
    CliRuntime,
    String,
    std::sync::MutexGuard<'static, ()>,
) {
    let mut f = tempfile::Builder::new()
        .suffix(".sh")
        .tempfile()
        .expect("temp script");
    writeln!(f, "#!/bin/sh\nsleep 30\necho '{{}}'\nexit 0").unwrap();
    let path = f.path().to_string_lossy().to_string();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let runtime = CliRuntime::new(RuntimeConfig {
        runtime_type: Some("cli".to_string()),
        binary: Some("sh".to_string()),
        args: Some(vec![path.clone()]),
        ..RuntimeConfig::default()
    });

    let run_id = format!("{}-{}", id_prefix, ulid::Ulid::new());
    let (db_guard, lock) = setup_test_db(&run_id);

    let req = RuntimeRequest {
        run_id: run_id.clone(),
        agent_def: common::make_agent_def("cli"),
        prompt: "prompt".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        bot_name: None,
        plugin_dirs: vec![],
        db_path: db_guard.path().to_path_buf(),
    };

    runtime.spawn_validated(&req).expect("spawn must succeed");

    (f, db_guard, runtime, run_id, lock)
}

#[test]
fn test_cli_runtime_timeout_returns_no_result() {
    let (_script, _db, runtime, run_id, _lock) = spawn_slow_script("test-timeout");
    let result = runtime.poll(&run_id, None, Duration::from_millis(200), _db.path());
    assert!(
        matches!(result, Err(conductor_core::runtime::PollError::NoResult)),
        "poll must return NoResult on timeout, got: {result:?}"
    );
}

#[test]
fn test_cli_runtime_shutdown_flag_cancels_poll() {
    let (_script, _db, runtime, run_id, _lock) = spawn_slow_script("test-shutdown");
    // Pre-set the shutdown flag so poll() exits on the first iteration.
    let shutdown = Arc::new(AtomicBool::new(true));
    let result = runtime.poll(
        &run_id,
        Some(&shutdown),
        Duration::from_secs(10),
        _db.path(),
    );
    assert!(
        matches!(result, Err(conductor_core::runtime::PollError::Cancelled)),
        "poll with shutdown flag set must return Cancelled, got: {result:?}"
    );
}

#[test]
fn test_cli_runtime_cancel_kills_process_and_marks_cancelled() {
    let (_script, db_guard, runtime, run_id, _lock) = spawn_slow_script("test-cancel");

    // Fetch the run from DB to get subprocess_pid.
    let conn =
        conductor_core::db::open_database(db_guard.path()).expect("open test db for cancel test");
    let agent_mgr = conductor_core::agent::AgentManager::new(&conn);
    let run = agent_mgr
        .get_run(&run_id)
        .expect("get_run must succeed")
        .expect("run must exist in DB");

    assert!(
        run.subprocess_pid.is_some(),
        "subprocess_pid must be set after spawn"
    );
    assert!(runtime.is_alive(&run), "run must be alive before cancel");

    runtime
        .cancel(&run, db_guard.path())
        .expect("cancel must succeed");

    // Process should be gone.
    assert!(
        !runtime.is_alive(&run),
        "run must not be alive after cancel"
    );

    // DB row must be updated to Cancelled with ended_at set.
    assert_run_cancelled(&db_guard, &run_id);
}

#[test]
fn test_cli_runtime_cancel_with_no_pid_marks_cancelled() {
    let run_id = format!("cancel-no-pid-{}", ulid::Ulid::new());
    let (db_guard, _lock) = setup_test_db(&run_id);

    let conn =
        conductor_core::db::open_database(db_guard.path()).expect("open test db for cancel test");
    let agent_mgr = conductor_core::agent::AgentManager::new(&conn);

    let run = agent_mgr
        .get_run(&run_id)
        .expect("get_run must succeed")
        .expect("run must exist in DB");

    assert!(
        run.subprocess_pid.is_none(),
        "subprocess_pid must be None for this test"
    );

    let runtime = make_runtime("/bin/echo", "response", None);
    runtime
        .cancel(&run, db_guard.path())
        .expect("cancel must succeed when subprocess_pid is None");

    assert_run_cancelled(&db_guard, &run_id);
}

#[test]
fn test_cli_runtime_rejects_invalid_run_id() {
    let runtime = make_runtime("/bin/echo", "response", None);
    let req = RuntimeRequest {
        run_id: "../../etc/cron.d/payload".to_string(),
        agent_def: common::make_agent_def("cli"),
        prompt: "test".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        bot_name: None,
        plugin_dirs: vec![],
        db_path: conductor_core::config::db_path(),
    };
    let err = runtime
        .spawn_validated(&req)
        .expect_err("spawn must reject path-traversal run_id");
    assert!(
        err.to_string().contains("invalid run_id"),
        "error must mention invalid run_id, got: {err}"
    );
}

// Regression test: poll() must not panic when the output file is unreadable after the
// process exits. Instead it must log the error and return the fallback result.
#[cfg(unix)]
#[test]
fn test_cli_runtime_poll_handles_unreadable_output_file() {
    // A script that exits immediately with code 0 so poll() reaches the file-read path.
    let (_script, script_path) = make_mock_script("{}", 0);
    let runtime = make_runtime(&script_path, "response", None);

    let run_id = format!("test-unreadable-{}", ulid::Ulid::new());
    let (_db_guard, _lock) = setup_test_db(&run_id);

    let req = RuntimeRequest {
        run_id: run_id.clone(),
        agent_def: common::make_agent_def("cli"),
        prompt: "test".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        bot_name: None,
        plugin_dirs: vec![],
        db_path: _db_guard.path().to_path_buf(),
    };

    runtime.spawn_validated(&req).expect("spawn must succeed");

    // Give the process time to exit so try_wait() returns Some(status).
    std::thread::sleep(Duration::from_millis(200));

    // Make the output file unreadable to trigger the error-logging path.
    let output_path = conductor_core::config::conductor_dir()
        .join("workspaces")
        .join(&run_id)
        .join("output.json");
    std::fs::set_permissions(&output_path, std::fs::Permissions::from_mode(0o000))
        .expect("chmod output file to 000");

    // poll() must not panic and must return Ok with the run marked Completed.
    let result = runtime
        .poll(&run_id, None, Duration::from_secs(5), _db_guard.path())
        .expect("poll must succeed even when output file is unreadable");

    // Restore permissions so the temp directory can be cleaned up.
    let _ = std::fs::set_permissions(&output_path, std::fs::Permissions::from_mode(0o644));

    assert_eq!(
        result.status,
        conductor_core::agent::AgentRunStatus::Completed,
        "run must be Completed when process exits 0 even if output file is unreadable"
    );
}

// Regression: when a non-zero exit process has an unreadable output file, poll() must
// still return Ok (not panic) and mark the run Failed with the fallback exit-code message.
#[cfg(unix)]
#[test]
fn test_cli_runtime_poll_handles_unreadable_output_file_on_error_exit() {
    let (_script, script_path) = make_mock_script("{}", 1);
    let runtime = make_runtime(&script_path, "response", None);

    let run_id = format!("test-unreadable-err-{}", ulid::Ulid::new());
    let (_db_guard, _lock) = setup_test_db(&run_id);

    let req = RuntimeRequest {
        run_id: run_id.clone(),
        agent_def: common::make_agent_def("cli"),
        prompt: "test".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        bot_name: None,
        plugin_dirs: vec![],
        db_path: _db_guard.path().to_path_buf(),
    };

    runtime.spawn_validated(&req).expect("spawn must succeed");
    std::thread::sleep(Duration::from_millis(200));

    let output_path = conductor_core::config::conductor_dir()
        .join("workspaces")
        .join(&run_id)
        .join("output.json");
    std::fs::set_permissions(&output_path, std::fs::Permissions::from_mode(0o000))
        .expect("chmod output file to 000");

    let result = runtime
        .poll(&run_id, None, Duration::from_secs(5), _db_guard.path())
        .expect("poll must succeed even when output file is unreadable");

    let _ = std::fs::set_permissions(&output_path, std::fs::Permissions::from_mode(0o644));

    assert_eq!(
        result.status,
        conductor_core::agent::AgentRunStatus::Failed,
        "run must be Failed when process exits 1 even if output file is unreadable"
    );
    assert!(
        result
            .result_text
            .as_deref()
            .unwrap_or("")
            .contains("process exited with code 1"),
        "result_text must contain fallback exit-code message, got: {:?}",
        result.result_text
    );
}
