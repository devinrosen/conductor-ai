use chrono::Utc;
use rusqlite::{params, Connection};

use super::helpers::derive_branch_name;
use super::*;
use crate::config::Config;
use crate::db::migrations;
use crate::db::with_in_clause;
use crate::error::ConductorError;

fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    migrations::run(&conn).unwrap();
    conn
}

fn insert_repo(conn: &Connection) -> String {
    let id = crate::new_id();
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
         VALUES (?1, 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z')",
        params![id],
    ).unwrap();
    id
}

fn insert_feature(conn: &Connection, repo_id: &str, name: &str, branch: &str) -> String {
    let id = crate::new_id();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO features (id, repo_id, name, branch, base_branch, status, created_at)
         VALUES (?1, ?2, ?3, ?4, 'main', 'in_progress', ?5)",
        params![id, repo_id, name, branch, now],
    )
    .unwrap();
    id
}

fn insert_ticket(conn: &Connection, repo_id: &str, source_id: &str) -> String {
    let id = crate::new_id();
    conn.execute(
        "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json)
         VALUES (?1, ?2, 'github', ?3, 'Test ticket', '', 'open', '', 'https://example.com', '2024-01-01T00:00:00Z', '{}')",
        params![id, repo_id, source_id],
    ).unwrap();
    id
}

#[test]
fn test_create_feature_duplicate_via_manager() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let _repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    // First create succeeds
    let feature = mgr
        .create("test-repo", "notif-improvements", None, None, None, &[])
        .unwrap();
    assert_eq!(feature.name, "notif-improvements");

    // Second create with the same name should return FeatureAlreadyExists
    let err = mgr
        .create("test-repo", "notif-improvements", None, None, None, &[])
        .unwrap_err();
    assert!(
        matches!(err, ConductorError::FeatureAlreadyExists { .. }),
        "expected FeatureAlreadyExists, got: {err:?}"
    );
}

#[test]
fn test_list_features() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let feat_a_id = insert_feature(&conn, &repo_id, "feature-a", "feat/feature-a");
    insert_feature(&conn, &repo_id, "feature-b", "feat/feature-b");

    // Create a worktree record whose base_branch matches feature-a's branch
    let wt_id = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, created_at)
         VALUES (?1, ?2, 'wt-a', 'wt-branch', 'feat/feature-a', '/tmp/wt', '2024-01-01T00:00:00Z')",
        params![wt_id, repo_id],
    )
    .unwrap();

    // Link a ticket to feature-a
    let ticket_id = insert_ticket(&conn, &repo_id, "42");
    conn.execute(
        "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        params![feat_a_id, ticket_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let features = mgr.list("test-repo").unwrap();
    assert_eq!(features.len(), 2);

    // Features are ordered by created_at DESC, so feature-b is first
    let feat_a = features.iter().find(|f| f.name == "feature-a").unwrap();
    let feat_b = features.iter().find(|f| f.name == "feature-b").unwrap();
    assert_eq!(feat_a.worktree_count, 1);
    assert_eq!(feat_a.ticket_count, 1);
    assert_eq!(feat_b.worktree_count, 0);
    assert_eq!(feat_b.ticket_count, 0);
}

#[test]
fn test_list_active_filters_by_status() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    insert_feature(&conn, &repo_id, "active-feat", "feat/active-feat");
    let closed_id = insert_feature(&conn, &repo_id, "closed-feat", "feat/closed-feat");
    // Mark one feature as closed.
    conn.execute(
        "UPDATE features SET status = 'closed' WHERE id = ?1",
        params![closed_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    // list() returns both; list_active() returns only the active one.
    let all = mgr.list("test-repo").unwrap();
    assert_eq!(all.len(), 2);

    let active = mgr.list_active("test-repo").unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].name, "active-feat");
    assert_eq!(active[0].status, FeatureStatus::InProgress);
}

#[test]
fn test_list_all_active_groups_by_repo() {
    let conn = setup_db();
    let repo_id_a = insert_repo(&conn);
    // Insert a second repo.
    let repo_id_b = crate::new_id();
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
         VALUES (?1, 'second-repo', '/tmp/repo2', 'https://github.com/test/repo2.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
        params![repo_id_b],
    ).unwrap();

    let feat_a1_id = insert_feature(&conn, &repo_id_a, "feat-a1", "feat/a1");
    insert_feature(&conn, &repo_id_a, "feat-a2", "feat/a2");
    insert_feature(&conn, &repo_id_b, "feat-b1", "feat/b1");

    // Mark feat-a2 as closed — should be excluded.
    conn.execute(
        "UPDATE features SET status = 'closed' WHERE name = 'feat-a2'",
        params![],
    )
    .unwrap();

    // Insert a worktree under feat-a1 (base_branch matches feature branch).
    let wt_id = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, 'wt-a1', 'feat/a1-impl', ?3, '/tmp/wt', 'active', '2024-01-02T00:00:00Z')",
        params![wt_id, repo_id_a, "feat/a1"],
    )
    .unwrap();

    // Link a ticket to feat-a1 via feature_tickets.
    let ticket_id = insert_ticket(&conn, &repo_id_a, "42");
    conn.execute(
        "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        params![feat_a1_id, ticket_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let map = mgr.list_all_active().unwrap();

    // repo_a has 1 active feature (feat-a1), repo_b has 1 (feat-b1).
    assert_eq!(map.get(&repo_id_a).map(|v| v.len()), Some(1));
    let feat_a1 = &map.get(&repo_id_a).unwrap()[0];
    assert_eq!(feat_a1.name, "feat-a1");
    assert_eq!(feat_a1.worktree_count, 1);
    assert_eq!(feat_a1.ticket_count, 1);

    assert_eq!(map.get(&repo_id_b).map(|v| v.len()), Some(1));
    let feat_b1 = &map.get(&repo_id_b).unwrap()[0];
    assert_eq!(feat_b1.name, "feat-b1");
    assert_eq!(feat_b1.worktree_count, 0);
    assert_eq!(feat_b1.ticket_count, 0);
}

#[test]
fn test_link_unlink_tickets_via_manager() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let feature_id = insert_feature(&conn, &repo_id, "notif", "feat/notif");
    let _ticket_id_a = insert_ticket(&conn, &repo_id, "100");
    let _ticket_id_b = insert_ticket(&conn, &repo_id, "101");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    // Link via manager (using source_ids)
    mgr.link_tickets("test-repo", "notif", &["100".into(), "101".into()])
        .unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM feature_tickets WHERE feature_id = ?1",
            params![feature_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);

    // Unlink one via manager
    mgr.unlink_tickets("test-repo", "notif", &["100".into()])
        .unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM feature_tickets WHERE feature_id = ?1",
            params![feature_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn test_resolve_ticket_not_found() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let _feature_id = insert_feature(&conn, &repo_id, "notif", "feat/notif");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr.link_tickets("test-repo", "notif", &["999".into()]);
    assert!(matches!(result, Err(ConductorError::TicketNotFound { .. })));
}

/// Create a temp git repo with "origin" remote (bare) and a default "main" branch.
/// Returns (repo_dir, bare_dir) as TempDir handles (drop cleans up).
fn setup_git_repo() -> (tempfile::TempDir, tempfile::TempDir) {
    use std::process::Command;

    let bare = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "--bare"])
        .current_dir(bare.path())
        .output()
        .unwrap();

    let work = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init"])
        .current_dir(work.path())
        .output()
        .unwrap();
    // Configure user for commits
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(work.path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(work.path())
        .output()
        .unwrap();
    // Create initial commit on main
    Command::new("git")
        .args(["checkout", "-b", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::fs::write(work.path().join("README"), "init").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(work.path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(work.path())
        .output()
        .unwrap();
    // Add bare as origin and push
    Command::new("git")
        .args(["remote", "add", "origin", bare.path().to_str().unwrap()])
        .current_dir(work.path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["push", "-u", "origin", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();

    (work, bare)
}

fn insert_repo_at(conn: &Connection, local_path: &str) -> String {
    let id = crate::new_id();
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
         VALUES (?1, 'test-repo', ?2, 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z')",
        params![id, local_path],
    ).unwrap();
    id
}

#[test]
fn test_close_feature_sets_closed_status() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    // Create a feature branch with an extra commit NOT merged into main
    std::process::Command::new("git")
        .args(["checkout", "-b", "feat/done-feature", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::fs::write(work.path().join("unmerged.txt"), "unmerged work").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "unmerged commit"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["push", "origin", "feat/done-feature"])
        .current_dir(work.path())
        .output()
        .unwrap();
    // Switch back to main
    std::process::Command::new("git")
        .args(["checkout", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();

    insert_feature(&conn, &repo_id, "done-feature", "feat/done-feature");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.close("test-repo", "done-feature").unwrap();

    let f = mgr.get_by_name("test-repo", "done-feature").unwrap();
    assert_eq!(f.status, FeatureStatus::Closed);
    assert!(f.merged_at.is_none());
}

#[test]
fn test_close_feature_sets_merged_status() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    // Create a feature branch, make a commit, merge it into main, push both
    std::process::Command::new("git")
        .args(["checkout", "-b", "feat/merged-feature", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::fs::write(work.path().join("feature.txt"), "feature work").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "feature commit"])
        .current_dir(work.path())
        .output()
        .unwrap();
    // Merge into main
    std::process::Command::new("git")
        .args(["checkout", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["merge", "--no-ff", "feat/merged-feature", "-m", "merge"])
        .current_dir(work.path())
        .output()
        .unwrap();
    // Push both branches
    std::process::Command::new("git")
        .args(["push", "origin", "main", "feat/merged-feature"])
        .current_dir(work.path())
        .output()
        .unwrap();

    insert_feature(&conn, &repo_id, "merged-feature", "feat/merged-feature");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.close("test-repo", "merged-feature").unwrap();

    let f = mgr.get_by_name("test-repo", "merged-feature").unwrap();
    assert_eq!(f.status, FeatureStatus::Merged);
    assert!(f.merged_at.is_some());
}

#[test]
fn test_feature_not_found() {
    let conn = setup_db();
    let _repo_id = insert_repo(&conn);

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let result = mgr.get_by_name("test-repo", "nonexistent");
    assert!(matches!(
        result,
        Err(ConductorError::FeatureNotFound { .. })
    ));
}

#[test]
fn test_create_feature_happy_path() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let _repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let feature = mgr
        .create("test-repo", "my-feature", None, None, None, &[])
        .unwrap();

    assert_eq!(feature.name, "my-feature");
    assert_eq!(feature.branch, "feat/my-feature");
    assert_eq!(feature.base_branch, "main");
    assert!(matches!(feature.status, FeatureStatus::InProgress));
    assert!(feature.merged_at.is_none());

    // Verify the branch exists in git
    let output = std::process::Command::new("git")
        .args(["branch", "--list", "feat/my-feature"])
        .current_dir(work.path())
        .output()
        .unwrap();
    let branches = String::from_utf8_lossy(&output.stdout);
    assert!(
        branches.contains("feat/my-feature"),
        "branch should exist in git"
    );

    // Verify DB record via get_by_name
    let fetched = mgr.get_by_name("test-repo", "my-feature").unwrap();
    assert_eq!(fetched.id, feature.id);
}

#[test]
fn test_create_feature_with_custom_base_branch() {
    let (work, _bare) = setup_git_repo();

    // Create a "develop" branch and push it so it can be used as base
    std::process::Command::new("git")
        .args(["branch", "develop", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["push", "origin", "develop"])
        .current_dir(work.path())
        .output()
        .unwrap();

    let conn = setup_db();
    let _repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let feature = mgr
        .create("test-repo", "custom-base", Some("develop"), None, None, &[])
        .unwrap();

    assert_eq!(feature.name, "custom-base");
    assert_eq!(feature.branch, "feat/custom-base");
    assert_eq!(feature.base_branch, "develop");

    // Verify the branch was created from develop
    let output = std::process::Command::new("git")
        .args(["branch", "--list", "feat/custom-base"])
        .current_dir(work.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("feat/custom-base"));
}

#[test]
fn test_create_feature_with_ticket_source_ids() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    // Pre-create tickets with known source_ids
    let ticket_a = insert_ticket(&conn, &repo_id, "42");
    let ticket_b = insert_ticket(&conn, &repo_id, "43");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let feature = mgr
        .create(
            "test-repo",
            "with-tickets",
            None,
            None,
            None,
            &["42".into(), "43".into()],
        )
        .unwrap();

    // Verify tickets were linked
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM feature_tickets WHERE feature_id = ?1",
            params![feature.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);

    // Verify the correct tickets were linked
    let linked: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT ticket_id FROM feature_tickets WHERE feature_id = ?1 ORDER BY ticket_id",
            )
            .unwrap();
        stmt.query_map(params![feature.id], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let mut expected = vec![ticket_a, ticket_b];
    expected.sort();
    assert_eq!(linked, expected);
}

#[test]
fn test_close_feature_merged_when_remote_branch_deleted() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    // Create a feature branch, commit, merge into main, push main, then delete the remote branch
    std::process::Command::new("git")
        .args(["checkout", "-b", "feat/auto-deleted", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::fs::write(work.path().join("ad.txt"), "work").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "feature work"])
        .current_dir(work.path())
        .output()
        .unwrap();
    // Merge into main
    std::process::Command::new("git")
        .args(["checkout", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["merge", "--no-ff", "feat/auto-deleted", "-m", "merge"])
        .current_dir(work.path())
        .output()
        .unwrap();
    // Push main only (simulate remote branch auto-deletion)
    std::process::Command::new("git")
        .args(["push", "origin", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();

    insert_feature(&conn, &repo_id, "auto-deleted", "feat/auto-deleted");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.close("test-repo", "auto-deleted").unwrap();

    let f = mgr.get_by_name("test-repo", "auto-deleted").unwrap();
    assert_eq!(
        f.status,
        FeatureStatus::Merged,
        "should detect merge via local fallback when remote branch is deleted"
    );
    assert!(f.merged_at.is_some());
}

#[test]
fn test_with_in_clause_generates_valid_sql() {
    // Single item
    let repo1: &str = "repo1";
    let (sql, _) = with_in_clause(
        "SELECT id FROM t WHERE repo_id = ?1 AND source_id IN",
        &[&repo1 as &dyn rusqlite::types::ToSql],
        &["a".to_string()],
        |sql, params| (sql.to_string(), params.len()),
    );
    assert_eq!(
        sql,
        "SELECT id FROM t WHERE repo_id = ?1 AND source_id IN (?2)"
    );

    // Multiple items
    let f1: &str = "f1";
    let (sql, param_count) = with_in_clause(
        "DELETE FROM ft WHERE fid = ?1 AND tid IN",
        &[&f1 as &dyn rusqlite::types::ToSql],
        &["a".to_string(), "b".to_string(), "c".to_string()],
        |sql, params| (sql.to_string(), params.len()),
    );
    assert_eq!(sql, "DELETE FROM ft WHERE fid = ?1 AND tid IN (?2, ?3, ?4)");
    assert_eq!(param_count, 4); // leading_param + 3 items
}

#[test]
fn test_create_pr_feature_not_found() {
    let conn = setup_db();
    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let err = mgr.create_pr("test-repo", "nonexistent", false);
    assert!(err.is_err(), "create_pr should fail for missing feature");
}

#[test]
fn test_create_pr_gh_failure() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());
    insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    // gh pr create will fail because there's no GitHub remote configured,
    // exercising the non-zero exit / GhCli error path
    let result = mgr.create_pr("test-repo", "my-feat", false);
    assert!(result.is_err(), "create_pr should fail when gh errors");
    let err_msg = format!("{}", result.unwrap_err());
    // Should be a GhCli error, not a generic git error
    assert!(
        err_msg.contains("gh") || err_msg.contains("Gh"),
        "error should reference gh CLI: {err_msg}"
    );
}

#[test]
fn test_create_pr_draft_flag() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());
    insert_feature(&conn, &repo_id, "draft-feat", "feat/draft-feat");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    // With draft=true, gh will also fail (no remote) but exercises the draft code path
    let result = mgr.create_pr("test-repo", "draft-feat", true);
    assert!(result.is_err());
}

#[test]
fn test_create_feature_cleans_up_branches_on_db_failure() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let _repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    // Add a trigger that makes INSERT INTO features fail, simulating a DB
    // error after git branch + push have already succeeded.
    conn.execute_batch(
        "CREATE TRIGGER fail_feature_insert BEFORE INSERT ON features
         BEGIN SELECT RAISE(ABORT, 'simulated DB failure'); END;",
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr.create("test-repo", "cleanup-test", None, None, None, &[]);
    assert!(result.is_err(), "create should fail due to trigger");

    // Verify the local branch was cleaned up
    let output = std::process::Command::new("git")
        .args(["branch", "--list", "feat/cleanup-test"])
        .current_dir(work.path())
        .output()
        .unwrap();
    let branches = String::from_utf8_lossy(&output.stdout);
    assert!(
        !branches.contains("feat/cleanup-test"),
        "local branch should be cleaned up after DB failure"
    );

    // Verify the remote branch was cleaned up
    let output = std::process::Command::new("git")
        .args(["ls-remote", "--heads", "origin", "feat/cleanup-test"])
        .current_dir(work.path())
        .output()
        .unwrap();
    let remote_refs = String::from_utf8_lossy(&output.stdout);
    assert!(
        !remote_refs.contains("feat/cleanup-test"),
        "remote branch should be cleaned up after DB failure"
    );
}

#[test]
fn test_branch_name_derivation() {
    // Simple name gets feat/ prefix
    assert_eq!(
        derive_branch_name("notification-improvements"),
        "feat/notification-improvements"
    );

    // Name with slash is used as-is
    assert_eq!(derive_branch_name("release/2.0"), "release/2.0");
}

#[test]
fn test_find_feature_for_ticket_none() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let ticket_id = insert_ticket(&conn, &repo_id, "100");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let result = mgr.find_feature_for_ticket(&ticket_id).unwrap();
    assert!(result.is_none(), "no feature linked to ticket");
}

#[test]
fn test_find_feature_for_ticket_found() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let ticket_id = insert_ticket(&conn, &repo_id, "200");
    let feature_id = insert_feature(&conn, &repo_id, "notif", "feat/notif");

    // Link ticket to feature
    conn.execute(
        "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        params![feature_id, ticket_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let result = mgr.find_feature_for_ticket(&ticket_id).unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().name, "notif");
}

#[test]
fn test_find_feature_for_ticket_skips_closed() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let ticket_id = insert_ticket(&conn, &repo_id, "300");
    let feature_id = insert_feature(&conn, &repo_id, "closed-feat", "feat/closed-feat");

    // Close the feature
    conn.execute(
        "UPDATE features SET status = 'closed' WHERE id = ?1",
        params![feature_id],
    )
    .unwrap();

    // Link ticket
    conn.execute(
        "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        params![feature_id, ticket_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let result = mgr.find_feature_for_ticket(&ticket_id).unwrap();
    assert!(result.is_none(), "closed feature should not be returned");
}

#[test]
fn test_find_feature_for_ticket_ambiguous() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let ticket_id = insert_ticket(&conn, &repo_id, "400");
    let feat_a = insert_feature(&conn, &repo_id, "feat-a", "feat/feat-a");
    let feat_b = insert_feature(&conn, &repo_id, "feat-b", "feat/feat-b");

    // Link ticket to both features
    conn.execute(
        "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        params![feat_a, ticket_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        params![feat_b, ticket_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let result = mgr.find_feature_for_ticket(&ticket_id);
    assert!(result.is_err(), "should error when ambiguous");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("specify which feature"),
        "error should mention disambiguation: {err_msg}"
    );
}

#[test]
fn test_get_by_id() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let result = mgr.get_by_id(&feature_id).unwrap();
    assert_eq!(result.name, "my-feat");
    assert_eq!(result.id, feature_id);
}

#[test]
fn test_resolve_active_feature_returns_active() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let f = mgr.resolve_active_feature("test-repo", "my-feat").unwrap();
    assert_eq!(f.name, "my-feat");
    assert_eq!(f.status, FeatureStatus::InProgress);
}

// -----------------------------------------------------------------------
// resolve_feature_id_for_run tests (4 code paths)
// -----------------------------------------------------------------------

#[test]
fn test_resolve_feature_id_for_run_none_inputs() {
    let conn = setup_db();
    let _repo_id = insert_repo(&conn);
    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    // No feature name, no ticket, no worktree → Ok(None)
    let result = mgr
        .resolve_feature_id_for_run(None, None, None, None)
        .unwrap();
    assert!(result.is_none());
}

#[test]
fn test_resolve_feature_id_for_run_explicit_name() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");
    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr
        .resolve_feature_id_for_run(Some("my-feat"), Some("test-repo"), None, None)
        .unwrap();
    assert_eq!(result, Some(feature_id));
}

// -----------------------------------------------------------------------
// transition() tests
// -----------------------------------------------------------------------

#[test]
fn test_transition_in_progress_to_ready_for_review() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let feature = mgr
        .transition("test-repo", "my-feat", FeatureStatus::ReadyForReview)
        .unwrap();
    assert_eq!(feature.status, FeatureStatus::ReadyForReview);
}

#[test]
fn test_transition_ready_for_review_to_approved() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");
    conn.execute(
        "UPDATE features SET status = 'ready_for_review' WHERE id = ?1",
        params![feature_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let feature = mgr
        .transition("test-repo", "my-feat", FeatureStatus::Approved)
        .unwrap();
    assert_eq!(feature.status, FeatureStatus::Approved);
}

#[test]
fn test_transition_invalid_in_progress_to_approved() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let err = mgr
        .transition("test-repo", "my-feat", FeatureStatus::Approved)
        .unwrap_err();
    assert!(
        matches!(err, ConductorError::InvalidFeatureTransition { .. }),
        "expected InvalidFeatureTransition, got: {err:?}"
    );
}

#[test]
fn test_transition_any_to_closed_succeeds() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let _repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    // Create a feature via the manager (so the branch exists)
    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.create("test-repo", "close-feat", None, None, None, &[])
        .unwrap();

    // Transition from in_progress → closed should be allowed
    let feature = mgr
        .transition("test-repo", "close-feat", FeatureStatus::Closed)
        .unwrap();
    assert!(
        feature.status == FeatureStatus::Closed || feature.status == FeatureStatus::Merged,
        "expected Closed or Merged, got: {:?}",
        feature.status
    );
}

// -----------------------------------------------------------------------
// auto_ready_for_review_if_complete() tests
// -----------------------------------------------------------------------

fn insert_worktree_for_feature(
    conn: &Connection,
    repo_id: &str,
    slug: &str,
    base_branch: &str,
    status: &str,
) {
    let id = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, '/tmp/wt', ?6, '2024-01-01T00:00:00Z')",
        rusqlite::params![
            id,
            repo_id,
            slug,
            format!("{slug}-branch"),
            base_branch,
            status
        ],
    )
    .unwrap();
}

#[test]
fn test_auto_ready_for_review_with_active_worktrees_no_transition() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

    // One active worktree remains
    insert_worktree_for_feature(&conn, &repo_id, "wt-active", "feat/my-feat", "active");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.auto_ready_for_review_if_complete(&repo_id, "feat/my-feat")
        .unwrap();

    let status: String = conn
        .query_row(
            "SELECT status FROM features WHERE id = ?1",
            params![feature_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "in_progress",
        "feature should remain in_progress while worktrees are active"
    );
}

#[test]
fn test_auto_ready_for_review_no_active_worktrees_transitions() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

    // Only merged worktree
    insert_worktree_for_feature(&conn, &repo_id, "wt-merged", "feat/my-feat", "merged");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.auto_ready_for_review_if_complete(&repo_id, "feat/my-feat")
        .unwrap();

    let status: String = conn
        .query_row(
            "SELECT status FROM features WHERE id = ?1",
            params![feature_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "ready_for_review",
        "feature should transition to ready_for_review when all worktrees merged"
    );
}

#[test]
fn test_auto_ready_for_review_no_feature_is_noop() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    // Should return Ok(()) even when no feature exists for this branch
    mgr.auto_ready_for_review_if_complete(&repo_id, "feat/nonexistent")
        .unwrap();
}

#[test]
fn test_resolve_feature_id_for_run_explicit_name_no_repo_errors() {
    let conn = setup_db();
    let _repo_id = insert_repo(&conn);
    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    // Feature name without repo context should error
    let err = mgr
        .resolve_feature_id_for_run(Some("my-feat"), None, None, None)
        .unwrap_err();
    assert!(
        matches!(err, ConductorError::Workflow(ref msg) if msg.contains("requires a repo context")),
        "expected Workflow error about repo context, got: {err:?}"
    );
}

#[test]
fn test_resolve_feature_id_for_run_explicit_name_via_ticket_repo() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");
    let ticket_id = insert_ticket(&conn, &repo_id, "77");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    // feature_name provided, repo_slug absent, ticket_id used to derive the repo
    let result = mgr
        .resolve_feature_id_for_run(Some("my-feat"), None, Some(&ticket_id), None)
        .unwrap();
    assert_eq!(result, Some(feature_id));
}

#[test]
fn test_resolve_feature_id_for_run_via_ticket() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");
    let ticket_id = insert_ticket(&conn, &repo_id, "42");
    conn.execute(
        "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        params![feature_id, ticket_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr
        .resolve_feature_id_for_run(None, None, Some(&ticket_id), None)
        .unwrap();
    assert_eq!(result, Some(feature_id));
}

#[test]
fn test_resolve_feature_id_for_run_via_worktree() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");
    let ticket_id = insert_ticket(&conn, &repo_id, "99");
    conn.execute(
        "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        params![feature_id, ticket_id],
    )
    .unwrap();
    // Create a worktree linked to the ticket
    let wt_id = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, ticket_id, created_at)
         VALUES (?1, ?2, 'wt-slug', 'wt-branch', 'main', '/tmp/wt', ?3, '2024-01-01T00:00:00Z')",
        params![wt_id, repo_id, ticket_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr
        .resolve_feature_id_for_run(None, Some("test-repo"), None, Some("wt-slug"))
        .unwrap();
    assert_eq!(result, Some(feature_id));
}

#[test]
fn test_resolve_feature_id_for_run_worktree_no_ticket() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    // Create a worktree with no linked ticket (ticket_id is NULL)
    let wt_id = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, created_at)
         VALUES (?1, ?2, 'wt-no-ticket', 'feat/no-ticket', 'main', '/tmp/wt', '2024-01-01T00:00:00Z')",
        params![wt_id, repo_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    // Should return Ok(None) — no ticket means no feature can be resolved
    let result = mgr
        .resolve_feature_id_for_run(None, Some("test-repo"), None, Some("wt-no-ticket"))
        .unwrap();
    assert_eq!(result, None);
}

// -----------------------------------------------------------------------
// branch_to_feature_name tests
// -----------------------------------------------------------------------

#[test]
fn test_branch_to_feature_name_strips_feat_prefix() {
    assert_eq!(
        branch_to_feature_name("feat/notification-improvements"),
        "notification-improvements"
    );
}

#[test]
fn test_branch_to_feature_name_strips_fix_prefix() {
    assert_eq!(
        branch_to_feature_name("fix/crash-on-startup"),
        "crash-on-startup"
    );
}

#[test]
fn test_branch_to_feature_name_leaves_other_prefixes() {
    assert_eq!(branch_to_feature_name("release/2.0"), "release/2.0");
}

#[test]
fn test_branch_to_feature_name_passthrough_no_prefix() {
    assert_eq!(branch_to_feature_name("my-branch"), "my-branch");
}

// -----------------------------------------------------------------------
// ensure_feature_for_branch tests
// -----------------------------------------------------------------------

fn make_repo(id: &str) -> crate::repo::Repo {
    make_repo_at(id, "/tmp/repo")
}

fn make_repo_at(id: &str, local_path: &str) -> crate::repo::Repo {
    crate::repo::Repo {
        id: id.to_string(),
        slug: "test-repo".to_string(),
        local_path: local_path.to_string(),
        remote_url: "https://github.com/test/repo.git".to_string(),
        default_branch: "main".to_string(),
        workspace_dir: "/tmp/ws".to_string(),
        created_at: "2024-01-01T00:00:00Z".to_string(),
        model: None,
        allow_agent_issue_creation: false,
    }
}

#[test]
fn test_ensure_feature_for_branch_creates_feature() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let repo = make_repo(&repo_id);
    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr
        .ensure_feature_for_branch(&repo, "feat/notifications", None)
        .unwrap();
    assert!(result.is_some(), "should create a new feature");
    let feature = result.unwrap();
    assert_eq!(feature.name, "notifications");
    assert_eq!(feature.branch, "feat/notifications");
    assert_eq!(feature.base_branch, "main"); // fallback to default
    assert_eq!(feature.status, FeatureStatus::InProgress);
}

#[test]
fn test_ensure_feature_for_branch_noop_when_exists() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let repo = make_repo(&repo_id);
    insert_feature(&conn, &repo_id, "notifications", "feat/notifications");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr
        .ensure_feature_for_branch(&repo, "feat/notifications", None)
        .unwrap();
    assert!(
        result.is_none(),
        "should be no-op when feature already exists"
    );
}

#[test]
fn test_ensure_feature_for_branch_noop_for_default_branch() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let repo = make_repo(&repo_id);
    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr.ensure_feature_for_branch(&repo, "main", None).unwrap();
    assert!(result.is_none(), "should be no-op for default branch");
}

#[test]
fn test_ensure_feature_for_branch_disambiguates_name() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let repo = make_repo(&repo_id);
    // Insert a feature with the name "notifications" but on a DIFFERENT branch
    // (e.g. it was closed/merged and a new branch was created with the same prefix).
    insert_feature(&conn, &repo_id, "notifications", "feat/notifications-old");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr
        .ensure_feature_for_branch(&repo, "feat/notifications", None)
        .unwrap();
    assert!(result.is_some());
    let feature = result.unwrap();
    assert_eq!(
        feature.name, "notifications-2",
        "should disambiguate with suffix"
    );
    assert_eq!(feature.branch, "feat/notifications");
}

#[test]
fn test_ensure_feature_for_branch_disambiguates_chained_suffix() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let repo = make_repo(&repo_id);
    // Pre-insert both "notifications" and "notifications-2" on different branches
    insert_feature(&conn, &repo_id, "notifications", "feat/notifications-old");
    insert_feature(&conn, &repo_id, "notifications-2", "feat/notifications-v2");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr
        .ensure_feature_for_branch(&repo, "feat/notifications", None)
        .unwrap();
    assert!(result.is_some());
    let feature = result.unwrap();
    assert_eq!(
        feature.name, "notifications-3",
        "should skip taken suffixes and use the next available one"
    );
    assert_eq!(feature.branch, "feat/notifications");
}

#[test]
fn test_ensure_feature_for_branch_reactivates_closed_feature() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let repo = make_repo(&repo_id);

    // Insert a feature with name "notifications" but mark it as merged (non-active).
    let feat_id = insert_feature(&conn, &repo_id, "notifications", "feat/notifications-old");
    conn.execute(
        "UPDATE features SET status = 'merged' WHERE id = ?1",
        params![feat_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr
        .ensure_feature_for_branch(&repo, "feat/notifications", None)
        .unwrap();
    assert!(result.is_some());
    let feature = result.unwrap();
    assert_eq!(
        feature.name, "notifications",
        "should reuse the name by reactivating the closed feature"
    );
    assert_eq!(feature.branch, "feat/notifications");
    assert_eq!(feature.status, FeatureStatus::InProgress);
    assert_eq!(feature.id, feat_id, "should reactivate the same record");
}

#[test]
fn test_ensure_feature_for_branch_uses_supplied_base_branch() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let repo = make_repo(&repo_id);

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr
        .ensure_feature_for_branch(&repo, "feat/notifications", Some("develop"))
        .unwrap();
    assert!(result.is_some());
    let feature = result.unwrap();
    assert_eq!(
        feature.base_branch, "develop",
        "should use caller-supplied base_branch"
    );
}

#[test]
fn test_ensure_feature_for_branch_defaults_to_repo_default_branch() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let repo = make_repo(&repo_id);

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let result = mgr
        .ensure_feature_for_branch(&repo, "feat/notifications", None)
        .unwrap();
    assert!(result.is_some());
    let feature = result.unwrap();
    assert_eq!(
        feature.base_branch, "main",
        "should fall back to repo default_branch when base_branch is None"
    );
}

// -----------------------------------------------------------------------
// list_unregistered_branches tests
// -----------------------------------------------------------------------

#[test]
fn test_list_unregistered_branches() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);

    // Create an active worktree whose branch is NOT a registered feature
    let wt_id = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, 'wt-orphan', 'feat/orphan', 'main', '/tmp/wt', 'active', '2024-01-01T00:00:00Z')",
        params![wt_id, repo_id],
    ).unwrap();

    // Create a worktree whose branch IS a registered feature (should NOT appear)
    insert_feature(&conn, &repo_id, "registered", "feat/registered");
    let wt_id2 = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, 'wt-reg', 'feat/registered', 'main', '/tmp/wt2', 'active', '2024-01-01T00:00:00Z')",
        params![wt_id2, repo_id],
    ).unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let orphans = mgr.list_unregistered_branches(&repo_id, "main").unwrap();

    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].branch, "feat/orphan");
    assert_eq!(orphans[0].worktree_count, 1);
    assert_eq!(orphans[0].base_branch.as_deref(), Some("main"));
}

#[test]
fn test_list_unregistered_branches_excludes_non_active_worktrees() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);

    // Create a merged worktree — should NOT appear
    let wt_id = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, 'wt-done', 'feat/done', 'main', '/tmp/wt-done', 'merged', '2024-01-01T00:00:00Z')",
        params![wt_id, repo_id],
    ).unwrap();

    // Create an abandoned worktree — should NOT appear
    let wt_id2 = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, 'wt-del', 'feat/abandoned', 'main', '/tmp/wt-del', 'abandoned', '2024-01-01T00:00:00Z')",
        params![wt_id2, repo_id],
    ).unwrap();

    // Create an active worktree — SHOULD appear
    let wt_id3 = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, 'wt-act', 'feat/active-orphan', 'main', '/tmp/wt-act', 'active', '2024-01-01T00:00:00Z')",
        params![wt_id3, repo_id],
    ).unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let orphans = mgr.list_unregistered_branches(&repo_id, "main").unwrap();

    // Only the active worktree's branch should be returned
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].branch, "feat/active-orphan");
    assert_eq!(orphans[0].worktree_count, 1);
    assert_eq!(orphans[0].base_branch.as_deref(), Some("main"));
}

#[test]
fn test_list_unregistered_branches_excludes_default_branch() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);

    // Create an active worktree on the default branch — should NOT appear
    let wt_id = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, 'wt-main', 'main', 'main', '/tmp/wt-main', 'active', '2024-01-01T00:00:00Z')",
        params![wt_id, repo_id],
    ).unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let orphans = mgr.list_unregistered_branches(&repo_id, "main").unwrap();

    assert!(orphans.is_empty());
}

// -----------------------------------------------------------------------
// auto_close_if_orphaned tests
// -----------------------------------------------------------------------

#[test]
fn test_auto_close_no_feature_is_noop() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let repo = make_repo(&repo_id);

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    // No feature exists for this branch — should succeed silently
    mgr.auto_close_if_orphaned(&repo, "feat/nonexistent")
        .unwrap();
}

#[test]
fn test_auto_close_skips_when_active_worktrees_remain() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let repo = make_repo(&repo_id);
    insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

    // Insert an active worktree targeting this feature's branch
    let wt_id = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, 'wt-a', 'wt-branch', 'feat/my-feat', '/tmp/wt', 'active', '2024-01-01T00:00:00Z')",
        params![wt_id, repo_id],
    ).unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.auto_close_if_orphaned(&repo, "feat/my-feat").unwrap();

    // Feature should still be active
    let f = mgr.get_by_name("test-repo", "my-feat").unwrap();
    assert_eq!(f.status, FeatureStatus::InProgress);
}

#[test]
fn test_auto_close_skips_already_closed_feature() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let repo = make_repo(&repo_id);
    let fid = insert_feature(&conn, &repo_id, "done-feat", "feat/done-feat");
    conn.execute(
        "UPDATE features SET status = 'closed' WHERE id = ?1",
        params![fid],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    // Should be a no-op since the feature is already closed
    mgr.auto_close_if_orphaned(&repo, "feat/done-feat").unwrap();
}

#[test]
fn test_auto_close_closes_orphaned_feature() {
    // Use a real git repo so we can control branch existence
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    // Create a feature branch, then delete it so local_branch_exists returns false
    std::process::Command::new("git")
        .args(["checkout", "-b", "feat/orphaned", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["checkout", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["branch", "-D", "feat/orphaned"])
        .current_dir(work.path())
        .output()
        .unwrap();

    insert_feature(&conn, &repo_id, "orphaned", "feat/orphaned");

    let repo = make_repo_at(&repo_id, work.path().to_str().unwrap());
    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.auto_close_if_orphaned(&repo, "feat/orphaned").unwrap();

    // Feature should now be closed (not merged, since the branch was never merged)
    let f = mgr.get_by_name("test-repo", "orphaned").unwrap();
    assert_eq!(f.status, FeatureStatus::Closed);
}

#[test]
fn test_auto_close_skips_when_branch_still_exists() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    // Create a feature branch but do NOT delete it
    std::process::Command::new("git")
        .args(["branch", "feat/still-here", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();

    insert_feature(&conn, &repo_id, "still-here", "feat/still-here");

    let repo = make_repo_at(&repo_id, work.path().to_str().unwrap());
    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.auto_close_if_orphaned(&repo, "feat/still-here")
        .unwrap();

    // Feature should remain active because the branch still exists
    let f = mgr.get_by_name("test-repo", "still-here").unwrap();
    assert_eq!(f.status, FeatureStatus::InProgress);
}

#[test]
fn test_auto_close_only_counts_active_worktrees() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    // Delete the branch so it doesn't exist locally
    std::process::Command::new("git")
        .args(["branch", "feat/has-merged-wt", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["branch", "-D", "feat/has-merged-wt"])
        .current_dir(work.path())
        .output()
        .unwrap();

    insert_feature(&conn, &repo_id, "has-merged-wt", "feat/has-merged-wt");

    // Insert a merged (non-active) worktree — should not prevent auto-close
    let wt_id = crate::new_id();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, 'wt-done', 'wt-branch', 'feat/has-merged-wt', '/tmp/wt', 'merged', '2024-01-01T00:00:00Z')",
        params![wt_id, repo_id],
    ).unwrap();

    let repo = make_repo_at(&repo_id, work.path().to_str().unwrap());
    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.auto_close_if_orphaned(&repo, "feat/has-merged-wt")
        .unwrap();

    // Feature should be closed — only merged worktrees remain (not active)
    let f = mgr.get_by_name("test-repo", "has-merged-wt").unwrap();
    assert_eq!(f.status, FeatureStatus::Closed);
}

#[test]
fn test_auto_close_after_worktree_delete_skips_default_branch() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    // Create a feature whose branch matches the repo's default branch ("main")
    insert_feature(&conn, &repo_id, "main-feat", "main");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    // base_branch == "main" == default_branch → should be a no-op
    mgr.auto_close_after_worktree_delete(&repo_id, Some("main"))
        .unwrap();

    // Feature should remain active
    let f = mgr.get_by_name("test-repo", "main-feat").unwrap();
    assert_eq!(f.status, FeatureStatus::InProgress);
}

/// Regression: FEATURE_ROW_FRAGMENT wt_count subquery must only count
/// active worktrees. Non-active (merged/abandoned) worktrees should not
/// inflate the count.
#[test]
fn test_feature_row_wt_count_ignores_non_active_worktrees() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    insert_feature(&conn, &repo_id, "counted", "feat/counted");

    // Insert one active worktree
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES ('wt-a', ?1, 'wt-active', 'wt-branch-a', 'feat/counted', '/tmp/wt-a', 'active', '2024-01-01T00:00:00Z')",
        params![repo_id],
    ).unwrap();
    // Insert one merged worktree (should NOT be counted)
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES ('wt-m', ?1, 'wt-merged', 'wt-branch-m', 'feat/counted', '/tmp/wt-m', 'merged', '2024-01-01T00:00:00Z')",
        params![repo_id],
    ).unwrap();
    // Insert one abandoned worktree (should NOT be counted)
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES ('wt-x', ?1, 'wt-abandoned', 'wt-branch-x', 'feat/counted', '/tmp/wt-x', 'abandoned', '2024-01-01T00:00:00Z')",
        params![repo_id],
    ).unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let rows = mgr.list("test-repo").unwrap();
    let row = rows.iter().find(|r| r.branch == "feat/counted").unwrap();
    assert_eq!(
        row.worktree_count, 1,
        "wt_count should only count active worktrees, got {}",
        row.worktree_count
    );
}

#[test]
fn test_resolve_active_feature_rejects_closed() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let fid = insert_feature(&conn, &repo_id, "done-feat", "feat/done-feat");
    conn.execute(
        "UPDATE features SET status = 'closed' WHERE id = ?1",
        params![fid],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let err = mgr
        .resolve_active_feature("test-repo", "done-feat")
        .unwrap_err();
    assert!(
        matches!(err, ConductorError::Workflow(ref msg) if msg.contains("only in-progress features")),
        "expected Workflow error about in-progress features, got: {err:?}"
    );
}

// -----------------------------------------------------------------------
// Staleness detection tests
// -----------------------------------------------------------------------

fn make_feature_row(last_commit_at: Option<&str>, last_wt_activity: Option<&str>) -> FeatureRow {
    FeatureRow {
        id: "test-id".to_string(),
        name: "test-feature".to_string(),
        branch: "feat/test".to_string(),
        base_branch: "main".to_string(),
        status: FeatureStatus::InProgress,
        created_at: "2024-01-01T00:00:00Z".to_string(),
        worktree_count: 0,
        ticket_count: 0,
        last_commit_at: last_commit_at.map(|s| s.to_string()),
        last_worktree_activity: last_wt_activity.map(|s| s.to_string()),
    }
}

#[test]
fn test_is_stale_within_threshold() {
    let recent = Utc::now().to_rfc3339();
    let row = make_feature_row(Some(&recent), None);
    assert!(!FeatureManager::is_stale(&row, 14));
}

#[test]
fn test_is_stale_past_threshold() {
    let old = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    let row = make_feature_row(Some(&old), None);
    assert!(FeatureManager::is_stale(&row, 14));
}

#[test]
fn test_is_stale_no_data() {
    let row = make_feature_row(None, None);
    assert!(FeatureManager::is_stale(&row, 14));
}

#[test]
fn test_is_stale_disabled() {
    let old = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    let row = make_feature_row(Some(&old), None);
    assert!(!FeatureManager::is_stale(&row, 0));
}

#[test]
fn test_stale_days_calculation() {
    let ten_days_ago = (Utc::now() - chrono::Duration::days(10)).to_rfc3339();
    let row = make_feature_row(Some(&ten_days_ago), None);
    let days = FeatureManager::stale_days(&row).unwrap();
    assert!((9..=11).contains(&days), "expected ~10 days, got {days}");
}

#[test]
fn test_stale_days_no_data() {
    let row = make_feature_row(None, None);
    assert!(FeatureManager::stale_days(&row).is_none());
}

#[test]
fn test_last_worktree_activity_prevents_stale() {
    let old_commit = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    let recent_wt = Utc::now().to_rfc3339();
    let row = make_feature_row(Some(&old_commit), Some(&recent_wt));
    assert!(!FeatureManager::is_stale(&row, 14));
}

#[test]
fn test_is_stale_non_active_feature() {
    let old = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    let mut row = make_feature_row(Some(&old), None);
    row.status = FeatureStatus::Closed;
    assert!(!FeatureManager::is_stale(&row, 14));
}

#[test]
fn test_last_commit_at_in_feature_row_query() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let feat_id = insert_feature(&conn, &repo_id, "stale-test", "feat/stale-test");

    // Set last_commit_at manually
    let ts = "2024-06-15T12:00:00+00:00";
    conn.execute(
        "UPDATE features SET last_commit_at = ?1 WHERE id = ?2",
        params![ts, feat_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let features = mgr.list("test-repo").unwrap();
    let f = features.iter().find(|f| f.name == "stale-test").unwrap();
    assert_eq!(f.last_commit_at.as_deref(), Some(ts));
}

#[test]
fn test_last_worktree_activity_in_feature_row_query() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let _feat_id = insert_feature(&conn, &repo_id, "wt-activity", "feat/wt-activity");

    // Insert a worktree targeting the feature branch
    let wt_id = crate::new_id();
    let wt_created = "2024-08-20T10:00:00Z";
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, status, created_at)
         VALUES (?1, ?2, 'wt-act', 'feat/wt-activity-impl', 'feat/wt-activity', '/tmp/wt', 'active', ?3)",
        params![wt_id, repo_id, wt_created],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let features = mgr.list("test-repo").unwrap();
    let f = features.iter().find(|f| f.name == "wt-activity").unwrap();
    assert_eq!(f.last_worktree_activity.as_deref(), Some(wt_created));
}

// ---------------------------------------------------------------------------
// delete() tests
// ---------------------------------------------------------------------------

#[test]
fn test_delete_active_feature_rejected() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let err = mgr.delete("test-repo", "my-feat").unwrap_err();
    assert!(
        matches!(err, ConductorError::FeatureStillActive { .. }),
        "expected FeatureStillActive, got: {err:?}"
    );
}

#[test]
fn test_delete_nonexistent_feature() {
    let conn = setup_db();
    let _repo_id = insert_repo(&conn);

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let err = mgr.delete("test-repo", "does-not-exist").unwrap_err();
    assert!(
        matches!(err, ConductorError::FeatureNotFound { .. }),
        "expected FeatureNotFound, got: {err:?}"
    );
}

#[test]
fn test_delete_closed_feature_removes_row_and_tickets() {
    // Use a real git repo so the git subprocess can run.
    // The branch "feat/done-feat" is never created, so git outputs
    // "error: branch 'feat/done-feat' not found" which is treated as a no-op.
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());
    let feature_id = insert_feature(&conn, &repo_id, "done-feat", "feat/done-feat");

    // Mark as closed
    conn.execute(
        "UPDATE features SET status = 'closed' WHERE id = ?1",
        params![feature_id],
    )
    .unwrap();

    // Link a ticket
    let ticket_id = insert_ticket(&conn, &repo_id, "99");
    conn.execute(
        "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        params![feature_id, ticket_id],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    // Branch doesn't exist locally → treated as no-op; DB deletions still happen.
    mgr.delete("test-repo", "done-feat").unwrap();

    // Feature row gone
    let feature_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM features WHERE id = ?1",
            params![feature_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(feature_count, 0, "feature row should be deleted");

    // feature_tickets rows gone
    let ft_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM feature_tickets WHERE feature_id = ?1",
            params![feature_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ft_count, 0, "feature_tickets rows should be deleted");

    // Underlying ticket row should still exist
    let ticket_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tickets WHERE id = ?1",
            params![ticket_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ticket_count, 1, "ticket row should be preserved");
}

#[test]
fn test_delete_with_git_branch() {
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    // Create a local branch that is fully merged (so -d succeeds)
    std::process::Command::new("git")
        .args(["branch", "feat/del-feat", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();

    insert_feature(&conn, &repo_id, "del-feat", "feat/del-feat");
    // Mark as closed
    conn.execute(
        "UPDATE features SET status = 'closed' WHERE name = ?1",
        params!["del-feat"],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.delete("test-repo", "del-feat").unwrap();

    // Verify branch is gone
    let output = std::process::Command::new("git")
        .args(["branch", "--list", "feat/del-feat"])
        .current_dir(work.path())
        .output()
        .unwrap();
    let branches = String::from_utf8_lossy(&output.stdout);
    assert!(
        !branches.contains("feat/del-feat"),
        "branch should have been deleted"
    );

    // Verify DB record is gone
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM features WHERE name = 'del-feat'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "feature row should be gone");
}

#[test]
fn test_delete_unmerged_branch_returns_git_error() {
    // An unmerged branch causes `git branch -d` to fail with a message that
    // does NOT contain "not found" or "no branch named", so the manager must
    // propagate a ConductorError::Git instead of treating it as a no-op.
    let (work, _bare) = setup_git_repo();
    let conn = setup_db();
    let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

    // Create a branch with an unmerged commit so `git branch -d` will refuse.
    std::process::Command::new("git")
        .args(["checkout", "-b", "feat/unmerged-feat", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::fs::write(work.path().join("unmerged.txt"), "unmerged work").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(work.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "unmerged commit"])
        .current_dir(work.path())
        .output()
        .unwrap();
    // Switch back to main so we can attempt to delete the feature branch.
    std::process::Command::new("git")
        .args(["checkout", "main"])
        .current_dir(work.path())
        .output()
        .unwrap();

    insert_feature(&conn, &repo_id, "unmerged-feat", "feat/unmerged-feat");
    // Mark as closed so the active-feature guard doesn't fire.
    conn.execute(
        "UPDATE features SET status = 'closed' WHERE name = ?1",
        params!["unmerged-feat"],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);

    let err = mgr.delete("test-repo", "unmerged-feat").unwrap_err();
    assert!(
        matches!(err, ConductorError::Git(_)),
        "expected ConductorError::Git for unmerged branch, got: {err:?}"
    );
}

#[test]
fn test_insert_feature_record_new_fields_round_trip() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let now = Utc::now().to_rfc3339();

    let id = crate::new_id();
    let feature = Feature {
        id: id.clone(),
        repo_id: repo_id.clone(),
        name: "milestone-feature".to_string(),
        branch: "feat/milestone-feature".to_string(),
        base_branch: "main".to_string(),
        status: FeatureStatus::InProgress,
        created_at: now.clone(),
        merged_at: None,
        source_type: Some("github".to_string()),
        source_id: Some("github.com/owner/repo/milestones/1".to_string()),
        tickets_total: 5,
        tickets_merged: 3,
    };

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    mgr.insert_feature_record(&feature).unwrap();

    let fetched = mgr.get_by_id(&id).unwrap();
    assert_eq!(fetched.source_type, Some("github".to_string()));
    assert_eq!(
        fetched.source_id,
        Some("github.com/owner/repo/milestones/1".to_string())
    );
    assert_eq!(fetched.tickets_total, 5);
    assert_eq!(fetched.tickets_merged, 3);
    assert_eq!(fetched.status, FeatureStatus::InProgress);
}

#[test]
fn test_map_feature_row_negative_ticket_count_returns_error() {
    let conn = setup_db();
    let repo_id = insert_repo(&conn);
    let now = Utc::now().to_rfc3339();
    let id = crate::new_id();

    // Insert a row with a negative tickets_total to simulate corrupt/unexpected DB data.
    conn.execute(
        "INSERT INTO features (id, repo_id, name, branch, base_branch, status, created_at, tickets_total, tickets_merged)
         VALUES (?1, ?2, 'neg-feat', 'feat/neg-feat', 'main', 'in_progress', ?3, -1, 0)",
        params![id, repo_id, now],
    )
    .unwrap();

    let config = Config::default();
    let mgr = FeatureManager::new(&conn, &config);
    let result = mgr.get_by_id(&id);
    assert!(
        result.is_err(),
        "expected error when tickets_total is negative, got: {result:?}"
    );
}
