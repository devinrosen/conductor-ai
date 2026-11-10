use rusqlite::{Connection, OpenFlags};

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

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

static TEMPLATE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Short hash identifying this test binary build (path, length, mtime). The migrations are
/// compiled into the binary, so any rebuild gets a new key and never reuses a stale schema.
/// `None` when the executable's metadata is unavailable.
fn binary_key() -> Option<String> {
    use std::hash::{Hash, Hasher};
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let meta = std::fs::metadata(&exe).ok()?;
    let mtime = meta.modified().ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    exe.hash(&mut h);
    meta.len().hash(&mut h);
    mtime.hash(&mut h);
    Some(format!("{:016x}", h.finish()))
}

/// Best-effort removal of template files untouched for 24 h, bounding growth across rebuilds.
fn prune_stale_templates(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let cutoff = Duration::from_secs(24 * 60 * 60);
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > cutoff);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Path of the migrated template database for this test binary build, creating it on first use.
///
/// The template is a file in `std::env::temp_dir()` rather than under `CONDUCTOR_HOME`: nextest
/// runs every test in its own process, each with its own isolated home, so only a file outside
/// it can be shared. `temp_dir()` is never the real `~/.conductor`. Returns `None` when the
/// binary can't be keyed; callers then fall back to a per-process in-memory template.
fn template_path() -> Option<&'static Path> {
    TEMPLATE_PATH
        .get_or_init(|| {
            let key = binary_key()?;
            let dir = std::env::temp_dir().join("conductor-test-db-templates");
            std::fs::create_dir_all(&dir).unwrap();
            let name = format!("schema{}-{}.db", db::migrations::LATEST_SCHEMA_VERSION, key);
            let path = dir.join(&name);
            if !path.exists() {
                // Concurrent processes may race; each writes its own tmp file and renames
                // atomically, and every winner writes equivalent content.
                let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
                let _ = std::fs::remove_file(&tmp);
                let conn = Connection::open(&tmp).unwrap();
                db::migrations::run(&conn).unwrap();
                drop(conn);
                std::fs::rename(&tmp, &path).unwrap();
                prune_stale_templates(&dir);
            }
            Some(path)
        })
        .as_deref()
}

/// Fallback template for binaries whose executable can't be keyed: migrated once per process.
fn memory_template() -> &'static std::sync::Mutex<Connection> {
    static MEM: OnceLock<std::sync::Mutex<Connection>> = OnceLock::new();
    MEM.get_or_init(|| {
        let conn = Connection::open_in_memory().unwrap();
        db::migrations::run(&conn).unwrap();
        std::sync::Mutex::new(conn)
    })
}

/// Copy the migrated template into `dest`, an empty database (in-memory or a new/0-byte file).
/// Copies pages only: connection pragmas such as `foreign_keys` are not carried over.
fn copy_template_into(dest: &mut Connection) {
    fn run(src: &Connection, dest: &mut Connection) {
        let backup = rusqlite::backup::Backup::new(src, dest).unwrap();
        // pages_per_step must be > 0; i32::MAX copies everything in one step.
        backup
            .run_to_completion(i32::MAX, Duration::ZERO, None)
            .unwrap();
    }
    match template_path() {
        Some(path) => {
            let src = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .unwrap();
            run(&src, dest);
        }
        None => run(&memory_template().lock().unwrap(), dest),
    }
}

/// Opens an in-memory SQLite database with migrations applied (foreign keys ON). No seed data
/// is inserted.
///
/// The schema comes from copying a per-build migrated template (see [`template_path`]) instead
/// of running every migration, so it costs microseconds, not tens of milliseconds. Tests of
/// the migrations themselves must call `db::migrations::run` on a fresh connection instead.
pub fn create_test_conn() -> Connection {
    let conn = create_test_conn_fk_off();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    conn
}

/// Like [`create_test_conn`] but leaves foreign keys OFF (SQLite's default).
pub fn create_test_conn_fk_off() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    copy_template_into(&mut conn);
    // The bundled SQLite may default foreign_keys to ON, so turn it off explicitly.
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    conn
}

/// Copy the migrated template into the new or empty file at `path`. Panics if the file already
/// holds a schema, so a populated test database is never overwritten.
pub fn seed_test_db_file(path: &Path) {
    let mut conn = Connection::open(path).unwrap();
    let objects: i64 = conn
        .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        objects,
        0,
        "seed_test_db_file refuses a populated database: {}",
        path.display()
    );
    copy_template_into(&mut conn);
}

/// Seed `path` from the template, then open it with [`db::open_database`]. A drop-in for a
/// test's first `open_database` on a fresh temp path.
pub fn open_test_db_file(path: &Path) -> Connection {
    seed_test_db_file(path);
    db::open_database(path).unwrap()
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

/// Seed a workflow run on `worktree_id` with the given status, optional parent run and
/// `feature_base_branch` input. Returns the run ID.
pub fn seed_workflow_run_on_worktree(
    conn: &Connection,
    worktree_id: &str,
    workflow_name: &str,
    status: crate::workflow::WorkflowRunStatus,
    parent_workflow_run_id: Option<&str>,
    base: Option<&str>,
) -> String {
    let agent = crate::agent::AgentManager::new(conn)
        .create_run(Some(worktree_id), "workflow", None)
        .unwrap();
    let run = crate::workflow::create_workflow_run_with_targets(
        conn,
        workflow_name,
        Some(worktree_id),
        None,
        None,
        &agent.id,
        false,
        "manual",
        None,
        parent_workflow_run_id,
        None,
    )
    .unwrap();
    if let Some(b) = base {
        let inputs =
            std::collections::HashMap::from([("feature_base_branch".to_string(), b.to_string())]);
        crate::workflow::set_workflow_run_inputs(conn, &run.id, &inputs).unwrap();
    }
    conn.execute(
        "UPDATE workflow_runs SET status = :s WHERE id = :id",
        rusqlite::named_params! { ":s": status.to_string(), ":id": run.id },
    )
    .unwrap();
    run.id.clone()
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

/// Fields of an `agent_runs` row for [`insert_test_agent_run_with`]. `repo_id` is `r1`.
pub struct TestAgentRun<'a> {
    pub id: &'a str,
    pub worktree_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub status: &'a str,
    pub runtime: &'a str,
    pub parent_run_id: Option<&'a str>,
    pub started_at: &'a str,
}

/// Insert an `agent_runs` row on repo `r1` with the given shape: the one place tests build
/// headless (`runtime = 'claude'`) and interactive/takeover rows.
pub fn insert_test_agent_run_with(conn: &Connection, run: &TestAgentRun<'_>) {
    conn.execute(
        "INSERT INTO agent_runs (id, worktree_id, repo_id, session_id, prompt, status, \
         started_at, runtime, parent_run_id) VALUES (?1, ?2, 'r1', ?3, 'p', ?4, ?5, ?6, ?7)",
        rusqlite::params![
            run.id,
            run.worktree_id,
            run.session_id,
            run.status,
            run.started_at,
            run.runtime,
            run.parent_run_id
        ],
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
    seed_test_db_file(&db_path);
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    (conn, path_str)
}

/// Create `<home>/conductor.db` with repo `r1`/`hook-repo` and an active
/// worktree `w1`/`hook-wt` at `/tmp/ws/hook-wt` (session-hook test fixture).
pub fn seed_hook_db(home: &std::path::Path) -> Connection {
    let conn = open_test_db_file(&home.join("conductor.db"));
    insert_test_repo(&conn, "r1", "hook-repo", "/tmp/hook-repo");
    insert_test_worktree(&conn, "w1", "r1", "hook-wt", "/tmp/ws/hook-wt");
    conn
}

/// `SELECT COUNT(*)` over `table`.
pub fn count_rows(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

static ISOLATED_HOME: OnceLock<PathBuf> = OnceLock::new();

/// Re-export so [`isolate_test_home!`] can reach `ctor` from downstream crates.
#[doc(hidden)]
pub use ctor as __ctor;

/// Whether `CONDUCTOR_HOME` must be replaced with a temp dir: it is unset,
/// empty, or points at the real `~/.conductor`.
fn should_isolate(existing: Option<std::ffi::OsString>, real: Option<PathBuf>) -> bool {
    match existing.filter(|h| !h.is_empty()) {
        None => true,
        Some(existing) => Some(PathBuf::from(existing)) == real,
    }
}

/// Point `CONDUCTOR_HOME` at a per-process temp dir so tests never touch the
/// real `~/.conductor` (#3172).
///
/// Call from a `#[ctor::ctor]` in every test binary (use [`isolate_test_home!`]):
/// it must run before `conductor_dir()` caches its result. A `CONDUCTOR_HOME`
/// that is already set to something other than the real home is left alone.
pub fn isolate_conductor_home() {
    let real = dirs::home_dir().map(|h| h.join(".conductor"));
    if !should_isolate(std::env::var_os("CONDUCTOR_HOME"), real) {
        return;
    }
    let dir = tempfile::Builder::new()
        .prefix("conductor-test-home-")
        .tempdir()
        .expect("create isolated CONDUCTOR_HOME")
        .keep();
    // Runs pre-main, before any other thread exists, so set_var is race-free.
    std::env::set_var("CONDUCTOR_HOME", &dir);
    let _ = ISOLATED_HOME.set(dir);
}

/// Remove the temp dir created by [`isolate_conductor_home`], if any.
/// Call from a `#[ctor::dtor]` in every test binary.
pub fn cleanup_isolated_conductor_home() {
    if let Some(dir) = ISOLATED_HOME.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Install the `CONDUCTOR_HOME` isolation hook (ctor + dtor) in a test binary.
/// Requires the `test-helpers` feature of `conductor-core`.
#[macro_export]
macro_rules! isolate_test_home {
    () => {
        mod test_home_init {
            #[$crate::test_helpers::__ctor::ctor(crate_path = $crate::test_helpers::__ctor)]
            fn init() {
                $crate::test_helpers::isolate_conductor_home();
            }

            #[$crate::test_helpers::__ctor::dtor(crate_path = $crate::test_helpers::__ctor)]
            fn fini() {
                $crate::test_helpers::cleanup_isolated_conductor_home();
            }
        }
    };
}

#[cfg(test)]
mod isolate_home_tests {
    use super::*;
    use std::ffi::OsString;

    fn real() -> Option<PathBuf> {
        Some(PathBuf::from("/home/u/.conductor"))
    }

    #[test]
    fn isolates_when_unset() {
        assert!(should_isolate(None, real()));
    }

    #[test]
    fn isolates_when_empty() {
        assert!(should_isolate(Some(OsString::new()), real()));
    }

    #[test]
    fn isolates_when_real_home() {
        assert!(should_isolate(
            Some(OsString::from("/home/u/.conductor")),
            real()
        ));
    }

    #[test]
    fn leaves_custom_path_alone() {
        assert!(!should_isolate(Some(OsString::from("/tmp/custom")), real()));
    }

    #[test]
    fn isolates_when_real_home_unknown_but_unset() {
        assert!(should_isolate(None, None));
        assert!(!should_isolate(Some(OsString::from("/tmp/x")), None));
    }

    /// The ctor hook ran for this test binary: `CONDUCTOR_HOME` is set, is not
    /// the real home, and exists on disk.
    #[test]
    fn hook_sets_isolated_home() {
        let home = PathBuf::from(std::env::var_os("CONDUCTOR_HOME").expect("CONDUCTOR_HOME set"));
        assert_ne!(
            Some(home.clone()),
            dirs::home_dir().map(|h| h.join(".conductor"))
        );
        assert!(home.is_dir());
    }
}

#[cfg(test)]
mod template_tests {
    use super::*;

    fn schema_dump(conn: &Connection) -> Vec<(String, String, String, Option<String>)> {
        let mut stmt = conn
            .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows
    }

    fn fk_state(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn template_matches_fresh_migration() {
        let fresh = Connection::open_in_memory().unwrap();
        db::migrations::run(&fresh).unwrap();
        let copy = create_test_conn();
        assert_eq!(schema_dump(&copy), schema_dump(&fresh));
        let version: i64 = copy
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM _conductor_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, db::migrations::LATEST_SCHEMA_VERSION as i64);
    }

    #[test]
    fn create_test_conn_has_foreign_keys_on() {
        assert_eq!(fk_state(&create_test_conn()), 1);
    }

    #[test]
    fn create_test_conn_fk_off_has_foreign_keys_off() {
        assert_eq!(fk_state(&create_test_conn_fk_off()), 0);
    }

    #[test]
    fn template_is_shared_file_outside_conductor_home() {
        let path = template_path().expect("test binary can be keyed");
        assert!(!path.starts_with(crate::config::conductor_dir()));
        if let Some(home) = dirs::home_dir() {
            assert!(!path.starts_with(home.join(".conductor")));
        }
    }

    #[test]
    fn copies_are_independent() {
        let a = create_test_conn();
        insert_test_repo(&a, "r1", "a", "/tmp/a");
        assert_eq!(count_rows(&a, "repos"), 1);
        assert_eq!(count_rows(&create_test_conn(), "repos"), 0);
    }

    #[test]
    fn open_test_db_file_on_empty_tempfile() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let conn = open_test_db_file(tmp.path());
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        assert_eq!(count_rows(&conn, "repos"), 0);
        assert_eq!(
            db::connection_db_path(&conn).and_then(|p| p.canonicalize().ok()),
            tmp.path().canonicalize().ok(),
            "connection_db_path should return the file path"
        );
    }

    #[test]
    #[should_panic(expected = "refuses a populated database")]
    fn seed_test_db_file_refuses_populated_db() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        seed_test_db_file(tmp.path());
        seed_test_db_file(tmp.path());
    }
}
