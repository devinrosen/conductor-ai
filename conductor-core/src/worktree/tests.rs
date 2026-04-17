use super::*;
use rusqlite::{params, Connection};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

use crate::config::Config;
use crate::error::ConductorError;

/// Helper: run a git command in a directory, panicking on failure.
fn git(args: &[&str], dir: &std::path::Path) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Create a bare "remote" repo and a local clone that tracks it.
/// Returns (tmp_dir, remote_path, local_path). TempDir must be kept alive.
fn setup_repo_with_remote() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let remote_path = tmp.path().join("remote.git");
    let local_path = tmp.path().join("local");

    // Create bare remote with explicit main branch
    fs::create_dir_all(&remote_path).unwrap();
    git(&["init", "--bare", "-b", "main"], &remote_path);

    // Clone it
    git(
        &[
            "clone",
            &remote_path.to_string_lossy(),
            &local_path.to_string_lossy(),
        ],
        tmp.path(),
    );

    // Configure user for commits
    git(&["config", "user.email", "test@test.com"], &local_path);
    git(&["config", "user.name", "Test"], &local_path);

    // Ensure we're on the main branch (CI may not have init.defaultBranch=main)
    git(&["checkout", "-b", "main"], &local_path);

    // Create initial commit on main
    let file = local_path.join("README.md");
    fs::write(&file, "initial").unwrap();
    git(&["add", "README.md"], &local_path);
    git(&["commit", "-m", "initial"], &local_path);
    git(&["push", "-u", "origin", "main"], &local_path);

    (tmp, remote_path, local_path)
}

/// Create a second clone of the remote for simulating remote changes.
/// Returns (tmp_dir, clone_path). TempDir must be kept alive.
fn setup_second_clone(remote_path: &std::path::Path) -> (TempDir, std::path::PathBuf) {
    let tmp2 = TempDir::new().unwrap();
    let other = tmp2.path().join("other");
    git(
        &[
            "clone",
            &remote_path.to_string_lossy(),
            &other.to_string_lossy(),
        ],
        tmp2.path(),
    );
    git(&["config", "user.email", "test@test.com"], &other);
    git(&["config", "user.name", "Test"], &other);
    (tmp2, other)
}

#[test]
fn test_branch_exists() {
    let (_tmp, _, local) = setup_repo_with_remote();
    assert!(git_helpers::branch_exists(local.to_str().unwrap(), "main"));
    assert!(!git_helpers::branch_exists(
        local.to_str().unwrap(),
        "nonexistent"
    ));
}

#[test]
fn test_detect_remote_head() {
    let (_tmp, _, local) = setup_repo_with_remote();
    // Local clones don't auto-set origin/HEAD; set it explicitly (as GitHub does)
    git(&["remote", "set-head", "origin", "main"], &local);
    let detected = git_helpers::detect_remote_head(local.to_str().unwrap());
    assert_eq!(detected, Some("main".to_string()));
}

#[test]
fn test_detect_remote_head_not_set() {
    let (_tmp, _, local) = setup_repo_with_remote();
    // Without setting origin/HEAD, detection returns None
    let detected = git_helpers::detect_remote_head(local.to_str().unwrap());
    assert_eq!(detected, None);
}

#[test]
fn test_resolve_base_branch_uses_configured() {
    let (_tmp, _, local) = setup_repo_with_remote();
    let result = git_helpers::resolve_base_branch(local.to_str().unwrap(), "main");
    assert_eq!(result, "main");
}

#[test]
fn test_resolve_base_branch_falls_back_to_detection() {
    let (_tmp, _, local) = setup_repo_with_remote();
    // Pass a non-existent configured default; should detect "main" via remote HEAD
    let result = git_helpers::resolve_base_branch(local.to_str().unwrap(), "nonexistent");
    assert_eq!(result, "main");
}

#[test]
fn test_ensure_base_up_to_date_clean_fast_forward() {
    let (_tmp, remote, local) = setup_repo_with_remote();

    // Simulate a new commit on remote by cloning elsewhere and pushing
    let (_tmp2, other) = setup_second_clone(&remote);
    let file = other.join("new_file.txt");
    fs::write(&file, "new content").unwrap();
    git(&["add", "new_file.txt"], &other);
    git(&["commit", "-m", "remote commit"], &other);
    git(&["push", "origin", "main"], &other);

    // Local is now behind origin/main
    let warnings =
        git_helpers::ensure_base_up_to_date(local.to_str().unwrap(), "main", false, false).unwrap();
    assert!(warnings.is_empty(), "unexpected warnings: {:?}", warnings);

    // Verify local main now has the new file
    assert!(local.join("new_file.txt").exists());
}

#[test]
fn test_ensure_base_up_to_date_dirty_working_tree() {
    let (_tmp, _, local) = setup_repo_with_remote();

    // Make the working tree dirty by modifying a tracked file (untracked files are
    // intentionally ignored — see `check_main_health`).
    fs::write(local.join("README.md"), "modified").unwrap();

    let result = git_helpers::ensure_base_up_to_date(local.to_str().unwrap(), "main", false, false);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("uncommitted changes"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_ensure_base_up_to_date_allows_untracked_files() {
    let (_tmp, _, local) = setup_repo_with_remote();

    // Untracked files don't affect `git worktree add` and must not block creation.
    fs::write(local.join("untracked.txt"), "untracked").unwrap();
    fs::create_dir_all(local.join("untracked_dir")).unwrap();
    fs::write(local.join("untracked_dir").join("file"), "x").unwrap();

    let result = git_helpers::ensure_base_up_to_date(local.to_str().unwrap(), "main", false, false);
    assert!(
        result.is_ok(),
        "untracked files should not block worktree creation; got: {:?}",
        result.err()
    );
}

#[test]
fn test_ensure_base_up_to_date_diverged_branch() {
    let (_tmp, remote, local) = setup_repo_with_remote();

    // Push a commit from another clone
    let (_tmp2, other) = setup_second_clone(&remote);
    fs::write(other.join("remote.txt"), "from remote").unwrap();
    git(&["add", "remote.txt"], &other);
    git(&["commit", "-m", "remote diverge"], &other);
    git(&["push", "origin", "main"], &other);

    // Make a LOCAL commit on main that diverges
    fs::write(local.join("local.txt"), "from local").unwrap();
    git(&["add", "local.txt"], &local);
    git(&["commit", "-m", "local diverge"], &local);

    // Now ensure_base_up_to_date should warn about divergence
    let warnings =
        git_helpers::ensure_base_up_to_date(local.to_str().unwrap(), "main", false, false).unwrap();
    assert!(
        warnings.iter().any(|w| w.contains("diverged")),
        "expected divergence warning, got: {:?}",
        warnings
    );
}

#[test]
fn test_check_main_health_clean_repo() {
    let (_tmp, _, local) = setup_repo_with_remote();
    let health = git_helpers::check_main_health(local.to_str().unwrap(), "main");
    assert!(!health.is_dirty, "clean repo should not be dirty");
    assert!(health.dirty_files.is_empty());
    assert_eq!(
        health.commits_behind, 0,
        "clean repo should be 0 commits behind"
    );
}

#[test]
fn test_check_main_health_dirty_repo() {
    let (_tmp, _, local) = setup_repo_with_remote();

    // Modify a tracked file — that's real uncommitted work.
    fs::write(local.join("README.md"), "modified").unwrap();

    let health = git_helpers::check_main_health(local.to_str().unwrap(), "main");
    assert!(
        health.is_dirty,
        "repo with modified tracked file should be dirty"
    );
    assert!(
        !health.dirty_files.is_empty(),
        "dirty_files should be non-empty"
    );
}

#[test]
fn test_check_main_health_untracked_files_not_dirty() {
    let (_tmp, _, local) = setup_repo_with_remote();

    // Untracked files and directories don't affect `git worktree add`. Flagging them
    // as dirty produces false positives that block ticket starts (e.g. test-helper
    // submodule directories that exist locally but aren't committed).
    fs::write(local.join("untracked.txt"), "untracked").unwrap();
    fs::create_dir_all(local.join("untracked_dir")).unwrap();
    fs::write(local.join("untracked_dir").join("file"), "x").unwrap();

    let health = git_helpers::check_main_health(local.to_str().unwrap(), "main");
    assert!(
        !health.is_dirty,
        "untracked files must not be reported as dirty; got dirty_files: {:?}",
        health.dirty_files
    );
}

#[test]
fn test_check_main_health_no_remote_ref_commits_behind_zero() {
    let (_tmp, _, local) = setup_repo_with_remote();

    // Remove the remote so origin/<branch> ref doesn't exist — commits_behind must be 0.
    git(&["remote", "remove", "origin"], &local);

    let health = git_helpers::check_main_health(local.to_str().unwrap(), "main");
    assert_eq!(
        health.commits_behind, 0,
        "commits_behind should be 0 when remote tracking ref is absent"
    );
}

#[test]
fn test_check_main_health_commits_behind_positive() {
    let (_tmp, remote, local) = setup_repo_with_remote();

    // Clone the remote to a second directory, commit a new file, and push
    let (_tmp2, other) = setup_second_clone(&remote);
    fs::write(other.join("behind.txt"), "behind").unwrap();
    git(&["add", "behind.txt"], &other);
    git(&["commit", "-m", "remote-only commit"], &other);
    git(&["push", "origin", "main"], &other);

    // Fetch in local so origin/main tracking ref is updated, but do NOT merge
    git(&["fetch", "origin"], &local);

    let health = git_helpers::check_main_health(local.to_str().unwrap(), "main");
    assert_eq!(
        health.commits_behind, 1,
        "local should be 1 commit behind origin/main after remote push + fetch"
    );
}

#[test]
fn test_ensure_base_up_to_date_force_dirty_skips_check() {
    let (_tmp, _, local) = setup_repo_with_remote();

    // Make the working tree dirty via a tracked-file modification.
    fs::write(local.join("README.md"), "modified").unwrap();

    // With force_dirty=true, the dirty check is skipped — should succeed
    let result = git_helpers::ensure_base_up_to_date(local.to_str().unwrap(), "main", true, false);
    assert!(
        result.is_ok(),
        "force_dirty=true should skip dirty check; got: {:?}",
        result.err()
    );
}

#[test]
fn test_ensure_base_up_to_date_skips_status_when_pre_verified_clean() {
    let (_tmp, _, local) = setup_repo_with_remote();

    // Make the working tree dirty via a tracked-file modification — would normally
    // cause an error.
    fs::write(local.join("README.md"), "modified").unwrap();

    // With pre_verified_clean=true, the git status check is skipped
    // (the fetch may fail too since there's no network, but that's a soft warning)
    let result = git_helpers::ensure_base_up_to_date(local.to_str().unwrap(), "main", false, true);
    assert!(
        result.is_ok(),
        "pre_verified_clean=true should skip dirty check; got: {:?}",
        result.err()
    );
}

#[test]
fn test_list_by_ticket() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    // Insert tickets referenced by worktrees
    conn.execute(
        "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
         VALUES ('t1', 'r1', 'github', '1', 'Ticket 1', '', 'open', '[]', '', '2024-01-01T00:00:00Z', '{}')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
         VALUES ('t2', 'r1', 'github', '2', 'Ticket 2', '', 'open', '[]', '', '2024-01-01T00:00:00Z', '{}')",
        [],
    ).unwrap();

    // Insert worktrees with ticket_id
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at) \
         VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 't1', 'active', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at) \
         VALUES ('wt2', 'r1', 'feat-b', 'feat/b', '/tmp/ws/feat-b', 't1', 'merged', '2024-01-02T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at) \
         VALUES ('wt3', 'r1', 'feat-c', 'feat/c', '/tmp/ws/feat-c', 't2', 'active', '2024-01-03T00:00:00Z')",
        [],
    ).unwrap();

    let mgr = WorktreeManager::new(&conn, &config);

    // Should return 2 worktrees for ticket t1, ordered by created_at DESC
    let worktrees = mgr.list_by_ticket("t1").unwrap();
    assert_eq!(worktrees.len(), 2);
    assert_eq!(worktrees[0].id, "wt2"); // newer first
    assert_eq!(worktrees[1].id, "wt1");

    // Should return 1 worktree for ticket t2
    let worktrees = mgr.list_by_ticket("t2").unwrap();
    assert_eq!(worktrees.len(), 1);
    assert_eq!(worktrees[0].id, "wt3");

    // Should return empty for unknown ticket
    let worktrees = mgr.list_by_ticket("nonexistent").unwrap();
    assert!(worktrees.is_empty());
}

#[test]
fn test_ensure_base_up_to_date_detached_head() {
    let (_tmp, remote, local) = setup_repo_with_remote();

    // Push a second commit from another clone so there's something to ff
    let tmp2 = TempDir::new().unwrap();
    let other = tmp2.path().join("other");
    git(
        &["clone", &remote.to_string_lossy(), &other.to_string_lossy()],
        tmp2.path(),
    );
    git(&["config", "user.email", "test@test.com"], &other);
    git(&["config", "user.name", "Test"], &other);
    fs::write(other.join("extra.txt"), "extra").unwrap();
    git(&["add", "extra.txt"], &other);
    git(&["commit", "-m", "extra commit"], &other);
    git(&["push", "origin", "main"], &other);

    // Detach HEAD in local
    git(&["checkout", "--detach", "HEAD"], &local);

    let warnings =
        git_helpers::ensure_base_up_to_date(local.to_str().unwrap(), "main", false, false).unwrap();
    // Should succeed (fast-forward refs/heads/main) with no warnings
    assert!(warnings.is_empty(), "unexpected warnings: {:?}", warnings);

    // Verify refs/heads/main was updated to include the remote commit
    // (the working tree stays on detached HEAD — we only update the ref)
    let log = std::process::Command::new("git")
        .current_dir(&local)
        .args(["log", "--oneline", "refs/heads/main"])
        .output()
        .unwrap();
    let log_str = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_str.contains("extra commit"),
        "refs/heads/main should contain the fast-forwarded commit"
    );
}

#[test]
fn test_worktree_status_is_done() {
    assert!(!WorktreeStatus::Active.is_done());
    assert!(WorktreeStatus::Merged.is_done());
    assert!(WorktreeStatus::Abandoned.is_done());
}

#[test]
fn test_worktree_status_as_str() {
    assert_eq!(WorktreeStatus::Active.as_str(), "active");
    assert_eq!(WorktreeStatus::Merged.as_str(), "merged");
    assert_eq!(WorktreeStatus::Abandoned.as_str(), "abandoned");
}

#[test]
fn test_worktree_status_display() {
    assert_eq!(WorktreeStatus::Active.to_string(), "active");
    assert_eq!(WorktreeStatus::Merged.to_string(), "merged");
    assert_eq!(WorktreeStatus::Abandoned.to_string(), "abandoned");
}

#[test]
fn test_worktree_status_from_str_valid() {
    assert_eq!(
        "active".parse::<WorktreeStatus>().unwrap(),
        WorktreeStatus::Active
    );
    assert_eq!(
        "merged".parse::<WorktreeStatus>().unwrap(),
        WorktreeStatus::Merged
    );
    assert_eq!(
        "abandoned".parse::<WorktreeStatus>().unwrap(),
        WorktreeStatus::Abandoned
    );
}

#[test]
fn test_worktree_status_from_str_invalid() {
    let err = "unknown_value".parse::<WorktreeStatus>().unwrap_err();
    assert_eq!(err, "unknown WorktreeStatus: unknown_value");
}

#[test]
fn test_update_status_to_merged_sets_completed_at() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    mgr.update_status("wt1", WorktreeStatus::Merged).unwrap();

    let wt = mgr.get_by_id("wt1").unwrap();
    assert_eq!(wt.status, WorktreeStatus::Merged);
    assert!(wt.completed_at.is_some());
}

#[test]
fn test_update_status_to_abandoned_sets_completed_at() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    mgr.update_status("wt1", WorktreeStatus::Abandoned).unwrap();

    let wt = mgr.get_by_id("wt1").unwrap();
    assert_eq!(wt.status, WorktreeStatus::Abandoned);
    assert!(wt.completed_at.is_some());
}

// ---- get_by_slug_or_branch tests ----

fn insert_test_worktree(conn: &Connection, id: &str, repo_id: &str, slug: &str, branch: &str) {
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES (?1, ?2, ?3, ?4, '/tmp/ws', 'active', '2024-01-01T00:00:00Z')",
        params![id, repo_id, slug, branch],
    )
    .unwrap();
}

#[test]
fn test_get_by_slug_or_branch_slug_match() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    insert_test_worktree(
        &conn,
        "wt1",
        "r1",
        "feat-123-my-feature",
        "feat/123-my-feature",
    );

    let mgr = WorktreeManager::new(&conn, &config);
    let wt = mgr
        .get_by_slug_or_branch("r1", "feat-123-my-feature")
        .unwrap();
    assert_eq!(wt.id, "wt1");
}

#[test]
fn test_get_by_slug_or_branch_branch_match() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    insert_test_worktree(
        &conn,
        "wt1",
        "r1",
        "feat-123-my-feature",
        "feat/123-my-feature",
    );

    let mgr = WorktreeManager::new(&conn, &config);
    let wt = mgr
        .get_by_slug_or_branch("r1", "feat/123-my-feature")
        .unwrap();
    assert_eq!(wt.id, "wt1");
}

#[test]
fn test_get_by_slug_or_branch_did_you_mean() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    insert_test_worktree(
        &conn,
        "wt1",
        "r1",
        "feat-123-my-feature",
        "feat/123-my-feature",
    );
    insert_test_worktree(&conn, "wt2", "r1", "fix-456-other", "fix/456-other");

    let mgr = WorktreeManager::new(&conn, &config);
    let err = mgr
        .get_by_slug_or_branch("r1", "totally-wrong")
        .unwrap_err()
        .to_string();
    assert!(err.contains("totally-wrong"), "error: {err}");
    assert!(err.contains("did you mean"), "error: {err}");
    assert!(err.contains("feat-123-my-feature"), "error: {err}");
}

#[test]
fn test_get_by_slug_or_branch_empty_repo() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    // Use a repo ID that has no worktrees seeded in the test DB.
    let mgr = WorktreeManager::new(&conn, &config);
    let err = mgr
        .get_by_slug_or_branch("repo-with-no-worktrees", "anything")
        .unwrap_err()
        .to_string();
    assert!(err.contains("anything"), "error: {err}");
    // No "did you mean" hint when repo has no worktrees
    assert!(!err.contains("did you mean"), "error: {err}");
}

#[test]
fn test_update_status_to_active_clears_completed_at() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, completed_at) \
         VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'merged', '2024-01-01T00:00:00Z', '2024-02-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    mgr.update_status("wt1", WorktreeStatus::Active).unwrap();

    let wt = mgr.get_by_id("wt1").unwrap();
    assert_eq!(wt.status, WorktreeStatus::Active);
    assert!(wt.completed_at.is_none());
}

#[test]
fn test_remove_git_artifacts_success() {
    let (_tmp, _, local) = setup_repo_with_remote();
    let local_str = local.to_str().unwrap();

    // Create a branch and a worktree for it
    let wt_path = local.parent().unwrap().join("feat-test-wt");
    git(
        &[
            "worktree",
            "add",
            wt_path.to_str().unwrap(),
            "-b",
            "feat/test-wt",
        ],
        &local,
    );

    assert!(wt_path.exists());
    assert!(git_helpers::branch_exists(local_str, "feat/test-wt"));

    // remove_git_artifacts should cleanly remove both
    git_helpers::remove_git_artifacts(local_str, wt_path.to_str().unwrap(), "feat/test-wt");

    assert!(!wt_path.exists());
    assert!(!git_helpers::branch_exists(local_str, "feat/test-wt"));
}

#[test]
fn test_remove_git_artifacts_nonexistent_does_not_panic() {
    let (_tmp, _, local) = setup_repo_with_remote();
    let local_str = local.to_str().unwrap();

    // Both the worktree path and branch are nonexistent; must not panic
    git_helpers::remove_git_artifacts(local_str, "/nonexistent/path/wt", "feat/no-such-branch");
}

#[test]
#[tracing_test::traced_test]
fn test_remove_git_artifacts_no_warn_when_already_gone() {
    let (_tmp, _, local) = setup_repo_with_remote();
    let local_str = local.to_str().unwrap();

    // Both path and branch are nonexistent — should be silently skipped, no WARN
    git_helpers::remove_git_artifacts(local_str, "/nonexistent/path/wt", "feat/no-such-branch");

    assert!(!logs_contain("git worktree remove failed"));
    assert!(!logs_contain("git branch -D failed"));
}

#[test]
fn test_reap_stale_worktrees_backfills_completed_at() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    // Insert a merged worktree with no completed_at and a nonexistent path
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt-stale', 'r1', 'feat-stale', 'feat/stale', '/nonexistent/stale-wt', 'merged', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    let reaped = mgr.reap_stale_worktrees().unwrap();
    assert_eq!(reaped, 1);

    // completed_at should now be set
    let completed_at: Option<String> = conn
        .query_row(
            "SELECT completed_at FROM worktrees WHERE id = 'wt-stale'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(completed_at.is_some());
}

#[test]
fn test_reap_stale_worktrees_skips_active() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    // w1 from setup_db is active — should not be reaped
    let mgr = WorktreeManager::new(&conn, &config);
    let reaped = mgr.reap_stale_worktrees().unwrap();
    assert_eq!(reaped, 0);
}

#[test]
fn test_reap_stale_worktrees_skips_already_completed() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    // Insert a merged worktree that already has completed_at and nonexistent path
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, completed_at) \
         VALUES ('wt-done', 'r1', 'feat-done', 'feat/done', '/nonexistent/done-wt', 'merged', '2024-01-01T00:00:00Z', '2024-02-01T00:00:00Z')",
        [],
    ).unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    let reaped = mgr.reap_stale_worktrees().unwrap();
    // Path doesn't exist and completed_at is already set → not reaped
    assert_eq!(reaped, 0);
}

#[test]
fn test_reap_stale_worktrees_removes_existing_path() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    let (_tmp, _, local) = setup_repo_with_remote();
    let local_str = local.to_str().unwrap();

    // Update repo to use real local path
    conn.execute(
        "UPDATE repos SET local_path = ?1 WHERE id = 'r1'",
        params![local_str],
    )
    .unwrap();

    // Create a real worktree
    let wt_path = local.parent().unwrap().join("stale-wt");
    git(&["branch", "feat/stale-wt"], &local);
    git(
        &[
            "worktree",
            "add",
            &wt_path.to_string_lossy(),
            "feat/stale-wt",
        ],
        &local,
    );
    assert!(wt_path.exists());

    // Insert as merged with no completed_at
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt-real', 'r1', 'feat-stale-wt', 'feat/stale-wt', ?1, 'merged', '2024-01-01T00:00:00Z')",
        params![wt_path.to_str().unwrap()],
    ).unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    let reaped = mgr.reap_stale_worktrees().unwrap();
    assert_eq!(reaped, 1);

    // Worktree directory should be removed
    assert!(!wt_path.exists());

    // completed_at should be backfilled
    let completed_at: Option<String> = conn
        .query_row(
            "SELECT completed_at FROM worktrees WHERE id = 'wt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(completed_at.is_some());
}

#[test]
fn test_create_auto_clones_missing_local_path() {
    let (tmp, remote, _local) = setup_repo_with_remote();

    // Point local_path to a directory that does not yet exist
    let missing_local = tmp.path().join("not-yet-cloned");

    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();

    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    let _repo = repo_mgr
        .register(
            "myrepo",
            missing_local.to_str().unwrap(),
            remote.to_str().unwrap(),
            Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
        )
        .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    let result = mgr.create("myrepo", "feat-auto-clone", Default::default());
    assert!(
        result.is_ok(),
        "expected Ok, got: {:?}",
        result.unwrap_err()
    );

    // The local repo should now exist on disk (cloned)
    assert!(missing_local.exists(), "local_path should have been cloned");

    // The worktree directory should also exist
    let (wt, _) = result.unwrap();
    assert!(
        Path::new(&wt.path).exists(),
        "worktree path should exist: {}",
        wt.path
    );
}

#[test]
fn test_create_clone_fails_with_bad_remote() {
    let tmp = TempDir::new().unwrap();
    let missing_local = tmp.path().join("not-yet-cloned");

    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();

    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    repo_mgr
        .register(
            "badrepo",
            missing_local.to_str().unwrap(),
            "file:///this/does/not/exist/at/all",
            Some(tmp.path().join("workspaces/badrepo").to_str().unwrap()),
        )
        .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    let result = mgr.create("badrepo", "feat-should-fail", Default::default());
    assert!(result.is_err(), "expected Err for bad remote");
    match result.unwrap_err() {
        ConductorError::Git(_) => {}
        other => panic!("expected ConductorError::Git, got: {other:?}"),
    }
}

#[test]
fn test_reap_stale_worktrees_handles_abandoned() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt-aband', 'r1', 'feat-aband', 'feat/aband', '/nonexistent/aband-wt', 'abandoned', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    let reaped = mgr.reap_stale_worktrees().unwrap();
    assert_eq!(reaped, 1);

    let completed_at: Option<String> = conn
        .query_row(
            "SELECT completed_at FROM worktrees WHERE id = 'wt-aband'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(completed_at.is_some());
}

#[test]
fn test_reap_stale_worktrees_removes_deregistered_path() {
    // Simulate a git-deregistered worktree: the directory exists on disk but
    // git no longer tracks it (git worktree prune was run externally).
    // reap_stale_worktrees() must delete the directory and not loop forever.
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    let tmp = TempDir::new().unwrap();
    let deregistered_path = tmp.path().join("deregistered-wt");
    fs::create_dir_all(&deregistered_path).unwrap();
    assert!(deregistered_path.exists());

    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt-dereg', 'r1', 'feat-dereg', 'feat/dereg', ?1, 'merged', '2024-01-01T00:00:00Z')",
        params![deregistered_path.to_str().unwrap()],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);

    // First call: directory should be removed and completed_at backfilled
    let reaped = mgr.reap_stale_worktrees().unwrap();
    assert_eq!(reaped, 1);
    assert!(
        !deregistered_path.exists(),
        "directory should have been removed by fs fallback"
    );

    let completed_at: Option<String> = conn
        .query_row(
            "SELECT completed_at FROM worktrees WHERE id = 'wt-dereg'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(completed_at.is_some());

    // Second call: nothing left to reap — confirms the loop is broken
    let reaped2 = mgr.reap_stale_worktrees().unwrap();
    assert_eq!(
        reaped2, 0,
        "should not loop: nothing to reap on second call"
    );
}

// ── parse_pr_view_output tests ────────────────────────────────────────────

#[test]
fn test_parse_pr_view_output_same_repo() {
    let raw = "feat/my-feature|main|owner/repo|false";
    let (head, base, head_repo, is_fork) = git_helpers::parse_pr_view_output(raw).unwrap();
    assert_eq!(head, "feat/my-feature");
    assert_eq!(base, "main");
    assert_eq!(head_repo, "owner/repo");
    assert!(!is_fork);
}

#[test]
fn test_parse_pr_view_output_fork() {
    let raw = "feat/my-feature|main|fork-user/repo|true";
    let (head, base, head_repo, is_fork) = git_helpers::parse_pr_view_output(raw).unwrap();
    assert_eq!(head, "feat/my-feature");
    assert_eq!(base, "main");
    assert_eq!(head_repo, "fork-user/repo");
    assert!(is_fork);
}

#[test]
fn test_parse_pr_view_output_non_default_base() {
    // PR targeting a release branch rather than the repo default
    let raw = "feat/my-feature|release/v2|owner/repo|false";
    let (head, base, _head_repo, is_fork) = git_helpers::parse_pr_view_output(raw).unwrap();
    assert_eq!(head, "feat/my-feature");
    assert_eq!(base, "release/v2");
    assert!(!is_fork);
}

#[test]
fn test_parse_pr_view_output_fork_headrepository_owner_null() {
    // Regression test for #1597: when headRepository.owner is null (some fork
    // PRs), the old jq expression `.headRepository.owner.login + "/" + .headRepository.name`
    // produced "/repo" (empty owner before the slash).  The fix uses
    // `.headRepositoryOwner.login` which is always populated for fork PRs.
    //
    // This test confirms that the broken output "/repo" (what the old jq would
    // emit) yields an empty fork_owner string, which validate_remote_name then
    // rejects with "fork owner name is empty".  A regression back to
    // .headRepository.owner.login would produce this broken output in production
    // and the downstream path would surface this specific error rather than
    // silently building a remote URL with an empty owner.
    let broken_output = "feat/my-feature|main|/repo|true";
    let (_head, _base, head_repo, is_fork) =
        git_helpers::parse_pr_view_output(broken_output).unwrap();
    assert_eq!(head_repo, "/repo");
    assert!(is_fork);

    // Replicate what fetch_pr_branch does to extract the fork owner.
    let fork_owner = head_repo.split('/').next().unwrap_or(&head_repo);
    assert_eq!(
        fork_owner, "",
        "broken jq output yields an empty fork owner"
    );

    let err = git_helpers::validate_remote_name(fork_owner).unwrap_err();
    assert!(
        err.to_string().contains("fork owner name is empty"),
        "expected 'fork owner name is empty', got: {err}"
    );
}

#[test]
fn test_parse_pr_view_output_bad_format() {
    let raw = "incomplete|data";
    let result = git_helpers::parse_pr_view_output(raw);
    let err = result.unwrap_err();
    assert!(
        matches!(&err, crate::error::ConductorError::GhCli(_)),
        "expected GhCli variant, got: {err:?}"
    );
    assert!(err.to_string().contains("unexpected gh pr view output"));
}

#[test]
fn test_parse_pr_view_output_empty() {
    let result = git_helpers::parse_pr_view_output("");
    assert!(result.is_err());
}

#[test]
fn test_fetch_pr_branch_fails_without_github_remote() {
    // A local-only repo has no GitHub remote, so gh pr view will fail.
    // This exercises the error path of fetch_pr_branch.
    let (_tmp, _, local) = setup_repo_with_remote();
    let result = git_helpers::fetch_pr_branch(local.to_str().unwrap(), 999);
    let err = result.unwrap_err();
    assert!(
        matches!(err, ConductorError::GhCli(_)),
        "expected GhCli error, got: {err:?}"
    );
}

#[test]
fn test_create_from_pr_propagates_fetch_error() {
    // Verify that create() with from_pr = Some(n) takes the from_pr branch,
    // calls fetch_pr_branch, and propagates the error when gh is unavailable.
    let (_tmp, _, local) = setup_repo_with_remote();
    let local_str = local.to_str().unwrap().to_string();

    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    // Point the test repo at the real local path so clone check passes
    conn.execute(
        "UPDATE repos SET local_path = ?1, workspace_dir = ?2 WHERE id = 'r1'",
        params![
            local_str,
            local.parent().unwrap().join("ws").to_str().unwrap()
        ],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    let result = mgr.create(
        "test-repo",
        "from-pr-test",
        WorktreeCreateOptions {
            from_pr: Some(42),
            ..Default::default()
        },
    );
    // fetch_pr_branch will fail because the local repo has no GitHub remote
    let err = result.unwrap_err();
    assert!(
        matches!(err, ConductorError::GhCli(_)),
        "expected GhCli error, got: {err:?}"
    );
}

#[test]
fn test_find_by_cwd_no_match() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    let result = mgr.find_by_cwd(Path::new("/tmp/other/path")).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_find_by_cwd_exact_match() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    let result = mgr.find_by_cwd(Path::new("/tmp/ws/feat-a")).unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().id, "wt1");
}

#[test]
fn test_find_by_cwd_subdirectory_match() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    let result = mgr
        .find_by_cwd(Path::new("/tmp/ws/feat-a/src/lib"))
        .unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().id, "wt1");
}

#[test]
fn test_find_by_cwd_longest_prefix_wins() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    // wt1 is a prefix of wt2's path — wt2 should win when cwd is inside wt2
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt2', 'r1', 'feat-b', 'feat/b', '/tmp/ws/feat-a/nested', 'active', '2024-01-02T00:00:00Z')",
        [],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    // cwd is inside the nested worktree — should return wt2, not wt1
    let result = mgr
        .find_by_cwd(Path::new("/tmp/ws/feat-a/nested/src"))
        .unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().id, "wt2");
}

#[test]
fn test_worktree_columns_w_derivation() {
    // Every column in WORKTREE_COLUMNS must appear in WORKTREE_COLUMNS_W
    // with the "w." prefix, in the same order, with no extra whitespace.
    let expected: String = WORKTREE_COLUMNS
        .split(',')
        .map(|col| format!("w.{}", col.trim()))
        .collect::<Vec<_>>()
        .join(", ");

    assert_eq!(*WORKTREE_COLUMNS_W, expected);
}

#[test]
fn test_get_by_ids_empty_returns_empty_vec() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    let mgr = WorktreeManager::new(&conn, &config);
    // Empty slice must not produce an `IN ()` SQL syntax error
    let result = mgr.get_by_ids(&[]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_get_by_ids_returns_matching_worktrees() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt2', 'r1', 'feat-b', 'feat/b', '/tmp/ws/feat-b', 'active', '2024-01-02T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('wt3', 'r1', 'feat-c', 'feat/c', '/tmp/ws/feat-c', 'active', '2024-01-03T00:00:00Z')",
        [],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);

    // Fetch two of the three; the third must not appear
    let mut result = mgr.get_by_ids(&["wt1", "wt2"]).unwrap();
    result.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].id, "wt1");
    assert_eq!(result[1].id, "wt2");

    // Nonexistent ID returns nothing extra
    let result = mgr.get_by_ids(&["nonexistent"]).unwrap();
    assert!(result.is_empty());
}

// -----------------------------------------------------------------------
// Worktree::belongs_to_feature() tests
// -----------------------------------------------------------------------

fn make_worktree_with_base(repo_id: &str, base_branch: Option<&str>) -> Worktree {
    Worktree {
        id: "wt-test".into(),
        repo_id: repo_id.into(),
        slug: "test-wt".into(),
        branch: "feat/child".into(),
        path: "/tmp/test".into(),
        ticket_id: None,
        status: WorktreeStatus::Active,
        created_at: "2026-01-01T00:00:00Z".into(),
        completed_at: None,
        model: None,
        base_branch: base_branch.map(String::from),
    }
}

#[test]
fn belongs_to_feature_matching_repo_and_branch() {
    let wt = make_worktree_with_base("repo1", Some("feat/parent"));
    assert!(wt.belongs_to_feature("repo1", "feat/parent"));
}

#[test]
fn belongs_to_feature_mismatched_repo() {
    let wt = make_worktree_with_base("repo1", Some("feat/parent"));
    assert!(!wt.belongs_to_feature("repo2", "feat/parent"));
}

#[test]
fn belongs_to_feature_mismatched_branch() {
    let wt = make_worktree_with_base("repo1", Some("feat/parent"));
    assert!(!wt.belongs_to_feature("repo1", "feat/other"));
}

#[test]
fn belongs_to_feature_none_base_branch() {
    let wt = make_worktree_with_base("repo1", None);
    assert!(!wt.belongs_to_feature("repo1", "feat/parent"));
}

#[test]
fn test_create_non_default_base_branch_does_not_register_feature() {
    // Since RFC-018 explicit lifecycle, creating a worktree on a non-default
    // branch should NOT auto-register a feature. Features must be created
    // explicitly via `conductor feature create`.
    let (tmp, remote, local) = setup_repo_with_remote();

    // Create a feature branch in the repo to use as a non-default base
    git(&["checkout", "-b", "feat/parent"], &local);
    let file = local.join("feature.txt");
    fs::write(&file, "feature work").unwrap();
    git(&["add", "feature.txt"], &local);
    git(&["commit", "-m", "feature commit"], &local);
    git(&["push", "-u", "origin", "feat/parent"], &local);
    git(&["checkout", "main"], &local);

    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();

    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    repo_mgr
        .register(
            "myrepo",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
            Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
        )
        .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    let (wt, _warnings) = mgr
        .create(
            "myrepo",
            "feat-child",
            WorktreeCreateOptions {
                from_branch: Some("feat/parent".to_string()),
                ..Default::default()
            },
        )
        .expect("create should succeed");

    // Worktree should have feat/parent as its base branch
    assert_eq!(wt.base_branch.as_deref(), Some("feat/parent"));

    // No feature should be auto-registered — explicit lifecycle enforced.
    let fm = crate::feature::FeatureManager::new(&conn, &config);
    let features = fm.list_active("myrepo").unwrap();
    assert!(
        features.is_empty(),
        "no feature should be auto-registered under explicit lifecycle, got: {features:?}"
    );
}

#[test]
fn test_create_default_branch_does_not_register_feature() {
    // Creating a worktree from the default branch should not register a feature.
    let (tmp, remote, local) = setup_repo_with_remote();

    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();

    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    repo_mgr
        .register(
            "myrepo",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
            Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
        )
        .unwrap();

    // Create a worktree from main (default branch)
    let mgr = WorktreeManager::new(&conn, &config);
    let (wt, _warnings) = mgr
        .create("myrepo", "feat-on-main", Default::default())
        .expect("create should succeed");

    assert!(
        wt.base_branch.is_none() || wt.base_branch.as_deref() == Some("main"),
        "expected no non-default base_branch"
    );
    let fm = crate::feature::FeatureManager::new(&conn, &config);
    let features = fm.list_active("myrepo").unwrap();
    assert!(
        features.is_empty(),
        "should not have any features for default branch, got: {features:?}"
    );
}

#[test]
fn test_set_base_branch() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    let mgr = WorktreeManager::new(&conn, &config);

    // Initially base_branch should be NULL
    let wt: Option<String> = conn
        .query_row(
            "SELECT base_branch FROM worktrees WHERE slug = 'feat-test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(wt.is_none(), "expected NULL base_branch initially");

    // Set base branch to a feature branch
    mgr.set_base_branch("test-repo", "feat-test", Some("develop"))
        .unwrap();
    let wt: Option<String> = conn
        .query_row(
            "SELECT base_branch FROM worktrees WHERE slug = 'feat-test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(wt.as_deref(), Some("develop"));

    // Clear base branch (reset to repo default)
    mgr.set_base_branch("test-repo", "feat-test", None).unwrap();
    let wt: Option<String> = conn
        .query_row(
            "SELECT base_branch FROM worktrees WHERE slug = 'feat-test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(wt.is_none(), "expected NULL after clearing base_branch");
}

#[test]
fn test_set_base_branch_not_found() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    let mgr = WorktreeManager::new(&conn, &config);

    let result = mgr.set_base_branch("test-repo", "nonexistent", Some("develop"));
    assert!(result.is_err());
    match result.unwrap_err() {
        ConductorError::WorktreeNotFound { slug } => {
            assert_eq!(slug, "nonexistent");
        }
        other => panic!("expected WorktreeNotFound, got: {other}"),
    }
}

/// Integration: deleting the last active worktree for a feature whose
/// branch is gone should auto-close the feature automatically (the
/// auto-close call is inside `delete_internal`).
#[test]
fn test_delete_then_auto_close_orphaned_feature() {
    let (_tmp, _remote, local) = setup_repo_with_remote();
    let local_str = local.to_str().unwrap();

    let conn = crate::test_helpers::create_test_conn();
    let repo_id = crate::new_id();
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
         VALUES (?1, 'test-repo', ?2, 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z')",
        rusqlite::params![repo_id, local_str],
    ).unwrap();

    // Create a feature branch, then delete it so the branch is gone
    git(&["branch", "feat/ephemeral", "main"], &local);
    git(&["branch", "-D", "feat/ephemeral"], &local);

    let feature_id = crate::new_id();
    conn.execute(
        "INSERT INTO features (id, repo_id, name, branch, base_branch, status, created_at)
         VALUES (?1, ?2, 'ephemeral', 'feat/ephemeral', 'main', 'in_progress', '2024-01-01T00:00:00Z')",
        rusqlite::params![feature_id, repo_id],
    )
    .unwrap();

    // Create a worktree record pointing at that feature branch (use a
    // fake path — delete_internal's remove_git_artifacts will just no-op)
    let wt_id = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, 'wt-eph', 'wt-eph-branch', 'feat/ephemeral', '/tmp/nonexistent-wt', 'active', '2024-01-01T00:00:00Z')",
        rusqlite::params![wt_id, repo_id],
    ).unwrap();

    let config = Config::default();
    let wt_mgr = WorktreeManager::new(&conn, &config);
    let _wt = wt_mgr.delete("test-repo", "wt-eph").unwrap();

    // The feature should now be auto-closed by delete_internal
    let status: String = conn
        .query_row(
            "SELECT status FROM features WHERE id = ?1",
            rusqlite::params![feature_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "closed",
        "feature should be auto-closed after last worktree deleted and branch gone"
    );
}

// -----------------------------------------------------------------------
// cleanup_merged_worktrees tests
// -----------------------------------------------------------------------

#[test]
fn test_cleanup_merged_worktrees_marks_merged() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    // w1 from setup_db is active with branch "feat/test"
    let mgr = WorktreeManager::new(&conn, &config);
    // Simulate merged PR: merge_check returns all branches as merged
    let count = mgr
        .cleanup_merged_worktrees_with_merge_check(
            None,
            |_, branches| branches.iter().map(|b| (b.clone(), String::new())).collect(),
            |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(count, 1);

    let status: String = conn
        .query_row("SELECT status FROM worktrees WHERE id = 'w1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "merged");

    let completed_at: Option<String> = conn
        .query_row(
            "SELECT completed_at FROM worktrees WHERE id = 'w1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(completed_at.is_some(), "completed_at should be set");
}

#[test]
fn test_cleanup_merged_worktrees_skips_unmerged() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    let mgr = WorktreeManager::new(&conn, &config);
    // merge_check returns empty — no cleanup
    let count = mgr
        .cleanup_merged_worktrees_with_merge_check(
            None,
            |_, _| std::collections::HashMap::new(),
            |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(count, 0);

    let status: String = conn
        .query_row("SELECT status FROM worktrees WHERE id = 'w1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "active");
}

#[test]
fn test_cleanup_merged_worktrees_skips_already_merged() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    // Mark w1 as already merged
    conn.execute(
        "UPDATE worktrees SET status = 'merged', completed_at = '2024-06-01T00:00:00Z' WHERE id = 'w1'",
        [],
    ).unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    // merge_check returns all branches, but w1 is already merged so should be skipped
    let count = mgr
        .cleanup_merged_worktrees_with_merge_check(
            None,
            |_, branches| branches.iter().map(|b| (b.clone(), String::new())).collect(),
            |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_cleanup_merged_worktrees_multiple_repos() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    // Add a second repo with an active worktree
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
         VALUES ('r2', 'other-repo', '/tmp/repo2', 'https://github.com/test/other.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r2', 'feat-other', 'feat/other', '/tmp/ws2/feat-other', 'active', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    // Both worktrees have merged PRs
    let count = mgr
        .cleanup_merged_worktrees_with_merge_check(
            None,
            |_, branches| branches.iter().map(|b| (b.clone(), String::new())).collect(),
            |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(count, 2);

    // Both should be merged
    let s1: String = conn
        .query_row("SELECT status FROM worktrees WHERE id = 'w1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let s2: String = conn
        .query_row("SELECT status FROM worktrees WHERE id = 'w2'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(s1, "merged");
    assert_eq!(s2, "merged");
}

/// When two sub-worktrees for the same feature branch both merge in a single
/// cleanup run, auto_ready_for_review_if_complete must fire AFTER all worktrees
/// are marked merged — otherwise it would see sibling worktrees as still active
/// and skip the transition.
#[test]
fn test_cleanup_multi_worktrees_same_feature_triggers_ready_for_review() {
    let conn = crate::test_helpers::setup_db();
    // auto_ready_for_review defaults to true in Config::default()
    let config = Config::default();

    // Insert a feature tracked on branch "feat/epic"
    conn.execute(
        "INSERT INTO features (id, repo_id, name, branch, base_branch, status, created_at) \
         VALUES ('feat1', 'r1', 'epic', 'feat/epic', 'main', 'in_progress', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    // Two sub-worktrees whose base_branch = "feat/epic" (both active initially)
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at) \
         VALUES ('sub1', 'r1', 'sub-a', 'feat/sub-a', 'feat/epic', '/tmp/sub-a', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at) \
         VALUES ('sub2', 'r1', 'sub-b', 'feat/sub-b', 'feat/epic', '/tmp/sub-b', 'active', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    // Both sub-worktrees merge in the same cleanup run (w1 from setup_db is also "merged" here
    // but it has no base_branch so it won't affect the feature check)
    let count = mgr
        .cleanup_merged_worktrees_with_merge_check(
            None,
            |_, branches| branches.iter().map(|b| (b.clone(), String::new())).collect(),
            |_, _| Ok(()),
        )
        .unwrap();
    // w1 + sub1 + sub2 all get cleaned up
    assert_eq!(count, 3);

    // Feature should have transitioned to ready_for_review because both sub-worktrees are merged
    let status: String = conn
        .query_row(
            "SELECT status FROM features WHERE id = 'feat1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "ready_for_review",
        "feature should transition to ready_for_review after all sub-worktrees merge in one run"
    );
}

/// A new worktree whose branch name matches an OLD merged PR (branch reuse)
/// must NOT be cleaned up — the merge happened before the worktree was created.
#[test]
fn test_cleanup_merged_worktrees_skips_branch_reuse_after_old_merge() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    // w1 from setup_db has created_at = '2024-01-01T00:00:00Z'.
    // Simulate a merge_check that returns mergedAt BEFORE the worktree was created.
    let count = WorktreeManager::new(&conn, &config)
        .cleanup_merged_worktrees_with_merge_check(
            None,
            |_, branches| {
                // mergedAt is 2023 — before the worktree's 2024 created_at
                branches
                    .iter()
                    .map(|b| (b.clone(), "2023-12-31T23:59:59Z".to_string()))
                    .collect()
            },
            |_, _| Ok(()),
        )
        .unwrap();

    assert_eq!(count, 0, "worktree created after old merge should not be cleaned up");

    let status: String = conn
        .query_row("SELECT status FROM worktrees WHERE id = 'w1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "active");
}

/// A worktree whose branch was merged AFTER the worktree was created (the normal
/// merge path) must still be cleaned up.
#[test]
fn test_cleanup_merged_worktrees_cleans_up_genuine_merge() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    // w1 created_at = '2024-01-01T00:00:00Z'. mergedAt = 2024-06 (after creation).
    let count = WorktreeManager::new(&conn, &config)
        .cleanup_merged_worktrees_with_merge_check(
            None,
            |_, branches| {
                branches
                    .iter()
                    .map(|b| (b.clone(), "2024-06-01T00:00:00Z".to_string()))
                    .collect()
            },
            |_, _| Ok(()),
        )
        .unwrap();

    assert_eq!(count, 1);

    let status: String = conn
        .query_row("SELECT status FROM worktrees WHERE id = 'w1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "merged");
}

// -----------------------------------------------------------------------
// label_to_branch_prefix tests
// -----------------------------------------------------------------------

#[test]
fn test_label_prefix_bug_maps_to_fix() {
    assert_eq!(manager::label_to_branch_prefix(&["bug"]), "fix");
}

#[test]
fn test_label_prefix_fix_maps_to_fix() {
    assert_eq!(manager::label_to_branch_prefix(&["fix"]), "fix");
}

#[test]
fn test_label_prefix_security_maps_to_fix() {
    assert_eq!(manager::label_to_branch_prefix(&["security"]), "fix");
}

#[test]
fn test_label_prefix_enhancement_maps_to_feat() {
    assert_eq!(manager::label_to_branch_prefix(&["enhancement"]), "feat");
}

#[test]
fn test_label_prefix_feature_maps_to_feat() {
    assert_eq!(manager::label_to_branch_prefix(&["feature"]), "feat");
}

#[test]
fn test_label_prefix_chore_maps_to_chore() {
    assert_eq!(manager::label_to_branch_prefix(&["chore"]), "chore");
}

#[test]
fn test_label_prefix_maintenance_maps_to_chore() {
    assert_eq!(manager::label_to_branch_prefix(&["maintenance"]), "chore");
}

#[test]
fn test_label_prefix_documentation_maps_to_docs() {
    assert_eq!(manager::label_to_branch_prefix(&["documentation"]), "docs");
}

#[test]
fn test_label_prefix_docs_maps_to_docs() {
    assert_eq!(manager::label_to_branch_prefix(&["docs"]), "docs");
}

#[test]
fn test_label_prefix_refactor_maps_to_refactor() {
    assert_eq!(manager::label_to_branch_prefix(&["refactor"]), "refactor");
}

#[test]
fn test_label_prefix_test_maps_to_test() {
    assert_eq!(manager::label_to_branch_prefix(&["test"]), "test");
}

#[test]
fn test_label_prefix_testing_maps_to_test() {
    assert_eq!(manager::label_to_branch_prefix(&["testing"]), "test");
}

#[test]
fn test_label_prefix_ci_maps_to_ci() {
    assert_eq!(manager::label_to_branch_prefix(&["ci"]), "ci");
}

#[test]
fn test_label_prefix_build_maps_to_ci() {
    assert_eq!(manager::label_to_branch_prefix(&["build"]), "ci");
}

#[test]
fn test_label_prefix_perf_maps_to_perf() {
    assert_eq!(manager::label_to_branch_prefix(&["perf"]), "perf");
}

#[test]
fn test_label_prefix_performance_maps_to_perf() {
    assert_eq!(manager::label_to_branch_prefix(&["performance"]), "perf");
}

#[test]
fn test_label_prefix_case_insensitive() {
    assert_eq!(manager::label_to_branch_prefix(&["Bug"]), "fix");
    assert_eq!(manager::label_to_branch_prefix(&["BUG"]), "fix");
    assert_eq!(manager::label_to_branch_prefix(&["CHORE"]), "chore");
    assert_eq!(manager::label_to_branch_prefix(&["Docs"]), "docs");
}

#[test]
fn test_label_prefix_empty_slice_falls_back_to_feat() {
    assert_eq!(manager::label_to_branch_prefix(&[]), "feat");
}

#[test]
fn test_label_prefix_unknown_label_falls_back_to_feat() {
    assert_eq!(manager::label_to_branch_prefix(&["foobar"]), "feat");
    assert_eq!(manager::label_to_branch_prefix(&["wontfix"]), "feat");
}

#[test]
fn test_label_prefix_first_match_wins() {
    // "bug" should win over "enhancement"
    assert_eq!(
        manager::label_to_branch_prefix(&["bug", "enhancement"]),
        "fix"
    );
}

// -----------------------------------------------------------------------
// WorktreeManager::create prefix normalization tests
// -----------------------------------------------------------------------

#[test]
fn test_create_chore_prefix_produces_correct_branch() {
    let (tmp, remote, local) = setup_repo_with_remote();
    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();
    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    repo_mgr
        .register(
            "myrepo",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
            Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
        )
        .unwrap();
    let mgr = WorktreeManager::new(&conn, &config);
    let (wt, _) = mgr
        .create("myrepo", "chore-123-cleanup", Default::default())
        .expect("create should succeed");
    assert_eq!(wt.slug, "chore-123-cleanup");
    assert_eq!(wt.branch, "chore/123-cleanup");
}

#[test]
fn test_create_docs_prefix_produces_correct_branch() {
    let (tmp, remote, local) = setup_repo_with_remote();
    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();
    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    repo_mgr
        .register(
            "myrepo",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
            Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
        )
        .unwrap();
    let mgr = WorktreeManager::new(&conn, &config);
    let (wt, _) = mgr
        .create("myrepo", "docs-456-readme", Default::default())
        .expect("create should succeed");
    assert_eq!(wt.slug, "docs-456-readme");
    assert_eq!(wt.branch, "docs/456-readme");
}

#[test]
fn test_create_bug_prefix_maps_to_fix_branch() {
    let (tmp, remote, local) = setup_repo_with_remote();
    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();
    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    repo_mgr
        .register(
            "myrepo",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
            Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
        )
        .unwrap();
    let mgr = WorktreeManager::new(&conn, &config);
    let (wt, _) = mgr
        .create("myrepo", "bug-789-null-crash", Default::default())
        .expect("create should succeed");
    assert_eq!(wt.slug, "bug-789-null-crash");
    assert_eq!(wt.branch, "fix/789-null-crash");
}

#[test]
fn test_create_refactor_prefix_produces_correct_branch() {
    let (tmp, remote, local) = setup_repo_with_remote();
    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();
    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    repo_mgr
        .register(
            "myrepo",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
            Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
        )
        .unwrap();
    let mgr = WorktreeManager::new(&conn, &config);
    let (wt, _) = mgr
        .create("myrepo", "refactor-10-extract-fn", Default::default())
        .expect("create should succeed");
    assert_eq!(wt.slug, "refactor-10-extract-fn");
    assert_eq!(wt.branch, "refactor/10-extract-fn");
}

#[test]
fn test_create_test_prefix_produces_correct_branch() {
    let (tmp, remote, local) = setup_repo_with_remote();
    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();
    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    repo_mgr
        .register(
            "myrepo",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
            Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
        )
        .unwrap();
    let mgr = WorktreeManager::new(&conn, &config);
    let (wt, _) = mgr
        .create("myrepo", "test-11-add-coverage", Default::default())
        .expect("create should succeed");
    assert_eq!(wt.slug, "test-11-add-coverage");
    assert_eq!(wt.branch, "test/11-add-coverage");
}

#[test]
fn test_create_ci_prefix_produces_correct_branch() {
    let (tmp, remote, local) = setup_repo_with_remote();
    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();
    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    repo_mgr
        .register(
            "myrepo",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
            Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
        )
        .unwrap();
    let mgr = WorktreeManager::new(&conn, &config);
    let (wt, _) = mgr
        .create("myrepo", "ci-12-update-actions", Default::default())
        .expect("create should succeed");
    assert_eq!(wt.slug, "ci-12-update-actions");
    assert_eq!(wt.branch, "ci/12-update-actions");
}

#[test]
fn test_create_perf_prefix_produces_correct_branch() {
    let (tmp, remote, local) = setup_repo_with_remote();
    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();
    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    repo_mgr
        .register(
            "myrepo",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
            Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
        )
        .unwrap();
    let mgr = WorktreeManager::new(&conn, &config);
    let (wt, _) = mgr
        .create("myrepo", "perf-13-cache-results", Default::default())
        .expect("create should succeed");
    assert_eq!(wt.slug, "perf-13-cache-results");
    assert_eq!(wt.branch, "perf/13-cache-results");
}

#[test]
fn test_create_release_prefix_produces_correct_branch() {
    let (tmp, remote, local) = setup_repo_with_remote();
    let conn = crate::test_helpers::setup_db();
    let mut config = Config::default();
    config.general.workspace_root = tmp.path().to_path_buf();
    let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
    repo_mgr
        .register(
            "myrepo",
            local.to_str().unwrap(),
            remote.to_str().unwrap(),
            Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
        )
        .unwrap();
    let mgr = WorktreeManager::new(&conn, &config);
    let (wt, _) = mgr
        .create("myrepo", "release-0.4.2", Default::default())
        .expect("create should succeed");
    assert_eq!(wt.slug, "release-0.4.2");
    assert_eq!(wt.branch, "release/0.4.2");
}

#[test]
fn test_cleanup_merged_worktrees_filters_by_repo() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();

    // Add a second repo with an active worktree
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
         VALUES ('r2', 'other-repo', '/tmp/repo2', 'https://github.com/test/other.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r2', 'feat-other', 'feat/other', '/tmp/ws2/feat-other', 'active', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();

    let mgr = WorktreeManager::new(&conn, &config);
    // Only clean up "other-repo"
    let count = mgr
        .cleanup_merged_worktrees_with_merge_check(
            Some("other-repo"),
            |_, branches| branches.iter().map(|b| (b.clone(), String::new())).collect(),
            |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(count, 1);

    // w1 should still be active, w2 should be merged
    let s1: String = conn
        .query_row("SELECT status FROM worktrees WHERE id = 'w1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let s2: String = conn
        .query_row("SELECT status FROM worktrees WHERE id = 'w2'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(s1, "active");
    assert_eq!(s2, "merged");
}

#[test]
fn test_delete_remote_branch_best_effort() {
    // delete_remote_branch on a nonexistent repo/branch should not panic
    git_helpers::delete_remote_branch("/nonexistent/repo/path", "feat/no-such-branch");
}

#[test]
fn test_validate_remote_name_valid() {
    assert!(git_helpers::validate_remote_name("alice").is_ok());
    assert!(git_helpers::validate_remote_name("bob-dev").is_ok());
    assert!(git_helpers::validate_remote_name("user123").is_ok());
    assert!(git_helpers::validate_remote_name("org.name").is_ok());
}

#[test]
fn test_validate_remote_name_empty() {
    let err = git_helpers::validate_remote_name("").unwrap_err();
    assert!(
        matches!(err, ConductorError::InvalidInput(_)),
        "expected InvalidInput error, got: {err:?}"
    );
    assert!(err.to_string().contains("empty"));
}

#[test]
fn test_validate_remote_name_starts_with_dash() {
    let err = git_helpers::validate_remote_name("-evil").unwrap_err();
    assert!(
        matches!(err, ConductorError::InvalidInput(_)),
        "expected InvalidInput error, got: {err:?}"
    );
    assert!(err.to_string().contains("'-'"));
}

#[test]
fn test_validate_remote_name_space() {
    let err = git_helpers::validate_remote_name("name with space").unwrap_err();
    assert!(
        matches!(err, ConductorError::InvalidInput(_)),
        "expected InvalidInput error, got: {err:?}"
    );
    assert!(err.to_string().contains("unsafe character"));
}

#[test]
fn test_validate_remote_name_path_chars() {
    // '..' is fine char-by-char, but backslash and colon are rejected
    let err = git_helpers::validate_remote_name("a\\b").unwrap_err();
    assert!(matches!(err, ConductorError::InvalidInput(_)));
    let err2 = git_helpers::validate_remote_name("a:b").unwrap_err();
    assert!(matches!(err2, ConductorError::InvalidInput(_)));
}

#[test]
fn test_validate_remote_name_null_byte() {
    let err = git_helpers::validate_remote_name("a\0b").unwrap_err();
    assert!(
        matches!(err, ConductorError::InvalidInput(_)),
        "expected InvalidInput error, got: {err:?}"
    );
    assert!(err.to_string().contains("unsafe character"));
}

#[test]
fn test_validate_branch_name_valid() {
    assert!(git_helpers::validate_branch_name("main").is_ok());
    assert!(git_helpers::validate_branch_name("feat/foo").is_ok());
    assert!(git_helpers::validate_branch_name("fix-123").is_ok());
    assert!(git_helpers::validate_branch_name("v1.2").is_ok());
}

#[test]
fn test_validate_branch_name_empty() {
    let err = git_helpers::validate_branch_name("").unwrap_err();
    assert!(
        matches!(err, ConductorError::InvalidInput(_)),
        "expected InvalidInput error, got: {err:?}"
    );
    assert!(err.to_string().contains("empty"));
}

#[test]
fn test_validate_branch_name_starts_with_dash() {
    let err = git_helpers::validate_branch_name("-q").unwrap_err();
    assert!(
        matches!(err, ConductorError::InvalidInput(_)),
        "expected InvalidInput error, got: {err:?}"
    );
    assert!(err.to_string().contains("'-'"));
    let err2 = git_helpers::validate_branch_name("--upload-pack=evil").unwrap_err();
    assert!(matches!(err2, ConductorError::InvalidInput(_)));
}

#[test]
fn test_validate_branch_name_double_dot() {
    let err = git_helpers::validate_branch_name("foo..bar").unwrap_err();
    assert!(
        matches!(err, ConductorError::InvalidInput(_)),
        "expected InvalidInput error, got: {err:?}"
    );
    assert!(err.to_string().contains("'..'"));
}

#[test]
fn test_validate_branch_name_at_brace() {
    let err = git_helpers::validate_branch_name("foo@{1}").unwrap_err();
    assert!(
        matches!(err, ConductorError::InvalidInput(_)),
        "expected InvalidInput error, got: {err:?}"
    );
    assert!(err.to_string().contains("'@{'"));
}

#[test]
fn test_validate_branch_name_space() {
    let err = git_helpers::validate_branch_name("my branch").unwrap_err();
    assert!(
        matches!(err, ConductorError::InvalidInput(_)),
        "expected InvalidInput error, got: {err:?}"
    );
    assert!(err.to_string().contains("unsafe character"));
}

#[test]
fn test_validate_branch_name_null_byte() {
    let err = git_helpers::validate_branch_name("a\0b").unwrap_err();
    assert!(
        matches!(err, ConductorError::InvalidInput(_)),
        "expected InvalidInput error, got: {err:?}"
    );
    assert!(err.to_string().contains("unsafe character"));
}

#[test]
fn test_validate_branch_name_caret() {
    let err = git_helpers::validate_branch_name("a^b").unwrap_err();
    assert!(matches!(err, ConductorError::InvalidInput(_)));
}

#[test]
fn test_validate_branch_name_colon() {
    let err = git_helpers::validate_branch_name("a:b").unwrap_err();
    assert!(matches!(err, ConductorError::InvalidInput(_)));
}

// -----------------------------------------------------------------------
// list_all_with_status() tests
// -----------------------------------------------------------------------

fn insert_agent_run(
    conn: &Connection,
    id: &str,
    worktree_id: &str,
    status: &str,
    started_at: &str,
) {
    conn.execute(
        "INSERT INTO agent_runs (id, worktree_id, status, started_at, prompt) \
         VALUES (?1, ?2, ?3, ?4, 'test prompt')",
        rusqlite::params![id, worktree_id, status, started_at],
    )
    .unwrap();
}

#[test]
fn test_list_all_with_status_no_agent_runs() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();
    let mgr = WorktreeManager::new(&conn, &config);

    let results = mgr.list_all_with_status(false).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].worktree.id, "w1");
    assert!(
        results[0].agent_status.is_none(),
        "worktree with no agent runs should have None agent_status"
    );
}

#[test]
fn test_list_all_with_status_running() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();

    insert_agent_run(&conn, "ar1", "w1", "running", "2024-01-01T10:00:00Z");

    let mgr = WorktreeManager::new(&conn, &config);
    let results = mgr.list_all_with_status(false).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].agent_status,
        Some(crate::agent::AgentRunStatus::Running)
    );
}

#[test]
fn test_list_all_with_status_waiting_for_feedback() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();

    insert_agent_run(
        &conn,
        "ar1",
        "w1",
        "waiting_for_feedback",
        "2024-01-01T10:00:00Z",
    );

    let mgr = WorktreeManager::new(&conn, &config);
    let results = mgr.list_all_with_status(false).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].agent_status,
        Some(crate::agent::AgentRunStatus::WaitingForFeedback)
    );
}

#[test]
fn test_list_all_with_status_latest_run_wins() {
    // Two runs for the same worktree — only the most recent started_at should appear.
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();

    insert_agent_run(&conn, "ar1", "w1", "completed", "2024-01-01T08:00:00Z");
    insert_agent_run(&conn, "ar2", "w1", "running", "2024-01-01T10:00:00Z");

    let mgr = WorktreeManager::new(&conn, &config);
    let results = mgr.list_all_with_status(false).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].agent_status,
        Some(crate::agent::AgentRunStatus::Running),
        "latest run (running) should win over older completed run"
    );
}

#[test]
fn test_list_all_with_status_active_only_filter() {
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();

    // Insert a completed worktree
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r1', 'feat-done', 'feat/done', '/tmp/ws/feat-done', 'merged', '2024-01-02T00:00:00Z')",
        [],
    )
    .unwrap();

    let mgr = WorktreeManager::new(&conn, &config);

    // active_only=true should exclude 'merged' worktree
    let active = mgr.list_all_with_status(true).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].worktree.id, "w1");

    // active_only=false should include both
    let all = mgr.list_all_with_status(false).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn test_list_all_with_status_duplicate_timestamp_deduplication() {
    // Two runs with identical started_at — the query must return exactly one row per worktree.
    let conn = crate::test_helpers::setup_db();
    let config = crate::config::Config::default();

    let ts = "2024-01-01T10:00:00Z";
    insert_agent_run(&conn, "ar1", "w1", "completed", ts);
    insert_agent_run(&conn, "ar2", "w1", "failed", ts);

    let mgr = WorktreeManager::new(&conn, &config);
    let results = mgr.list_all_with_status(false).unwrap();
    // Should get exactly one WorktreeWithStatus row, not two
    assert_eq!(
        results.len(),
        1,
        "duplicate started_at should not produce duplicate rows"
    );
}

#[test]
fn test_get_by_id_for_repo_happy_path() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    // setup_db seeds repo r1 and worktree w1 belonging to r1
    let mgr = WorktreeManager::new(&conn, &config);
    let wt = mgr.get_by_id_for_repo("w1", "r1").unwrap();
    assert_eq!(wt.id, "w1");
    assert_eq!(wt.repo_id, "r1");
}

#[test]
fn test_get_by_id_for_repo_not_found() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    let mgr = WorktreeManager::new(&conn, &config);
    let err = mgr.get_by_id_for_repo("nonexistent", "r1").unwrap_err();
    assert!(
        matches!(err, ConductorError::WorktreeNotFound { .. }),
        "expected WorktreeNotFound, got: {err:?}"
    );
}

#[test]
fn test_get_by_id_for_repo_cross_repo_isolation() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    // Insert a second repo
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
         VALUES ('r2', 'other-repo', '/tmp/repo2', 'https://github.com/test/repo2.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    let mgr = WorktreeManager::new(&conn, &config);
    // w1 belongs to r1 — querying it against r2 must return WorktreeNotFound
    let err = mgr.get_by_id_for_repo("w1", "r2").unwrap_err();
    assert!(
        matches!(err, ConductorError::WorktreeNotFound { .. }),
        "expected WorktreeNotFound for cross-repo access, got: {err:?}"
    );
}

#[test]
fn test_delete_by_id_for_repo_happy_path() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    let mgr = WorktreeManager::new(&conn, &config);
    // w1 belongs to r1 — delete should succeed
    let wt = mgr.delete_by_id_for_repo("w1", "r1").unwrap();
    assert_eq!(wt.id, "w1");
    // Confirm it is no longer active
    let status: String = conn
        .query_row("SELECT status FROM worktrees WHERE id = 'w1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_ne!(status, "active");
}

#[test]
fn test_delete_by_id_for_repo_cross_repo_isolation() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    // Insert a second repo
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
         VALUES ('r2', 'other-repo', '/tmp/repo2', 'https://github.com/test/repo2.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    let mgr = WorktreeManager::new(&conn, &config);
    // w1 belongs to r1 — deleting it against r2 must return WorktreeNotFound
    let err = mgr.delete_by_id_for_repo("w1", "r2").unwrap_err();
    assert!(
        matches!(err, ConductorError::WorktreeNotFound { .. }),
        "expected WorktreeNotFound for cross-repo delete, got: {err:?}"
    );
    // w1 must still exist (not deleted)
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM worktrees WHERE id = 'w1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "w1 should not have been deleted");
}

// -----------------------------------------------------------------------
// get_by_id_enriched / get_by_id_for_repo_enriched / list_by_repo_id_enriched tests
// -----------------------------------------------------------------------

fn insert_ticket(
    conn: &Connection,
    id: &str,
    repo_id: &str,
    title: &str,
    source_id: &str,
    url: &str,
) {
    conn.execute(
        "INSERT INTO tickets \
         (id, repo_id, source_type, source_id, title, url, synced_at) \
         VALUES (?1, ?2, 'github', ?3, ?4, ?5, '2024-01-01T00:00:00Z')",
        rusqlite::params![id, repo_id, source_id, title, url],
    )
    .unwrap();
}

fn link_ticket(conn: &Connection, worktree_id: &str, ticket_id: &str) {
    conn.execute(
        "UPDATE worktrees SET ticket_id = ?1 WHERE id = ?2",
        rusqlite::params![ticket_id, worktree_id],
    )
    .unwrap();
}

#[test]
fn test_get_by_id_enriched_no_ticket_no_agent() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    let mgr = WorktreeManager::new(&conn, &config);

    let result = mgr.get_by_id_enriched("w1").unwrap();
    assert_eq!(result.worktree.id, "w1");
    assert!(result.agent_status.is_none());
    assert!(result.ticket_title.is_none());
    assert!(result.ticket_number.is_none());
}

#[test]
fn test_get_by_id_enriched_with_ticket() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    insert_ticket(&conn, "t1", "r1", "Fix the bug", "42", "");
    link_ticket(&conn, "w1", "t1");

    let mgr = WorktreeManager::new(&conn, &config);
    let result = mgr.get_by_id_enriched("w1").unwrap();
    assert_eq!(result.ticket_title.as_deref(), Some("Fix the bug"));
    assert_eq!(result.ticket_number.as_deref(), Some("42"));
}

#[test]
fn test_get_by_id_enriched_with_agent_run() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    insert_agent_run(&conn, "ar1", "w1", "running", "2024-01-01T10:00:00Z");

    let mgr = WorktreeManager::new(&conn, &config);
    let result = mgr.get_by_id_enriched("w1").unwrap();
    assert_eq!(
        result.agent_status,
        Some(crate::agent::AgentRunStatus::Running)
    );
}

#[test]
fn test_get_by_id_enriched_not_found() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    let mgr = WorktreeManager::new(&conn, &config);
    let err = mgr.get_by_id_enriched("nonexistent").unwrap_err();
    assert!(
        matches!(err, ConductorError::WorktreeNotFound { .. }),
        "expected WorktreeNotFound, got: {err:?}"
    );
}

#[test]
fn test_get_by_id_for_repo_enriched_no_ticket_no_agent() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    let mgr = WorktreeManager::new(&conn, &config);

    let result = mgr.get_by_id_for_repo_enriched("w1", "r1").unwrap();
    assert_eq!(result.worktree.id, "w1");
    assert!(result.agent_status.is_none());
    assert!(result.ticket_title.is_none());
}

#[test]
fn test_get_by_id_for_repo_enriched_with_ticket_and_agent() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    insert_ticket(&conn, "t1", "r1", "Implement feature", "99", "");
    link_ticket(&conn, "w1", "t1");
    insert_agent_run(&conn, "ar1", "w1", "completed", "2024-01-01T10:00:00Z");

    let mgr = WorktreeManager::new(&conn, &config);
    let result = mgr.get_by_id_for_repo_enriched("w1", "r1").unwrap();
    assert_eq!(result.ticket_title.as_deref(), Some("Implement feature"));
    assert_eq!(result.ticket_number.as_deref(), Some("99"));
    assert_eq!(
        result.agent_status,
        Some(crate::agent::AgentRunStatus::Completed)
    );
}

#[test]
fn test_get_by_id_for_repo_enriched_cross_repo_isolation() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
         VALUES ('r2', 'other-repo', '/tmp/repo2', 'https://github.com/test/repo2.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    let mgr = WorktreeManager::new(&conn, &config);
    let err = mgr.get_by_id_for_repo_enriched("w1", "r2").unwrap_err();
    assert!(
        matches!(err, ConductorError::WorktreeNotFound { .. }),
        "expected WorktreeNotFound for cross-repo access, got: {err:?}"
    );
}

#[test]
fn test_list_by_repo_id_enriched_no_ticket_no_agent() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    let mgr = WorktreeManager::new(&conn, &config);

    let results = mgr.list_by_repo_id_enriched("r1", false).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].worktree.id, "w1");
    assert!(results[0].agent_status.is_none());
    assert!(results[0].ticket_title.is_none());
}

#[test]
fn test_list_by_repo_id_enriched_with_ticket_and_agent() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    insert_ticket(&conn, "t1", "r1", "Do the thing", "7", "");
    link_ticket(&conn, "w1", "t1");
    insert_agent_run(&conn, "ar1", "w1", "running", "2024-01-01T10:00:00Z");

    let mgr = WorktreeManager::new(&conn, &config);
    let results = mgr.list_by_repo_id_enriched("r1", false).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ticket_title.as_deref(), Some("Do the thing"));
    assert_eq!(results[0].ticket_number.as_deref(), Some("7"));
    assert_eq!(
        results[0].agent_status,
        Some(crate::agent::AgentRunStatus::Running)
    );
}

#[test]
fn test_ticket_url_populated_in_enriched_response() {
    let conn = crate::test_helpers::setup_db();
    let config = Config::default();
    insert_ticket(
        &conn,
        "t1",
        "r1",
        "Fix the bug",
        "42",
        "https://github.com/owner/repo/issues/42",
    );
    link_ticket(&conn, "w1", "t1");

    let mgr = WorktreeManager::new(&conn, &config);

    // get_by_id_enriched
    let result = mgr.get_by_id_enriched("w1").unwrap();
    assert_eq!(
        result.ticket_url.as_deref(),
        Some("https://github.com/owner/repo/issues/42")
    );

    // list_by_repo_id_enriched
    let results = mgr.list_by_repo_id_enriched("r1", false).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].ticket_url.as_deref(),
        Some("https://github.com/owner/repo/issues/42")
    );
}

#[test]
fn test_resolve_and_update_base_prefix_fallback() {
    let (_tmp, remote, local) = setup_repo_with_remote();

    // Create a branch with the feat/ prefix on the remote
    let (_tmp2, other) = setup_second_clone(&remote);
    git(&["checkout", "-b", "feat/user-auth"], &other);
    let file = other.join("feature.txt");
    fs::write(&file, "new feature").unwrap();
    git(&["add", "feature.txt"], &other);
    git(&["commit", "-m", "Add user auth feature"], &other);
    git(&["push", "-u", "origin", "feat/user-auth"], &other);

    // Fetch the new branch in local so it's available for tracking
    git(&["fetch", "origin"], &local);

    // Try to resolve "user-auth" (without prefix) - should find "feat/user-auth"
    let result = git_helpers::resolve_and_update_base(
        local.to_str().unwrap(),
        Some("user-auth"),
        "main",
        false,
        false,
    );
    assert!(
        result.is_ok(),
        "resolve_and_update_base should succeed: {:?}",
        result.err()
    );
    let (resolved_branch, _warnings) = result.unwrap();
    assert_eq!(
        resolved_branch, "feat/user-auth",
        "should resolve to feat/ prefixed branch"
    );
}

#[test]
fn test_resolve_and_update_base_no_prefix_fallback_when_already_prefixed() {
    let (_tmp, _remote, local) = setup_repo_with_remote();

    // Request a branch that already has a prefix but doesn't exist
    // Should NOT try alternatives and should fail
    let result = git_helpers::resolve_and_update_base(
        local.to_str().unwrap(),
        Some("feat/nonexistent"),
        "main",
        false,
        false,
    );
    assert!(
        result.is_err(),
        "should fail when prefixed branch doesn't exist"
    );
}

#[test]
fn test_ensure_base_up_to_date_creates_local_tracking_branch_from_remote_only() {
    let (_tmp, remote, local) = setup_repo_with_remote();

    // Create a new branch on the remote only
    let (_tmp2, other) = setup_second_clone(&remote);
    git(&["checkout", "-b", "new-feature"], &other);
    let file = other.join("remote_only.txt");
    fs::write(&file, "remote only content").unwrap();
    git(&["add", "remote_only.txt"], &other);
    git(&["commit", "-m", "remote only feature"], &other);
    git(&["push", "-u", "origin", "new-feature"], &other);

    // Verify the branch doesn't exist locally yet
    assert!(!git_helpers::branch_exists(
        local.to_str().unwrap(),
        "new-feature"
    ));

    // Call ensure_base_up_to_date on the remote-only branch
    // This should create a local tracking branch
    let result =
        git_helpers::ensure_base_up_to_date(local.to_str().unwrap(), "new-feature", false, false);
    assert!(
        result.is_ok(),
        "ensure_base_up_to_date should succeed: {:?}",
        result.err()
    );

    // Verify the local tracking branch was created
    assert!(git_helpers::branch_exists(
        local.to_str().unwrap(),
        "new-feature"
    ));

    // Verify the branch tracks the remote by checking git branch -vv output
    let track_output = std::process::Command::new("git")
        .args(["branch", "-vv"])
        .current_dir(&local)
        .output()
        .unwrap();
    let track_info = String::from_utf8_lossy(&track_output.stdout);
    assert!(
        track_info.contains("new-feature"),
        "branch should exist in branch list"
    );
    assert!(
        track_info.contains("origin/new-feature"),
        "branch should track origin/new-feature"
    );
}
