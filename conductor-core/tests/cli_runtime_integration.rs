//! Integration tests for CliRuntime.
//!
//! Uses a mock shell script to simulate a CLI agent. Requires tmux to be
//! available on the system; tests are skipped when tmux is absent.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

use conductor_core::agent_config::{AgentDef, AgentRole};
use conductor_core::config::AgentPermissionMode;
use conductor_core::config::RuntimeConfig;
use conductor_core::runtime::cli::CliRuntime;
use conductor_core::runtime::{AgentRuntime, RuntimeRequest};

fn tmux_available() -> bool {
    // tmux must be installed AND have an active server session
    std::process::Command::new("tmux")
        .arg("info")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

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

fn make_agent_def() -> AgentDef {
    AgentDef {
        name: "test".to_string(),
        role: AgentRole::Actor,
        can_commit: false,
        model: None,
        runtime: "cli".to_string(),
        prompt: "test prompt".to_string(),
    }
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

fn setup_test_db(run_id: &str) -> tempfile::NamedTempFile {
    let tmp = tempfile::NamedTempFile::new().expect("temp db file");
    let path = tmp.path().to_string_lossy().to_string();

    // Point CliRuntime at our test DB for this process (test threads are
    // serialised by cargo test when using env vars in a single binary).
    std::env::set_var("CONDUCTOR_DB_PATH", &path);

    let conn = conductor_core::db::open_database(tmp.path()).expect("open test db");
    // Insert a minimal run row so UPDATE in poll() has something to hit
    // and get_run() can return it.
    conn.execute(
        "INSERT INTO agent_runs (id, prompt, status, started_at, runtime) \
         VALUES (?1, 'test', 'running', '2024-01-01T00:00:00Z', 'claude')",
        rusqlite::params![run_id],
    )
    .expect("insert run");

    tmp
}

#[test]
fn test_cli_runtime_success() {
    if !tmux_available() {
        eprintln!("skipping cli_runtime test: tmux not available");
        return;
    }

    let json_body = r#"{"response":"hello world","stats":{"total_tokens":42}}"#;
    let (_guard, script_path) = make_mock_script(json_body, 0);
    let runtime = make_runtime(&script_path, "response", Some("stats.total_tokens"));

    let run_id = format!("test-cli-{}", ulid::Ulid::new());
    let _db_guard = setup_test_db(&run_id);

    let req = RuntimeRequest {
        run_id: run_id.clone(),
        agent_def: make_agent_def(),
        prompt: "test prompt".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        permission_mode: AgentPermissionMode::SkipPermissions,
        config_dir: None,
        bot_name: None,
        plugin_dirs: vec![],
    };

    runtime.spawn(&req).expect("spawn must succeed");

    let result = runtime
        .poll(&run_id, None, Duration::from_secs(10))
        .expect("poll must succeed");

    assert_eq!(result.runtime.as_str(), "cli");
    assert_eq!(
        result.status,
        conductor_core::agent::AgentRunStatus::Completed
    );
    assert!(
        result.tmux_window.is_some(),
        "tmux_window must be persisted so is_alive() and orphan reaper can track the run"
    );
}

/// Assert that a non-zero exit code causes the run to be marked `Failed`.
fn assert_nonzero_exit_maps_to_failed(exit_code: i32, run_id_prefix: &str) {
    if !tmux_available() {
        eprintln!("skipping cli_runtime test: tmux not available");
        return;
    }

    let json_body = r#"{"response":"error"}"#;
    let (_guard, script_path) = make_mock_script(json_body, exit_code);
    let runtime = make_runtime(&script_path, "response", None);

    let run_id = format!("{}-{}", run_id_prefix, ulid::Ulid::new());
    let _db_guard = setup_test_db(&run_id);

    let req = RuntimeRequest {
        run_id: run_id.clone(),
        agent_def: make_agent_def(),
        prompt: "bad prompt".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        permission_mode: AgentPermissionMode::SkipPermissions,
        config_dir: None,
        bot_name: None,
        plugin_dirs: vec![],
    };

    runtime.spawn(&req).expect("spawn must succeed");

    let result = runtime
        .poll(&run_id, None, Duration::from_secs(10))
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
    if !tmux_available() {
        eprintln!("skipping cli_runtime stdin test: tmux not available");
        return;
    }

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
    let _db_guard = setup_test_db(&run_id);

    let req = RuntimeRequest {
        run_id: run_id.clone(),
        agent_def: make_agent_def(),
        prompt: "hello from stdin".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        permission_mode: AgentPermissionMode::SkipPermissions,
        config_dir: None,
        bot_name: None,
        plugin_dirs: vec![],
    };

    runtime.spawn(&req).expect("stdin spawn must succeed");

    let result = runtime
        .poll(&run_id, None, Duration::from_secs(10))
        .expect("stdin poll must succeed");

    assert_eq!(
        result.status,
        conductor_core::agent::AgentRunStatus::Completed
    );
    let _ = f; // keep tempfile alive
}

/// Spawn a slow-sleeping script via CliRuntime and return the handles needed
/// to call `poll()` in the test.  Returns `(script_guard, db_guard, runtime,
/// run_id)` — callers must keep all guards alive for the duration of the test.
fn spawn_slow_script(
    id_prefix: &str,
) -> (
    tempfile::NamedTempFile,
    tempfile::NamedTempFile,
    CliRuntime,
    String,
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
    let db_guard = setup_test_db(&run_id);

    let req = RuntimeRequest {
        run_id: run_id.clone(),
        agent_def: make_agent_def(),
        prompt: "prompt".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        permission_mode: conductor_core::config::AgentPermissionMode::SkipPermissions,
        config_dir: None,
        bot_name: None,
        plugin_dirs: vec![],
    };

    runtime.spawn(&req).expect("spawn must succeed");

    (f, db_guard, runtime, run_id)
}

#[test]
fn test_cli_runtime_timeout_returns_no_result() {
    if !tmux_available() {
        eprintln!("skipping cli_runtime timeout test: tmux not available");
        return;
    }

    let (_script, _db, runtime, run_id) = spawn_slow_script("test-timeout");
    let result = runtime.poll(&run_id, None, Duration::from_millis(200));
    assert!(
        matches!(result, Err(conductor_core::runtime::PollError::NoResult)),
        "poll must return NoResult on timeout, got: {result:?}"
    );
}

#[test]
fn test_cli_runtime_shutdown_flag_cancels_poll() {
    if !tmux_available() {
        eprintln!("skipping cli_runtime shutdown test: tmux not available");
        return;
    }

    let (_script, _db, runtime, run_id) = spawn_slow_script("test-shutdown");
    // Pre-set the shutdown flag so poll() exits on the first iteration.
    let shutdown = Arc::new(AtomicBool::new(true));
    let result = runtime.poll(&run_id, Some(&shutdown), Duration::from_secs(10));
    assert!(
        matches!(result, Err(conductor_core::runtime::PollError::Cancelled)),
        "poll with shutdown flag set must return Cancelled, got: {result:?}"
    );
}

#[test]
fn test_cli_runtime_cancel_kills_window_and_marks_cancelled() {
    if !tmux_available() {
        eprintln!("skipping cli_runtime cancel test: tmux not available");
        return;
    }

    let (_script, db_guard, runtime, run_id) = spawn_slow_script("test-cancel");

    // Fetch the run from DB to get tmux_window name.
    let conn =
        conductor_core::db::open_database(db_guard.path()).expect("open test db for cancel test");
    let agent_mgr = conductor_core::agent::AgentManager::new(&conn);
    let run = agent_mgr
        .get_run(&run_id)
        .expect("get_run must succeed")
        .expect("run must exist in DB");

    assert!(
        run.tmux_window.is_some(),
        "tmux_window must be set after spawn"
    );
    assert!(runtime.is_alive(&run), "run must be alive before cancel");

    runtime.cancel(&run).expect("cancel must succeed");

    // Window should be gone.
    assert!(
        !runtime.is_alive(&run),
        "run must not be alive after cancel"
    );

    // DB row must be updated to Cancelled with ended_at set.
    assert_run_cancelled(&db_guard, &run_id);
}

#[test]
fn test_cli_runtime_cancel_with_no_tmux_window_marks_cancelled() {
    let run_id = format!("cancel-no-tmux-{}", ulid::Ulid::new());
    let db_guard = setup_test_db(&run_id);

    let conn =
        conductor_core::db::open_database(db_guard.path()).expect("open test db for cancel test");
    let agent_mgr = conductor_core::agent::AgentManager::new(&conn);

    let run = agent_mgr
        .get_run(&run_id)
        .expect("get_run must succeed")
        .expect("run must exist in DB");

    assert!(
        run.tmux_window.is_none(),
        "tmux_window must be None for this test"
    );

    let runtime = make_runtime("/bin/echo", "response", None);
    runtime
        .cancel(&run)
        .expect("cancel must succeed when tmux_window is None");

    assert_run_cancelled(&db_guard, &run_id);
}

#[test]
fn test_cli_runtime_rejects_invalid_run_id() {
    let runtime = make_runtime("/bin/echo", "response", None);
    let req = RuntimeRequest {
        run_id: "../../etc/cron.d/payload".to_string(),
        agent_def: make_agent_def(),
        prompt: "test".to_string(),
        model: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        permission_mode: AgentPermissionMode::SkipPermissions,
        config_dir: None,
        bot_name: None,
        plugin_dirs: vec![],
    };
    let err = runtime
        .spawn(&req)
        .expect_err("spawn must reject path-traversal run_id");
    assert!(
        err.to_string().contains("invalid run_id"),
        "error must mention invalid run_id, got: {err}"
    );
}
