//! Integration tests for ScriptRuntime.
//!
//! Uses /bin/sh commands only for CI portability (no Python dependency).

#[path = "common.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use conductor_core::config::RuntimeConfig;
use conductor_core::runtime::script::ScriptRuntime;
use conductor_core::runtime::AgentRuntime;

fn make_runtime(command: Option<&str>) -> ScriptRuntime {
    ScriptRuntime::new(RuntimeConfig {
        command: command.map(|s| s.to_string()),
        ..RuntimeConfig::default()
    })
}



#[test]
fn test_script_runtime_success() {
    let run_id = format!("test-script-{}", ulid::Ulid::new());
    let _db_guard = common::setup_test_db(&run_id, "script");

    let runtime = make_runtime(Some("echo hello"));
    let req = common::make_request(&run_id, "test prompt", _db_guard.path().to_path_buf(), "script");

    runtime.spawn_validated(&req).expect("spawn must succeed");

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
    let run_id = format!("test-script-prompt-{}", ulid::Ulid::new());
    let _db_guard = common::setup_test_db(&run_id, "script");

    let runtime = make_runtime(Some("echo $CONDUCTOR_PROMPT"));
    let req = common::make_request(
        &run_id,
        "my-unique-prompt-string",
        _db_guard.path().to_path_buf(),
        "script",
    );

    runtime.spawn_validated(&req).expect("spawn must succeed");

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
    let run_id = format!("test-script-nocmd-{}", ulid::Ulid::new());
    let _db_guard = common::setup_test_db(&run_id, "script");

    let runtime = make_runtime(None);
    let req = common::make_request(&run_id, "prompt", _db_guard.path().to_path_buf(), "script");

    let err = runtime
        .spawn_validated(&req)
        .expect_err("spawn must fail without command");
    assert!(
        err.to_string().contains("command"),
        "error must mention 'command', got: {err}"
    );
}

#[test]
fn test_script_runtime_nonzero_exit_is_failed() {
    let run_id = format!("test-script-fail-{}", ulid::Ulid::new());
    let _db_guard = common::setup_test_db(&run_id, "script");

    let runtime = make_runtime(Some("exit 1"));
    let req = common::make_request(&run_id, "prompt", _db_guard.path().to_path_buf(), "script");

    runtime
        .spawn_validated(&req)
        .expect("spawn must succeed even for non-zero exit");

    let result = runtime.poll(&run_id, None, Duration::from_secs(5));
    assert!(
        matches!(result, Err(conductor_core::runtime::PollError::Failed(_))),
        "non-zero exit must map to PollError::Failed, got: {result:?}"
    );
}

#[test]
fn test_script_runtime_nonzero_exit_with_stderr() {
    let run_id = format!("test-script-stderr-{}", ulid::Ulid::new());
    let _db_guard = common::setup_test_db(&run_id, "script");

    let runtime = make_runtime(Some("echo 'something went wrong' >&2; exit 2"));
    let req = common::make_request(&run_id, "prompt", _db_guard.path().to_path_buf(), "script");

    runtime
        .spawn_validated(&req)
        .expect("spawn must succeed even for non-zero exit");

    let result = runtime.poll(&run_id, None, Duration::from_secs(5));
    match result {
        Err(conductor_core::runtime::PollError::Failed(msg)) => {
            assert!(
                msg.contains("something went wrong"),
                "error message must include stderr, got: {msg}"
            );
            assert!(
                msg.contains("exit code 2") || msg.contains("code 2"),
                "error message must include exit code, got: {msg}"
            );
        }
        other => panic!("expected PollError::Failed with stderr content, got: {other:?}"),
    }
}

#[test]
fn test_script_runtime_resolve_via_config() {
    use conductor_core::config::{AgentPermissionMode, Config, RuntimeConfig};
    use conductor_core::runtime::RuntimeOptions;
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

    let options = RuntimeOptions {
        binary_path: std::path::PathBuf::from("conductor"),
        log_path_for_run: Arc::new(|run_id| {
            conductor_core::config::agent_log_path(run_id)
                .unwrap_or_else(|_| std::env::temp_dir().join(format!("{run_id}.log")))
        }),
        workspace_root: std::path::PathBuf::from("/tmp"),
    };

    let runtime = conductor_core::runtime::resolve_runtime(
        "my-script",
        AgentPermissionMode::default(),
        &config.runtimes,
        &options,
    );
    assert!(
        runtime.is_ok(),
        "resolve_runtime must return Ok for type=script"
    );
}

#[test]
fn test_script_runtime_rejects_invalid_run_id() {
    let runtime = make_runtime(Some("echo hello"));
    let req = common::make_request(
        "../../etc/cron.d/payload",
        "test",
        conductor_core::config::db_path(),
        "script",
    );
    let err = runtime
        .spawn_validated(&req)
        .expect_err("spawn must reject path-traversal run_id");
    assert!(
        err.to_string().contains("invalid run_id"),
        "error must mention invalid run_id, got: {err}"
    );
}
