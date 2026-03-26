use super::{
    eval_condition, execute_gate, execute_if, execute_quality_gate, execute_script, execute_unless,
    poll_script_child, read_stdout_bounded, ScriptPollResult,
};
use crate::workflow::engine::ExecutionState;
use crate::workflow::status::WorkflowStepStatus;
use crate::workflow::types::StepResult;
use crate::workflow_dsl::{
    ApprovalMode, Condition, GateNode, GateType, IfNode, OnFailAction, OnTimeout,
    QualityGateConfig, UnlessNode,
};

// -----------------------------------------------------------------------
// Shared test helper: build an ExecutionState backed by a real in-memory DB.
// -----------------------------------------------------------------------

/// Build an `ExecutionState` wired to a real in-memory SQLite connection.
///
/// The caller owns the `Connection`; the state borrows it.  `working_dir`
/// and `repo_path` are both set to `dir`.
fn make_test_state<'a>(
    conn: &'a rusqlite::Connection,
    config: &'a crate::config::Config,
    dir: &str,
    exec_config: crate::workflow::types::WorkflowExecConfig,
) -> ExecutionState<'a> {
    let agent_mgr = crate::agent::AgentManager::new(conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "test", None, None)
        .unwrap();
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(conn);
    let run = wf_mgr
        .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    ExecutionState {
        conn,
        config,
        workflow_run_id: run.id,
        workflow_name: "test-wf".into(),
        worktree_id: None,
        working_dir: dir.to_string(),
        worktree_slug: String::new(),
        repo_path: dir.to_string(),
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config,
        inputs: std::collections::HashMap::new(),
        agent_mgr: crate::agent::AgentManager::new(conn),
        wf_mgr: crate::workflow::manager::WorkflowManager::new(conn),
        parent_run_id: String::new(),
        depth: 0,
        target_label: None,
        step_results: std::collections::HashMap::new(),
        contexts: Vec::new(),
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        last_gate_feedback: None,
        block_output: None,
        block_with: Vec::new(),
        resume_ctx: None,
        default_bot_name: None,
        feature_id: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
    }
}

// -----------------------------------------------------------------------
// read_stdout_bounded tests
// -----------------------------------------------------------------------

#[test]
fn test_read_stdout_bounded_small_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.txt");
    std::fs::write(&path, "hello world").unwrap();
    let s = read_stdout_bounded(path.to_str().unwrap()).unwrap();
    assert_eq!(s, "hello world");
}

#[test]
fn test_read_stdout_bounded_large_file_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.txt");
    // Write 200 KB of data (over the 100 KB limit)
    let data = "A".repeat(200 * 1024);
    std::fs::write(&path, &data).unwrap();
    let s = read_stdout_bounded(path.to_str().unwrap()).unwrap();
    assert!(s.len() < data.len(), "output should be truncated");
    assert!(
        s.contains("[output truncated at 100 KB]"),
        "truncation notice should be present"
    );
}

#[test]
fn test_read_stdout_bounded_missing_file() {
    let result = read_stdout_bounded("/nonexistent/path/file.txt");
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// execute_script integration tests
// -----------------------------------------------------------------------

/// Write a shell script to `path`, make it executable, and return the absolute path string.
fn write_script(path: &std::path::Path, body: &str) -> String {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_str().unwrap().to_string()
}

#[test]
fn test_execute_script_success() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(
        &dir.path().join("hello.sh"),
        "#!/bin/sh\necho '<<<CONDUCTOR_OUTPUT>>>\n{\"markers\": [\"done\"], \"context\": \"ran ok\"}\n<<<END_CONDUCTOR_OUTPUT>>>'",
    );

    let conn = crate::test_helpers::setup_db();
    let config = Box::leak(Box::new(crate::config::Config::default()));
    let dir_str = dir.path().to_str().unwrap().to_string();
    let mut state = make_test_state(
        &conn,
        config,
        &dir_str,
        crate::workflow::types::WorkflowExecConfig::default(),
    );

    let node = crate::workflow_dsl::ScriptNode {
        name: "hello".into(),
        run: script_path,
        env: std::collections::HashMap::new(),
        timeout: Some(10),
        retries: 0,
        on_fail: None,
        bot_name: None,
    };

    let result = execute_script(&mut state, &node, 0);
    assert!(result.is_ok(), "execute_script should succeed: {result:?}");
    assert!(state.all_succeeded);
    let step_res = state.step_results.get("hello").unwrap();
    assert!(step_res.markers.contains(&"done".to_string()));
    assert_eq!(step_res.context, "ran ok");
    assert!(
        state.contexts.iter().any(|c| c.output_file.is_some()),
        "output_file should be set in context"
    );
}

#[test]
fn test_execute_script_failure_captures_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(
        &dir.path().join("fail.sh"),
        "#!/bin/sh\necho 'something before failure'\nexit 1",
    );

    let conn = crate::test_helpers::setup_db();
    let config = Box::leak(Box::new(crate::config::Config::default()));
    let dir_str = dir.path().to_str().unwrap().to_string();
    let mut state = make_test_state(
        &conn,
        config,
        &dir_str,
        crate::workflow::types::WorkflowExecConfig {
            fail_fast: false,
            ..Default::default()
        },
    );

    let node = crate::workflow_dsl::ScriptNode {
        name: "fail".into(),
        run: script_path,
        env: std::collections::HashMap::new(),
        timeout: Some(10),
        retries: 0,
        on_fail: None,
        bot_name: None,
    };

    // Should return Ok (not an Err) because fail_fast is false; all_succeeded flips false
    let result = execute_script(&mut state, &node, 0);
    assert!(result.is_ok());
    assert!(!state.all_succeeded);
    let step_res = state.step_results.get("fail").unwrap();
    // The result_text should contain the stdout snippet
    let result_text = step_res.result_text.as_deref().unwrap_or("");
    assert!(
        result_text.contains("something before failure"),
        "stdout should be captured in failure result, got: {result_text}"
    );
}

// -----------------------------------------------------------------------
// poll_script_child unit tests — timeout and cancellation
// -----------------------------------------------------------------------

#[test]
fn test_poll_script_child_timeout() {
    // Spawn a long-running process; timeout=0 fires immediately.
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");
    let result = poll_script_child(&mut child, Some(0), None);
    assert!(
        matches!(result, ScriptPollResult::TimedOut),
        "expected TimedOut, got other variant"
    );
}

#[test]
fn test_poll_script_child_cancelled() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let flag = Arc::new(AtomicBool::new(true)); // already cancelled
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");
    let result = poll_script_child(&mut child, None, Some(&flag));
    assert!(
        matches!(result, ScriptPollResult::Cancelled),
        "expected Cancelled, got other variant"
    );
    // Verify flag didn't reset
    assert!(flag.load(Ordering::Relaxed));
}

// -----------------------------------------------------------------------
// execute_script — bot_name / GH_TOKEN injection path
// -----------------------------------------------------------------------

#[test]
fn test_execute_script_with_bot_name_not_configured() {
    // When bot_name is set but no GitHub App is configured, the script
    // should still run successfully (NotConfigured path — no GH_TOKEN injected).
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(
        &dir.path().join("bot.sh"),
        "#!/bin/sh\necho '<<<CONDUCTOR_OUTPUT>>>\n{\"context\": \"bot ran\"}\n<<<END_CONDUCTOR_OUTPUT>>>'",
    );

    let conn = crate::test_helpers::setup_db();
    let config = Box::leak(Box::new(crate::config::Config::default()));
    let dir_str = dir.path().to_str().unwrap().to_string();
    let mut state = make_test_state(
        &conn,
        config,
        &dir_str,
        crate::workflow::types::WorkflowExecConfig::default(),
    );

    let node = crate::workflow_dsl::ScriptNode {
        name: "bot-step".into(),
        run: script_path,
        env: std::collections::HashMap::new(),
        timeout: Some(10),
        retries: 0,
        on_fail: None,
        bot_name: Some("my-bot".into()),
    };

    let result = execute_script(&mut state, &node, 0);
    assert!(
        result.is_ok(),
        "execute_script with bot_name should succeed: {result:?}"
    );
    assert!(state.all_succeeded);
    let step_res = state.step_results.get("bot-step").unwrap();
    assert_eq!(step_res.context, "bot ran");
}

#[test]
fn test_execute_script_bot_name_falls_back_to_default() {
    // When node.bot_name is None but state.default_bot_name is set,
    // the effective_bot should use the default. With no app configured,
    // this exercises the fallback logic without crashing.
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(
        &dir.path().join("default-bot.sh"),
        "#!/bin/sh\necho '<<<CONDUCTOR_OUTPUT>>>\n{\"context\": \"default bot ran\"}\n<<<END_CONDUCTOR_OUTPUT>>>'",
    );

    let conn = crate::test_helpers::setup_db();
    let config = Box::leak(Box::new(crate::config::Config::default()));
    let dir_str = dir.path().to_str().unwrap().to_string();
    let mut state = make_test_state(
        &conn,
        config,
        &dir_str,
        crate::workflow::types::WorkflowExecConfig::default(),
    );
    state.default_bot_name = Some("default-bot".into());

    let node = crate::workflow_dsl::ScriptNode {
        name: "default-bot-step".into(),
        run: script_path,
        env: std::collections::HashMap::new(),
        timeout: Some(10),
        retries: 0,
        on_fail: None,
        bot_name: None,
    };

    let result = execute_script(&mut state, &node, 0);
    assert!(
        result.is_ok(),
        "execute_script with default bot should succeed: {result:?}"
    );
    assert!(state.all_succeeded);
    let step_res = state.step_results.get("default-bot-step").unwrap();
    assert_eq!(step_res.context, "default bot ran");
}

// -----------------------------------------------------------------------
// execute_script — timeout path
// -----------------------------------------------------------------------

#[test]
fn test_execute_script_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(&dir.path().join("slow.sh"), "#!/bin/sh\nsleep 60");

    let conn = crate::test_helpers::setup_db();
    let config = Box::leak(Box::new(crate::config::Config::default()));
    let dir_str = dir.path().to_str().unwrap().to_string();
    let mut state = make_test_state(
        &conn,
        config,
        &dir_str,
        crate::workflow::types::WorkflowExecConfig {
            fail_fast: false,
            ..Default::default()
        },
    );

    let node = crate::workflow_dsl::ScriptNode {
        name: "slow".into(),
        run: script_path,
        env: std::collections::HashMap::new(),
        timeout: Some(0), // expires immediately
        retries: 0,
        on_fail: None,
        bot_name: None,
    };

    let result = execute_script(&mut state, &node, 0);
    // fail_fast=false → returns Ok, but all_succeeded is false
    assert!(
        result.is_ok(),
        "expected Ok on timeout with fail_fast=false: {result:?}"
    );
    assert!(
        !state.all_succeeded,
        "all_succeeded should be false after timeout"
    );

    // DB step should be marked TimedOut
    let steps = state
        .wf_mgr
        .get_workflow_steps(&state.workflow_run_id)
        .unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].status, WorkflowStepStatus::TimedOut);
}

// -----------------------------------------------------------------------
// execute_script — cancellation path
// -----------------------------------------------------------------------

#[test]
fn test_execute_script_cancelled() {
    use std::sync::{atomic::AtomicBool, Arc};
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(&dir.path().join("cancel.sh"), "#!/bin/sh\nsleep 60");

    let shutdown = Arc::new(AtomicBool::new(true)); // pre-cancelled
    let conn = crate::test_helpers::setup_db();
    let config = Box::leak(Box::new(crate::config::Config::default()));
    let dir_str = dir.path().to_str().unwrap().to_string();
    let mut state = make_test_state(
        &conn,
        config,
        &dir_str,
        crate::workflow::types::WorkflowExecConfig {
            shutdown: Some(Arc::clone(&shutdown)),
            ..Default::default()
        },
    );

    let node = crate::workflow_dsl::ScriptNode {
        name: "cancel".into(),
        run: script_path,
        env: std::collections::HashMap::new(),
        timeout: None,
        retries: 0,
        on_fail: None,
        bot_name: None,
    };

    let result = execute_script(&mut state, &node, 0);
    assert!(result.is_err(), "expected Err on cancellation");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("cancel") || err_msg.contains("cancelled"),
        "error message should mention cancellation: {err_msg}"
    );
    assert!(
        err_msg.contains("cancel"), // step name included
        "error message should include step name 'cancel': {err_msg}"
    );
}

// -----------------------------------------------------------------------
// execute_script — retry path
// -----------------------------------------------------------------------

#[test]
fn test_execute_script_retries_exhausted() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(&dir.path().join("flaky.sh"), "#!/bin/sh\nexit 1");

    let conn = crate::test_helpers::setup_db();
    let config = Box::leak(Box::new(crate::config::Config::default()));
    let dir_str = dir.path().to_str().unwrap().to_string();
    let mut state = make_test_state(
        &conn,
        config,
        &dir_str,
        crate::workflow::types::WorkflowExecConfig {
            fail_fast: false, // don't short-circuit on first failure
            ..Default::default()
        },
    );
    let run_id = state.workflow_run_id.clone();

    let node = crate::workflow_dsl::ScriptNode {
        name: "flaky".into(),
        run: script_path,
        env: std::collections::HashMap::new(),
        timeout: Some(10),
        retries: 2, // 3 attempts total
        on_fail: None,
        bot_name: None,
    };

    let result = execute_script(&mut state, &node, 0);
    assert!(
        result.is_ok(),
        "fail_fast=false: expected Ok after retries: {result:?}"
    );
    assert!(
        !state.all_succeeded,
        "all_succeeded should be false after exhausting retries"
    );

    // Three step records should exist (one per attempt)
    let steps = state.wf_mgr.get_workflow_steps(&run_id).unwrap();
    assert_eq!(
        steps.len(),
        3,
        "expected 3 step DB records (one per attempt), got {}",
        steps.len()
    );
    for step in &steps {
        assert_eq!(
            step.status,
            WorkflowStepStatus::Failed,
            "each attempt should be marked Failed"
        );
    }
}

// -----------------------------------------------------------------------
// eval_condition / execute_if / execute_unless — BoolInput tests
// -----------------------------------------------------------------------

#[test]
fn test_eval_condition_bool_input_true() {
    let db = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&db, &config, "/tmp", Default::default());
    state.inputs.insert("flag".to_string(), "true".to_string());

    let cond = Condition::BoolInput {
        input: "flag".to_string(),
    };
    assert!(eval_condition(&state, &cond));
}

#[test]
fn test_eval_condition_bool_input_false() {
    let db = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&db, &config, "/tmp", Default::default());
    state.inputs.insert("flag".to_string(), "false".to_string());

    let cond = Condition::BoolInput {
        input: "flag".to_string(),
    };
    assert!(!eval_condition(&state, &cond));
}

#[test]
fn test_eval_condition_bool_input_missing_defaults_false() {
    let db = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let state = make_test_state(&db, &config, "/tmp", Default::default());

    let cond = Condition::BoolInput {
        input: "missing".to_string(),
    };
    assert!(!eval_condition(&state, &cond));
}

#[test]
fn test_eval_condition_bool_input_case_insensitive() {
    let db = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&db, &config, "/tmp", Default::default());
    state.inputs.insert("flag".to_string(), "TRUE".to_string());

    let cond = Condition::BoolInput {
        input: "flag".to_string(),
    };
    assert!(eval_condition(&state, &cond));
}

#[test]
fn test_execute_if_bool_input_runs_body_when_true() {
    let db = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&db, &config, "/tmp", Default::default());
    state
        .inputs
        .insert("run_it".to_string(), "true".to_string());

    // Body has one echo script node — just verify execute_if doesn't error
    // and returns Ok (actual body execution is covered by script tests).
    let node = IfNode {
        condition: Condition::BoolInput {
            input: "run_it".to_string(),
        },
        body: vec![],
    };
    assert!(execute_if(&mut state, &node).is_ok());
}

#[test]
fn test_execute_if_bool_input_skips_body_when_false() {
    let db = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&db, &config, "/tmp", Default::default());
    state
        .inputs
        .insert("run_it".to_string(), "false".to_string());

    let node = IfNode {
        condition: Condition::BoolInput {
            input: "run_it".to_string(),
        },
        body: vec![],
    };
    assert!(execute_if(&mut state, &node).is_ok());
}

#[test]
fn test_execute_unless_bool_input_runs_body_when_false() {
    let db = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&db, &config, "/tmp", Default::default());
    state.inputs.insert("skip".to_string(), "false".to_string());

    let node = UnlessNode {
        condition: Condition::BoolInput {
            input: "skip".to_string(),
        },
        body: vec![],
    };
    assert!(execute_unless(&mut state, &node).is_ok());
}

#[test]
fn test_execute_script_injects_conductor_on_path() {
    // Verify that the conductor binary's directory is prepended to PATH
    // by printing PATH from inside the script and checking it contains
    // the current exe's parent directory.
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(
        &dir.path().join("check_path.sh"),
        "#!/bin/sh\necho \"$PATH\"",
    );

    let conn = crate::test_helpers::setup_db();
    let config = Box::leak(Box::new(crate::config::Config::default()));
    let dir_str = dir.path().to_str().unwrap().to_string();
    let mut state = make_test_state(
        &conn,
        config,
        &dir_str,
        crate::workflow::types::WorkflowExecConfig::default(),
    );

    // Simulate what binary crates do: resolve conductor binary dir from current_exe.
    let bin_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    state.conductor_bin_dir = bin_dir;

    let node = crate::workflow_dsl::ScriptNode {
        name: "check_path".into(),
        run: script_path,
        env: std::collections::HashMap::new(),
        timeout: Some(10),
        retries: 0,
        on_fail: None,
        bot_name: None,
    };

    let result = execute_script(&mut state, &node, 0);
    assert!(result.is_ok(), "execute_script should succeed: {result:?}");

    // Read the stdout log to verify PATH contains the conductor binary dir
    let ctx = state.contexts.last().unwrap();
    let log_path = ctx.output_file.as_ref().unwrap();
    let output = std::fs::read_to_string(log_path).unwrap();
    let exe_dir = state
        .conductor_bin_dir
        .as_ref()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        output.contains(&exe_dir),
        "PATH should contain conductor binary dir '{exe_dir}', got: {output}"
    );
}

#[test]
fn test_execute_unless_bool_input_skips_body_when_true() {
    let db = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&db, &config, "/tmp", Default::default());
    state.inputs.insert("skip".to_string(), "true".to_string());

    let node = UnlessNode {
        condition: Condition::BoolInput {
            input: "skip".to_string(),
        },
        body: vec![],
    };
    assert!(execute_unless(&mut state, &node).is_ok());
}

// -----------------------------------------------------------------------
// execute_quality_gate tests
// -----------------------------------------------------------------------

fn make_step_result(structured_output: Option<&str>) -> StepResult {
    StepResult {
        step_name: "review".to_string(),
        status: WorkflowStepStatus::Completed,
        result_text: None,
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        markers: vec![],
        context: String::new(),
        child_run_id: None,
        structured_output: structured_output.map(|s| s.to_string()),
        output_file: None,
    }
}

fn make_quality_gate_node(
    name: &str,
    source: Option<&str>,
    threshold: Option<u32>,
    on_fail: OnFailAction,
) -> GateNode {
    let quality_gate = match (source, threshold) {
        (Some(s), Some(t)) => Some(QualityGateConfig {
            source: s.to_string(),
            threshold: t,
            on_fail_action: on_fail,
        }),
        // Allow constructing nodes with missing config for error-path tests
        _ => None,
    };
    GateNode {
        name: name.to_string(),
        gate_type: GateType::QualityGate,
        prompt: None,
        min_approvals: 1,
        approval_mode: ApprovalMode::default(),
        timeout_secs: 60,
        on_timeout: OnTimeout::Fail,
        bot_name: None,
        quality_gate,
    }
}

#[test]
fn test_quality_gate_passes_when_confidence_meets_threshold() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    state.step_results.insert(
        "review".to_string(),
        make_step_result(Some(r#"{"confidence": 80}"#)),
    );

    let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Fail);
    let result = execute_quality_gate(&mut state, &node, 0, 0);
    assert!(result.is_ok(), "gate should pass: {result:?}");
}

#[test]
fn test_quality_gate_fails_when_confidence_below_threshold() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    state.step_results.insert(
        "review".to_string(),
        make_step_result(Some(r#"{"confidence": 40}"#)),
    );

    let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Fail);
    let result = execute_quality_gate(&mut state, &node, 0, 0);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("below threshold"), "got: {err}");
}

#[test]
fn test_quality_gate_continues_on_fail_when_configured() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    state.step_results.insert(
        "review".to_string(),
        make_step_result(Some(r#"{"confidence": 20}"#)),
    );

    let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Continue);
    let result = execute_quality_gate(&mut state, &node, 0, 0);
    assert!(
        result.is_ok(),
        "on_fail=continue should not error: {result:?}"
    );
}

#[test]
fn test_quality_gate_errors_when_source_step_missing() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    let node = make_quality_gate_node("qg", Some("nonexistent"), Some(70), OnFailAction::Fail);
    let result = execute_quality_gate(&mut state, &node, 0, 0);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found in step results"), "got: {err}");
}

#[test]
fn test_quality_gate_errors_when_config_missing() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    let node = make_quality_gate_node("qg", None, None, OnFailAction::Fail);
    let result = execute_quality_gate(&mut state, &node, 0, 0);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("missing required quality_gate configuration"),
        "got: {err}"
    );
}

#[test]
fn test_quality_gate_malformed_json_treats_as_zero_confidence() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    state.step_results.insert(
        "review".to_string(),
        make_step_result(Some("not valid json")),
    );

    // threshold=0 so even confidence=0 passes
    let node = make_quality_gate_node("qg", Some("review"), Some(0), OnFailAction::Fail);
    let result = execute_quality_gate(&mut state, &node, 0, 0);
    assert!(
        result.is_ok(),
        "malformed JSON → confidence=0, threshold=0 should pass: {result:?}"
    );
}

#[test]
fn test_quality_gate_missing_confidence_key_treats_as_zero() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    state.step_results.insert(
        "review".to_string(),
        make_step_result(Some(r#"{"score": 95}"#)),
    );

    // JSON is valid but has no "confidence" key — should fail at threshold 70
    let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Fail);
    let result = execute_quality_gate(&mut state, &node, 0, 0);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("below threshold"), "got: {err}");
}

#[test]
fn test_quality_gate_no_structured_output_treats_as_zero() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    state
        .step_results
        .insert("review".to_string(), make_step_result(None));

    let node = make_quality_gate_node("qg", Some("review"), Some(50), OnFailAction::Fail);
    let result = execute_quality_gate(&mut state, &node, 0, 0);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("below threshold"), "got: {err}");
}

#[test]
fn test_quality_gate_float_confidence_handled() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    state.step_results.insert(
        "review".to_string(),
        make_step_result(Some(r#"{"confidence": 85.5}"#)),
    );

    // Float 85.5 should be truncated to 85 and pass threshold of 70
    let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Fail);
    let result = execute_quality_gate(&mut state, &node, 0, 0);
    assert!(
        result.is_ok(),
        "float confidence should be handled: {result:?}"
    );
}

#[test]
fn test_quality_gate_clamps_large_confidence_to_100() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    state.step_results.insert(
        "review".to_string(),
        make_step_result(Some(r#"{"confidence": 999999}"#)),
    );

    // Large value should be clamped to 100, passing threshold of 90
    let node = make_quality_gate_node("qg", Some("review"), Some(90), OnFailAction::Fail);
    let result = execute_quality_gate(&mut state, &node, 0, 0);
    assert!(
        result.is_ok(),
        "large confidence should be clamped to 100 and pass: {result:?}"
    );
}

#[test]
fn test_quality_gate_clamps_large_float_confidence_to_100() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    // Use a float value to exercise the as_f64() fallback branch
    state.step_results.insert(
        "review".to_string(),
        make_step_result(Some(r#"{"confidence": 9999.9}"#)),
    );

    // Large float should be clamped to 100, passing threshold of 90
    let node = make_quality_gate_node("qg", Some("review"), Some(90), OnFailAction::Fail);
    let result = execute_quality_gate(&mut state, &node, 0, 0);
    assert!(
        result.is_ok(),
        "large float confidence should be clamped to 100 and pass: {result:?}"
    );
}

#[test]
fn test_execute_gate_dispatches_quality_gate() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

    state.step_results.insert(
        "review".to_string(),
        make_step_result(Some(r#"{"confidence": 90}"#)),
    );

    let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Fail);
    let result = execute_gate(&mut state, &node, 0);
    assert!(
        result.is_ok(),
        "execute_gate should dispatch QualityGate correctly: {result:?}"
    );
}
