pub mod migrations;
pub mod seed;
pub(crate) mod trace;

use rusqlite::config::DbConfig;
use rusqlite::types::ToSql;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use crate::error::Result;

/// How long a connection waits for a conflicting lock before returning `SQLITE_BUSY`.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Apply connection pragmas. The busy timeout is set first so that every later
/// statement that takes a lock (notably `journal_mode=wal`) waits instead of
/// failing immediately.
fn configure_connection(conn: &Connection, busy_timeout: Duration) -> Result<()> {
    conn.busy_timeout(busy_timeout)?;
    conn.pragma_update(None, "journal_mode", "wal")?;
    conn.pragma_update(None, "foreign_keys", "on")?;
    Ok(())
}

/// Path of the file `conn` is open on, or `None` for an in-memory connection.
///
/// Background threads that open their own connection must use this rather than
/// [`crate::config::db_path`], so a caller working on a temp database (as tests
/// do) never sends the thread to the real `~/.conductor` database.
pub fn connection_db_path(conn: &Connection) -> Option<std::path::PathBuf> {
    conn.path()
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
}

/// Open (or create) the SQLite database with WAL mode enabled and a 5 s busy
/// timeout.
pub fn open_database(path: &Path) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    trace::install(&mut conn);
    configure_connection(&conn, BUSY_TIMEOUT)?;
    migrations::run(&conn)?;
    Ok(conn)
}

/// Open an existing database read-only, without migrating it.
///
/// Used by static `workflow validate` (#3470), which must never create, migrate
/// or write the database. Returns `Ok(None)` when `path` doesn't exist and
/// `Err(ConductorError::Schema)` when the schema is newer than this binary
/// supports. An older schema is allowed and left as it is.
pub fn open_database_read_only(path: &Path) -> Result<Option<Connection>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    trace::install(&mut conn);
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "query_only", "1")?;
    let version = migrations::read_schema_version(&conn)?;
    migrations::check_not_newer(version)?;
    Ok(Some(conn))
}

/// Open (or create) the SQLite database in compatibility mode.
///
/// Same as [`open_database`] but uses [`migrations::run_compat`], which treats
/// a DB schema version ahead of this binary as a warning rather than a fatal
/// error.  Use this in headless subprocesses and drain threads that must keep
/// running after an `implement` agent step has applied a newer migration.
pub fn open_database_compat(path: &Path) -> Result<Connection> {
    open_compat(path, BUSY_TIMEOUT, false)
}

/// Like [`open_database_compat`] but with a caller-chosen busy timeout.
///
/// Used by `conductor session hook`, which must never stall the developer's
/// Claude Code session: it waits a fraction of a second for a lock, then gives up.
///
/// Also disables the checkpoint SQLite runs when the last connection closes
/// (`SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE`). A hook that is the only open connection
/// would otherwise checkpoint the whole WAL under an exclusive lock on close, making
/// a concurrent hook get busy and spool. The WAL is still checkpointed by the
/// auto-checkpoint after a later commit in any process, so it stays bounded.
pub fn open_database_compat_with_busy_timeout(
    path: &Path,
    busy_timeout: Duration,
) -> Result<Connection> {
    open_compat(path, busy_timeout, true)
}

/// Shared compat opener. `skip_close_checkpoint` is set only for the hook path,
/// so headless subprocesses and drain threads keep SQLite's default close behavior.
fn open_compat(
    path: &Path,
    busy_timeout: Duration,
    skip_close_checkpoint: bool,
) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    trace::install(&mut conn);
    if skip_close_checkpoint {
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
    }
    configure_connection(&conn, busy_timeout)?;
    migrations::run_compat(&conn)?;
    Ok(conn)
}

/// Prepend `prefix` to every column token in a comma-separated column list.
///
/// Splits `cols` on `','`, trims whitespace from each token, prepends `prefix`,
/// and joins the results with `", "`. Used to derive table-aliased column lists
/// (e.g. `"s.id, s.name"`) from bare column constants without duplication.
pub(crate) fn prefix_columns(cols: &str, prefix: &str) -> String {
    cols.split(',')
        .map(|col| format!("{}{}", prefix, col.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build a comma-separated list of anonymous SQLite positional placeholders:
/// `?, ?, …`.  Returns an empty string when `n == 0`.
pub(crate) fn sql_placeholders(n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let mut s = String::with_capacity(n.saturating_mul(3));
    for i in 0..n {
        if i > 0 {
            s.push_str(", ");
        }
        s.push('?');
    }
    s
}

/// Build a parameterised IN-clause query and execute a closure with the
/// prepared params slice.
///
/// `prefix` is everything before the `IN (...)` — e.g.
/// `"SELECT id FROM tickets WHERE repo_id = ? AND source_id IN"`.
/// `leading_params` are bound first; `items` are bound after as anonymous `?`.
///
/// The closure receives `(&str, &[&dyn ToSql])` — the SQL string and a
/// ready-to-use params slice — so callers never need to manually convert
/// boxed params.
pub(crate) fn with_in_clause<T>(
    prefix: &str,
    leading_params: &[&dyn ToSql],
    items: &[String],
    f: impl FnOnce(&str, &[&dyn ToSql]) -> T,
) -> T {
    debug_assert!(
        !items.is_empty(),
        "with_in_clause called with empty items — produces invalid SQL `IN ()`"
    );
    let placeholders = sql_placeholders(items.len());
    let sql = format!("{prefix} ({placeholders})");
    let mut params: Vec<&dyn ToSql> = leading_params.to_vec();
    for item in items {
        params.push(item);
    }
    f(&sql, &params)
}

/// Return the set of `parent_run_id` values for all non-terminal workflow runs.
///
/// A free DB helper — intentionally not on `WorkflowManager` — so that the
/// agent orphan reaper can call it without creating a mutual module dependency
/// between the `agent` and `workflow` modules.
pub(crate) fn active_workflow_parent_run_ids(conn: &Connection) -> Result<HashSet<String>> {
    let ids: Vec<String> = query_collect(
        conn,
        "SELECT parent_run_id FROM workflow_runs \
         WHERE status IN ('pending', 'running', 'waiting')",
        [],
        |row| row.get("parent_run_id"),
    )?;
    Ok(ids.into_iter().collect())
}

/// Prepare a query, map each row, and collect results into a `Vec`.
pub fn query_collect<T, P, F>(conn: &Connection, sql: &str, params: P, f: F) -> Result<Vec<T>>
where
    P: rusqlite::Params,
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
{
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(params, f)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    /// Hold an exclusive lock on a non-WAL database for ~300 ms from another
    /// connection, and check that `open` waits for it rather than failing.
    fn assert_open_waits_for_lock(open: fn(&Path) -> Result<Connection>) {
        let tmp = NamedTempFile::new().unwrap();
        let holder = Connection::open(tmp.path()).unwrap();
        holder
            .execute_batch("BEGIN EXCLUSIVE; CREATE TABLE lock_probe (id INTEGER);")
            .unwrap();

        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            holder.execute_batch("COMMIT").unwrap();
        });

        let start = std::time::Instant::now();
        let conn = open(tmp.path()).expect("open should wait for the lock, not fail");
        let elapsed = start.elapsed();
        handle.join().unwrap();

        assert!(
            elapsed >= Duration::from_millis(200),
            "open returned after {elapsed:?}; expected it to wait for the lock"
        );
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let timeout: i64 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
    }

    #[test]
    fn migration_check_takes_no_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conductor.db");
        drop(open_database(&path).unwrap());

        let holder = Connection::open(&path).unwrap();
        holder
            .execute_batch(
                "BEGIN IMMEDIATE; UPDATE _conductor_meta SET value = value WHERE key = 'schema_version';",
            )
            .unwrap();

        // A zero timeout fails at once with SQLITE_BUSY if anything wants the write lock.
        let conn = open_database_compat_with_busy_timeout(&path, Duration::ZERO)
            .expect("migration check must not need the write lock");
        drop(conn);
        holder.execute_batch("COMMIT").unwrap();
    }

    /// Migrate a fresh DB, write through a connection from `open`, close it, and
    /// return the `-wal` file length left behind.
    fn wal_len_after_compat_write(
        open: impl FnOnce(&std::path::Path) -> Result<rusqlite::Connection>,
    ) -> u64 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conductor.db");
        // Create and migrate, then make sure the next open is the only connection.
        drop(open_database(&path).unwrap());

        let conn = open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS ckpt_probe (id INTEGER); INSERT INTO ckpt_probe VALUES (1);",
        )
        .unwrap();
        drop(conn);

        let mut wal = path.into_os_string();
        wal.push("-wal");
        std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0)
    }

    #[test]
    fn hook_connection_skips_checkpoint_on_close() {
        let len = wal_len_after_compat_write(|p| {
            open_database_compat_with_busy_timeout(p, Duration::ZERO)
        });
        assert!(len > 0, "WAL should survive close of the hook connection");
    }

    #[test]
    fn plain_compat_connection_checkpoints_on_close() {
        let len = wal_len_after_compat_write(open_database_compat);
        assert_eq!(
            len, 0,
            "non-hook compat connection should checkpoint on close"
        );
    }

    #[test]
    fn open_database_waits_for_lock() {
        assert_open_waits_for_lock(open_database);
    }

    #[test]
    fn open_database_compat_waits_for_lock() {
        assert_open_waits_for_lock(open_database_compat);
    }

    #[test]
    fn open_database_success() {
        let tmp = NamedTempFile::new().unwrap();
        let conn = open_database(tmp.path()).expect("open_database should succeed");
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let fk: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn open_database_compat_success() {
        let tmp = NamedTempFile::new().unwrap();
        let conn = open_database_compat(tmp.path()).expect("open_database_compat should succeed");
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let fk: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn connection_db_path_none_for_in_memory() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(connection_db_path(&conn), None);
    }

    #[test]
    fn connection_db_path_some_for_file_backed() {
        let tmp = NamedTempFile::new().unwrap();
        let conn = Connection::open(tmp.path()).unwrap();
        let got = connection_db_path(&conn).expect("file-backed connection has a path");
        assert_eq!(
            got.canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn open_database_error_on_bad_path() {
        let bad = std::path::Path::new("/tmp/conductor_no_such_dir_xyz/test.db");
        assert!(open_database(bad).is_err());
    }

    #[test]
    fn open_database_compat_error_on_bad_path() {
        let bad = std::path::Path::new("/tmp/conductor_no_such_dir_xyz/test.db");
        assert!(open_database_compat(bad).is_err());
    }
}

#[cfg(test)]
mod read_only_tests {
    use super::*;
    use crate::error::ConductorError;

    fn set_version(path: &Path, v: i64) {
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "UPDATE _conductor_meta SET value = ?1 WHERE key = 'schema_version'",
            [v.to_string()],
        )
        .unwrap();
    }

    fn version_of(path: &Path) -> i64 {
        let conn = Connection::open(path).unwrap();
        migrations::read_schema_version(&conn).unwrap()
    }

    #[test]
    fn missing_file_is_none_and_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conductor.db");
        assert!(open_database_read_only(&path).unwrap().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn opens_and_rejects_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conductor.db");
        crate::test_helpers::open_test_db_file(&path);
        let conn = open_database_read_only(&path).unwrap().unwrap();
        conn.query_row("SELECT COUNT(*) FROM repos", [], |r| r.get::<_, i64>(0))
            .unwrap();
        assert!(conn.execute("DELETE FROM _conductor_meta", []).is_err());
    }

    #[test]
    fn newer_schema_is_an_error_and_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conductor.db");
        crate::test_helpers::open_test_db_file(&path);
        let newer = migrations::LATEST_SCHEMA_VERSION as i64 + 1;
        set_version(&path, newer);
        let err = open_database_read_only(&path).unwrap_err();
        assert!(matches!(err, ConductorError::Schema(_)));
        assert_eq!(version_of(&path), newer);
    }

    #[test]
    fn older_schema_opens_and_is_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conductor.db");
        crate::test_helpers::open_test_db_file(&path);
        let older = migrations::LATEST_SCHEMA_VERSION as i64 - 1;
        set_version(&path, older);
        assert!(open_database_read_only(&path).unwrap().is_some());
        assert_eq!(version_of(&path), older);
    }
}
