use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use runkon_runtimes::tracker::{EventSink, RunTracker, RuntimeEvent};
use runkon_runtimes::{RunHandle, RuntimeError};
use rusqlite::Connection;

use std::time::Duration;

use crate::agent::log_parsing::try_recover_running_from_log_at;
use crate::agent::types::LogResult;
use crate::agent::{AgentManager, AgentRunStatus};
use crate::db::open_database_compat;
use crate::error::ConductorError;

/// Host adapter that implements both `RunTracker` and `RunEventSink` over SQLite.
///
/// Holds a single `Connection` behind a `Mutex` so that every trait call re-uses
/// the same underlying database connection instead of opening a new one (N+1
/// anti-pattern).
pub struct SqliteHostAdapter {
    conn: Arc<Mutex<Connection>>,
    /// Directory holding `{run_id}.log`, used to recover a terminal status the
    /// database write missed.
    log_dir: PathBuf,
}

/// Sleeps between retries of a terminal write that hit a busy or locked database.
const TERMINAL_WRITE_BACKOFF: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

/// Run `op`, retrying after each `backoff` sleep while it fails with
/// `SQLITE_BUSY` or `SQLITE_LOCKED`. Any other error returns immediately.
/// `op` must take and release the connection lock itself so no lock is held
/// while sleeping.
fn retry_on_busy<T>(
    backoff: &[Duration],
    mut op: impl FnMut() -> crate::error::Result<T>,
) -> crate::error::Result<T> {
    let mut attempt = 0;
    loop {
        match op() {
            Err(ConductorError::Database(rusqlite::Error::SqliteFailure(e, _)))
                if attempt < backoff.len()
                    && matches!(
                        e.code,
                        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                    ) =>
            {
                std::thread::sleep(backoff[attempt]);
                attempt += 1;
            }
            other => return other,
        }
    }
}

impl SqliteHostAdapter {
    pub fn new(db_path: PathBuf) -> Result<Self, RuntimeError> {
        Self::with_log_dir(db_path, crate::config::agent_log_dir())
    }

    pub(crate) fn with_log_dir(db_path: PathBuf, log_dir: PathBuf) -> Result<Self, RuntimeError> {
        let conn = open_database_compat(&db_path).map_err(|e| {
            RuntimeError::Agent(format!(
                "failed to open database at {}: {e}",
                db_path.display()
            ))
        })?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            log_dir,
        })
    }

    /// Like [`Self::with_mgr`] but keeps the `ConductorError`, so callers can
    /// classify it (e.g. busy/locked) before it is flattened to a string.
    fn with_mgr_raw<F, T>(&self, op: F) -> crate::error::Result<T>
    where
        F: FnOnce(&AgentManager<'_>) -> crate::error::Result<T>,
    {
        let guard = self
            .conn
            .lock()
            .map_err(|e| ConductorError::Agent(format!("database mutex poisoned: {e}")))?;
        let mgr = AgentManager::new(&guard);
        op(&mgr)
    }

    /// [`Self::with_mgr_raw`] retried on busy/locked errors with
    /// [`TERMINAL_WRITE_BACKOFF`], for writes that must not be lost (#3075).
    fn with_terminal_write<F>(&self, op: F) -> crate::error::Result<()>
    where
        F: Fn(&AgentManager<'_>) -> crate::error::Result<()>,
    {
        retry_on_busy(&TERMINAL_WRITE_BACKOFF, || self.with_mgr_raw(&op))
    }

    /// Helper that locks the connection, builds an `AgentManager`, and delegates
    /// to the provided closure. Centralises the mutex-lock + error-mapping
    /// boilerplate shared by every `RunTracker` method.
    fn with_mgr<F, T>(&self, op: F) -> Result<T, RuntimeError>
    where
        F: FnOnce(&AgentManager<'_>) -> crate::error::Result<T>,
    {
        self.with_mgr_raw(op)
            .map_err(|e| RuntimeError::Agent(e.to_string()))
    }
}

impl RunTracker for SqliteHostAdapter {
    /// Runkon's structured-output correction re-spawns the session under the
    /// same `run_id` after the first attempt already marked the row
    /// `completed`, then reads the row back in `poll`. Reopen it here so the
    /// correction run's `Completed` event isn't dropped by the
    /// `status = 'running'` guard. A fresh row is already `running`, so this
    /// is a no-op for first spawns.
    fn record_pid(&self, run_id: &str, pid: u32) -> Result<(), RuntimeError> {
        self.with_mgr(|mgr| {
            mgr.reopen_run_for_resume(run_id)?;
            mgr.update_run_subprocess_pid(run_id, pid)
        })
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
        //
        // Runtimes call this only after the agent has finished. A row that is
        // still `running` then means the terminal write was lost (#3075), so
        // recover the status from the log's `result` event. That is only safe
        // because the result line in the log is the final one by this point.
        self.with_mgr(|mgr| {
            let mut run = mgr.get_run(run_id)?;
            if run
                .as_ref()
                .is_some_and(|r| r.status == AgentRunStatus::Running)
            {
                if let Some(recovered) =
                    try_recover_running_from_log_at(mgr, run_id, &self.log_dir)
                {
                    tracing::warn!(
                        "recovered terminal status for run {run_id} from log after a Running read-back (#3075)"
                    );
                    run = Some(recovered);
                }
            }
            Ok(run.map(|r| r.to_run_handle()))
        })
    }
}

impl EventSink for SqliteHostAdapter {
    fn on_event(&self, run_id: &str, event: RuntimeEvent) {
        let event_label = event_label(&event);
        let result = match event {
            RuntimeEvent::Init { model, session_id } => self.with_mgr_raw(|mgr| {
                mgr.update_run_model_and_session(run_id, model.as_deref(), session_id.as_deref())
            }),
            RuntimeEvent::Tokens {
                input,
                output,
                cache_read,
                cache_create,
            } => self.with_mgr_raw(|mgr| {
                mgr.update_run_tokens_partial(run_id, input, output, cache_read, cache_create)
            }),
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
                self.with_terminal_write(|mgr| {
                    mgr.update_run_completed_if_running_full(run_id, &log_result)
                })
            }
            RuntimeEvent::Failed { error, session_id } => self.with_terminal_write(|mgr| {
                mgr.update_run_failed_with_session(run_id, &error, session_id.as_deref())
            }),
        };

        if let Err(ref e) = result {
            let terminal = matches!(event_label, "Completed" | "Failed");
            if terminal {
                tracing::error!(
                    "SqliteHostAdapter::on_event failed for run {run_id} ({event_label}): {e}"
                );
            } else {
                tracing::warn!(
                    "SqliteHostAdapter::on_event failed for run {run_id} ({event_label}): {e}"
                );
            }
        }
    }
}

/// Thin `RuntimeResolver` adapter that wraps `runkon_runtimes::runtime::resolve_runtime`.
///
/// Bakes in `permission_mode`, `runtimes`, and `RuntimeOptions` (including per-step
/// `max_turns`) at construction time, so the one-argument `resolve(&name)` call stays
/// clean without needing to thread `Config` into `runkon-flow-executors`.
pub struct ConductorRuntimeResolver {
    pub permission_mode: runkon_runtimes::PermissionMode,
    pub runtimes: std::collections::HashMap<String, runkon_runtimes::RuntimeConfig>,
    pub options: runkon_runtimes::RuntimeOptions,
}

impl runkon_runtimes::RuntimeResolver for ConductorRuntimeResolver {
    fn resolve(
        &self,
        name: &str,
    ) -> runkon_runtimes::Result<Box<dyn runkon_runtimes::AgentRuntime>> {
        if name == "claude" {
            let claude_opts = runkon_anthropic::ClaudeRuntimeOptions {
                permission_mode: self.permission_mode.clone(),
                binary_path: self.options.binary_path.clone(),
                env: self.options.env.clone(),
                log_path_for_run: self.options.log_path_for_run.clone(),
                argv_builder: crate::agent_runtime::conductor_argv_builder(),
                stall_threshold: self.options.stall_threshold,
                max_turns: self.options.max_turns,
            };
            return Ok(Box::new(runkon_anthropic::ClaudeRuntime::new(claude_opts)));
        }
        if name == "gemini" {
            // binary: explicit [runtimes.gemini].binary config field, or "gemini" from PATH.
            let binary = self
                .runtimes
                .get("gemini")
                .and_then(|rc| rc.binary.as_deref())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("gemini"));
            // env overlay: from [runtimes.gemini.env], or empty (GEMINI_API_KEY
            // is typically inherited from the parent process environment).
            let env = self
                .runtimes
                .get("gemini")
                .map(|rc| rc.env.clone())
                .unwrap_or_default();
            let gemini_opts = runkon_google::GeminiRuntimeOptions {
                binary_path: binary,
                env,
                permission_mode: self.permission_mode.clone(),
                log_path_for_run: self.options.log_path_for_run.clone(),
                argv_builder: crate::agent_runtime::conductor_gemini_argv_builder(),
                stall_threshold: self.options.stall_threshold,
                max_turns: self.options.max_turns,
            };
            return Ok(Box::new(runkon_google::GeminiRuntime::new(gemini_opts)));
        }
        // Maintain backward compat: runkon 0.7.0-alpha changed DEFAULT_SCRIPT_PROMPT_ENV from
        // "CONDUCTOR_PROMPT" to "FLOW_PROMPT". Inject the old default for script-type runtimes
        // that don't explicitly set prompt_env, so existing conductor workflow scripts that read
        // $CONDUCTOR_PROMPT continue to work without config changes. Construction still goes
        // through resolve_runtime, with only the overridden entry in the map.
        if let Some(rt) = self.runtimes.get(name) {
            if rt.runtime_type.as_deref() == Some("script") && rt.prompt_env.is_none() {
                let config = runkon_runtimes::RuntimeConfig {
                    prompt_env: Some("CONDUCTOR_PROMPT".to_string()),
                    ..rt.clone()
                };
                let runtimes = std::collections::HashMap::from([(name.to_string(), config)]);
                return runkon_runtimes::runtime::resolve_runtime(
                    name,
                    self.permission_mode.clone(),
                    &runtimes,
                    &self.options,
                );
            }
        }
        runkon_runtimes::runtime::resolve_runtime(
            name,
            self.permission_mode.clone(),
            &self.runtimes,
            &self.options,
        )
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
    use runkon_runtimes::tracker::{EventSink, RunTracker};

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

    fn completed_event(text: &str, session: &str) -> RuntimeEvent {
        RuntimeEvent::Completed {
            result_text: Some(text.to_string()),
            session_id: Some(session.to_string()),
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }
    }

    #[test]
    fn test_correction_respawn_records_corrected_result() {
        let (adapter, _tmp) = setup_adapter_with_run("test-run-corr");
        let run_id = "test-run-corr";

        adapter.record_pid(run_id, 1111).unwrap();
        adapter.on_event(run_id, completed_event("A (missing tasks[4].id)", "sess-a"));
        // Correction re-spawns the same run_id, then completes again.
        adapter.record_pid(run_id, 2222).unwrap();
        adapter.on_event(run_id, completed_event("B (corrected)", "sess-b"));

        let run = adapter.get_run(run_id).unwrap().unwrap();
        assert_eq!(run.status, runkon_runtimes::RunStatus::Completed);
        assert_eq!(run.result_text, Some("B (corrected)".to_string()));
        assert_eq!(run.session_id, Some("sess-b".to_string()));
    }

    #[test]
    fn test_record_pid_keeps_failed_run_failed() {
        let (adapter, _tmp) = setup_adapter_with_run("test-run-failed-pid");
        let run_id = "test-run-failed-pid";

        adapter.mark_failed_if_running(run_id, "boom").unwrap();
        adapter.record_pid(run_id, 3333).unwrap();

        let run = adapter.get_run(run_id).unwrap().unwrap();
        assert_eq!(run.status, runkon_runtimes::RunStatus::Failed);
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

    fn setup_adapter_with_log_dir(
        run_id: &str,
    ) -> (
        SqliteHostAdapter,
        tempfile::NamedTempFile,
        tempfile::TempDir,
    ) {
        let (_adapter, tmp) = setup_adapter_with_run(run_id);
        let log_dir = tempfile::tempdir().unwrap();
        let adapter =
            SqliteHostAdapter::with_log_dir(tmp.path().to_path_buf(), log_dir.path().to_path_buf())
                .unwrap();
        (adapter, tmp, log_dir)
    }

    fn write_log(dir: &tempfile::TempDir, run_id: &str, result_line: Option<&str>) {
        let mut body =
            String::from("{\"type\":\"system\",\"subtype\":\"init\"}\n{\"type\":\"assistant\"}\n");
        if let Some(line) = result_line {
            body.push_str(line);
            body.push('\n');
        }
        std::fs::write(dir.path().join(format!("{run_id}.log")), body).unwrap();
    }

    #[test]
    fn get_run_recovers_completed_from_log_when_terminal_write_missing() {
        let run_id = "recover-completed";
        let (adapter, tmp, log_dir) = setup_adapter_with_log_dir(run_id);
        // The process has exited and written its result line, but the
        // `Completed` write never reached the database (no on_event call).
        write_log(
            &log_dir,
            run_id,
            Some(
                r#"{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"s1","total_cost_usd":0.1,"num_turns":3,"duration_ms":1000,"usage":{"input_tokens":10,"output_tokens":5}}"#,
            ),
        );

        let run = adapter.get_run(run_id).unwrap().unwrap();
        assert_eq!(run.status, runkon_runtimes::RunStatus::Completed);
        assert_eq!(run.result_text, Some("done".to_string()));

        let conn = open_database_compat(tmp.path()).unwrap();
        let row = AgentManager::new(&conn).get_run(run_id).unwrap().unwrap();
        assert_eq!(row.status, AgentRunStatus::Completed);
        assert_eq!(row.session_id, Some("s1".to_string()));
        assert_eq!(row.cost_usd, Some(0.1));
        assert_eq!(row.num_turns, Some(3));
        assert_eq!(row.input_tokens, Some(10));
    }

    #[test]
    fn get_run_recovers_failed_from_log_error_result() {
        let run_id = "recover-failed";
        let (adapter, _tmp, log_dir) = setup_adapter_with_log_dir(run_id);
        write_log(
            &log_dir,
            run_id,
            Some(
                r#"{"type":"result","subtype":"error","is_error":true,"result":"it broke","session_id":"s2"}"#,
            ),
        );

        let run = adapter.get_run(run_id).unwrap().unwrap();
        assert_eq!(run.status, runkon_runtimes::RunStatus::Failed);
        assert_eq!(run.result_text, Some("it broke".to_string()));
        assert_eq!(run.session_id, Some("s2".to_string()));
    }

    #[test]
    fn get_run_leaves_running_without_log_result() {
        let run_id = "recover-none";
        let (adapter, _tmp, log_dir) = setup_adapter_with_log_dir(run_id);
        // No log file at all.
        let run = adapter.get_run(run_id).unwrap().unwrap();
        assert_eq!(run.status, runkon_runtimes::RunStatus::Running);

        // A log without a result line.
        write_log(&log_dir, run_id, None);
        let run = adapter.get_run(run_id).unwrap().unwrap();
        assert_eq!(run.status, runkon_runtimes::RunStatus::Running);
    }

    #[test]
    fn get_run_does_not_touch_terminal_rows() {
        let run_id = "recover-terminal";
        let (adapter, _tmp, log_dir) = setup_adapter_with_log_dir(run_id);
        adapter.on_event(run_id, completed_event("A", "sess-a"));
        write_log(
            &log_dir,
            run_id,
            Some(r#"{"type":"result","is_error":false,"result":"B"}"#),
        );

        let run = adapter.get_run(run_id).unwrap().unwrap();
        assert_eq!(run.status, runkon_runtimes::RunStatus::Completed);
        assert_eq!(run.result_text, Some("A".to_string()));
    }

    fn busy_error() -> ConductorError {
        ConductorError::Database(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        ))
    }

    #[test]
    fn retry_on_busy_retries_then_succeeds() {
        let mut calls = 0;
        let backoff = [Duration::ZERO; 3];
        let result = retry_on_busy(&backoff, || {
            calls += 1;
            if calls <= 2 {
                Err(busy_error())
            } else {
                Ok(calls)
            }
        });
        assert_eq!(result.unwrap(), 3);
        assert_eq!(calls, 3);
    }

    #[test]
    fn retry_on_busy_gives_up_after_backoff_exhausted() {
        let mut calls = 0;
        let backoff = [Duration::ZERO; 2];
        let result: crate::error::Result<()> = retry_on_busy(&backoff, || {
            calls += 1;
            Err(busy_error())
        });
        assert!(result.is_err());
        assert_eq!(calls, 3);
    }

    #[test]
    fn retry_on_busy_does_not_retry_other_errors() {
        let mut calls = 0;
        let backoff = [Duration::ZERO; 3];
        let result: crate::error::Result<()> = retry_on_busy(&backoff, || {
            calls += 1;
            Err(ConductorError::Agent("nope".into()))
        });
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }

    fn make_resolver(
        runtimes: std::collections::HashMap<String, runkon_runtimes::RuntimeConfig>,
    ) -> ConductorRuntimeResolver {
        ConductorRuntimeResolver {
            permission_mode: runkon_runtimes::PermissionMode::Default,
            runtimes,
            options: runkon_runtimes::RuntimeOptions {
                binary_path: std::path::PathBuf::from("conductor"),
                env: std::collections::HashMap::new(),
                log_path_for_run: std::sync::Arc::new(|run_id: &str| {
                    std::env::temp_dir().join(format!("{run_id}.log"))
                }),
                workspace_root: std::env::temp_dir(),
                stall_threshold: None,
                max_turns: None,
            },
        }
    }

    #[test]
    fn resolve_gemini_returns_ok_without_config_entry() {
        let resolver = make_resolver(std::collections::HashMap::new());
        let result = runkon_runtimes::RuntimeResolver::resolve(&resolver, "gemini");
        assert!(result.is_ok(), "resolve(\"gemini\") must return Ok");
    }

    #[test]
    fn resolve_gemini_uses_configured_binary() {
        let mut runtimes = std::collections::HashMap::new();
        runtimes.insert(
            "gemini".to_string(),
            runkon_runtimes::RuntimeConfig {
                binary: Some("/usr/local/bin/gemini-custom".to_string()),
                ..Default::default()
            },
        );
        let resolver = make_resolver(runtimes);
        // Resolve must succeed — we cannot inspect the binary_path on the returned
        // Box<dyn AgentRuntime>, but the test confirms no panic/error.
        let result = runkon_runtimes::RuntimeResolver::resolve(&resolver, "gemini");
        assert!(result.is_ok());
    }
}
