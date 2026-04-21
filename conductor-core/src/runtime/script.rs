use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

use crate::agent::types::AgentRun;
use crate::config::RuntimeConfig;
use crate::error::{ConductorError, Result};

use super::{AgentRuntime, PollError, RuntimeRequest};

/// ScriptRuntime runs any shell command synchronously via `sh -c <command>`,
/// passing the prompt through the `CONDUCTOR_PROMPT` environment variable and
/// capturing stdout as `result_text`. No tmux dependency.
pub struct ScriptRuntime {
    config: RuntimeConfig,
}

impl ScriptRuntime {
    pub fn new(config: RuntimeConfig) -> Self {
        Self { config }
    }
}

impl AgentRuntime for ScriptRuntime {
    fn spawn_impl(&self, request: &RuntimeRequest) -> Result<()> {
        let command = self.config.command.as_deref().ok_or_else(|| {
            ConductorError::Config(
                "ScriptRuntime: `command` is required in the runtime config".to_string(),
            )
        })?;

        if request.agent_def.can_commit {
            tracing::warn!(
                "ScriptRuntime: agent '{}' has can_commit=true but ScriptRuntime produces no \
                 output schema — commit validation is not enforced",
                request.agent_def.name
            );
        }

        // KNOWN LIMITATION: output() blocks the calling thread until the script exits.
        // step_timeout and the shutdown AtomicBool (available in poll()) are not
        // consulted here; callers that need cancellation should wrap spawn() in a
        // thread and enforce the timeout externally.
        let output = std::process::Command::new("sh")
            .args(["-c", command])
            .env("CONDUCTOR_PROMPT", &request.prompt)
            .current_dir(&request.working_dir)
            .output()
            .map_err(|e| {
                ConductorError::Agent(format!("ScriptRuntime: failed to spawn command: {e}"))
            })?;

        let conn = crate::db::open_database_compat(&request.db_path)
            .map_err(|e| ConductorError::Agent(format!("ScriptRuntime: failed to open DB: {e}")))?;
        let agent_mgr = crate::agent::AgentManager::new(&conn);

        if output.status.success() {
            let result_text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            agent_mgr
                .update_run_completed(
                    &request.run_id,
                    None,
                    Some(&result_text),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .map_err(|e| {
                    ConductorError::Agent(format!(
                        "ScriptRuntime: failed to mark run {} completed: {e}",
                        request.run_id
                    ))
                })?;
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let exit_code = output.status.code().unwrap_or(1);
            let err_msg = if stderr.is_empty() {
                format!("process exited with code {exit_code}")
            } else {
                format!("process exited with code {exit_code}: {stderr}")
            };
            agent_mgr
                .update_run_failed(&request.run_id, &err_msg)
                .map_err(|e| {
                    ConductorError::Agent(format!(
                        "ScriptRuntime: failed to mark run {} failed: {e}",
                        request.run_id
                    ))
                })?;
        }

        Ok(())
    }

    fn poll(
        &self,
        run_id: &str,
        _shutdown: Option<&Arc<AtomicBool>>,
        _step_timeout: Duration,
        db_path: &std::path::Path,
    ) -> std::result::Result<AgentRun, PollError> {
        let conn = crate::db::open_database_compat(db_path)
            .map_err(|e| PollError::Failed(format!("ScriptRuntime: failed to open DB: {e}")))?;
        let agent_mgr = crate::agent::AgentManager::new(&conn);

        let run = agent_mgr
            .get_run(run_id)
            .map_err(|e| {
                PollError::Failed(format!(
                    "ScriptRuntime: failed to fetch run {run_id} from DB: {e}"
                ))
            })?
            .ok_or_else(|| PollError::Failed(format!("run {run_id} not found in DB")))?;

        use crate::agent::status::AgentRunStatus;
        match run.status {
            AgentRunStatus::Failed => Err(PollError::Failed(
                run.result_text
                    .clone()
                    .unwrap_or_else(|| "script failed".to_string()),
            )),
            AgentRunStatus::Completed => Ok(run),
            _ => Err(PollError::NoResult),
        }
    }

    fn is_alive(&self, _run: &AgentRun) -> bool {
        false
    }

    fn cancel(&self, _run: &AgentRun) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RuntimeConfig;

    fn make_runtime(command: Option<&str>) -> ScriptRuntime {
        ScriptRuntime::new(RuntimeConfig {
            command: command.map(|s| s.to_string()),
            ..RuntimeConfig::default()
        })
    }

    fn make_test_run() -> AgentRun {
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
            subprocess_pid: None,
            runtime: "script".to_string(),
        }
    }

    #[test]
    fn is_alive_always_false() {
        let runtime = make_runtime(Some("echo hi"));
        assert!(!runtime.is_alive(&make_test_run()));
    }

    #[test]
    fn cancel_is_noop() {
        let runtime = make_runtime(Some("echo hi"));
        assert!(runtime.cancel(&make_test_run()).is_ok());
    }
}
