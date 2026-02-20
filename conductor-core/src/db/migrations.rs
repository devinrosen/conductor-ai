use rusqlite::Connection;

use crate::error::Result;

/// Run all schema migrations. Uses a simple version counter in a meta table.
pub fn run(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _conductor_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;

    let version: i64 = conn.query_row(
        "SELECT COALESCE(
                (SELECT CAST(value AS INTEGER) FROM _conductor_meta WHERE key = 'schema_version'),
                0
            )",
        [],
        |row| row.get(0),
    )?;

    if version < 1 {
        conn.execute_batch(include_str!("migrations/001_initial.sql"))?;
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '1')",
            [],
        )?;
    }

    Ok(())
}
