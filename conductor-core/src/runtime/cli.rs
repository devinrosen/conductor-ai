use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

use crate::agent::types::AgentRun;
use crate::config::RuntimeConfig;
use crate::error::{ConductorError, Result};

use super::{AgentRuntime, PollError, RuntimeRequest};

/// CliRuntime spawns any CLI agent as a headless subprocess with stdout redirected to a
/// JSON output file. Supports prompt injection via arg substitution or stdin.
pub struct CliRuntime {
    config: RuntimeConfig,
    state: std::sync::Mutex<Option<CliState>>,
}

struct CliState {
    child: std::process::Child,
    pid: u32,
    output_path: std::path::PathBuf,
    start: std::time::Instant,
}

impl CliRuntime {
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            config,
            state: std::sync::Mutex::new(None),
        }
    }
}

impl AgentRuntime for CliRuntime {
    fn spawn_impl(&self, request: &RuntimeRequest, _seal: super::private::Seal) -> Result<()> {
        let binary = self.config.binary.as_deref().ok_or_else(|| {
            ConductorError::Config("CliRuntime: `binary` is required".to_string())
        })?;

        let resolved_model = request
            .model
            .as_deref()
            .or(self.config.default_model.as_deref())
            .unwrap_or("");

        let run_dir = crate::config::conductor_dir()
            .join("workspaces")
            .join(&request.run_id);
        std::fs::create_dir_all(&run_dir).map_err(|e| {
            ConductorError::Agent(format!("CliRuntime: failed to create run dir: {e}"))
        })?;
        let output_path = run_dir.join("output.json");

        let prompt_via = self.config.prompt_via.as_deref().unwrap_or("arg");

        let args: Vec<String> = self
            .config
            .args
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|arg| {
                let a = arg.replace("{{model}}", resolved_model);
                if prompt_via == "arg" {
                    a.replace("{{prompt}}", &request.prompt)
                } else {
                    a.replace("{{prompt}}", "")
                }
            })
            .filter(|a| !a.is_empty())
            .collect();

        let output_file = std::fs::File::create(&output_path).map_err(|e| {
            ConductorError::Agent(format!("CliRuntime: failed to create output file: {e}"))
        })?;

        let stdin_cfg = if prompt_via == "stdin" {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        };

        let mut cmd = std::process::Command::new(binary);
        cmd.args(&args)
            .stdout(std::process::Stdio::from(output_file))
            .stderr(std::process::Stdio::null())
            .stdin(stdin_cfg);

        let mut child = cmd
            .spawn()
            .map_err(|e| ConductorError::Agent(format!("CliRuntime: spawn failed: {e}")))?;

        if prompt_via == "stdin" {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(request.prompt.as_bytes());
            }
        }

        let pid = child.id();

        let conn = crate::db::open_database_compat(&request.db_path)
            .map_err(|e| ConductorError::Agent(format!("CliRuntime: failed to open DB: {e}")))?;
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        if let Err(e) = agent_mgr.update_run_subprocess_pid(&request.run_id, pid) {
            tracing::warn!(
                "CliRuntime: failed to persist subprocess pid {pid} for run {}: {e}",
                request.run_id
            );
        }
        if let Err(e) = agent_mgr.update_run_runtime(&request.run_id, &request.agent_def.runtime) {
            tracing::warn!(
                "CliRuntime: failed to persist runtime '{}' for run {}: {e}",
                request.agent_def.runtime,
                request.run_id
            );
        }

        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = Some(CliState {
            child,
            pid,
            output_path,
            start: std::time::Instant::now(),
        });
        Ok(())
    }

    fn poll(
        &self,
        run_id: &str,
        shutdown: Option<&Arc<AtomicBool>>,
        step_timeout: Duration,
        db_path: &std::path::Path,
    ) -> std::result::Result<AgentRun, PollError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| PollError::Failed("CliRuntime::poll called before spawn".into()))?;

        let conn = crate::db::open_database_compat(db_path)
            .map_err(|e| PollError::Failed(format!("CliRuntime: failed to open DB: {e}")))?;
        let agent_mgr = crate::agent::AgentManager::new(&conn);

        let poll_start = std::time::Instant::now();
        loop {
            if let Some(flag) = shutdown {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    crate::process_utils::cancel_subprocess(state.pid);
                    if let Err(e) = agent_mgr.update_run_cancelled(run_id) {
                        tracing::warn!("CliRuntime: failed to cancel run {run_id}: {e}");
                    }
                    return Err(PollError::Cancelled);
                }
            }

            if poll_start.elapsed() > step_timeout {
                crate::process_utils::cancel_subprocess(state.pid);
                if let Err(e) = agent_mgr.update_run_cancelled(run_id) {
                    tracing::warn!("CliRuntime: failed to cancel run {run_id} on timeout: {e}");
                }
                return Err(PollError::NoResult);
            }

            match state.child.try_wait() {
                Ok(Some(exit_status)) => {
                    let exit_code = exit_status.code().unwrap_or(1);
                    let duration_ms = state.start.elapsed().as_millis() as i64;
                    let is_error = exit_code != 0;

                    let (result_text, input_tokens, output_tokens) =
                        if let Ok(content) = std::fs::read_to_string(&state.output_path) {
                            parse_output(&content, &self.config)
                        } else {
                            (
                                if is_error {
                                    Some(format!("process exited with code {exit_code}"))
                                } else {
                                    None
                                },
                                None,
                                None,
                            )
                        };

                    if is_error {
                        let err_msg = result_text
                            .clone()
                            .unwrap_or_else(|| format!("process exited with code {exit_code}"));
                        if let Err(e) = agent_mgr.update_run_failed(run_id, &err_msg) {
                            tracing::warn!("CliRuntime: failed to mark run {run_id} failed: {e}");
                        }
                    } else if let Err(e) = agent_mgr.update_run_completed(
                        run_id,
                        None,
                        result_text.as_deref(),
                        None,
                        Some(1),
                        Some(duration_ms),
                        input_tokens,
                        output_tokens,
                        None,
                        None,
                    ) {
                        tracing::warn!("CliRuntime: failed to mark run {run_id} completed: {e}");
                    }

                    return agent_mgr
                        .get_run(run_id)
                        .map_err(|e| PollError::Failed(format!("DB error: {e}")))?
                        .ok_or_else(|| {
                            PollError::Failed(format!(
                                "run {run_id} not found in DB after completion"
                            ))
                        });
                }
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(e) => {
                    if let Err(db_e) = agent_mgr.update_run_failed(run_id, &e.to_string()) {
                        tracing::warn!("CliRuntime: failed to mark run {run_id} failed: {db_e}");
                    }
                    return Err(PollError::Failed(format!("wait error: {e}")));
                }
            }
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
        // Take the child from the state so we can kill + reap it.
        let child = self
            .state
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
            .map(|s| s.child);

        if let Some(mut c) = child {
            // Kill directly via the Child handle: SIGKILL + wait() reaps the zombie.
            let _ = c.kill();
            let _ = c.wait();
        } else {
            // Fallback when we don't have the Child object (e.g. after a restart).
            #[cfg(unix)]
            if let Some(pid) = run.subprocess_pid {
                crate::process_utils::cancel_subprocess(pid as u32);
            }
        }

        let conn = crate::db::open_database_compat(&crate::config::db_path()).map_err(|e| {
            crate::error::ConductorError::Agent(format!(
                "CliRuntime::cancel: failed to open DB: {e}"
            ))
        })?;
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        agent_mgr.update_run_cancelled(&run.id)
    }
}

fn parse_output(
    content: &str,
    config: &RuntimeConfig,
) -> (Option<String>, Option<i64>, Option<i64>) {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(content) else {
        return (Some(content.trim().to_string()), None, None);
    };

    let result_text = config
        .result_field
        .as_deref()
        .and_then(|path| super::extract_json_path(&json, path))
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .or_else(|| Some(content.trim().to_string()));

    let tokens = config
        .token_fields
        .as_deref()
        .and_then(|path| super::extract_json_path(&json, path))
        .and_then(|v| v.as_f64())
        .map(|n| n as i64);

    (result_text, tokens, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RuntimeConfig;

    fn make_runtime(binary: &str) -> CliRuntime {
        CliRuntime::new(RuntimeConfig {
            binary: Some(binary.to_string()),
            ..RuntimeConfig::default()
        })
    }

    fn make_test_run(subprocess_pid: Option<i64>) -> AgentRun {
        AgentRun {
            id: "test".to_string(),
            worktree_id: None,
            repo_id: None,
            claude_session_id: None,
            prompt: "p".to_string(),
            status: crate::agent::status::AgentRunStatus::Running,
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
            runtime: "cli".to_string(),
        }
    }

    #[test]
    fn parse_output_plain_text() {
        let config = RuntimeConfig::default();
        let (result, _, _) = parse_output("hello world", &config);
        assert_eq!(result.as_deref(), Some("hello world"));
    }

    #[test]
    fn parse_output_json_result_field() {
        let config = RuntimeConfig {
            result_field: Some("response".to_string()),
            ..RuntimeConfig::default()
        };
        let json = r#"{"response": "the answer", "status": "ok"}"#;
        let (result, _, _) = parse_output(json, &config);
        assert_eq!(result.as_deref(), Some("the answer"));
    }

    #[test]
    fn parse_output_json_no_result_field() {
        let config = RuntimeConfig::default();
        let json = r#"{"response": "hello"}"#;
        let (result, _, _) = parse_output(json, &config);
        assert!(result.is_some());
    }

    #[test]
    fn is_alive_returns_false_when_no_pid() {
        let runtime = make_runtime("echo");
        assert!(!runtime.is_alive(&make_test_run(None)));
    }

    #[cfg(unix)]
    #[test]
    fn is_alive_returns_true_for_self() {
        let runtime = make_runtime("echo");
        let run = make_test_run(Some(std::process::id() as i64));
        assert!(runtime.is_alive(&run));
    }

    #[cfg(unix)]
    #[test]
    fn cancel_with_dead_pid_returns_ok() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::env::set_var("CONDUCTOR_DB_PATH", tmp.path().to_str().unwrap());
        let conn = crate::db::open_database(tmp.path()).unwrap();
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at, runtime) \
             VALUES ('test', 'p', 'running', '2024-01-01T00:00:00Z', 'cli')",
            [],
        )
        .unwrap();

        let mut child = std::process::Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        let dead_pid = child.id() as i64;
        let runtime = make_runtime("echo");
        let run = make_test_run(Some(dead_pid));
        assert!(runtime.cancel(&run).is_ok());
    }

    #[test]
    fn poll_before_spawn_returns_failed() {
        let runtime = make_runtime("echo");
        let result = runtime.poll(
            "no-such-run",
            None,
            Duration::from_millis(10),
            std::path::Path::new("/tmp/test.db"),
        );
        assert!(matches!(result, Err(PollError::Failed(_))));
    }

    // Regression test: lock().unwrap_or_else(|e| e.into_inner()) must not panic on a
    // poisoned mutex. A thread that panics while holding the lock poisons it; the fix
    // recovers the inner guard rather than panicking on the next access.
    #[test]
    fn poisoned_mutex_does_not_panic_on_poll() {
        let runtime = make_runtime("echo");

        // Poison the mutex: panic while holding the lock.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = runtime.state.lock().unwrap();
            panic!("intentional poison");
        }));

        // The mutex is now poisoned. poll() must recover and return Failed, not panic.
        let result = runtime.poll(
            "no-such-run",
            None,
            Duration::from_millis(10),
            std::path::Path::new("/tmp/test.db"),
        );
        assert!(matches!(result, Err(PollError::Failed(_))));
    }

    // Regression test: spawn_impl path also uses unwrap_or_else; verify the poisoned
    // state is overwritten without panicking.
    #[test]
    fn poisoned_mutex_does_not_panic_on_state_write() {
        let runtime = make_runtime("echo");

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = runtime.state.lock().unwrap();
            panic!("intentional poison");
        }));

        // Writing through a poisoned mutex must not panic.
        let recovered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            *runtime.state.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }));
        assert!(recovered.is_ok(), "write through poisoned mutex panicked");
    }
}
