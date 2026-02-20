pub mod migrations;

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
