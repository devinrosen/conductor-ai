use std::path::PathBuf;

use runkon_runtimes::tracker::{RunEventSink, RunTracker, RuntimeEvent};
use runkon_runtimes::{AgentRun, RuntimeError};

use crate::agent::types::LogResult;
use crate::agent::AgentManager;
use crate::db::open_database_compat;

/// Host adapter that implements both `RunTracker` and `RunEventSink` over SQLite.
///
/// Opens a fresh `Connection` inside each trait call via `open_database_compat`.
pub struct SqliteHostAdapter {
    db_path: PathBuf,
}

impl SqliteHostAdapter {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }
}

impl RunTracker for SqliteHostAdapter {
    fn record_pid(&self, run_id: &str, pid: u32) -> Result<(), RuntimeError> {
        let conn = open_database_compat(&self.db_path)
            .map_err(|e| RuntimeError::Agent(format!("open db: {e}")))?;
        let mgr = AgentManager::new(&conn);
        mgr.update_run_subprocess_pid(run_id, pid)
            .map_err(|e| RuntimeError::Agent(e.to_string()))
    }

    fn record_runtime(&self, run_id: &str, runtime_name: &str) -> Result<(), RuntimeError> {
        let conn = open_database_compat(&self.db_path)
            .map_err(|e| RuntimeError::Agent(format!("open db: {e}")))?;
        let mgr = AgentManager::new(&conn);
        mgr.update_run_runtime(run_id, runtime_name)
            .map_err(|e| RuntimeError::Agent(e.to_string()))
    }

    fn mark_cancelled(&self, run_id: &str) -> Result<(), RuntimeError> {
        let conn = open_database_compat(&self.db_path)
            .map_err(|e| RuntimeError::Agent(format!("open db: {e}")))?;
        let mgr = AgentManager::new(&conn);
        mgr.update_run_cancelled(run_id)
            .map_err(|e| RuntimeError::Agent(e.to_string()))
    }

    fn mark_failed_if_running(&self, run_id: &str, reason: &str) -> Result<(), RuntimeError> {
        let conn = open_database_compat(&self.db_path)
            .map_err(|e| RuntimeError::Agent(format!("open db: {e}")))?;
        let mgr = AgentManager::new(&conn);
        mgr.update_run_failed_if_running(run_id, reason)
            .map_err(|e| RuntimeError::Agent(e.to_string()))
    }

    fn get_run(&self, run_id: &str) -> Result<Option<AgentRun>, RuntimeError> {
        let conn = open_database_compat(&self.db_path)
            .map_err(|e| RuntimeError::Agent(format!("open db: {e}")))?;
        let mgr = AgentManager::new(&conn);
        mgr.get_run(run_id)
            .map_err(|e| RuntimeError::Agent(e.to_string()))
    }
}

impl RunEventSink for SqliteHostAdapter {
    fn on_event(&self, run_id: &str, event: RuntimeEvent) {
        let Ok(conn) = open_database_compat(&self.db_path) else { return };
        let mgr = AgentManager::new(&conn);
        match event {
            RuntimeEvent::Init { model, session_id } => {
                let _ = mgr.update_run_model_and_session(
                    run_id,
                    model.as_deref(),
                    session_id.as_deref(),
                );
            }
            RuntimeEvent::Tokens {
                input,
                output,
                cache_read,
                cache_create,
            } => {
                let _ = mgr.update_run_tokens_partial(
                    run_id, input, output, cache_read, cache_create,
                );
            }
            RuntimeEvent::Completed {
                result_text,
                session_id,
                cost_usd,
                num_turns,
                duration_ms,
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            } => {
                let log_result = LogResult {
                    result_text,
                    session_id,
                    cost_usd,
                    num_turns,
                    duration_ms,
                    is_error: false,
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                };
                let _ = mgr.update_run_completed_if_running_full(run_id, &log_result);
            }
            RuntimeEvent::Failed { error, session_id } => {
                let _ = mgr.update_run_failed_with_session(
                    run_id,
                    &error,
                    session_id.as_deref(),
                );
            }
        }
    }
}
