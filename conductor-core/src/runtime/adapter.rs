use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use runkon_runtimes::tracker::{RunEventSink, RunTracker, RuntimeEvent};
use runkon_runtimes::{RunHandle, RuntimeError};
use rusqlite::Connection;

use crate::agent::types::LogResult;
use crate::agent::AgentManager;
use crate::db::open_database_compat;

/// Host adapter that implements both `RunTracker` and `RunEventSink` over SQLite.
///
/// Holds a single `Connection` behind a `Mutex` so that every trait call re-uses
/// the same underlying database connection instead of opening a new one (N+1
/// anti-pattern).
pub struct SqliteHostAdapter {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteHostAdapter {
    pub fn new(db_path: PathBuf) -> Result<Self, RuntimeError> {
        let conn = open_database_compat(&db_path).map_err(|e| {
            RuntimeError::Agent(format!(
                "failed to open database at {}: {e}",
                db_path.display()
            ))
        })?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Helper that locks the connection, builds an `AgentManager`, and delegates
    /// to the provided closure. Centralises the mutex-lock + error-mapping
    /// boilerplate shared by every `RunTracker` method.
    fn with_mgr<F, T>(&self, op: F) -> Result<T, RuntimeError>
    where
        F: FnOnce(&AgentManager<'_>) -> crate::error::Result<T>,
    {
        let guard = self
            .conn
            .lock()
            .map_err(|e| RuntimeError::Agent(format!("database mutex poisoned: {e}")))?;
        let mgr = AgentManager::new(&guard);
        op(&mgr).map_err(|e| RuntimeError::Agent(e.to_string()))
    }
}

impl RunTracker for SqliteHostAdapter {
    fn record_pid(&self, run_id: &str, pid: u32) -> Result<(), RuntimeError> {
        self.with_mgr(|mgr| mgr.update_run_subprocess_pid(run_id, pid))
    }

    fn record_runtime(&self, run_id: &str, runtime_name: &str) -> Result<(), RuntimeError> {
        self.with_mgr(|mgr| mgr.update_run_runtime(run_id, runtime_name))
    }

    fn mark_cancelled(&self, run_id: &str) -> Result<(), RuntimeError> {
        self.with_mgr(|mgr| mgr.update_run_cancelled(run_id))
    }

    fn mark_failed_if_running(&self, run_id: &str, reason: &str) -> Result<(), RuntimeError> {
        self.with_mgr(|mgr| mgr.update_run_failed_if_running(run_id, reason))
    }

    fn get_run(&self, run_id: &str) -> Result<Option<RunHandle>, RuntimeError> {
        // The runtime layer only needs the RunHandle subset; project the full
        // conductor AgentRun down at the boundary so worktree_id / repo_id /
        // prompt / plan etc. never cross into runkon-runtimes.
        self.with_mgr(|mgr| {
            mgr.get_run(run_id)
                .map(|opt| opt.map(|r| r.to_run_handle()))
        })
    }
}

impl RunEventSink for SqliteHostAdapter {
    fn on_event(&self, run_id: &str, event: RuntimeEvent) {
        let event_label = event_label(&event);
        let result = self.with_mgr(|mgr| match event {
            RuntimeEvent::Init { model, session_id } => {
                mgr.update_run_model_and_session(run_id, model.as_deref(), session_id.as_deref())
            }
            RuntimeEvent::Tokens {
                input,
                output,
                cache_read,
                cache_create,
            } => mgr.update_run_tokens_partial(run_id, input, output, cache_read, cache_create),
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
                mgr.update_run_completed_if_running_full(run_id, &log_result)
            }
            RuntimeEvent::Failed { error, session_id } => {
                mgr.update_run_failed_with_session(run_id, &error, session_id.as_deref())
            }
        });

        if let Err(ref e) = result {
            tracing::warn!(
                "SqliteHostAdapter::on_event failed for run {run_id} ({event_label}): {e}"
            );
        }
    }
}

fn event_label(event: &RuntimeEvent) -> &'static str {
    match event {
        RuntimeEvent::Init { .. } => "Init",
        RuntimeEvent::Tokens { .. } => "Tokens",
        RuntimeEvent::Completed { .. } => "Completed",
        RuntimeEvent::Failed { .. } => "Failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runkon_runtimes::tracker::RunTracker;

    fn setup_adapter_with_run(run_id: &str) -> (SqliteHostAdapter, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Insert a baseline row so UPDATEs in AgentManager succeed.
        {
            let conn = open_database_compat(tmp.path()).unwrap();
            conn.execute(
                "INSERT INTO agent_runs (id, prompt, status, started_at, runtime) \
                 VALUES (?1, 'test', 'running', '2024-01-01T00:00:00Z', 'claude')",
                rusqlite::params![run_id],
            )
            .unwrap();
        }
        let adapter = SqliteHostAdapter::new(tmp.path().to_path_buf()).unwrap();
        (adapter, tmp)
    }

    #[test]
    fn test_record_pid_and_get_run() {
        let (adapter, _tmp) = setup_adapter_with_run("test-run-001");
        let run_id = "test-run-001";

        adapter.record_pid(run_id, 12345).unwrap();

        let run = adapter.get_run(run_id).unwrap();
        assert!(run.is_some());
        let run = run.unwrap();
        assert_eq!(run.subprocess_pid, Some(12345));
    }

    #[test]
    fn test_record_runtime() {
        let (adapter, _tmp) = setup_adapter_with_run("test-run-002");
        let run_id = "test-run-002";

        adapter.record_runtime(run_id, "cli").unwrap();

        let run = adapter.get_run(run_id).unwrap();
        assert!(run.is_some());
        let run = run.unwrap();
        assert_eq!(run.runtime, "cli");
    }

    #[test]
    fn test_mark_cancelled() {
        let (adapter, _tmp) = setup_adapter_with_run("test-run-003");
        let run_id = "test-run-003";

        adapter.record_pid(run_id, 9999).unwrap();
        adapter.mark_cancelled(run_id).unwrap();

        let run = adapter.get_run(run_id).unwrap();
        assert!(run.is_some());
        let run = run.unwrap();
        assert_eq!(run.status, runkon_runtimes::RunStatus::Cancelled);
    }

    #[test]
    fn test_mark_failed_if_running() {
        let (adapter, _tmp) = setup_adapter_with_run("test-run-004");
        let run_id = "test-run-004";

        adapter.record_pid(run_id, 8888).unwrap();
        adapter
            .mark_failed_if_running(run_id, "test failure reason")
            .unwrap();

        let run = adapter.get_run(run_id).unwrap();
        assert!(run.is_some());
        let run = run.unwrap();
        assert_eq!(run.status, runkon_runtimes::RunStatus::Failed);
    }

    #[test]
    fn test_on_event_init() {
        let (adapter, _tmp) = setup_adapter_with_run("test-run-005");
        let run_id = "test-run-005";

        adapter.record_pid(run_id, 7777).unwrap();
        adapter.on_event(
            run_id,
            RuntimeEvent::Init {
                model: Some("sonnet".to_string()),
                session_id: Some("sess-123".to_string()),
            },
        );

        let run = adapter.get_run(run_id).unwrap();
        assert!(run.is_some());
        let run = run.unwrap();
        assert_eq!(run.model, Some("sonnet".to_string()));
        assert_eq!(run.session_id, Some("sess-123".to_string()));
    }

    #[test]
    fn test_on_event_completed() {
        let (adapter, _tmp) = setup_adapter_with_run("test-run-006");
        let run_id = "test-run-006";

        adapter.record_pid(run_id, 6666).unwrap();
        adapter.on_event(
            run_id,
            RuntimeEvent::Completed {
                result_text: Some("done".to_string()),
                session_id: None,
                cost_usd: Some(0.42),
                num_turns: Some(3),
                duration_ms: Some(5000),
                input_tokens: Some(100),
                output_tokens: Some(50),
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            },
        );

        let run = adapter.get_run(run_id).unwrap();
        assert!(run.is_some());
        let run = run.unwrap();
        assert_eq!(run.status, runkon_runtimes::RunStatus::Completed);
        assert_eq!(run.result_text, Some("done".to_string()));
        assert_eq!(run.cost_usd, Some(0.42));
    }

    #[test]
    fn test_on_event_failed() {
        let (adapter, _tmp) = setup_adapter_with_run("test-run-007");
        let run_id = "test-run-007";

        adapter.record_pid(run_id, 5555).unwrap();
        adapter.on_event(
            run_id,
            RuntimeEvent::Failed {
                error: "something broke".to_string(),
                session_id: Some("sess-err".to_string()),
            },
        );

        let run = adapter.get_run(run_id).unwrap();
        assert!(run.is_some());
        let run = run.unwrap();
        assert_eq!(run.status, runkon_runtimes::RunStatus::Failed);
    }

    #[test]
    fn test_on_event_tokens() {
        let (adapter, _tmp) = setup_adapter_with_run("test-run-008");
        let run_id = "test-run-008";

        adapter.record_pid(run_id, 4444).unwrap();
        adapter.on_event(
            run_id,
            RuntimeEvent::Tokens {
                input: 100,
                output: 50,
                cache_read: 10,
                cache_create: 5,
            },
        );

        let run = adapter.get_run(run_id).unwrap();
        assert!(run.is_some());
        let run = run.unwrap();
        assert_eq!(run.input_tokens, Some(100));
        assert_eq!(run.output_tokens, Some(50));
        assert_eq!(run.cache_read_input_tokens, Some(10));
        assert_eq!(run.cache_creation_input_tokens, Some(5));
    }

    #[test]
    fn test_new_returns_runtime_error_for_invalid_path() {
        // Pass a directory path — open_database_compat should fail to open it
        // as a SQLite file.
        let dir = tempfile::tempdir().unwrap();
        let result = SqliteHostAdapter::new(dir.path().to_path_buf());
        assert!(matches!(result, Err(RuntimeError::Agent(_))));
        if let Err(RuntimeError::Agent(msg)) = result {
            assert!(
                msg.contains("failed to open database"),
                "unexpected error message: {msg}"
            );
        }
    }

    #[test]
    fn test_on_event_with_missing_row_does_not_panic() {
        // Build an adapter against a fresh DB but call on_event with a run_id
        // that was never inserted. Each variant exercises a different
        // mgr.update_* path; the function must log a warning and return
        // without panicking even though the UPDATE affects zero rows.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let _ = open_database_compat(tmp.path()).unwrap();
        let adapter = SqliteHostAdapter::new(tmp.path().to_path_buf()).unwrap();

        adapter.on_event(
            "missing-run",
            RuntimeEvent::Init {
                model: Some("sonnet".into()),
                session_id: Some("sess".into()),
            },
        );
        adapter.on_event(
            "missing-run",
            RuntimeEvent::Tokens {
                input: 1,
                output: 2,
                cache_read: 3,
                cache_create: 4,
            },
        );
        adapter.on_event(
            "missing-run",
            RuntimeEvent::Failed {
                error: "boom".into(),
                session_id: None,
            },
        );

        // The row never existed, so `get_run` should still return None.
        assert!(adapter.get_run("missing-run").unwrap().is_none());
    }
}
