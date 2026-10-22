pub mod migrations;
pub mod seed;

use rusqlite::types::ToSql;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use crate::error::Result;

/// How long a connection waits for a conflicting lock before returning `SQLITE_BUSY`.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Apply connection pragmas. The busy timeout is set first so that every later
/// statement that takes a lock (notably `journal_mode=wal`) waits instead of
/// failing immediately.
fn configure_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "journal_mode", "wal")?;
    conn.pragma_update(None, "foreign_keys", "on")?;
    Ok(())
}

/// Open (or create) the SQLite database with WAL mode enabled and a 5 s busy
/// timeout.
pub fn open_database(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure_connection(&conn)?;
    migrations::run(&conn)?;
    Ok(conn)
}

/// Open (or create) the SQLite database in compatibility mode.
///
/// Same as [`open_database`] but uses [`migrations::run_compat`], which treats
/// a DB schema version ahead of this binary as a warning rather than a fatal
/// error.  Use this in headless subprocesses and drain threads that must keep
/// running after an `implement` agent step has applied a newer migration.
pub fn open_database_compat(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure_connection(&conn)?;
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
