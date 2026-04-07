use std::sync::Arc;

use conductor_core::config::Config;
use tempfile::{NamedTempFile, TempDir};
use tokio::sync::{Mutex, RwLock};

use crate::events::EventBus;
use crate::state::AppState;

/// Create an AppState backed by a temporary on-disk SQLite database.
/// Both `state.db` and `state.db_path` point to the same file so that
/// `spawn_blocking` closures opening their own connection see the same data.
///
/// The caller must hold the returned `NamedTempFile` alive for the duration
/// of the test — dropping it deletes the file.
fn state_with_file_db(setup: impl FnOnce(&rusqlite::Connection)) -> (AppState, NamedTempFile) {
    let tmp = NamedTempFile::new().expect("create temp db file");
    let conn = conductor_core::db::open_database(tmp.path()).expect("open temp db");
    setup(&conn);
    let db_path = tmp.path().to_path_buf();
    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        config: Arc::new(RwLock::new(Config::default())),
        events: EventBus::new(1),
        db_path,
        workflow_done_notify: None,
    };
    (state, tmp)
}

/// AppState backed by a fresh on-disk DB with migrations applied. No seed data.
pub fn empty_state() -> (AppState, NamedTempFile) {
    state_with_file_db(|_| {})
}

/// AppState with repo `r1` + worktree `w1` pre-seeded.
pub fn seeded_state() -> (AppState, NamedTempFile) {
    state_with_file_db(|conn| {
        conductor_core::test_helpers::insert_test_repo(conn, "r1", "test-repo", "/tmp/repo");
        conductor_core::test_helpers::insert_test_worktree(
            conn,
            "w1",
            "r1",
            "feat-test",
            "/tmp/ws/feat-test",
        );
    })
}

/// AppState with repo `r1` pointing at a real git TempDir that has an uncommitted
/// file, making the working tree dirty. Used to test the HTTP 409 path in
/// `create_worktree`.
///
/// The caller must keep all three returned values alive for the duration of the
/// test — dropping any of them may delete the underlying file or directory.
pub fn seeded_state_with_dirty_repo() -> (AppState, NamedTempFile, TempDir) {
    let git_dir = TempDir::new().expect("create temp git dir");
    let git_path = git_dir.path().to_str().unwrap().to_owned();

    // Initialise a real git repo so that `git status --porcelain` succeeds.
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&git_path)
            .output()
            .expect("git command failed");
    };
    run(&["init"]);
    run(&[
        "-c",
        "user.email=test@test.com",
        "-c",
        "user.name=Test",
        "commit",
        "--allow-empty",
        "-m",
        "init",
    ]);

    // Write an uncommitted file so that `git status --porcelain` reports dirty.
    std::fs::write(git_dir.path().join("dirty.txt"), "dirty").expect("write dirty file");

    let git_path_for_seed = git_path.clone();
    let (state, tmp) = state_with_file_db(move |conn| {
        conductor_core::test_helpers::insert_test_repo(conn, "r1", "test-repo", &git_path_for_seed);
    });
    (state, tmp, git_dir)
}

/// AppState with repo `r1`, worktree `w1`, and agent_run `ar1` pre-seeded.
pub fn seeded_state_with_agent_run() -> (AppState, NamedTempFile) {
    state_with_file_db(|conn| {
        conductor_core::test_helpers::insert_test_repo(conn, "r1", "test-repo", "/tmp/repo");
        conductor_core::test_helpers::insert_test_worktree(
            conn,
            "w1",
            "r1",
            "feat-test",
            "/tmp/ws/feat-test",
        );
        conductor_core::test_helpers::insert_test_agent_run(conn, "ar1", "w1");
    })
}
