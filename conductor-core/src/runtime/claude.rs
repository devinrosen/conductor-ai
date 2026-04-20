//! ClaudeRuntime — wraps the existing headless subprocess spawn/poll logic.

use std::sync::{atomic::AtomicBool, Arc, Mutex};

use crate::agent::types::AgentRun;
use crate::error::Result;

use super::{AgentRuntime, PollError, RuntimeRequest};

/// Runtime that spawns a `conductor agent run` subprocess (headless mode).
pub struct ClaudeRuntime {
    #[cfg(unix)]
    handle: Arc<Mutex<Option<crate::agent_runtime::HeadlessHandle>>>,
    prompt_file: Arc<Mutex<Option<std::path::PathBuf>>>,
}

impl ClaudeRuntime {
    pub fn new() -> Self {
        Self {
            #[cfg(unix)]
            handle: Arc::new(Mutex::new(None)),
            prompt_file: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for ClaudeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRuntime for ClaudeRuntime {
    fn spawn(&self, request: &RuntimeRequest) -> Result<()> {
        super::validate_run_id(&request.run_id)?;
        #[cfg(unix)]
        {
            let params = crate::agent_runtime::SpawnHeadlessParams {
                run_id: &request.run_id,
                working_dir: request.working_dir.to_str().unwrap_or("."),
                prompt: &request.prompt,
                resume_session_id: None,
                model: request.model.as_deref(),
                bot_name: request.bot_name.as_deref(),
                permission_mode: Some(&request.permission_mode),
                plugin_dirs: &request.plugin_dirs,
            };
            let (h, pf) = crate::agent_runtime::try_spawn_headless_run(&params)
                .map_err(crate::error::ConductorError::Workflow)?;
            *self.handle.lock().unwrap() = Some(h);
            *self.prompt_file.lock().unwrap() = Some(pf);
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
        .unwrap()
        .take()
        .ok_or_else(|| PollError::Failed("ClaudeRuntime::poll called before spawn".into()))?;

    let prompt_file = rt.prompt_file.lock().unwrap().take();
    let pid = handle.pid();

    let tracking_conn =
        crate::db::open_agent_db("ClaudeRuntime").map_err(|e| PollError::Failed(e.to_string()))?;
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
        let conn = match crate::db::open_agent_db("ClaudeRuntime") {
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
