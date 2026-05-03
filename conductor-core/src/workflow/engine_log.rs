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

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter, Layer, Registry};

/// Forwards every event to a captured `tracing::Dispatch` so the host binary's
/// global subscriber still receives events when `with_default` overrides the
/// thread-local dispatcher.
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
    log_dir: &PathBuf,
) -> (impl tracing::Subscriber + Send + Sync, WorkerGuard) {
    // Create directory; best-effort — if it fails, the file open below will also fail
    // and we'll fall back to a no-op writer.
    let _ = std::fs::create_dir_all(log_dir);

    let log_path = log_dir.join(format!("{run_id}.log"));

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
    match crate::db::open_database(db_path) {
        Ok(conn) => {
            if let Err(e) = conn.execute(
                "UPDATE workflow_runs SET status = 'failed', error = ?1 \
                 WHERE id = ?2 AND status = 'running'",
                rusqlite::params![error_msg, run_id],
            ) {
                tracing::error!(run_id = %run_id, "engine_log: failed to record panic in DB: {e}");
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

        // DB row must be flipped to failed.
        let conn = Connection::open(&db_path).unwrap();
        let (status, error): (String, Option<String>) = conn
            .query_row(
                "SELECT status, error FROM workflow_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failed", "status should be flipped to failed");
        assert!(
            error.as_deref().unwrap_or("").starts_with("engine panic:"),
            "error should start with 'engine panic:'; got: {error:?}"
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
}
