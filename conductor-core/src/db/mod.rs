pub mod migrations;
pub mod seed;

use rusqlite::Connection;
use std::path::Path;

use crate::error::Result;

/// Open (or create) the SQLite database with WAL mode enabled.
pub fn open_database(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "wal")?;
    conn.pragma_update(None, "foreign_keys", "on")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    migrations::run(&conn)?;
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

/// Build a comma-separated list of numbered SQLite positional placeholders:
/// `?1, ?2, …, ?n`.  Returns an empty string when `n == 0`.
pub(crate) fn sql_placeholders(n: usize) -> String {
    sql_placeholders_from(n, 1)
}

/// `?start, ?{start+1}, …, ?{start+n-1}`.  Returns an empty string when `n == 0`.
pub(crate) fn sql_placeholders_from(n: usize, start: usize) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(n.saturating_mul(4));
    for i in start..start + n {
        if i > start {
            s.push_str(", ");
        }
        write!(s, "?{i}").unwrap();
    }
    s
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
