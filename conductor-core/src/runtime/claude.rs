//! ClaudeRuntime — wraps the existing headless subprocess spawn/poll logic.

use std::sync::{atomic::AtomicBool, Arc, Mutex};

use crate::agent::types::AgentRun;
use crate::config::AgentPermissionMode;
use crate::error::Result;

use super::{AgentRuntime, PollError, RuntimeRequest};

/// Claude-specific configuration captured at construction time.
#[derive(Clone, Default)]
pub struct ClaudeRuntimeOptions {
    pub permission_mode: AgentPermissionMode,
    pub config_dir: Option<String>,
}

/// Runtime that spawns a `conductor agent run` subprocess (headless mode).
pub struct ClaudeRuntime {
    options: ClaudeRuntimeOptions,
    #[cfg(unix)]
    handle: Arc<Mutex<Option<crate::agent_runtime::HeadlessHandle>>>,
    prompt_file: Arc<Mutex<Option<std::path::PathBuf>>>,
    db_path: Arc<Mutex<Option<std::path::PathBuf>>>,
}

impl ClaudeRuntime {
    pub fn new(options: ClaudeRuntimeOptions) -> Self {
        Self {
            options,
            #[cfg(unix)]
            handle: Arc::new(Mutex::new(None)),
            prompt_file: Arc::new(Mutex::new(None)),
            db_path: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for ClaudeRuntime {
    fn default() -> Self {
        Self::new(ClaudeRuntimeOptions::default())
    }
}

impl AgentRuntime for ClaudeRuntime {
    fn spawn(&self, request: &RuntimeRequest) -> Result<()> {
        crate::text_util::validate_run_id(&request.run_id)?;
        #[cfg(unix)]
        {
            let params = crate::agent_runtime::SpawnHeadlessParams {
                run_id: &request.run_id,
                working_dir: request.working_dir.to_str().unwrap_or("."),
                prompt: &request.prompt,
                resume_session_id: None,
                model: request.model.as_deref(),
                bot_name: request.bot_name.as_deref(),
                permission_mode: Some(&self.options.permission_mode),
                plugin_dirs: &request.plugin_dirs,
            };
            let (h, pf) = crate::agent_runtime::try_spawn_headless_run(&params)
                .map_err(crate::error::ConductorError::Workflow)?;
            if let Ok(mut guard) = self.handle.lock() {
                *guard = Some(h);
            }
            if let Ok(mut guard) = self.prompt_file.lock() {
                *guard = Some(pf);
            }
            if let Ok(mut guard) = self.db_path.lock() {
                *guard = Some(request.db_path.clone());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = request;
            Err(crate::error::ConductorError::Workflow(
                "ClaudeRuntime headless spawn is not supported on non-Unix platforms".into(),
            ))
        }
    }

    fn poll(
        &self,
        run_id: &str,
        shutdown: Option<&Arc<AtomicBool>>,
        step_timeout: std::time::Duration,
    ) -> std::result::Result<AgentRun, PollError> {
        #[cfg(unix)]
        {
            poll_unix(self, run_id, shutdown, step_timeout)
        }
        #[cfg(not(unix))]
        {
            let _ = (run_id, shutdown, step_timeout);
            Err(PollError::Failed(
                "ClaudeRuntime poll is not supported on non-Unix platforms".into(),
            ))
        }
    }

    fn is_alive(&self, run: &AgentRun) -> bool {
        #[cfg(unix)]
        if let Some(pid) = run.subprocess_pid {
            return crate::process_utils::pid_is_alive(pid as u32);
        }
        let _ = run;
        false
    }

    fn cancel(&self, run: &AgentRun) -> Result<()> {
        #[cfg(unix)]
        {
            if let Ok(mut guard) = self.handle.lock() {
                if let Some(h) = guard.take() {
                    h.abort();
                    return Ok(());
                }
            }
            if let Some(pid) = run.subprocess_pid {
                crate::process_utils::cancel_subprocess(pid as u32);
            }
        }
        let _ = run;
        Ok(())
    }
}

#[cfg(unix)]
fn poll_unix(
    rt: &ClaudeRuntime,
    run_id: &str,
    shutdown: Option<&Arc<AtomicBool>>,
    step_timeout: std::time::Duration,
) -> std::result::Result<AgentRun, PollError> {
    use crate::agent_runtime::DrainOutcome;

    let handle = rt
        .handle
        .lock()
        .map_err(|_| PollError::Failed("ClaudeRuntime handle mutex poisoned".into()))?
        .take()
        .ok_or_else(|| PollError::Failed("ClaudeRuntime::poll called before spawn".into()))?;

    let prompt_file = rt.prompt_file.lock().ok().and_then(|mut g| g.take());
    let db_path = rt
        .db_path
        .lock()
        .map_err(|_| PollError::Failed("ClaudeRuntime db_path mutex poisoned".into()))?
        .clone()
        .ok_or_else(|| PollError::Failed("ClaudeRuntime::poll called before spawn".into()))?;
    let pid = handle.pid();

    let tracking_conn = crate::db::open_database_compat(&db_path)
        .map_err(|e| PollError::Failed(format!("ClaudeRuntime: failed to open DB: {e}")))?;
    let tracking_mgr = crate::agent::AgentManager::new(&tracking_conn);

    if let Err(e) = tracking_mgr.update_run_subprocess_pid(run_id, pid) {
        tracing::warn!("ClaudeRuntime: failed to persist subprocess pid {pid}: {e}");
    }

    let (stderr_pipe, stdout_pipe, finish) = handle.into_stderr_drain_parts();

    let run_id_owned = run_id.to_string();
    let log_path = crate::config::agent_log_path(run_id);
    let (tx, rx) = std::sync::mpsc::channel::<DrainOutcome>();

    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let reader = BufReader::new(stderr_pipe);
        for line in reader.lines().map_while(|l| l.ok()) {
            tracing::trace!(target: "conductor::agent::stderr", "{line}");
        }
    });

    std::thread::spawn(move || {
        let conn = match crate::db::open_database_compat(&db_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("ClaudeRuntime drain thread: failed to open DB: {e}");
                if let Some(pf) = prompt_file {
                    let _ = std::fs::remove_file(pf);
                }
                let _ = tx.send(DrainOutcome::NoResult);
                return;
            }
        };
        let drain_mgr = crate::agent::AgentManager::new(&conn);
        let outcome = crate::agent_runtime::drain_stream_json(
            stdout_pipe,
            &run_id_owned,
            &log_path,
            &drain_mgr,
            |_| {},
        );
        if let Some(pf) = prompt_file {
            let _ = std::fs::remove_file(pf);
        }
        finish();
        let _ = tx.send(outcome);
    });

    let start = std::time::Instant::now();
    let drain_outcome = loop {
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(outcome) => break outcome,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(flag) = shutdown {
                    if flag.load(std::sync::atomic::Ordering::Relaxed) {
                        tracing::warn!(
                            "ClaudeRuntime: shutdown requested, cancelling run {run_id}"
                        );
                        let _ = tracking_mgr.update_run_cancelled(run_id);
                        crate::process_utils::cancel_subprocess(pid);
                        let _ = rx.recv_timeout(std::time::Duration::from_secs(6));
                        return Err(PollError::Cancelled);
                    }
                }
                if start.elapsed() > step_timeout {
                    tracing::warn!("ClaudeRuntime: step timeout reached for run {run_id}");
                    let _ = tracking_mgr.update_run_cancelled(run_id);
                    crate::process_utils::cancel_subprocess(pid);
                    let _ = rx.recv_timeout(std::time::Duration::from_secs(6));
                    break DrainOutcome::NoResult;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                tracing::warn!("ClaudeRuntime: drain thread disconnected for run {run_id}");
                break DrainOutcome::NoResult;
            }
        }
    };

    match drain_outcome {
        DrainOutcome::Completed => tracking_mgr
            .get_run(run_id)
            .map_err(|e| PollError::Failed(format!("DB error after drain: {e}")))?
            .ok_or_else(|| PollError::Failed(format!("run {run_id} not found in DB after drain"))),
        DrainOutcome::NoResult => {
            let _ =
                tracking_mgr.update_run_failed_if_running(run_id, "agent exited without result");
            Err(PollError::NoResult)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::status::AgentRunStatus;
    use crate::agent_config::{AgentDef, AgentRole};

    fn make_request(run_id: &str) -> RuntimeRequest {
        RuntimeRequest {
            run_id: run_id.to_string(),
            agent_def: AgentDef {
                name: "test".to_string(),
                role: AgentRole::Reviewer,
                can_commit: false,
                model: None,
                runtime: "claude".to_string(),
                prompt: String::new(),
            },
            prompt: "p".to_string(),
            working_dir: std::path::PathBuf::from("/tmp"),
            model: None,
            bot_name: None,
            plugin_dirs: vec![],
            db_path: crate::config::db_path(),
        }
    }

    fn make_test_run(subprocess_pid: Option<i64>) -> AgentRun {
        AgentRun {
            id: "test-run".to_string(),
            worktree_id: None,
            repo_id: None,
            claude_session_id: None,
            prompt: "p".to_string(),
            status: AgentRunStatus::Running,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: None,
            log_file: None,
            model: None,
            plan: None,
            parent_run_id: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            bot_name: None,
            conversation_id: None,
            subprocess_pid,
            runtime: "claude".to_string(),
        }
    }

    #[test]
    fn spawn_rejects_path_traversal_run_id() {
        let runtime = ClaudeRuntime::default();
        let request = make_request("../../etc/cron.d/payload");
        let err = runtime
            .spawn(&request)
            .expect_err("expected Err for path-traversal run_id");
        assert!(
            matches!(err, crate::error::ConductorError::InvalidInput(_)),
            "expected InvalidInput, got: {err:?}"
        );
    }

    #[test]
    fn spawn_rejects_slash_in_run_id() {
        let runtime = ClaudeRuntime::default();
        let request = make_request("run/id");
        assert!(runtime.spawn(&request).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn poll_before_spawn_returns_failed() {
        let runtime = ClaudeRuntime::default();
        let result = runtime.poll("some-run-id", None, std::time::Duration::from_millis(10));
        assert!(
            matches!(result, Err(PollError::Failed(_))),
            "expected Failed, got: {result:?}"
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn poll_fails_on_non_unix() {
        let runtime = ClaudeRuntime::default();
        let result = runtime.poll("some-run-id", None, std::time::Duration::from_millis(10));
        assert!(
            matches!(result, Err(PollError::Failed(_))),
            "expected Failed on non-Unix, got: {result:?}"
        );
    }

    #[test]
    fn is_alive_returns_false_when_no_pid() {
        let runtime = ClaudeRuntime::default();
        let run = make_test_run(None);
        assert!(!runtime.is_alive(&run));
    }

    #[cfg(unix)]
    #[test]
    fn is_alive_returns_true_for_self() {
        let runtime = ClaudeRuntime::default();
        let run = make_test_run(Some(std::process::id() as i64));
        assert!(runtime.is_alive(&run));
    }

    #[cfg(unix)]
    #[test]
    fn is_alive_returns_false_for_dead_pid() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        let dead_pid = child.id() as i64;
        let runtime = ClaudeRuntime::default();
        let run = make_test_run(Some(dead_pid));
        assert!(!runtime.is_alive(&run));
    }

    #[test]
    fn cancel_with_no_handle_and_no_pid() {
        let runtime = ClaudeRuntime::default();
        let run = make_test_run(None);
        assert!(runtime.cancel(&run).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn cancel_with_dead_pid_returns_ok() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        let dead_pid = child.id() as i64;
        let runtime = ClaudeRuntime::default();
        let run = make_test_run(Some(dead_pid));
        assert!(runtime.cancel(&run).is_ok());
    }

    // spawn() reaches the binary-exec path when run_id is valid — on unix this
    // attempts to exec the conductor binary (not present in test env), so the
    // error is Workflow (exec failure), not InvalidInput (validation failure).
    #[cfg(unix)]
    #[test]
    fn spawn_valid_run_id_reaches_exec_attempt() {
        let runtime = ClaudeRuntime::default();
        let request = make_request("valid-run-id-01");
        let result = runtime.spawn(&request);
        // The subprocess spawn will fail because the conductor binary is not
        // present in the test binary's directory, but the error must come from
        // the exec attempt (Workflow), not from run_id validation (InvalidInput).
        match result {
            Ok(()) => {
                // Binary was present — handle must be populated after a successful spawn.
                assert!(
                    runtime.handle.lock().unwrap().is_some(),
                    "handle should be populated after successful spawn"
                );
            }
            Err(crate::error::ConductorError::Workflow(_)) => {} // expected in CI
            Err(other) => panic!("expected Ok or Workflow error, got: {other:?}"),
        }
    }

    #[cfg(not(unix))]
    #[test]
    fn spawn_returns_platform_error_on_non_unix() {
        let runtime = ClaudeRuntime::default();
        let request = make_request("valid-run-id-01");
        let err = runtime
            .spawn(&request)
            .expect_err("expected Err on non-Unix platform");
        assert!(
            matches!(err, crate::error::ConductorError::Workflow(_)),
            "expected Workflow error, got: {err:?}"
        );
    }

    // cancel() takes the handle and calls abort() — the handle field is cleared.
    #[cfg(unix)]
    #[test]
    fn cancel_with_live_handle_aborts_and_clears_handle() {
        use std::process::Stdio;
        let child = std::process::Command::new("sleep")
            .arg("1000")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("sleep should be available");
        let handle = crate::agent_runtime::HeadlessHandle::from_child(child)
            .expect("HeadlessHandle::from_child failed");
        let runtime = ClaudeRuntime::default();
        *runtime.handle.lock().unwrap() = Some(handle);
        let run = make_test_run(None);
        assert!(runtime.cancel(&run).is_ok());
        // handle must have been taken (aborted)
        assert!(
            runtime.handle.lock().unwrap().is_none(),
            "handle should be None after cancel"
        );
    }

    // Spawns a `sleep 1000` child and wraps it in a ClaudeRuntime ready for
    // poll() tests.  Returns (runtime, tmp) — caller must hold `tmp` alive for
    // the duration of the test so the temp dir is not cleaned up early.
    #[cfg(unix)]
    fn make_sleeping_runtime() -> (ClaudeRuntime, tempfile::TempDir) {
        use std::process::Stdio;
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_file = tmp.path().join("test.db");
        let child = std::process::Command::new("sleep")
            .arg("1000")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("sleep should be available");
        let handle = crate::agent_runtime::HeadlessHandle::from_child(child)
            .expect("HeadlessHandle::from_child failed");
        let runtime = ClaudeRuntime::default();
        *runtime.handle.lock().unwrap() = Some(handle);
        *runtime.db_path.lock().unwrap() = Some(db_file);
        (runtime, tmp)
    }

    // poll() returns Cancelled when the shutdown flag is set.
    #[cfg(unix)]
    #[test]
    fn poll_shutdown_flag_returns_cancelled() {
        let (runtime, _tmp) = make_sleeping_runtime();

        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let result = runtime.poll(
            "test-shutdown-run",
            Some(&shutdown),
            std::time::Duration::from_secs(60),
        );

        assert!(
            matches!(result, Err(PollError::Cancelled)),
            "expected Cancelled, got: {result:?}"
        );
    }

    // poll() returns NoResult when step_timeout elapses before the agent finishes.
    #[cfg(unix)]
    #[test]
    fn poll_timeout_returns_no_result() {
        let (runtime, _tmp) = make_sleeping_runtime();

        let result = runtime.poll(
            "test-timeout-run",
            None,
            std::time::Duration::from_millis(10),
        );

        assert!(
            matches!(result, Err(PollError::NoResult)),
            "expected NoResult, got: {result:?}"
        );
    }
}
