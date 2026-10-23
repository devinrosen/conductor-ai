use rusqlite::Connection;

use crate::db;
use crate::tickets::TicketInput;

/// Global mutex to serialize tests that mutate `ANTHROPIC_API_KEY`.
///
/// `cargo test` runs tests across modules in parallel by default; any test
/// that calls `std::env::set_var`/`remove_var` on a shared env-var must hold
/// this lock for the duration of the mutation + assertion to prevent races.
///
/// # IMPORTANT — usage contract
///
/// Store the guard in a named binding (e.g. `let _guard = ENV_MUTEX.lock()…`),
/// **not** in `_` (which drops the lock immediately). The binding must remain
/// in scope for the entire test body, including any cleanup `set_var` calls.
/// Dropping it early re-introduces the race this mutex is meant to prevent.
pub static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// Insert a `workflow_runs` row with test defaults. Turns foreign keys off because
/// `parent_run_id` points at a placeholder agent run.
pub fn insert_test_wf_run(
    conn: &Connection,
    id: &str,
    name: &str,
    status: &str,
    error: Option<&str>,
) {
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, parent_run_id, status, dry_run, trigger, started_at, iteration, error) \
         VALUES (?1, ?2, 'agent', ?3, 0, 'manual', '2026-01-01T00:00:00Z', 0, ?4)",
        rusqlite::params![id, name, status, error],
    )
    .unwrap();
}

/// Insert a `workflow_run_steps` row; `error` fills both `step_error` and `result_text`.
#[allow(clippy::too_many_arguments)]
pub fn insert_test_wf_step(
    conn: &Connection,
    id: &str,
    run_id: &str,
    name: &str,
    role: &str,
    status: &str,
    position: i64,
    started: &str,
    child_run_id: Option<&str>,
    error: Option<&str>,
) {
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, started_at, child_run_id, step_error, result_text) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        rusqlite::params![id, run_id, name, role, position, status, started, child_run_id, error],
    )
    .unwrap();
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
) -> crate::workflow::item_provider::ProviderContext<'a> {
    crate::workflow::item_provider::ProviderContext { conn, config }
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
    let run = crate::workflow::create_workflow_run(
        conn,
        "test-wf",
        Some("w1"),
        &parent_id,
        false,
        "manual",
        None,
    )
    .unwrap();
    crate::workflow::insert_step(conn, &run.id, "foreach-step", "foreach", false, 0, 0).unwrap()
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

/// A workflow step seeded by [`seed_step_with_agent_run`].
pub struct SeededStep {
    pub step_id: String,
    pub child_run_id: String,
    pub log_path: std::path::PathBuf,
}

/// Insert a step of `wf_run_id` linked to a new agent run whose log file holds `log`
/// and whose stored `result_text` is `stored_result`. The caller removes `log_path`.
/// Needs a worktree `w1` (see [`setup_db`]).
#[allow(clippy::too_many_arguments)]
pub fn seed_step_with_agent_run(
    conn: &Connection,
    wf_run_id: &str,
    step_name: &str,
    iteration: i64,
    status: crate::workflow::WorkflowStepStatus,
    step_error: Option<&str>,
    stored_result: Option<&str>,
    log: &str,
) -> SeededStep {
    let mgr = crate::agent::AgentManager::new(conn);
    let child = mgr.create_run(Some("w1"), "child", None).unwrap();
    let log_path = ensure_agent_log_dir().join(format!("test-seed-{}.log", child.id));
    std::fs::write(&log_path, log).unwrap();
    mgr.update_run_log_file(&child.id, log_path.to_str().unwrap())
        .unwrap();
    if stored_result.is_some() {
        mgr.update_run_completed(
            &child.id,
            None,
            stored_result,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    }
    let step_id =
        crate::workflow::insert_step(conn, wf_run_id, step_name, "actor", false, 0, iteration)
            .unwrap();
    crate::workflow::update_step_status_full(
        conn,
        &step_id,
        status,
        Some(&child.id),
        None,
        None,
        None,
        None,
        None,
        step_error,
    )
    .unwrap();
    SeededStep {
        step_id,
        child_run_id: child.id,
        log_path,
    }
}

/// Returns the agent log dir, creating it if necessary.
pub fn ensure_agent_log_dir() -> std::path::PathBuf {
    let dir = crate::config::agent_log_dir();
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Create a file-based test database with migrations applied.
/// Returns the path to the database file. The file is created in a temporary directory.
/// Use with `CONDUCTOR_TEST_DB` environment variable to make fire_* functions use it.
pub fn create_file_test_db() -> (Connection, String) {
    let tmpdir = std::env::temp_dir();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db_path = tmpdir.join(format!("conductor_test_{}.db", nonce));
    let path_str = db_path.to_string_lossy().to_string();
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    db::migrations::run(&conn).unwrap();
    (conn, path_str)
}
