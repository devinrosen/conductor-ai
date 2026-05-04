use std::path::{Path, PathBuf};

use chrono::Utc;
use runkon_flow::events::{EngineEvent, EngineEventData, EventSink};

pub struct PanicDbSink {
    db_path: PathBuf,
}

impl PanicDbSink {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }
}

impl EventSink for PanicDbSink {
    fn emit(&self, event: &EngineEventData) {
        if let EngineEvent::Panicked { run_id, message, .. } = &event.event {
            record_panic_in_db(&self.db_path, run_id, message);
        }
    }
}

fn record_panic_in_db(db_path: &Path, run_id: &str, msg: &str) {
    let error_msg = format!("engine panic: {msg}");
    let now = Utc::now().to_rfc3339();
    match crate::db::open_database(db_path) {
        Ok(conn) => {
            // Guard on status='running' so we don't overwrite a row that another path
            // (cancel, reaper, sibling failure) has already finalized. If the guard
            // matches zero rows, log a warning so the gap is auditable: the panic is
            // still in the per-run log file and re-raised, but the DB record reflects
            // the prior terminal status rather than the panic.
            match conn.execute(
                "UPDATE workflow_runs SET status = 'failed', error = ?1, ended_at = ?2 \
                 WHERE id = ?3 AND status = 'running'",
                rusqlite::params![error_msg, now, run_id],
            ) {
                Ok(0) => {
                    tracing::warn!(
                        run_id = %run_id,
                        "engine_log: panic UPDATE matched 0 rows — run no longer in 'running'; \
                         panic recorded in per-run log only"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(run_id = %run_id, "engine_log: failed to record panic in DB: {e}");
                }
            }
        }
        Err(e) => {
            tracing::error!(run_id = %run_id, "engine_log: failed to open DB to record panic: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use rusqlite::Connection;

    fn make_test_db_with_run() -> (tempfile::TempDir, PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
        crate::db::migrations::run(&conn).unwrap();

        let run_id = ulid::Ulid::new().to_string();
        // Insert a minimal agent_run (FK parent for workflow_runs).
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at) \
             VALUES (?1, 'test', 'running', '2024-01-01T00:00:00Z')",
            rusqlite::params![format!("ar-{run_id}")],
        )
        .unwrap();
        // Insert a running workflow_runs row.
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, parent_run_id, status, dry_run, trigger, started_at) \
             VALUES (?1, 'test-wf', ?2, 'running', 0, 'manual', '2024-01-01T00:00:00Z')",
            rusqlite::params![run_id, format!("ar-{run_id}")],
        )
        .unwrap();

        (dir, db_path, run_id)
    }

    #[test]
    fn engine_panic_is_recorded() {
        let (_dir, db_path, run_id) = make_test_db_with_run();
        let log_dir = _dir.path().join("workflow-logs");

        let sinks: Vec<Arc<dyn runkon_flow::EventSink>> =
            vec![Arc::new(PanicDbSink::new(db_path.clone()))];

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runkon_flow::run_with_per_run_log(
                &run_id,
                log_dir.clone(),
                sinks,
                || panic!("boom"),
            );
        }));

        assert!(result.is_err(), "panic should be re-raised");

        // Log file must exist and contain the panic message.
        let log_path = log_dir.join(format!("{run_id}.log"));
        assert!(log_path.exists(), "per-run log file should be created");
        let log_content = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            log_content.contains("engine panic:"),
            "log should contain 'engine panic:'; got: {log_content}"
        );
        assert!(
            log_content.contains("boom"),
            "log should contain panic message; got: {log_content}"
        );

        // DB row must be flipped to failed with ended_at populated.
        let conn = Connection::open(&db_path).unwrap();
        let (status, error, ended_at): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT status, error, ended_at FROM workflow_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "failed", "status should be flipped to failed");
        assert!(
            error.as_deref().unwrap_or("").starts_with("engine panic:"),
            "error should start with 'engine panic:'; got: {error:?}"
        );
        assert!(
            ended_at.is_some(),
            "ended_at must be set so duration math and 'completed runs' filters work"
        );
    }

    #[test]
    fn missing_db_does_not_propagate_panic_handler_error() {
        let dir = tempfile::tempdir().unwrap();
        let bad_db_path = dir.path().join("does-not-exist").join("conductor.db");
        let log_dir = dir.path().join("workflow-logs");
        let run_id = ulid::Ulid::new().to_string();

        let sinks: Vec<Arc<dyn runkon_flow::EventSink>> =
            vec![Arc::new(PanicDbSink::new(bad_db_path))];

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runkon_flow::run_with_per_run_log(
                &run_id,
                log_dir.clone(),
                sinks,
                || panic!("explode"),
            );
        }));

        assert!(result.is_err(), "original panic must still propagate");
        let log_path = log_dir.join(format!("{run_id}.log"));
        assert!(
            log_path.exists(),
            "per-run log file should still be created"
        );
        let log_content = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            log_content.contains("explode"),
            "panic message should be in log; got: {log_content}"
        );
    }

    #[test]
    fn unwritable_log_dir_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let blocker_file = dir.path().join("not-a-dir");
        std::fs::write(&blocker_file, b"").unwrap();
        let log_dir = blocker_file.join("workflow-logs");

        let (_db_dir, db_path, run_id) = make_test_db_with_run();

        let sinks: Vec<Arc<dyn runkon_flow::EventSink>> =
            vec![Arc::new(PanicDbSink::new(db_path.clone()))];

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runkon_flow::run_with_per_run_log(
                &run_id,
                log_dir,
                sinks,
                || panic!("no log file but still panicking"),
            );
        }));

        assert!(result.is_err(), "panic must still propagate");

        // DB record must still reflect the panic — that path is independent of the log.
        let conn = Connection::open(&db_path).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            status, "failed",
            "status must be flipped even when log file open fails"
        );
    }

    #[test]
    fn already_terminal_run_does_not_overwrite_status() {
        let (_dir, db_path, run_id) = make_test_db_with_run();
        let log_dir = _dir.path().join("workflow-logs");

        // Pre-mark the run as cancelled.
        Connection::open(&db_path)
            .unwrap()
            .execute(
                "UPDATE workflow_runs SET status = 'cancelled', error = 'user cancel' \
                 WHERE id = ?1",
                rusqlite::params![run_id],
            )
            .unwrap();

        let sinks: Vec<Arc<dyn runkon_flow::EventSink>> =
            vec![Arc::new(PanicDbSink::new(db_path.clone()))];

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runkon_flow::run_with_per_run_log(
                &run_id,
                log_dir,
                sinks,
                || panic!("late panic"),
            );
        }));

        assert!(result.is_err(), "panic must still re-raise");

        let conn = Connection::open(&db_path).unwrap();
        let (status, error): (String, Option<String>) = conn
            .query_row(
                "SELECT status, error FROM workflow_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            status, "cancelled",
            "prior terminal status must be preserved"
        );
        assert_eq!(
            error.as_deref(),
            Some("user cancel"),
            "prior error must be preserved"
        );
    }
}
