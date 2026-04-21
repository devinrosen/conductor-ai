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

/// Build a `ProviderContext` for item-provider unit tests.
pub fn make_provider_ctx<'a>(
    conn: &'a rusqlite::Connection,
    config: &'a crate::config::Config,
    repo_id: Option<&'a str>,
    worktree_id: Option<&'a str>,
) -> crate::workflow::item_provider::ProviderContext<'a> {
    crate::workflow::item_provider::ProviderContext {
        conn,
        config,
        repo_id,
        worktree_id,
    }
}

/// Create an agent run attached to worktree `w1` and return its id.
///
/// Used in tests that need a parent run id before creating workflow runs.
pub fn make_agent_parent_id(conn: &Connection) -> String {
    let agent_mgr = crate::agent::AgentManager::new(conn);
    agent_mgr
        .create_run(Some("w1"), "workflow", None)
        .unwrap()
        .id
}

/// Create a workflow run + foreach step and return the step id.
///
/// Suitable for tests that need a fan-out step to attach items to (e.g. dependency edge tests).
pub fn make_foreach_step(conn: &Connection) -> String {
    let parent_id = make_agent_parent_id(conn);
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(conn);
    let run = wf_mgr
        .create_workflow_run("test-wf", Some("w1"), &parent_id, false, "manual", None)
        .unwrap();
    wf_mgr
        .insert_step(&run.id, "foreach-step", "foreach", false, 0, 0)
        .unwrap()
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
