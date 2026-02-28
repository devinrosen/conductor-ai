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

    // Migration 002: add completed_at to worktrees.
    // Check column existence rather than version number to handle DBs that jumped
    // past version 1 via other feature branches.
    let has_completed_at: bool = conn
        .prepare("SELECT completed_at FROM worktrees LIMIT 0")
        .is_ok();
    if !has_completed_at {
        conn.execute_batch(include_str!("migrations/002_worktree_completed_at.sql"))?;
    }
    if version < 2 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '2')",
            [],
        )?;
    }

    if version < 3 {
        conn.execute_batch(include_str!("migrations/003_agent_runs.sql"))?;
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '3')",
            [],
        )?;
    }

    // Migration 004: add tmux_window to agent_runs.
    // Check column existence to handle DBs that already have it from feature branches.
    let has_tmux_window: bool = conn
        .prepare("SELECT tmux_window FROM agent_runs LIMIT 0")
        .is_ok();
    if !has_tmux_window {
        conn.execute_batch(include_str!("migrations/004_agent_tmux.sql"))?;
    }
    if version < 4 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '4')",
            [],
        )?;
    }

    // Migration 005: add log_file to agent_runs.
    let has_log_file: bool = conn
        .prepare("SELECT log_file FROM agent_runs LIMIT 0")
        .is_ok();
    if !has_log_file {
        conn.execute_batch(include_str!("migrations/005_agent_log_file.sql"))?;
    }
    if version < 5 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '5')",
            [],
        )?;
    }

    Ok(())
}
