use rusqlite::Connection;

use crate::db;

/// Opens an in-memory SQLite database with migrations applied. No seed data is inserted.
pub fn create_test_conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    db::migrations::run(&conn).unwrap();
    conn
}

/// Opens an in-memory SQLite database with migrations applied and a test repo + worktree inserted.
///
/// Provides:
/// - repo `r1` (slug `test-repo`)
/// - worktree `w1` (slug `feat-test`, branch `feat/test`, status `active`)
pub fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    db::migrations::run(&conn).unwrap();
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
         VALUES ('r1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', 'main', '/tmp/ws', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w1', 'r1', 'feat-test', 'feat/test', '/tmp/ws/feat-test', 'active', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn
}
