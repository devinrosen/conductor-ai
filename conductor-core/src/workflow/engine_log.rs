//! Per-run engine diagnostics: scoped tracing subscriber and panic capture.
//!
//! Each engine thread gets its own log file at `~/.conductor/workflow-logs/<run_id>.log`.
//! `tracing::subscriber::with_default` installs a thread-local subscriber that writes to the
//! per-run file.  A `GlobalForwardLayer` included in that subscriber forwards every event to
//! the host binary's global dispatch as well, so the host binary's subscriber (stderr, log
//! file, etc.) continues to receive engine events for the duration of the call.
//!
//! Panics that escape the engine are caught, written to the per-run log and to
//! `workflow_runs.error`, then re-raised so the host binary's panic handler still fires.

use std::panic::UnwindSafe;
use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter, Layer, Registry};

/// Forwards every event to a captured `tracing::Dispatch` so the host binary's
/// global subscriber still receives events when `with_default` overrides the
/// thread-local dispatcher.
///
/// # Span context limitation
///
/// Only events are forwarded — span lifecycle (`on_new_span`, `on_enter`, etc.) is not.
/// Span IDs are scoped to the Subscriber that created them: forwarding our local span IDs
/// to the host's Subscriber would arrive without a matching `new_span` and produce broken
/// span trees on the host side. Engine code in this codebase emits ad-hoc events
/// (`tracing::info!`, `tracing::error!`) rather than rich span trees, so the events-only
/// forward covers the common case. Span fields that need to reach the host log should be
/// attached to events directly (e.g. `tracing::info!(run_id = %id, "msg")`).
struct GlobalForwardLayer(tracing::Dispatch);

impl<S: tracing::Subscriber> Layer<S> for GlobalForwardLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.0.event(event);
    }
}

/// Run `f` on the current thread with a per-run tracing subscriber installed and a
/// top-level `catch_unwind` guard.
///
/// On panic:
/// 1. The panic message + backtrace are emitted via `tracing::error!` into the per-run log.
/// 2. `workflow_runs.error` is updated to `"engine panic: <msg>"` via a fresh DB connection.
/// 3. The panic is re-raised with `std::panic::resume_unwind` so the host binary's panic
///    handler still fires and the user sees something on stderr.
///
/// The `log_dir` parameter overrides `~/.conductor/workflow-logs/`. Pass `None` in
/// production; pass `Some(temp_dir)` in tests.
pub fn run_engine_with_diagnostics<F>(
    run_id: &str,
    db_path: PathBuf,
    log_dir: Option<PathBuf>,
    f: F,
) where
    F: FnOnce() + UnwindSafe,
{
    let log_dir = log_dir.unwrap_or_else(crate::config::workflow_log_dir);
    let (subscriber, guard) = setup_per_run_tracing(run_id, &log_dir);

    let panic_payload = tracing::subscriber::with_default(subscriber, || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match result {
            Ok(()) => None,
            Err(payload) => {
                let msg = extract_panic_msg(&payload);
                let bt = std::backtrace::Backtrace::force_capture();
                tracing::error!(run_id = %run_id, "engine panic: {msg}\n{bt}");
                record_panic_in_db(&db_path, run_id, &msg);
                Some(payload)
            }
        }
    });

    // Drop the guard to flush the log file before re-raising the panic.
    drop(guard);

    if let Some(payload) = panic_payload {
        std::panic::resume_unwind(payload);
    }
}

fn setup_per_run_tracing(
    run_id: &str,
    log_dir: &Path,
) -> (impl tracing::Subscriber + Send + Sync, WorkerGuard) {
    // Create directory; best-effort — if it fails, the file open below will also fail
    // and we'll fall back to a no-op writer.
    let _ = std::fs::create_dir_all(log_dir);

    // Validate run_id via workflow_log_path (ULID guard). The function bakes in the
    // production directory, so we only use it for validation and join the validated
    // basename back into the caller's log_dir (tests pass a tempdir override). In
    // practice run_id is always a freshly-minted ULID so the error branch is
    // unreachable; the guard is defense-in-depth against path traversal.
    let log_basename = match crate::config::workflow_log_path(run_id) {
        Ok(_) => format!("{run_id}.log"),
        Err(e) => {
            tracing::warn!(
                run_id = %run_id,
                "engine_log: invalid run_id, using fallback log path: {e}"
            );
            "invalid-run-id.log".to_string()
        }
    };
    let log_path = log_dir.join(log_basename);

    // Open (or create) the log file in append mode so resumed engine threads extend
    // rather than overwrite an existing log from a prior attempt on the same run_id.
    let writer: Box<dyn std::io::Write + Send + 'static> = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => Box::new(f),
        Err(e) => {
            tracing::warn!(run_id = %run_id, "engine_log: cannot open log file {}: {e}", log_path.display());
            Box::new(std::io::sink())
        }
    };

    let (non_blocking, guard) = tracing_appender::non_blocking(writer);

    // Respect RUST_LOG but default to `info` so postmortem evidence is preserved even
    // when the host binary's filter is stricter (e.g. `warn`).
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // Capture the current global dispatch so events are forwarded to the host binary's
    // subscriber even while with_default overrides the thread-local dispatcher.
    let global_dispatch = tracing::dispatcher::get_default(|d| d.clone());

    let subscriber = Registry::default()
        .with(filter)
        .with(fmt::Layer::new().with_writer(non_blocking).with_ansi(false))
        .with(GlobalForwardLayer(global_dispatch));

    (subscriber, guard)
}

fn extract_panic_msg(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
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

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_engine_with_diagnostics(&run_id, db_path.clone(), Some(log_dir.clone()), || {
                panic!("boom")
            });
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
    fn log_file_created_on_normal_run() {
        let (_dir, db_path, run_id) = make_test_db_with_run();
        let log_dir = _dir.path().join("workflow-logs");

        run_engine_with_diagnostics(&run_id, db_path, Some(log_dir.clone()), || {
            tracing::info!(run_id = %run_id, "normal engine event");
        });

        let log_path = log_dir.join(format!("{run_id}.log"));
        assert!(
            log_path.exists(),
            "per-run log file should be created on normal run"
        );
    }

    #[test]
    fn missing_db_does_not_propagate_panic_handler_error() {
        // db_path points at a non-existent location. record_panic_in_db must log and
        // swallow the error rather than mask the original panic.
        let dir = tempfile::tempdir().unwrap();
        let bad_db_path = dir.path().join("does-not-exist").join("conductor.db");
        let log_dir = dir.path().join("workflow-logs");
        let run_id = ulid::Ulid::new().to_string();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_engine_with_diagnostics(&run_id, bad_db_path, Some(log_dir.clone()), || {
                panic!("explode")
            });
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
        // log_dir is a path under a regular file (not a directory). Directory
        // creation + file open both fail; the closure must still execute and the
        // re-raise path must still fire.
        let dir = tempfile::tempdir().unwrap();
        let blocker_file = dir.path().join("not-a-dir");
        std::fs::write(&blocker_file, b"").unwrap();
        let log_dir = blocker_file.join("workflow-logs"); // can't be created — parent is a file

        let (_db_dir, db_path, run_id) = make_test_db_with_run();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_engine_with_diagnostics(&run_id, db_path.clone(), Some(log_dir), || {
                panic!("no log file but still panicking")
            });
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
        // Acceptance criterion edge case: another path (cancel, reaper) finalized the
        // run before the panic fired. record_panic_in_db's status='running' guard must
        // leave the prior terminal status alone; the warning is in the per-run log.
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

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_engine_with_diagnostics(&run_id, db_path.clone(), Some(log_dir), || {
                panic!("late panic")
            });
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
