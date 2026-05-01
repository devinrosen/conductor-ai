use rusqlite::Connection;

use crate::db;
use crate::tickets::TicketInput;
use crate::workflow::action_executor::{ActionParams, ExecutionContext};

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

/// Build a minimal `ExecutionContext` for unit tests.
pub fn make_ectx() -> ExecutionContext {
    ExecutionContext {
        run_id: "run-1".to_string(),
        working_dir: std::path::PathBuf::from("/tmp"),
        repo_path: "/tmp".to_string(),
        db_path: std::path::PathBuf::from("/tmp/test.db"),
        step_timeout: std::time::Duration::from_secs(30),
        shutdown: None,
        model: None,
        bot_name: None,
        plugin_dirs: vec![],
        workflow_name: "test".to_string(),
        worktree_id: None,
        parent_run_id: "parent".to_string(),
        step_id: "step-1".to_string(),
    }
}

/// Build a minimal `ActionParams` for unit tests.
pub fn make_action_params(schema: Option<crate::schema_config::OutputSchema>) -> ActionParams {
    ActionParams {
        name: "test-agent".to_string(),
        inputs: std::collections::HashMap::new(),
        retries_remaining: 0,
        retry_error: None,
        snippets: vec![],
        dry_run: false,
        gate_feedback: None,
        schema,
    }
}

/// Build `ActionParams` with a specific name for dispatch tests.
///
/// The `name` field controls which executor the `ActionRegistry` routes to;
/// tests that verify named-executor dispatch must use the same name here as
/// the executor's `name()` implementation.
pub fn make_params(name: &str) -> ActionParams {
    ActionParams {
        name: name.to_string(),
        inputs: std::collections::HashMap::new(),
        retries_remaining: 0,
        retry_error: None,
        snippets: vec![],
        dry_run: false,
        gate_feedback: None,
        schema: None,
    }
}

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
    let wf_mgr = crate::workflow::manager::WorkflowManager::new(conn);
    let parent_id = make_agent_parent_id(conn);
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

/// Returns the agent log dir, creating it if necessary.
pub fn ensure_agent_log_dir() -> std::path::PathBuf {
    let dir = crate::config::agent_log_dir();
    std::fs::create_dir_all(&dir).ok();
    dir
}
