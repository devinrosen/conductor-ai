use std::sync::{atomic::AtomicBool, Arc, Mutex};
use std::time::Duration;

use crate::config::RuntimeConfig;
use crate::error::{RuntimeError, Result};
use crate::run::AgentRun;
use crate::run::AgentRunStatus;
use crate::tracker::{RunEventSink, RunTracker, RuntimeEvent};

use super::{AgentRuntime, PollError, RuntimeRequest};

/// ScriptRuntime runs any shell command synchronously via `sh -c <command>`,
/// passing the prompt through the `CONDUCTOR_PROMPT` environment variable and
/// capturing stdout as `result_text`. No tmux dependency.
pub struct ScriptRuntime {
    config: RuntimeConfig,
    tracker: Mutex<Option<Arc<dyn RunTracker>>>,
    event_sink: Mutex<Option<Arc<dyn RunEventSink>>>,
}

impl ScriptRuntime {
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            config,
            tracker: Mutex::new(None),
            event_sink: Mutex::new(None),
        }
    }
}

impl AgentRuntime for ScriptRuntime {
    fn spawn_impl(&self, request: &RuntimeRequest, _seal: super::private::Seal) -> Result<()> {
        let command = self.config.command.as_deref().ok_or_else(|| {
            RuntimeError::Config(
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

        let output = std::process::Command::new("sh")
            .args(["-c", command])
            .env("CONDUCTOR_PROMPT", &request.prompt)
            .current_dir(&request.working_dir)
            .output()
            .map_err(|e| {
                RuntimeError::Agent(format!("ScriptRuntime: failed to spawn command: {e}"))
            })?;

        if output.status.success() {
            let result_text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            request.event_sink.on_event(
                &request.run_id,
                RuntimeEvent::Completed {
                    result_text: Some(result_text),
                    session_id: None,
                    cost_usd: None,
                    num_turns: None,
                    duration_ms: None,
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                },
            );
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let exit_code = output.status.code().unwrap_or(1);
            let err_msg = if stderr.is_empty() {
                format!("process exited with code {exit_code}")
            } else {
                format!("process exited with code {exit_code}: {stderr}")
            };
            request.event_sink.on_event(
                &request.run_id,
                RuntimeEvent::Failed {
                    error: err_msg,
                    session_id: None,
                },
            );
        }

        *self.tracker.lock().unwrap_or_else(|e| e.into_inner()) = Some(request.tracker.clone());
        *self.event_sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(request.event_sink.clone());
        Ok(())
    }

    fn poll(
        &self,
        run_id: &str,
        _shutdown: Option<&Arc<AtomicBool>>,
        _step_timeout: Duration,
    ) -> std::result::Result<AgentRun, PollError> {
        let tracker = self
            .tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| PollError::Failed("ScriptRuntime::poll called before spawn".into()))?;

        let run = tracker
            .get_run(run_id)
            .map_err(|e| {
                PollError::Failed(format!(
                    "ScriptRuntime: failed to fetch run {run_id} from DB: {e}"
                ))
            })?
            .ok_or_else(|| PollError::Failed(format!("run {run_id} not found in DB")))?;

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

    fn cancel(&self, run: &AgentRun) -> Result<()> {
        if let Ok(mut guard) = self.tracker.lock() {
            if let Some(ref tracker) = guard.take() {
                if let Err(e) = tracker.mark_cancelled(&run.id) {
                    tracing::warn!("ScriptRuntime: failed to mark run {} cancelled: {e}", run.id);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RuntimeConfig;
    use crate::tracker::NoopEventSink;

    fn make_runtime(command: Option<&str>) -> ScriptRuntime {
        ScriptRuntime::new(RuntimeConfig {
            command: command.map(|s| s.to_string()),
            ..RuntimeConfig::default()
        })
    }

    fn make_test_run() -> crate::run::AgentRun {
        crate::run::AgentRun {
            id: "test".to_string(),
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
