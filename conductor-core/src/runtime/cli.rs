use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

use crate::agent::types::AgentRun;
use crate::config::RuntimeConfig;
use crate::error::{ConductorError, Result};

use super::{AgentRuntime, PollError, RuntimeRequest};

/// CliRuntime spawns any CLI agent via a tmux window with stdout redirected to a
/// JSON output file. Supports prompt injection via arg substitution or stdin.
pub struct CliRuntime {
    config: RuntimeConfig,
    state: std::sync::Mutex<Option<CliState>>,
    db_path: std::sync::Mutex<PathBuf>,
}

struct CliState {
    exit_code_path: std::path::PathBuf,
    output_path: std::path::PathBuf,
    window_name: String,
    start: std::time::Instant,
}

impl CliRuntime {
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            config,
            state: std::sync::Mutex::new(None),
            db_path: std::sync::Mutex::new(crate::config::db_path()),
        }
    }

    fn shell_single_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }

    /// Kill the tmux window (best-effort) and mark the run cancelled in the DB.
    fn teardown_window(
        agent_mgr: &crate::agent::AgentManager<'_>,
        run_id: &str,
        window_name: Option<&str>,
    ) -> Result<()> {
        if let Some(window) = window_name {
            let result = std::process::Command::new("tmux")
                .args(["kill-window", "-t", window])
                .output();
            if let Err(e) = result {
                tracing::warn!("CliRuntime: tmux kill-window failed: {e}");
            }
        }
        agent_mgr.update_run_cancelled(run_id)
    }
}

impl AgentRuntime for CliRuntime {
    fn spawn(&self, request: &RuntimeRequest) -> Result<()> {
        crate::text_util::validate_run_id(&request.run_id)?;

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
        let exit_code_path = run_dir.join("exit_code");

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

        let args_str = args
            .iter()
            .map(|a| Self::shell_single_quote(a))
            .collect::<Vec<_>>()
            .join(" ");

        // Quote all paths: binary for injection, output/exit paths for spaces in conductor_dir.
        let binary_quoted = Self::shell_single_quote(binary);
        let output_path_quoted = Self::shell_single_quote(&output_path.to_string_lossy());
        let exit_code_path_quoted = Self::shell_single_quote(&exit_code_path.to_string_lossy());

        let shell_cmd = if prompt_via == "stdin" {
            let prompt_quoted = Self::shell_single_quote(&request.prompt);
            format!(
                "echo {prompt_quoted} | {binary_quoted} {args_str} > {output_path_quoted}; echo $? > {exit_code_path_quoted}"
            )
        } else {
            format!(
                "{binary_quoted} {args_str} > {output_path_quoted}; echo $? > {exit_code_path_quoted}"
            )
        };

        let window_name = format!("cli-{}", &request.run_id[..8.min(request.run_id.len())]);
        let status = std::process::Command::new("tmux")
            .args([
                "new-window",
                "-n",
                &window_name,
                &format!("sh -c {}", Self::shell_single_quote(&shell_cmd)),
            ])
            .status()
            .map_err(|e| ConductorError::Agent(format!("CliRuntime: tmux spawn failed: {e}")))?;

        if !status.success() {
            return Err(ConductorError::Agent(
                "CliRuntime: tmux new-window failed".to_string(),
            ));
        }

        // Store the injected DB path for use in poll() and cancel().
        if let Ok(mut guard) = self.db_path.lock() {
            *guard = request.db_path.clone();
        }

        // Persist runtime name and tmux window to DB so is_alive(), cancel(), and
        // the orphan reaper can operate correctly on this run.
        let conn = crate::db::open_database_compat(&request.db_path)
            .map_err(|e| ConductorError::Agent(format!("CliRuntime: failed to open DB: {e}")))?;
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        if let Err(e) = agent_mgr.update_run_runtime(&request.run_id, &request.agent_def.runtime) {
            tracing::warn!(
                "CliRuntime: failed to persist runtime '{}' for run {}: {e}",
                request.agent_def.runtime,
                request.run_id
            );
        }
        if let Err(e) = agent_mgr.update_run_tmux_window(&request.run_id, &window_name) {
            tracing::warn!(
                "CliRuntime: failed to persist tmux window '{window_name}' for run {}: {e}",
                request.run_id
            );
        }

        *self.state.lock().unwrap() = Some(CliState {
            exit_code_path,
            output_path,
            window_name,
            start: std::time::Instant::now(),
        });
        Ok(())
    }

    fn poll(
        &self,
        run_id: &str,
        shutdown: Option<&Arc<AtomicBool>>,
        step_timeout: Duration,
    ) -> std::result::Result<AgentRun, PollError> {
        let state = self
            .state
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| PollError::Failed("CliRuntime::poll called before spawn".into()))?;

        let db_path = self.db_path.lock().unwrap().clone();
        let conn = crate::db::open_database_compat(&db_path)
            .map_err(|e| PollError::Failed(format!("CliRuntime: failed to open DB: {e}")))?;
        let agent_mgr = crate::agent::AgentManager::new(&conn);

        let poll_start = std::time::Instant::now();
        loop {
            if let Some(flag) = shutdown {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Err(e) =
                        Self::teardown_window(&agent_mgr, run_id, Some(&state.window_name))
                    {
                        tracing::warn!("CliRuntime: teardown_window failed during shutdown: {e}");
                    }
                    return Err(PollError::Cancelled);
                }
            }

            if poll_start.elapsed() > step_timeout {
                if let Err(e) = Self::teardown_window(&agent_mgr, run_id, Some(&state.window_name))
                {
                    tracing::warn!("CliRuntime: teardown_window failed on timeout: {e}");
                }
                return Err(PollError::NoResult);
            }

            if state.exit_code_path.exists() {
                let exit_code: i64 = std::fs::read_to_string(&state.exit_code_path)
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(1);

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
                        PollError::Failed(format!("run {run_id} not found in DB after completion"))
                    });
            }

            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn is_alive(&self, run: &AgentRun) -> bool {
        if let Some(ref window) = run.tmux_window {
            let output = std::process::Command::new("tmux")
                .args(["list-windows", "-a", "-F", "#{window_name}"])
                .output()
                .ok();
            if let Some(out) = output {
                let stdout = String::from_utf8_lossy(&out.stdout);
                return stdout.lines().any(|l| l.trim() == window.as_str());
            }
        }
        false
    }

    fn cancel(&self, run: &AgentRun) -> Result<()> {
        let db_path = self.db_path.lock().unwrap().clone();
        let conn = crate::db::open_database_compat(&db_path).map_err(|e| {
            ConductorError::Agent(format!("CliRuntime: failed to open DB: {e}"))
        })?;
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        Self::teardown_window(&agent_mgr, &run.id, run.tmux_window.as_deref())
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

    fn make_test_run(tmux_window: Option<String>) -> AgentRun {
        AgentRun {
            id: "test".to_string(),
            tmux_window,
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
            subprocess_pid: None,
            runtime: "cli".to_string(),
        }
    }

    #[test]
    fn shell_single_quote_basic() {
        assert_eq!(CliRuntime::shell_single_quote("hello"), "'hello'");
    }

    #[test]
    fn shell_single_quote_with_single_quote() {
        assert_eq!(CliRuntime::shell_single_quote("it's"), "'it'\\''s'");
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
        // Falls back to raw content
        assert!(result.is_some());
    }

    #[test]
    fn binary_quoted_prevents_injection() {
        // Ensure a binary path with spaces/special chars is single-quoted in the shell command.
        let binary = "my binary; rm -rf ~/";
        let quoted = CliRuntime::shell_single_quote(binary);
        assert!(quoted.starts_with('\''));
        assert!(!quoted.contains("rm -rf ~/;") || quoted.contains("\\'"));
    }

    #[test]
    fn is_alive_returns_false_for_none_window() {
        let runtime = make_runtime("echo");
        assert!(!runtime.is_alive(&make_test_run(None)));
    }

    #[test]
    fn is_alive_returns_false_for_nonexistent_window() {
        let runtime = make_runtime("echo");
        // Either tmux isn't running (returns false) or window doesn't exist (returns false).
        assert!(!runtime.is_alive(&make_test_run(Some("no-such-window-xyz-99999".to_string()))));
    }

    #[test]
    fn poll_before_spawn_returns_failed() {
        let runtime = make_runtime("echo");
        let result = runtime.poll("no-such-run", None, Duration::from_millis(10));
        assert!(matches!(result, Err(PollError::Failed(_))));
    }
}
