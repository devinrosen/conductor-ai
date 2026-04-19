use rusqlite::Connection;

use crate::db;
use crate::tickets::TicketInput;

/// Opens an in-memory SQLite database with migrations applied. No seed data is inserted.
pub fn create_test_conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    db::migrations::run(&conn).unwrap();
    conn
}

/// Insert a single repo row. `id`, `slug`, and `local_path` are the only values
/// that differ across call sites; other fields use test defaults.
pub fn insert_test_repo(conn: &Connection, id: &str, slug: &str, local_path: &str) {
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
         VALUES (:id, :slug, :local_path, 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z')",
        rusqlite::named_params! { ":id": id, ":slug": slug, ":local_path": local_path },
    )
    .unwrap();
}

/// Insert a single worktree row. `id`, `repo_id`, `slug`, and `path` are parameterised;
/// other fields use test defaults (`branch = 'feat/test'`, `status = 'active'`).
pub fn insert_test_worktree(conn: &Connection, id: &str, repo_id: &str, slug: &str, path: &str) {
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES (:id, :repo_id, :slug, 'feat/test', :path, 'active', '2024-01-01T00:00:00Z')",
        rusqlite::named_params! { ":id": id, ":repo_id": repo_id, ":slug": slug, ":path": path },
    )
    .unwrap();
}

/// Insert a single agent_run row with `status = 'running'`.
pub fn insert_test_agent_run(conn: &Connection, id: &str, worktree_id: &str) {
    conn.execute(
        "INSERT INTO agent_runs (id, worktree_id, prompt, status, started_at) \
         VALUES (:id, :worktree_id, 'test', 'running', '2024-01-01T00:00:00Z')",
        rusqlite::named_params! { ":id": id, ":worktree_id": worktree_id },
    )
    .unwrap();
}

/// Opens an in-memory SQLite database with migrations applied and a test repo + worktree inserted.
///
/// Provides:
/// - repo `r1` (slug `test-repo`)
/// - worktree `w1` (slug `feat-test`, branch `feat/test`, status `active`)
pub fn setup_db() -> Connection {
    let conn = create_test_conn();
    insert_test_repo(&conn, "r1", "test-repo", "/tmp/repo");
    insert_test_worktree(&conn, "w1", "r1", "feat-test", "/tmp/ws/feat-test");
    conn
}

pub fn make_ticket(source_id: &str, title: &str) -> TicketInput {
    TicketInput {
        source_type: "github".to_string(),
        source_id: source_id.to_string(),
        title: title.to_string(),
        body: String::new(),
        state: "open".to_string(),
        labels: vec![],
        assignee: None,
        priority: None,
        url: String::new(),
        raw_json: None,
        label_details: vec![],
        blocked_by: vec![],
        children: vec![],
        parent: None,
    }
}

/// `setup_db()` + an agent_run `ar1` attached to worktree `w1`.
///
/// Provides everything in `setup_db()` plus:
/// - agent_run `ar1` (worktree `w1`, status `running`)
pub fn setup_db_with_agent_run() -> Connection {
    let conn = setup_db();
    insert_test_agent_run(&conn, "ar1", "w1");
    conn
}
