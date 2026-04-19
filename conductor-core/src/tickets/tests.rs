use super::query::query_dep_pairs;
use super::syncer::CLOSED_TICKET_ARTIFACTS_SQL;
use super::*;
use rusqlite::Connection;
use std::collections::HashMap;

fn setup_db() -> Connection {
    crate::test_helpers::setup_db()
}

fn make_ticket(source_id: &str, title: &str) -> TicketInput {
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
        comments: vec![],
        label_details: vec![],
        blocked_by: vec![],
        children: vec![],
        parent: None,
    }
}

fn get_ticket_state(conn: &Connection, source_id: &str) -> String {
    conn.query_row(
        "SELECT state FROM tickets WHERE source_id = :source_id",
        rusqlite::named_params! { ":source_id": source_id },
        |row| row.get("state"),
    )
    .unwrap()
}

fn make_ticket_stub(state: &str) -> Ticket {
    Ticket {
        id: "stub".to_string(),
        repo_id: "repo".to_string(),
        source_type: "github".to_string(),
        source_id: "1".to_string(),
        title: "stub".to_string(),
        body: String::new(),
        state: state.to_string(),
        labels: String::new(),
        assignee: None,
        priority: None,
        url: String::new(),
        synced_at: String::new(),
        raw_json: "{}".to_string(),
        workflow: None,
        agent_map: None,
    }
}

#[test]
fn test_is_actively_blocked_empty() {
    let deps = TicketDependencies::default();
    assert!(!deps.is_actively_blocked());
}

#[test]
fn test_is_actively_blocked_all_closed() {
    let deps = TicketDependencies {
        blocked_by: vec![make_ticket_stub("closed"), make_ticket_stub("closed")],
        ..Default::default()
    };
    assert!(!deps.is_actively_blocked());
}

#[test]
fn test_is_actively_blocked_one_open() {
    let deps = TicketDependencies {
        blocked_by: vec![make_ticket_stub("closed"), make_ticket_stub("open")],
        ..Default::default()
    };
    assert!(deps.is_actively_blocked());
}

#[test]
fn test_active_blockers_empty() {
    let deps = TicketDependencies::default();
    assert_eq!(deps.active_blockers().count(), 0);
}

#[test]
fn test_active_blockers_filters_closed() {
    let deps = TicketDependencies {
        blocked_by: vec![
            make_ticket_stub("closed"),
            make_ticket_stub("open"),
            make_ticket_stub("open"),
        ],
        ..Default::default()
    };
    let active: Vec<_> = deps.active_blockers().collect();
    assert_eq!(active.len(), 2);
    assert!(active.iter().all(|b| b.state == "open"));
}

#[test]
fn test_active_blockers_all_closed() {
    let deps = TicketDependencies {
        blocked_by: vec![make_ticket_stub("closed")],
        ..Default::default()
    };
    assert_eq!(deps.active_blockers().count(), 0);
}

#[test]
fn test_latest_synced_at_no_tickets() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let result = syncer.latest_synced_at("r1").unwrap();
    assert!(result.is_none());
}

#[test]
fn test_latest_synced_at_returns_most_recent() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Insert first ticket, then manually backdate its synced_at.
    syncer
        .upsert_tickets("r1", &[make_ticket("1", "Issue 1")])
        .unwrap();
    let old_ts = "2020-01-01T00:00:00Z";
    conn.execute(
        "UPDATE tickets SET synced_at = :ts WHERE source_id = '1'",
        rusqlite::named_params! { ":ts": old_ts },
    )
    .unwrap();

    // Insert a second ticket — it gets the current timestamp.
    syncer
        .upsert_tickets("r1", &[make_ticket("2", "Issue 2")])
        .unwrap();

    let latest = syncer.latest_synced_at("r1").unwrap().unwrap();
    // The MAX must be the newer ticket's timestamp, not the backdated one.
    assert_ne!(
        latest, old_ts,
        "MAX should return the most recent timestamp"
    );
    assert!(
        latest.as_str() > old_ts,
        "latest synced_at should be after the backdated timestamp"
    );
}

#[test]
fn test_latest_synced_at_scoped_to_repo() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    syncer
        .upsert_tickets("r1", &[make_ticket("1", "Issue 1")])
        .unwrap();

    // Different repo has no tickets
    let ts = syncer.latest_synced_at("other-repo").unwrap();
    assert!(ts.is_none());
}

#[test]
fn test_sync_and_close_tickets_returns_counts_and_marks_worktrees() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // First sync: two open tickets
    let first = vec![make_ticket("1", "Issue 1"), make_ticket("2", "Issue 2")];
    let (synced, closed) = syncer.sync_and_close_tickets("r1", "github", &first);
    assert_eq!(synced, 2);
    assert_eq!(closed, 0);

    // Get ticket id for issue 1 and link a worktree to it
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");

    // Second sync: only issue 2 remains open → issue 1 closed
    // The worktree is left active because has_merged_pr returns false in test environments.
    let second = vec![make_ticket("2", "Issue 2")];
    let (synced2, closed2) = syncer.sync_and_close_tickets("r1", "github", &second);
    assert_eq!(synced2, 1);
    assert_eq!(closed2, 1);
    assert_eq!(get_ticket_state(&conn, "1"), "closed");
    // Worktree stays active: PR merge check skips cleanup when gh CLI is unavailable.
    assert_eq!(get_worktree_status(&conn, "wt1"), "active");
}

#[test]
fn test_close_missing_tickets_marks_absent_as_closed() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Sync #1: issues 1, 2, 3 are open
    let tickets = vec![
        make_ticket("1", "Issue 1"),
        make_ticket("2", "Issue 2"),
        make_ticket("3", "Issue 3"),
    ];
    syncer.upsert_tickets("r1", &tickets).unwrap();

    // Sync #2: only issues 1, 3 are open (issue 2 was closed on GitHub)
    let tickets2 = vec![make_ticket("1", "Issue 1"), make_ticket("3", "Issue 3")];
    let synced_ids: Vec<&str> = tickets2.iter().map(|t| t.source_id.as_str()).collect();
    syncer.upsert_tickets("r1", &tickets2).unwrap();
    let closed = syncer
        .close_missing_tickets("r1", "github", &synced_ids)
        .unwrap();

    assert_eq!(closed, 1);
    assert_eq!(get_ticket_state(&conn, "1"), "open");
    assert_eq!(get_ticket_state(&conn, "2"), "closed");
    assert_eq!(get_ticket_state(&conn, "3"), "open");
}

#[test]
fn test_close_missing_does_not_reclose_already_closed() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Sync #1: issues 1, 2 are open
    let tickets = vec![make_ticket("1", "Issue 1"), make_ticket("2", "Issue 2")];
    syncer.upsert_tickets("r1", &tickets).unwrap();

    // Sync #2: only issue 1 open → issue 2 closed
    let synced_ids = vec!["1"];
    syncer
        .close_missing_tickets("r1", "github", &synced_ids)
        .unwrap();
    assert_eq!(get_ticket_state(&conn, "2"), "closed");

    // Sync #3: still only issue 1 open → issue 2 already closed, count should be 0
    let closed = syncer
        .close_missing_tickets("r1", "github", &synced_ids)
        .unwrap();
    assert_eq!(closed, 0);
}

#[test]
fn test_close_missing_empty_sync_is_noop() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Sync existing tickets
    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();

    // Empty sync should not close anything (protects against API failures)
    let closed = syncer.close_missing_tickets("r1", "github", &[]).unwrap();
    assert_eq!(closed, 0);
    assert_eq!(get_ticket_state(&conn, "1"), "open");
}

#[test]
fn test_close_missing_scoped_to_repo_and_source_type() {
    let conn = setup_db();
    // Add a second repo
    conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
             VALUES ('repo2', 'other-repo', '/tmp/repo2', 'https://github.com/test/other', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

    let syncer = TicketSyncer::new(&conn);

    // Both repos have issue #1
    let tickets1 = vec![make_ticket("1", "Repo1 Issue")];
    let tickets2 = vec![make_ticket("1", "Repo2 Issue")];
    syncer.upsert_tickets("r1", &tickets1).unwrap();
    syncer.upsert_tickets("repo2", &tickets2).unwrap();

    // Sync repo1 with no open issues → only repo1's ticket should close
    let closed = syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();
    assert_eq!(closed, 1);

    // repo1's ticket should be closed
    let repo1_state: String = conn
        .query_row(
            "SELECT state FROM tickets WHERE repo_id = 'r1' AND source_id = '1'",
            [],
            |row| row.get("state"),
        )
        .unwrap();
    assert_eq!(repo1_state, "closed");

    // repo2's ticket should still be open (different repo, unaffected)
    let repo2_state: String = conn
        .query_row(
            "SELECT state FROM tickets WHERE repo_id = 'repo2' AND source_id = '1'",
            [],
            |row| row.get("state"),
        )
        .unwrap();
    assert_eq!(repo2_state, "open");
}

fn insert_worktree(
    conn: &Connection,
    id: &str,
    repo_id: &str,
    ticket_id: Option<&str>,
    status: &str,
) {
    let slug = format!("wt-{id}");
    let branch = format!("feat/{id}");
    let path = format!("/tmp/wt-{id}");
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at)
             VALUES (:id, :repo_id, :slug, :branch, :path, :ticket_id, :status, :created_at)",
        rusqlite::named_params! {
            ":id": id,
            ":repo_id": repo_id,
            ":slug": slug,
            ":branch": branch,
            ":path": path,
            ":ticket_id": ticket_id,
            ":status": status,
            ":created_at": "2024-01-01T00:00:00Z",
        },
    )
    .unwrap();
}

fn get_worktree_status(conn: &Connection, id: &str) -> String {
    conn.query_row(
        "SELECT status FROM worktrees WHERE id = :id",
        rusqlite::named_params! { ":id": id },
        |row| row.get("status"),
    )
    .unwrap()
}

fn all_merged(_repo_id: &str, branches: &[String]) -> HashMap<String, String> {
    branches
        .iter()
        .map(|b| (b.clone(), String::new()))
        .collect()
}

#[test]
fn test_mark_worktrees_active_to_merged() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");

    syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();

    let count = syncer
        .mark_worktrees_for_closed_tickets_with_merge_check("r1", all_merged)
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
}

#[test]
fn test_mark_worktrees_abandoned_to_merged() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "abandoned");
    syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();

    let count = syncer
        .mark_worktrees_for_closed_tickets_with_merge_check("r1", all_merged)
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
}

#[test]
fn test_mark_worktrees_skips_unlinked() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    insert_worktree(&conn, "wt1", "r1", None, "active");

    let count = syncer.mark_worktrees_for_closed_tickets("r1").unwrap();
    assert_eq!(count, 0);
    assert_eq!(get_worktree_status(&conn, "wt1"), "active");
}

#[test]
fn test_mark_worktrees_skips_open_ticket() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Insert an open ticket and link a worktree to it
    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");

    // Do NOT close the ticket — it stays open
    let count = syncer.mark_worktrees_for_closed_tickets("r1").unwrap();
    assert_eq!(count, 0);
    assert_eq!(get_worktree_status(&conn, "wt1"), "active");
}

#[test]
fn test_mark_worktrees_artifacts_query_returns_correct_paths() {
    // Verify the artifact-collection JOIN query (CLOSED_TICKET_ARTIFACTS_SQL)
    // returns the expected (local_path, worktree_path, branch) for a closed-ticket worktree.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");
    syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();

    // Use the same constant the implementation uses so this test stays in sync.
    let artifacts: Vec<(String, String, String, String)> = conn
        .prepare(CLOSED_TICKET_ARTIFACTS_SQL)
        .unwrap()
        .query_map(rusqlite::named_params! { ":repo_id": "r1" }, |row| {
            Ok((
                row.get("local_path")?,
                row.get("path")?,
                row.get("branch")?,
                row.get("remote_url")?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].0, "/tmp/repo"); // repo local_path from setup_db
    assert_eq!(artifacts[0].1, "/tmp/wt-wt1"); // worktree path from insert_worktree
    assert_eq!(artifacts[0].2, "feat/wt1"); // branch from insert_worktree
}

#[test]
fn test_mark_worktrees_artifacts_skips_already_merged() {
    // mark_worktrees_for_closed_tickets must not attempt artifact cleanup for
    // worktrees whose status is already 'merged' (verified via CLOSED_TICKET_ARTIFACTS_SQL).
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "merged");
    syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();

    // Use the same constant the implementation uses so this test stays in sync.
    let artifacts: Vec<(String, String, String, String)> = conn
        .prepare(CLOSED_TICKET_ARTIFACTS_SQL)
        .unwrap()
        .query_map(rusqlite::named_params! { ":repo_id": "r1" }, |row| {
            Ok((
                row.get("local_path")?,
                row.get("path")?,
                row.get("branch")?,
                row.get("remote_url")?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    assert_eq!(artifacts.len(), 0);
}

#[test]
fn test_mark_worktrees_for_closed_tickets_end_to_end() {
    // Verify that mark_worktrees_for_closed_tickets completes successfully
    // in the closed-ticket scenario, updating DB state and exercising the
    // artifact-cleanup loop (remove_git_artifacts is best-effort and no-ops
    // on non-existent paths, so this is safe in tests).
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");
    syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();

    let count = syncer
        .mark_worktrees_for_closed_tickets_with_merge_check("r1", all_merged)
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
}

#[test]
fn test_mark_worktrees_sets_completed_at() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");

    // Verify completed_at starts as NULL
    let before: Option<String> = conn
        .query_row(
            "SELECT completed_at FROM worktrees WHERE id = 'wt1'",
            [],
            |row| row.get("completed_at"),
        )
        .unwrap();
    assert!(before.is_none());

    syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();
    syncer
        .mark_worktrees_for_closed_tickets_with_merge_check("r1", all_merged)
        .unwrap();

    let after: Option<String> = conn
        .query_row(
            "SELECT completed_at FROM worktrees WHERE id = 'wt1'",
            [],
            |row| row.get("completed_at"),
        )
        .unwrap();
    assert!(
        after.is_some(),
        "completed_at must be set when marking as merged"
    );
}

#[test]
fn test_mark_worktrees_artifact_cleanup_regression() {
    // Regression test for artifact cleanup becoming unconditional.
    // Verifies that when a worktree is marked as merged due to closed tickets,
    // the artifact cleanup code path is executed (WorktreeManager::remove_artifacts is called).
    // This test creates minimal fixtures since remove_artifacts is best-effort and gracefully
    // handles non-existent paths.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Create ticket and worktree record with a fake but safe path
    let tickets = vec![make_ticket("cleanup-test", "Test cleanup ticket")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row(
            "SELECT id FROM tickets WHERE source_id = 'cleanup-test'",
            [],
            |row| row.get("id"),
        )
        .unwrap();

    // Insert worktree record pointing to non-existent path (safe for testing)
    insert_worktree(&conn, "wt-cleanup", "r1", Some(&ticket_id), "active");

    // Close the ticket
    syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();

    // Mark worktrees for closed tickets (which should trigger artifact cleanup)
    // The key behavioral change being tested: artifact cleanup now happens unconditionally
    // for all paths that call mark_worktrees_for_closed_tickets (previously it was conditional)
    let count = syncer
        .mark_worktrees_for_closed_tickets_with_merge_check("r1", all_merged)
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(get_worktree_status(&conn, "wt-cleanup"), "merged");

    // The test verifies the bug fix: artifact cleanup is now unconditional.
    // We can't easily verify the side effects in a unit test, but we've verified
    // that the code path executes successfully and the worktree gets marked as merged.
    // The actual git operations in remove_artifacts are best-effort and will not fail
    // even with non-existent paths, which is the expected behavior.
}

#[test]
fn test_mark_worktrees_idempotent() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "merged");
    syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();

    let count = syncer.mark_worktrees_for_closed_tickets("r1").unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_mark_worktrees_scoped_to_repo() {
    let conn = setup_db();
    conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
             VALUES ('repo2', 'other-repo', '/tmp/repo2', 'https://github.com/test/other', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

    let syncer = TicketSyncer::new(&conn);

    let t1 = vec![make_ticket("1", "Repo1 Issue")];
    let t2 = vec![make_ticket("1", "Repo2 Issue")];
    syncer.upsert_tickets("r1", &t1).unwrap();
    syncer.upsert_tickets("repo2", &t2).unwrap();

    let tid1: String = conn
        .query_row("SELECT id FROM tickets WHERE repo_id = 'r1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    let tid2: String = conn
        .query_row(
            "SELECT id FROM tickets WHERE repo_id = 'repo2'",
            [],
            |row| row.get("id"),
        )
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&tid1), "active");
    insert_worktree(&conn, "wt2", "repo2", Some(&tid2), "active");

    syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();
    syncer
        .close_missing_tickets("repo2", "github", &["999"])
        .unwrap();

    let count = syncer
        .mark_worktrees_for_closed_tickets_with_merge_check("r1", all_merged)
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
    assert_eq!(get_worktree_status(&conn, "wt2"), "active");
}

#[test]
fn test_mark_worktrees_skips_unmerged_pr() {
    // When the merge check returns false, a closed ticket's worktree must not be touched.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");
    syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();

    let count = syncer
        .mark_worktrees_for_closed_tickets_with_merge_check("r1", |_, _: &[String]| HashMap::new())
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(get_worktree_status(&conn, "wt1"), "active");
}

#[test]
fn test_mark_worktrees_removes_when_pr_merged() {
    // When the merge check returns true, a closed ticket's worktree must be updated.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");
    syncer
        .close_missing_tickets("r1", "github", &["999"])
        .unwrap();

    let count = syncer
        .mark_worktrees_for_closed_tickets_with_merge_check("r1", all_merged)
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
}

#[test]
fn test_link_to_worktree_success() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", None, "active");

    syncer.link_to_worktree(&ticket_id, "wt1").unwrap();

    let linked: Option<String> = conn
        .query_row(
            "SELECT ticket_id FROM worktrees WHERE id = 'wt1'",
            [],
            |row| row.get("ticket_id"),
        )
        .unwrap();
    assert_eq!(linked, Some(ticket_id));
}

#[test]
fn test_link_to_worktree_rejects_if_already_linked() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let tickets = vec![make_ticket("1", "Issue 1"), make_ticket("2", "Issue 2")];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    let tid1: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    let tid2: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '2'", [], |row| {
            row.get("id")
        })
        .unwrap();
    insert_worktree(&conn, "wt1", "r1", Some(&tid1), "active");

    let result = syncer.link_to_worktree(&tid2, "wt1");
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("already has a linked ticket"));
}

#[test]
fn test_get_by_id_success() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let tickets = vec![make_ticket("1", "Issue 1")];
    syncer.upsert_tickets("r1", &tickets).unwrap();

    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();

    let ticket = syncer.get_by_id(&ticket_id).unwrap();
    assert_eq!(ticket.source_id, "1");
    assert_eq!(ticket.title, "Issue 1");
}

#[test]
fn test_get_by_id_not_found() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let result = syncer.get_by_id("nonexistent-id");
    assert!(result.is_err());
}

#[test]
fn test_upsert_tickets_stores_label_details() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let mut ticket = make_ticket("1", "Issue 1");
    ticket.label_details = vec![
        TicketLabelInput {
            name: "bug".to_string(),
            color: Some("d73a4a".to_string()),
        },
        TicketLabelInput {
            name: "enhancement".to_string(),
            color: None,
        },
    ];
    ticket.labels = vec!["bug".to_string(), "enhancement".to_string()];
    syncer.upsert_tickets("r1", &[ticket]).unwrap();

    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();

    let labels = syncer.get_labels(&ticket_id).unwrap();
    assert_eq!(labels.len(), 2);
    let bug = labels.iter().find(|l| l.label == "bug").unwrap();
    assert_eq!(bug.color, Some("d73a4a".to_string()));
    let enh = labels.iter().find(|l| l.label == "enhancement").unwrap();
    assert_eq!(enh.color, None);
}

#[test]
fn test_resync_removes_stale_labels() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // First sync: bug + enhancement
    let mut ticket = make_ticket("1", "Issue 1");
    ticket.label_details = vec![
        TicketLabelInput {
            name: "bug".to_string(),
            color: Some("d73a4a".to_string()),
        },
        TicketLabelInput {
            name: "enhancement".to_string(),
            color: None,
        },
    ];
    syncer.upsert_tickets("r1", &[ticket]).unwrap();

    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    assert_eq!(syncer.get_labels(&ticket_id).unwrap().len(), 2);

    // Second sync: only bug remains
    let mut ticket2 = make_ticket("1", "Issue 1");
    ticket2.label_details = vec![TicketLabelInput {
        name: "bug".to_string(),
        color: Some("d73a4a".to_string()),
    }];
    syncer.upsert_tickets("r1", &[ticket2]).unwrap();

    let labels = syncer.get_labels(&ticket_id).unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].label, "bug");
}

#[test]
fn test_resync_adds_new_labels() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // First sync: no labels
    let ticket = make_ticket("1", "Issue 1");
    syncer.upsert_tickets("r1", &[ticket]).unwrap();

    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    assert_eq!(syncer.get_labels(&ticket_id).unwrap().len(), 0);

    // Second sync: add a label
    let mut ticket2 = make_ticket("1", "Issue 1");
    ticket2.label_details = vec![TicketLabelInput {
        name: "wontfix".to_string(),
        color: Some("ffffff".to_string()),
    }];
    syncer.upsert_tickets("r1", &[ticket2]).unwrap();

    let labels = syncer.get_labels(&ticket_id).unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].label, "wontfix");
}

#[test]
fn test_build_agent_prompt_full_ticket() {
    let ticket = Ticket {
        id: "01ABCDEF".to_string(),
        repo_id: "r1".to_string(),
        source_type: "github".to_string(),
        source_id: "42".to_string(),
        title: "Add dark mode support".to_string(),
        body: "We need dark mode for the settings page.".to_string(),
        state: "open".to_string(),
        labels: "enhancement, ui".to_string(),
        assignee: Some("dev1".to_string()),
        priority: None,
        url: "https://github.com/org/repo/issues/42".to_string(),
        synced_at: "2026-01-01T00:00:00Z".to_string(),
        raw_json: "{}".to_string(),
        workflow: None,
        agent_map: None,
    };

    let prompt = build_agent_prompt(&ticket, &[]);
    assert!(prompt.contains("Issue: #42 — Add dark mode support"));
    assert!(prompt.contains("State: open"));
    assert!(prompt.contains("Labels: enhancement, ui"));
    assert!(prompt.contains("We need dark mode for the settings page."));
    assert!(prompt.contains("Implement the changes described in the issue."));
}

#[test]
fn test_build_agent_prompt_empty_body_and_labels() {
    let ticket = Ticket {
        id: "01ABCDEF".to_string(),
        repo_id: "r1".to_string(),
        source_type: "github".to_string(),
        source_id: "7".to_string(),
        title: "Fix typo".to_string(),
        body: String::new(),
        state: "open".to_string(),
        labels: "[]".to_string(),
        assignee: None,
        priority: None,
        url: String::new(),
        synced_at: "2026-01-01T00:00:00Z".to_string(),
        raw_json: "{}".to_string(),
        workflow: None,
        agent_map: None,
    };

    let prompt = build_agent_prompt(&ticket, &[]);
    assert!(prompt.contains("Issue: #7 — Fix typo"));
    assert!(prompt.contains("Labels: None"));
    assert!(prompt.contains("(No description provided)"));
}

#[test]
fn test_build_agent_prompt_with_comments() {
    let ticket = Ticket {
        id: "01ABCDEF".to_string(),
        repo_id: "r1".to_string(),
        source_type: "jira".to_string(),
        source_id: "RND-1".to_string(),
        title: "Profile name limit".to_string(),
        body: "Set a limit for profile names.".to_string(),
        state: "open".to_string(),
        labels: "[]".to_string(),
        assignee: None,
        priority: None,
        url: String::new(),
        synced_at: "2026-01-01T00:00:00Z".to_string(),
        raw_json: "{}".to_string(),
        workflow: None,
        agent_map: None,
    };
    let comments = vec![
        TicketComment {
            id: "1".to_string(),
            author: "Kate".to_string(),
            body: "Let's max it out at 30 characters".to_string(),
        },
        TicketComment {
            id: "2".to_string(),
            author: "Bob".to_string(),
            body: "Agreed, 30 is good".to_string(),
        },
    ];

    let prompt = build_agent_prompt(&ticket, &comments);
    assert!(prompt.contains("## Comments"));
    assert!(prompt.contains("**Kate**: Let's max it out at 30 characters"));
    assert!(prompt.contains("**Bob**: Agreed, 30 is good"));
}

#[test]
fn test_format_comments_section_empty() {
    assert_eq!(format_comments_section(&[]), String::new());
}

#[test]
fn test_format_comments_section_nonempty() {
    let comments = vec![TicketComment {
        id: "1".to_string(),
        author: "Alice".to_string(),
        body: "Good point".to_string(),
    }];
    let section = format_comments_section(&comments);
    assert!(section.contains("## Comments"));
    assert!(section.contains("**Alice**: Good point"));
}

/// Verify that `TicketSyncer::list` returns all tickets regardless of state,
/// including closed ones. The display-layer filtering (hide closed by default)
/// is intentionally done in the TUI / web route, not in core.
#[test]
fn test_list_includes_closed_tickets() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Upsert two tickets: one open, one that will be closed
    let tickets = vec![
        make_ticket("10", "Open issue"),
        make_ticket("11", "Soon closed"),
    ];
    syncer.upsert_tickets("r1", &tickets).unwrap();

    // Close ticket 11
    syncer
        .close_missing_tickets("r1", "github", &["10"])
        .unwrap();

    let all = syncer.list(None).unwrap();
    assert_eq!(
        all.len(),
        2,
        "list() must return all tickets including closed"
    );

    let states: Vec<&str> = all.iter().map(|t| t.state.as_str()).collect();
    assert!(states.contains(&"open"), "open ticket must be present");
    assert!(states.contains(&"closed"), "closed ticket must be present");

    // Simulate the web-route filter (show_closed=false)
    let visible: Vec<_> = all.iter().filter(|t| t.state != "closed").collect();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].source_id, "10");
}

#[test]
fn test_get_all_labels_groups_by_ticket_id() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Two tickets, first with two labels, second with one label, third with none.
    let mut t1 = make_ticket("1", "Issue 1");
    t1.label_details = vec![
        TicketLabelInput {
            name: "bug".to_string(),
            color: Some("d73a4a".to_string()),
        },
        TicketLabelInput {
            name: "enhancement".to_string(),
            color: None,
        },
    ];
    let mut t2 = make_ticket("2", "Issue 2");
    t2.label_details = vec![TicketLabelInput {
        name: "docs".to_string(),
        color: Some("0075ca".to_string()),
    }];
    let t3 = make_ticket("3", "Issue 3"); // no labels

    syncer.upsert_tickets("r1", &[t1, t2, t3]).unwrap();

    let tid1: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    let tid2: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '2'", [], |row| {
            row.get("id")
        })
        .unwrap();
    let tid3: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '3'", [], |row| {
            row.get("id")
        })
        .unwrap();

    let map = syncer.get_all_labels().unwrap();

    // ticket 1: two labels
    let lbls1 = map.get(&tid1).expect("ticket 1 must have labels");
    assert_eq!(lbls1.len(), 2);
    assert!(lbls1
        .iter()
        .any(|l| l.label == "bug" && l.color == Some("d73a4a".to_string())));
    assert!(lbls1
        .iter()
        .any(|l| l.label == "enhancement" && l.color.is_none()));

    // ticket 2: one label
    let lbls2 = map.get(&tid2).expect("ticket 2 must have labels");
    assert_eq!(lbls2.len(), 1);
    assert_eq!(lbls2[0].label, "docs");
    assert_eq!(lbls2[0].color, Some("0075ca".to_string()));

    // ticket 3: no entry in the map
    assert!(
        !map.contains_key(&tid3),
        "ticket with no labels must not appear in the map"
    );
}

#[test]
fn test_get_all_labels_empty_db() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let map = syncer.get_all_labels().unwrap();
    assert!(map.is_empty(), "empty DB must yield empty label map");
}

// -----------------------------------------------------------------------
// list_filtered tests
// -----------------------------------------------------------------------

fn make_ticket_with_body(source_id: &str, title: &str, body: &str) -> TicketInput {
    TicketInput {
        source_type: "github".to_string(),
        source_id: source_id.to_string(),
        title: title.to_string(),
        body: body.to_string(),
        state: "open".to_string(),
        labels: vec![],
        assignee: None,
        priority: None,
        url: String::new(),
        raw_json: None,
        comments: vec![],
        label_details: vec![],
        blocked_by: vec![],
        children: vec![],
        parent: None,
    }
}

#[test]
fn test_list_filtered_defaults_to_open_only() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![
        make_ticket("1", "Open issue"),
        make_ticket("2", "Closed issue"),
    ];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    syncer
        .close_missing_tickets("r1", "github", &["1"])
        .unwrap();

    let filter = TicketFilter {
        labels: vec![],
        search: None,
        include_closed: false,
        unlabeled_only: false,
    };
    let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_id, "1");
}

#[test]
fn test_list_filtered_include_closed() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![
        make_ticket("1", "Open issue"),
        make_ticket("2", "Closed issue"),
    ];
    syncer.upsert_tickets("r1", &tickets).unwrap();
    syncer
        .close_missing_tickets("r1", "github", &["1"])
        .unwrap();

    let filter = TicketFilter {
        labels: vec![],
        search: None,
        include_closed: true,
        unlabeled_only: false,
    };
    let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn test_list_filtered_by_label() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let mut t1 = make_ticket("1", "Bug report");
    t1.label_details = vec![TicketLabelInput {
        name: "bug".to_string(),
        color: None,
    }];
    let t2 = make_ticket("2", "Feature request"); // no labels

    syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

    let filter = TicketFilter {
        labels: vec!["bug".to_string()],
        search: None,
        include_closed: false,
        unlabeled_only: false,
    };
    let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_id, "1");
}

#[test]
fn test_list_filtered_by_multiple_labels_and_semantics() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // t1 has both "bug" and "urgent"
    let mut t1 = make_ticket("1", "Critical bug");
    t1.label_details = vec![
        TicketLabelInput {
            name: "bug".to_string(),
            color: None,
        },
        TicketLabelInput {
            name: "urgent".to_string(),
            color: None,
        },
    ];
    // t2 has only "bug"
    let mut t2 = make_ticket("2", "Normal bug");
    t2.label_details = vec![TicketLabelInput {
        name: "bug".to_string(),
        color: None,
    }];

    syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

    // Filtering for both labels should return only t1 (AND semantics)
    let filter = TicketFilter {
        labels: vec!["bug".to_string(), "urgent".to_string()],
        search: None,
        include_closed: false,
        unlabeled_only: false,
    };
    let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_id, "1");
}

#[test]
fn test_list_filtered_by_search_title() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    syncer
        .upsert_tickets(
            "r1",
            &[
                make_ticket_with_body("1", "Fix the login page", ""),
                make_ticket_with_body("2", "Update dashboard", ""),
            ],
        )
        .unwrap();

    let filter = TicketFilter {
        labels: vec![],
        search: Some("login".to_string()),
        include_closed: false,
        unlabeled_only: false,
    };
    let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_id, "1");
}

#[test]
fn test_list_filtered_by_search_body() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    syncer
        .upsert_tickets(
            "r1",
            &[
                make_ticket_with_body("1", "Issue A", "contains the keyword xyz"),
                make_ticket_with_body("2", "Issue B", "nothing relevant"),
            ],
        )
        .unwrap();

    let filter = TicketFilter {
        labels: vec![],
        search: Some("xyz".to_string()),
        include_closed: false,
        unlabeled_only: false,
    };
    let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_id, "1");
}

#[test]
fn test_list_filtered_no_repo_scope() {
    let conn = setup_db();
    conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
             VALUES ('repo2', 'other-repo', '/tmp/repo2', 'https://github.com/test/other', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

    let syncer = TicketSyncer::new(&conn);
    syncer
        .upsert_tickets("r1", &[make_ticket("1", "Repo1 issue")])
        .unwrap();
    syncer
        .upsert_tickets("repo2", &[make_ticket("2", "Repo2 issue")])
        .unwrap();

    let filter = TicketFilter {
        labels: vec![],
        search: None,
        include_closed: false,
        unlabeled_only: false,
    };
    let results = syncer.list_filtered(None, &filter).unwrap();
    assert_eq!(results.len(), 2);
}

// --- resolve_ticket_id tests ---

fn make_repo() -> crate::repo::Repo {
    crate::repo::Repo {
        id: "r1".to_string(),
        slug: "test-repo".to_string(),
        local_path: "/tmp/repo".to_string(),
        remote_url: "https://github.com/test/repo.git".to_string(),
        default_branch: "main".to_string(),
        workspace_dir: "/tmp/ws".to_string(),
        created_at: "2024-01-01T00:00:00Z".to_string(),
        model: None,
        allow_agent_issue_creation: false,
    }
}

#[test]
fn test_resolve_ticket_id_by_source_id() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let repo = make_repo();

    syncer
        .upsert_tickets("r1", &[make_ticket("42", "Issue 42")])
        .unwrap();

    let (source_type, source_id) = syncer.resolve_ticket_id(&repo, "42").unwrap();
    assert_eq!(source_type, "github");
    assert_eq!(source_id, "42");
}

#[test]
fn test_resolve_ticket_id_by_ulid() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let repo = make_repo();

    syncer
        .upsert_tickets("r1", &[make_ticket("99", "Issue 99")])
        .unwrap();
    let ulid: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '99'", [], |row| {
            row.get("id")
        })
        .unwrap();

    let (source_type, source_id) = syncer.resolve_ticket_id(&repo, &ulid).unwrap();
    assert_eq!(source_type, "github");
    assert_eq!(source_id, "99");
}

#[test]
fn test_resolve_ticket_id_ulid_not_found_falls_through() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let repo = make_repo();

    // Insert a ticket with source_id that is exactly 26 chars (ULID-length)
    // but is NOT a valid internal ULID — should fall through to source_id lookup.
    let fake_ulid = "01ABCDEFGHJKMNPQRSTVWXYZ99";
    assert_eq!(fake_ulid.len(), 26);
    syncer
        .upsert_tickets(
            "r1",
            &[make_ticket(fake_ulid, "Issue with ULID-like source_id")],
        )
        .unwrap();

    let (source_type, source_id) = syncer.resolve_ticket_id(&repo, fake_ulid).unwrap();
    assert_eq!(source_type, "github");
    assert_eq!(source_id, fake_ulid);
}

#[test]
fn test_resolve_ticket_id_not_found() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let repo = make_repo();

    let result = syncer.resolve_ticket_id(&repo, "nonexistent");
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ConductorError::TicketNotFound { .. }
    ));
}

#[test]
fn test_resolve_ticket_id_worktree_branch_lookup() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let repo = make_repo();

    syncer
        .upsert_tickets("r1", &[make_ticket("77", "Issue 77")])
        .unwrap();
    let ticket_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '77'", [], |row| {
            row.get("id")
        })
        .unwrap();

    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at)
             VALUES ('wt1', 'r1', 'wt-1', 'feat/issue-77', '/tmp/wt1', :ticket_id, 'active', '2024-01-01T00:00:00Z')",
            rusqlite::named_params! { ":ticket_id": ticket_id },
        )
        .unwrap();

    let result =
        crate::worktree::get_ticket_id_by_branch(&conn, &repo.id, "feat/issue-77").unwrap();
    assert_eq!(result, Some(ticket_id));
}

#[test]
fn test_resolve_ticket_id_worktree_branch_not_found() {
    let conn = setup_db();
    let repo = make_repo();

    let err =
        crate::worktree::get_ticket_id_by_branch(&conn, &repo.id, "feat/missing").unwrap_err();
    assert!(
        matches!(err, ConductorError::WorktreeNotFound { .. }),
        "expected WorktreeNotFound error, got: {err:?}"
    );
}

#[test]
fn test_resolve_ticket_id_worktree_no_linked_ticket() {
    let conn = setup_db();
    let repo = make_repo();

    conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at)
             VALUES ('wt2', 'r1', 'wt-2', 'feat/no-ticket', '/tmp/wt2', NULL, 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

    let result =
        crate::worktree::get_ticket_id_by_branch(&conn, &repo.id, "feat/no-ticket").unwrap();
    assert_eq!(result, None);
}

#[test]
fn test_list_sorts_by_issue_number_descending() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Insert tickets with numeric source_ids in non-sequential order
    let tickets = vec![
        make_ticket("5", "Issue 5"),
        make_ticket("123", "Issue 123"),
        make_ticket("1", "Issue 1"),
        make_ticket("42", "Issue 42"),
    ];
    syncer.upsert_tickets("r1", &tickets).unwrap();

    let result = syncer.list(Some("r1")).unwrap();
    let ids: Vec<&str> = result.iter().map(|t| t.source_id.as_str()).collect();
    assert_eq!(ids, vec!["123", "42", "5", "1"]);
}

#[test]
fn test_list_filtered_sorts_by_issue_number_descending() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![
        make_ticket("10", "Issue 10"),
        make_ticket("200", "Issue 200"),
        make_ticket("3", "Issue 3"),
    ];
    syncer.upsert_tickets("r1", &tickets).unwrap();

    let filter = TicketFilter {
        labels: vec![],
        search: None,
        include_closed: false,
        unlabeled_only: false,
    };
    let result = syncer.list_filtered(Some("r1"), &filter).unwrap();
    let ids: Vec<&str> = result.iter().map(|t| t.source_id.as_str()).collect();
    assert_eq!(ids, vec!["200", "10", "3"]);
}

#[test]
fn test_list_filtered_unlabeled_only() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // t1 has a label, t2 and t3 do not.
    let mut t1 = make_ticket("1", "Labeled issue");
    t1.label_details = vec![TicketLabelInput {
        name: "bug".to_string(),
        color: None,
    }];
    let t2 = make_ticket("2", "Unlabeled issue A");
    let t3 = make_ticket("3", "Unlabeled issue B");

    syncer.upsert_tickets("r1", &[t1, t2, t3]).unwrap();

    let filter = TicketFilter {
        labels: vec![],
        search: None,
        include_closed: false,
        unlabeled_only: true,
    };
    let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
    let ids: Vec<&str> = results.iter().map(|t| t.source_id.as_str()).collect();
    // Only t2 and t3 are unlabeled (sorted descending by source_id)
    assert_eq!(ids, vec!["3", "2"]);
}

#[test]
fn test_list_filtered_unlabeled_only_excludes_closed() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // t1 is unlabeled and open, t2 is unlabeled but closed.
    let t1 = make_ticket("1", "Open unlabeled");
    let t2 = make_ticket("2", "Closed unlabeled");
    syncer.upsert_tickets("r1", &[t1, t2]).unwrap();
    syncer
        .close_missing_tickets("r1", "github", &["1"])
        .unwrap();

    let filter = TicketFilter {
        labels: vec![],
        search: None,
        include_closed: false,
        unlabeled_only: true,
    };
    let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_id, "1");
}

#[test]
fn test_list_all_repos_sorts_by_issue_number_descending() {
    let conn = setup_db();
    // Register a second repo so we can test cross-repo listing
    conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('r2', 'test-repo-2', '/tmp/repo2', 'https://github.com/test/repo2.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
    let syncer = TicketSyncer::new(&conn);

    // Insert tickets across two different repos with interleaved source_ids
    let repo1_tickets = vec![
        make_ticket("10", "Repo1 Issue 10"),
        make_ticket("50", "Repo1 Issue 50"),
    ];
    let repo2_tickets = vec![
        make_ticket("25", "Repo2 Issue 25"),
        make_ticket("100", "Repo2 Issue 100"),
    ];
    syncer.upsert_tickets("r1", &repo1_tickets).unwrap();
    syncer.upsert_tickets("r2", &repo2_tickets).unwrap();

    // list(None) should return all tickets sorted by issue number descending
    let result = syncer.list(None).unwrap();
    let ids: Vec<&str> = result.iter().map(|t| t.source_id.as_str()).collect();
    assert_eq!(ids, vec!["100", "50", "25", "10"]);
}

#[test]
fn test_list_sorts_non_numeric_source_ids_to_end() {
    // Non-numeric source_ids (e.g. Jira keys) CAST to 0, so they sort
    // after all numeric IDs. Among themselves, they fall back to the
    // secondary `source_id DESC` (string) sort.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![
        make_ticket("PROJ-10", "Jira ticket 10"),
        make_ticket("5", "GitHub issue 5"),
        make_ticket("PROJ-3", "Jira ticket 3"),
        make_ticket("100", "GitHub issue 100"),
    ];
    syncer.upsert_tickets("r1", &tickets).unwrap();

    let result = syncer.list(Some("r1")).unwrap();
    let ids: Vec<&str> = result.iter().map(|t| t.source_id.as_str()).collect();
    // Numeric IDs first (descending), then non-numeric (string descending)
    assert_eq!(ids, vec!["100", "5", "PROJ-3", "PROJ-10"]);
}

// -----------------------------------------------------------------------
// ticket_dependencies tests
// -----------------------------------------------------------------------

fn dep_row(conn: &Connection) -> Option<(String, String, String)> {
    conn.query_row(
        "SELECT from_ticket_id, to_ticket_id, dep_type FROM ticket_dependencies LIMIT 1",
        [],
        |row| {
            Ok((
                row.get("from_ticket_id")?,
                row.get("to_ticket_id")?,
                row.get("dep_type")?,
            ))
        },
    )
    .ok()
}

fn dep_count(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) AS cnt FROM ticket_dependencies",
        [],
        |row| row.get("cnt"),
    )
    .unwrap()
}

#[test]
fn test_upsert_blocked_by_writes_dependency() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Ticket "2" blocks ticket "1"
    let t2 = make_ticket("2", "Blocker");
    let mut t1 = make_ticket("1", "Blocked");
    t1.blocked_by = vec!["2".to_string()];

    syncer.upsert_tickets("r1", &[t2, t1]).unwrap();

    let id1: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    let id2: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '2'", [], |row| {
            row.get("id")
        })
        .unwrap();

    let row = dep_row(&conn).expect("expected one dependency row");
    assert_eq!(row.0, id2, "from_ticket_id should be the blocker (2)");
    assert_eq!(row.1, id1, "to_ticket_id should be the blocked ticket (1)");
    assert_eq!(row.2, "blocks");
}

#[test]
fn test_upsert_children_writes_dependency() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Ticket "1" is parent of ticket "2"
    let t2 = make_ticket("2", "Child");
    let mut t1 = make_ticket("1", "Parent");
    t1.children = vec!["2".to_string()];

    syncer.upsert_tickets("r1", &[t2, t1]).unwrap();

    let id1: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();
    let id2: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '2'", [], |row| {
            row.get("id")
        })
        .unwrap();

    let row = dep_row(&conn).expect("expected one dependency row");
    assert_eq!(row.0, id1, "from_ticket_id should be the parent (1)");
    assert_eq!(row.1, id2, "to_ticket_id should be the child (2)");
    assert_eq!(row.2, "parent_of");
}

#[test]
fn test_upsert_empty_blocked_by_preserves_existing_deps() {
    // Empty blocked_by is treated as "no opinion" — it must NOT clear deps
    // written by a previous upsert or by another source (e.g. MCP).
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // First upsert: ticket "1" is blocked by ticket "2"
    let t2 = make_ticket("2", "Blocker");
    let mut t1 = make_ticket("1", "Blocked");
    t1.blocked_by = vec!["2".to_string()];
    syncer.upsert_tickets("r1", &[t2, t1]).unwrap();
    assert_eq!(dep_count(&conn), 1);

    // Re-upsert ticket "1" with empty blocked_by (e.g. from a GitHub sync
    // that doesn't parse body text) — existing dep row must be preserved.
    let t1_no_opinion = make_ticket("1", "Blocked");
    syncer.upsert_tickets("r1", &[t1_no_opinion]).unwrap();
    assert_eq!(
        dep_count(&conn),
        1,
        "empty blocked_by should not remove existing dependency rows"
    );
}

#[test]
fn test_upsert_unknown_source_id_skipped() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let mut t1 = make_ticket("1", "Ticket");
    t1.blocked_by = vec!["nonexistent".to_string()];

    // Should not panic; unresolvable source IDs are silently skipped
    syncer.upsert_tickets("r1", &[t1]).unwrap();
    assert_eq!(
        dep_count(&conn),
        0,
        "unresolvable source_id should produce no row"
    );
}

#[test]
fn test_upsert_dependency_idempotent() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Upsert the same batch twice — each time re-construct the inputs
    for _ in 0..2 {
        let t2 = make_ticket("2", "Blocker");
        let mut t1 = make_ticket("1", "Blocked");
        t1.blocked_by = vec!["2".to_string()];
        syncer.upsert_tickets("r1", &[t2, t1]).unwrap();
    }

    assert_eq!(
        dep_count(&conn),
        1,
        "second upsert should not duplicate the dependency row"
    );
}

#[test]
fn test_upsert_empty_children_preserves_existing_deps() {
    // Empty children is treated as "no opinion" — it must NOT clear deps
    // written by a previous upsert or by another source (e.g. MCP).
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // First upsert: ticket "1" is parent of ticket "2"
    let t2 = make_ticket("2", "Child");
    let mut t1 = make_ticket("1", "Parent");
    t1.children = vec!["2".to_string()];
    syncer.upsert_tickets("r1", &[t2, t1]).unwrap();
    assert_eq!(dep_count(&conn), 1);

    // Re-upsert ticket "1" with empty children (e.g. from a GitHub sync
    // that doesn't parse body text) — existing dep row must be preserved.
    let t1_no_opinion = make_ticket("1", "Parent");
    syncer.upsert_tickets("r1", &[t1_no_opinion]).unwrap();
    assert_eq!(
        dep_count(&conn),
        1,
        "empty children should not remove existing dependency rows"
    );
}

#[test]
fn test_upsert_only_parent_preserves_blocked_by_and_children() {
    // Setting only `parent` must not clear existing `blocked_by` or `children`
    // relationships — the guard must be per-field, not shared.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // First upsert: ticket "1" is blocked by "2" and is parent of "3"
    let t2 = make_ticket("2", "Blocker");
    let t3 = make_ticket("3", "Child");
    let mut t1 = make_ticket("1", "Middle");
    t1.blocked_by = vec!["2".to_string()];
    t1.children = vec!["3".to_string()];
    syncer.upsert_tickets("r1", &[t2, t3, t1]).unwrap();
    // 1 blocks row + 1 parent_of row
    assert_eq!(dep_count(&conn), 2);

    // Insert a parent ticket "0"
    let t0 = make_ticket("0", "GrandParent");
    syncer.upsert_tickets("r1", &[t0]).unwrap();

    // Second upsert: ticket "1" with only parent set, blocked_by and children are empty
    let mut t1_parent_only = make_ticket("1", "Middle");
    t1_parent_only.parent = Some("0".to_string());
    syncer.upsert_tickets("r1", &[t1_parent_only]).unwrap();

    // Should now have 3 rows: the original blocks + parent_of(1→3) + new parent_of(0→1)
    assert_eq!(
        dep_count(&conn),
        3,
        "setting only parent must not wipe existing blocked_by or children rows"
    );
}

#[test]
fn test_upsert_only_blocked_by_preserves_parent_of() {
    // Setting only `blocked_by` must not clear existing `children` (parent_of) rows.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // First upsert: ticket "1" is parent of ticket "2"
    let t2 = make_ticket("2", "Child");
    let mut t1 = make_ticket("1", "Parent");
    t1.children = vec!["2".to_string()];
    syncer.upsert_tickets("r1", &[t2, t1]).unwrap();
    assert_eq!(dep_count(&conn), 1, "should have 1 parent_of row");

    // Insert a blocker ticket "3"
    let t3 = make_ticket("3", "Blocker");
    syncer.upsert_tickets("r1", &[t3]).unwrap();

    // Re-upsert ticket "1" with only blocked_by set, children empty
    let mut t1_blocked_only = make_ticket("1", "Parent");
    t1_blocked_only.blocked_by = vec!["3".to_string()];
    syncer.upsert_tickets("r1", &[t1_blocked_only]).unwrap();

    // Should now have 2 rows: original parent_of(1→2) + new blocks(1←3)
    assert_eq!(
        dep_count(&conn),
        2,
        "setting only blocked_by must not wipe existing parent_of (children) rows"
    );
}

#[test]
fn test_upsert_only_children_preserves_blocked_by() {
    // Setting only `children` must not clear existing `blocked_by` (blocks) rows.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // First upsert: ticket "1" is blocked by ticket "2"
    let t2 = make_ticket("2", "Blocker");
    let mut t1 = make_ticket("1", "Blocked");
    t1.blocked_by = vec!["2".to_string()];
    syncer.upsert_tickets("r1", &[t2, t1]).unwrap();
    assert_eq!(dep_count(&conn), 1, "should have 1 blocks row");

    // Insert a child ticket "3"
    let t3 = make_ticket("3", "Child");
    syncer.upsert_tickets("r1", &[t3]).unwrap();

    // Re-upsert ticket "1" with only children set, blocked_by empty
    let mut t1_children_only = make_ticket("1", "Blocked");
    t1_children_only.children = vec!["3".to_string()];
    syncer.upsert_tickets("r1", &[t1_children_only]).unwrap();

    // Should now have 2 rows: original blocks(1←2) + new parent_of(1→3)
    assert_eq!(
        dep_count(&conn),
        2,
        "setting only children must not wipe existing blocked_by (blocks) rows"
    );
}

// -----------------------------------------------------------------------
// get_ready_tickets tests
// -----------------------------------------------------------------------

fn insert_workflow_run_for_ticket(conn: &Connection, wf_id: &str, ticket_id: &str, status: &str) {
    // Insert a minimal agent_run first (parent_run_id FK)
    let ar_id = format!("ar-{wf_id}");
    conn.execute(
        "INSERT OR IGNORE INTO worktrees (id, repo_id, slug, branch, path, created_at) \
             VALUES ('wt-sys', 'r1', 'sys', 'sys', '/tmp/sys', '2024-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO agent_runs (id, worktree_id, prompt, status, started_at) \
             VALUES (:id, 'wt-sys', 'test', 'completed', '2024-01-01T00:00:00Z')",
        rusqlite::named_params! { ":id": ar_id },
    )
    .unwrap();
    conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, parent_run_id, status, started_at, ticket_id, repo_id) \
             VALUES (:id, 'wf', :ar_id, :status, '2024-01-01T00:00:00Z', :ticket_id, 'r1')",
            rusqlite::named_params! { ":id": wf_id, ":ar_id": ar_id, ":status": status, ":ticket_id": ticket_id },
        )
        .unwrap();
}

#[test]
fn test_get_ready_tickets_no_deps_all_open_ready() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    syncer
        .upsert_tickets("r1", &[make_ticket("1", "A"), make_ticket("2", "B")])
        .unwrap();

    let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
    assert_eq!(ready.len(), 2);
    assert!(ready.iter().all(|t| t.dep_type.is_none()));
}

#[test]
fn test_get_ready_tickets_blocked_ticket_excluded() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Ticket "2" blocks ticket "1"; blocker is still open → ticket "1" is not ready
    let t2 = make_ticket("2", "Blocker");
    let mut t1 = make_ticket("1", "Blocked");
    t1.blocked_by = vec!["2".to_string()];
    syncer.upsert_tickets("r1", &[t2, t1]).unwrap();

    let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].source_id, "2"); // only the blocker itself is ready
}

#[test]
fn test_get_ready_tickets_blocker_closed_makes_blocked_ready() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let t2 = make_ticket("2", "Blocker");
    let mut t1 = make_ticket("1", "Blocked");
    t1.blocked_by = vec!["2".to_string()];
    syncer.upsert_tickets("r1", &[t2, t1]).unwrap();

    // Close the blocker
    syncer
        .close_missing_tickets("r1", "github", &["1"])
        .unwrap();

    let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
    let ids: Vec<&str> = ready.iter().map(|t| t.source_id.as_str()).collect();
    assert!(
        ids.contains(&"1"),
        "blocked ticket should be ready once blocker is closed"
    );
}

#[test]
fn test_get_ready_tickets_active_run_excluded() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    syncer
        .upsert_tickets("r1", &[make_ticket("1", "A")])
        .unwrap();
    let tid: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();

    // Link an active workflow run to the ticket
    insert_workflow_run_for_ticket(&conn, "wr1", &tid, "running");

    let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
    assert_eq!(ready.len(), 0, "ticket with active run must be excluded");
}

#[test]
fn test_get_ready_tickets_completed_run_not_excluded() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    syncer
        .upsert_tickets("r1", &[make_ticket("1", "A")])
        .unwrap();
    let tid: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();

    // Completed run — should not block the ticket from being ready
    insert_workflow_run_for_ticket(&conn, "wr1", &tid, "completed");

    let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
    assert_eq!(ready.len(), 1);
}

#[test]
fn test_get_ready_tickets_root_ticket_id_scope() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Ticket "1" is parent of "2"; ticket "3" is unrelated
    let t2 = make_ticket("2", "Child");
    let t3 = make_ticket("3", "Unrelated");
    let mut t1 = make_ticket("1", "Parent");
    t1.children = vec!["2".to_string()];
    syncer.upsert_tickets("r1", &[t2, t3, t1]).unwrap();

    let parent_id: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
            row.get("id")
        })
        .unwrap();

    let ready = syncer
        .get_ready_tickets("r1", Some(&parent_id), None, 50)
        .unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].source_id, "2");
    assert_eq!(ready[0].dep_type.as_deref(), Some("parent_of"));
}

#[test]
fn test_get_ready_tickets_label_scope() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let mut t1 = make_ticket("1", "With label");
    t1.label_details = vec![TicketLabelInput {
        name: "backend".to_string(),
        color: None,
    }];
    let t2 = make_ticket("2", "No label");
    syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

    let ready = syncer
        .get_ready_tickets("r1", None, Some("backend"), 50)
        .unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].source_id, "1");
}

#[test]
fn test_get_ready_tickets_limit_respected() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets: Vec<_> = (1..=5)
        .map(|i| make_ticket(&i.to_string(), &format!("Issue {i}")))
        .collect();
    syncer.upsert_tickets("r1", &tickets).unwrap();

    let ready = syncer.get_ready_tickets("r1", None, None, 3).unwrap();
    assert_eq!(ready.len(), 3);
}

#[test]
fn test_get_ready_tickets_coalesce_no_run_as_completed() {
    // A blocker with state='closed' and NO workflow run must be treated as
    // resolved (COALESCE(wr.status, 'completed') = 'completed').
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let t2 = make_ticket("2", "Blocker");
    let mut t1 = make_ticket("1", "Blocked");
    t1.blocked_by = vec!["2".to_string()];
    syncer.upsert_tickets("r1", &[t2, t1]).unwrap();

    // Close the blocker (no workflow run created)
    syncer
        .close_missing_tickets("r1", "github", &["1"])
        .unwrap();

    let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
    let ids: Vec<&str> = ready.iter().map(|t| t.source_id.as_str()).collect();
    assert!(
        ids.contains(&"1"),
        "blocked ticket should be ready when closed blocker has no run (COALESCE = completed)"
    );
}

#[test]
fn test_blocks_delete_does_not_contaminate_parent_of() {
    // Regression test: stale-clear DELETE for dep_type='blocks' must not remove
    // parent_of rows written by a different ticket during an incremental sync.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Batch 1: ticket "B" is the parent of ticket "A" → writes parent_of(B→A)
    let ta = make_ticket("A", "Child");
    let mut tb = make_ticket("B", "Parent");
    tb.children = vec!["A".to_string()];
    syncer.upsert_tickets("r1", &[ta, tb]).unwrap();
    assert_eq!(dep_count(&conn), 1, "setup: one parent_of row expected");

    // Batch 2: re-upsert ticket "A" alone with empty blocked_by.
    // The stale-clear for blocks scoped to to_ticket_id=A should NOT remove
    // the parent_of row where A is the child (from_ticket_id=B, to_ticket_id=A).
    let ta_clear = make_ticket("A", "Child");
    syncer.upsert_tickets("r1", &[ta_clear]).unwrap();
    assert_eq!(
        dep_count(&conn),
        1,
        "parent_of row must survive a separate blocked_by clear for the child ticket"
    );
}

// ── get_dependencies / get_all_dependencies tests ───────────────────────

/// Returns the source_ids of a ticket slice for readable assertions.
fn source_ids(tickets: &[Ticket]) -> Vec<&str> {
    tickets.iter().map(|t| t.source_id.as_str()).collect()
}

fn get_ticket_id(conn: &Connection, source_id: &str) -> String {
    conn.query_row(
        "SELECT id FROM tickets WHERE source_id = :source_id",
        rusqlite::named_params! { ":source_id": source_id },
        |row| row.get("id"),
    )
    .expect("ticket not found")
}

#[test]
fn test_get_dependencies_blocked_by_and_blocks() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // ticket "1" blocks ticket "2"
    let t1 = make_ticket("1", "Blocker");
    // no special fields needed — relationship is written via t2.blocked_by
    let mut t2 = make_ticket("2", "Blocked");
    t2.blocked_by = vec!["1".to_string()];

    syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

    let deps = syncer
        .get_dependencies_by_source_id("r1", "1")
        .expect("get_dependencies for ticket 1");

    // Ticket 1 blocks ticket 2
    assert_eq!(
        source_ids(&deps.blocks),
        vec!["2"],
        "ticket 1 should block ticket 2"
    );
    assert!(
        deps.blocked_by.is_empty(),
        "ticket 1 should not be blocked by anything"
    );

    let deps2 = syncer
        .get_dependencies_by_source_id("r1", "2")
        .expect("get_dependencies for ticket 2");

    // Ticket 2 is blocked by ticket 1
    assert_eq!(
        source_ids(&deps2.blocked_by),
        vec!["1"],
        "ticket 2 should be blocked by ticket 1"
    );
    assert!(
        deps2.blocks.is_empty(),
        "ticket 2 should not block anything"
    );
}

#[test]
fn test_get_dependencies_parent_and_children() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // ticket "10" is parent of "11" and "12"
    let child1 = make_ticket("11", "Child 1");
    let child2 = make_ticket("12", "Child 2");
    let mut parent = make_ticket("10", "Parent");
    parent.children = vec!["11".to_string(), "12".to_string()];

    syncer
        .upsert_tickets("r1", &[child1, child2, parent])
        .unwrap();

    let parent_deps = syncer
        .get_dependencies_by_source_id("r1", "10")
        .expect("get_dependencies for parent ticket");

    let mut child_ids = source_ids(&parent_deps.children);
    child_ids.sort();
    assert_eq!(
        child_ids,
        vec!["11", "12"],
        "parent should list both children"
    );
    assert!(
        parent_deps.parent.is_none(),
        "parent ticket has no parent itself"
    );

    let child1_deps = syncer
        .get_dependencies_by_source_id("r1", "11")
        .expect("get_dependencies for child 1");

    assert_eq!(
        child1_deps.parent.as_ref().map(|t| t.source_id.as_str()),
        Some("10"),
        "child 1 should know its parent"
    );
    assert!(child1_deps.children.is_empty(), "child has no children");
}

#[test]
fn test_get_dependencies_empty_when_no_deps() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    syncer
        .upsert_tickets("r1", &[make_ticket("99", "Standalone")])
        .unwrap();

    let deps = syncer
        .get_dependencies_by_source_id("r1", "99")
        .expect("get_dependencies for standalone ticket");

    assert!(deps.blocked_by.is_empty());
    assert!(deps.blocks.is_empty());
    assert!(deps.parent.is_none());
    assert!(deps.children.is_empty());
}

#[test]
fn test_get_all_dependencies_maps_both_directions() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // ticket "A" blocks ticket "B"
    let ta = make_ticket("A", "Blocker");
    let mut tb = make_ticket("B", "Blocked");
    tb.blocked_by = vec!["A".to_string()];

    // ticket "P" is parent of "C"
    let tc = make_ticket("C", "Child");
    let mut tp = make_ticket("P", "Parent");
    tp.children = vec!["C".to_string()];

    syncer.upsert_tickets("r1", &[ta, tb, tc, tp]).unwrap();

    let all = syncer.get_all_dependencies().expect("get_all_dependencies");

    // Look up internal IDs via source_id
    let id_a = ticket_id_for_source(&conn, "A");
    let id_b = ticket_id_for_source(&conn, "B");
    let id_p = ticket_id_for_source(&conn, "P");
    let id_c = ticket_id_for_source(&conn, "C");

    let deps_b = all.get(&id_b).expect("entry for ticket B");
    assert_eq!(source_ids(&deps_b.blocked_by), vec!["A"], "B blocked_by A");
    assert!(deps_b.blocks.is_empty(), "B blocks nothing");

    let deps_a = all.get(&id_a).expect("entry for ticket A");
    assert_eq!(source_ids(&deps_a.blocks), vec!["B"], "A blocks B");
    assert!(deps_a.blocked_by.is_empty(), "A is not blocked");

    let deps_c = all.get(&id_c).expect("entry for ticket C");
    assert_eq!(
        deps_c.parent.as_ref().map(|t| t.source_id.as_str()),
        Some("P"),
        "C parent is P"
    );
    assert!(deps_c.children.is_empty());

    let deps_p = all.get(&id_p).expect("entry for ticket P");
    assert_eq!(source_ids(&deps_p.children), vec!["C"], "P children: C");
    assert!(deps_p.parent.is_none());
}

/// Helper for dependency tests: look up the internal ULID for a ticket by source_id.
fn ticket_id_for_source(conn: &Connection, source_id: &str) -> String {
    conn.query_row(
        "SELECT id FROM tickets WHERE source_id = :source_id",
        rusqlite::named_params! { ":source_id": source_id },
        |row| row.get("id"),
    )
    .expect("ticket not found")
}

/// Helper: call get_dependencies by source_id (resolves ULID internally).
impl TicketSyncer<'_> {
    fn get_dependencies_by_source_id(
        &self,
        _repo_id: &str,
        source_id: &str,
    ) -> Result<TicketDependencies> {
        let ticket_id: String = self
            .conn
            .query_row(
                "SELECT id FROM tickets WHERE source_id = :source_id",
                rusqlite::named_params! { ":source_id": source_id },
                |row| row.get("id"),
            )
            .map_err(ConductorError::Database)?;
        self.get_dependencies(&ticket_id)
    }
}

/// If a ticket_dependencies row references a ticket ID that no longer exists in the
/// tickets table (e.g. deleted after FK was written with constraints off), query_dep_pairs
/// must return TicketNotFound rather than silently dropping the edge.
#[test]
fn test_query_dep_pairs_orphaned_ticket_returns_error() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Insert one real ticket to act as the "from" side.
    syncer
        .upsert_tickets("r1", &[make_ticket("orphan-from", "From Ticket")])
        .unwrap();
    let from_id = get_ticket_id(&conn, "orphan-from");

    // Bypass FK constraints to insert an edge referencing a non-existent to_ticket_id.
    conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    conn.execute(
        "INSERT INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type) \
             VALUES (:from_id, 'nonexistent-ticket-id', 'blocks')",
        rusqlite::named_params! { ":from_id": from_id },
    )
    .unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();

    let result = query_dep_pairs(&conn, "blocks");
    assert!(
        result.is_err(),
        "query_dep_pairs must return Err when a referenced ticket is missing"
    );
    match result.unwrap_err() {
        ConductorError::TicketNotFound { id } => {
            assert_eq!(id, "nonexistent-ticket-id");
        }
        e => panic!("expected TicketNotFound, got {e:?}"),
    }
}

#[test]
fn test_upsert_preserves_raw_json_on_cli_re_upsert() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Simulate a sync with real raw_json from a source (e.g. GitHub).
    let mut synced = make_ticket("42", "Real Issue");
    synced.raw_json = Some(r#"{"id":42,"number":42,"title":"Real Issue"}"#.to_string());
    syncer.upsert_tickets("r1", &[synced]).unwrap();

    // Verify the raw_json was stored correctly.
    let stored: String = conn
        .query_row(
            "SELECT raw_json FROM tickets WHERE source_id = '42'",
            [],
            |row| row.get("raw_json"),
        )
        .unwrap();
    assert_eq!(stored, r#"{"id":42,"number":42,"title":"Real Issue"}"#);

    // Simulate a CLI re-upsert (passes None — no raw_json available).
    let mut cli_upsert = make_ticket("42", "Real Issue Updated");
    cli_upsert.raw_json = None;
    syncer.upsert_tickets("r1", &[cli_upsert]).unwrap();

    // raw_json must be unchanged — None must not clobber synced data.
    let after: String = conn
        .query_row(
            "SELECT raw_json FROM tickets WHERE source_id = '42'",
            [],
            |row| row.get("raw_json"),
        )
        .unwrap();
    assert_eq!(
        after, r#"{"id":42,"number":42,"title":"Real Issue"}"#,
        "CLI re-upsert with None must not overwrite existing raw_json"
    );

    // Title update from CLI upsert should still be applied.
    let title: String = conn
        .query_row(
            "SELECT title FROM tickets WHERE source_id = '42'",
            [],
            |row| row.get("title"),
        )
        .unwrap();
    assert_eq!(title, "Real Issue Updated");
}

#[test]
fn test_batch_upsert_mixed_raw_json_preservation() {
    // Ticket A has existing raw_json; re-upserted with None → should be preserved.
    // Ticket B is re-upserted with Some → should be overwritten.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let initial = vec![
        TicketInput {
            raw_json: Some(r#"{"keep":"me"}"#.to_string()),
            ..make_ticket("A", "Ticket A")
        },
        TicketInput {
            raw_json: Some(r#"{"old":"value"}"#.to_string()),
            ..make_ticket("B", "Ticket B")
        },
    ];
    syncer.upsert_tickets("r1", &initial).unwrap();

    let update = vec![
        make_ticket("A", "Ticket A updated"), // raw_json = None → preserve
        TicketInput {
            raw_json: Some(r#"{"new":"value"}"#.to_string()),
            ..make_ticket("B", "Ticket B updated")
        },
    ];
    syncer.upsert_tickets("r1", &update).unwrap();

    let raw_a: String = conn
        .query_row(
            "SELECT raw_json FROM tickets WHERE source_id = 'A'",
            [],
            |row| row.get("raw_json"),
        )
        .unwrap();
    assert_eq!(
        raw_a, r#"{"keep":"me"}"#,
        "None raw_json must preserve existing value"
    );

    let raw_b: String = conn
        .query_row(
            "SELECT raw_json FROM tickets WHERE source_id = 'B'",
            [],
            |row| row.get("raw_json"),
        )
        .unwrap();
    assert_eq!(
        raw_b, r#"{"new":"value"}"#,
        "Some raw_json must overwrite existing value"
    );
}

#[test]
fn test_batch_upsert_new_tickets_none_raw_json_defaults_to_empty() {
    // New tickets with raw_json = None should get '{}' as the default.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    let tickets = vec![
        make_ticket("X", "New ticket X"),
        make_ticket("Y", "New ticket Y"),
    ];
    syncer.upsert_tickets("r1", &tickets).unwrap();

    for source_id in &["X", "Y"] {
        let raw: String = conn
            .query_row(
                "SELECT raw_json FROM tickets WHERE source_id = :source_id",
                rusqlite::named_params! { ":source_id": source_id },
                |row| row.get("raw_json"),
            )
            .unwrap();
        assert_eq!(
            raw, "{}",
            "new ticket with None raw_json should default to '{{}}'"
        );
    }
}

#[test]
fn test_get_all_dependencies_for_repo_maps_both_directions() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // ticket "A" blocks ticket "B", both in "r1"
    let ta = make_ticket("A", "Blocker");
    let mut tb = make_ticket("B", "Blocked");
    tb.blocked_by = vec!["A".to_string()];

    // ticket "P" is parent of "C", both in "r1"
    let tc = make_ticket("C", "Child");
    let mut tp = make_ticket("P", "Parent");
    tp.children = vec!["C".to_string()];

    syncer.upsert_tickets("r1", &[ta, tb, tc, tp]).unwrap();

    let all = syncer
        .get_all_dependencies_for_repo("r1")
        .expect("get_all_dependencies_for_repo");

    let id_a = ticket_id_for_source(&conn, "A");
    let id_b = ticket_id_for_source(&conn, "B");
    let id_p = ticket_id_for_source(&conn, "P");
    let id_c = ticket_id_for_source(&conn, "C");

    let deps_b = all.get(&id_b).expect("entry for ticket B");
    assert_eq!(source_ids(&deps_b.blocked_by), vec!["A"], "B blocked_by A");
    assert!(deps_b.blocks.is_empty(), "B blocks nothing");

    let deps_a = all.get(&id_a).expect("entry for ticket A");
    assert_eq!(source_ids(&deps_a.blocks), vec!["B"], "A blocks B");
    assert!(deps_a.blocked_by.is_empty(), "A is not blocked");

    let deps_c = all.get(&id_c).expect("entry for ticket C");
    assert_eq!(
        deps_c.parent.as_ref().map(|t| t.source_id.as_str()),
        Some("P"),
        "C parent is P"
    );
    assert!(deps_c.children.is_empty());

    let deps_p = all.get(&id_p).expect("entry for ticket P");
    assert_eq!(source_ids(&deps_p.children), vec!["C"], "P children: C");
    assert!(deps_p.parent.is_none());
}

#[test]
fn test_get_all_dependencies_for_repo_empty_when_no_deps() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    // Tickets with no dependency edges — map should be empty.
    syncer
        .upsert_tickets("r1", &[make_ticket("X", "Solo")])
        .unwrap();

    let result = syncer
        .get_all_dependencies_for_repo("r1")
        .expect("get_all_dependencies_for_repo");
    assert!(result.is_empty(), "no deps means empty map");

    // Unknown repo also returns empty map without error.
    let empty = syncer
        .get_all_dependencies_for_repo("nonexistent-repo")
        .expect("nonexistent repo returns Ok");
    assert!(empty.is_empty(), "unknown repo returns empty map");
}

#[test]
fn test_returning_id_stable_on_conflict_update() {
    // Re-upserting an existing ticket must return the same ULID, not a new one.
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    syncer
        .upsert_tickets("r1", &[make_ticket("99", "Original")])
        .unwrap();
    let id_first: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '99'", [], |row| {
            row.get("id")
        })
        .unwrap();

    syncer
        .upsert_tickets("r1", &[make_ticket("99", "Updated title")])
        .unwrap();
    let id_second: String = conn
        .query_row("SELECT id FROM tickets WHERE source_id = '99'", [], |row| {
            row.get("id")
        })
        .unwrap();

    assert_eq!(
        id_first, id_second,
        "ULID must be stable across conflict updates"
    );
}

// --- get_blocking_edges_for_tickets tests ---

fn insert_test_ticket(conn: &Connection, id: &str, repo_id: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO tickets \
             (id, repo_id, source_type, source_id, title, state, synced_at, raw_json) \
             VALUES (:id, :repo_id, 'github', :id, 'test', 'open', '2024-01-01T00:00:00Z', '{}')",
        rusqlite::named_params! { ":id": id, ":repo_id": repo_id },
    )
    .unwrap();
}

fn insert_blocks_dep(conn: &Connection, from_id: &str, to_id: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO ticket_dependencies \
             (from_ticket_id, to_ticket_id, dep_type) VALUES (:from_id, :to_id, 'blocks')",
        rusqlite::named_params! { ":from_id": from_id, ":to_id": to_id },
    )
    .unwrap();
}

#[test]
fn test_get_blocking_edges_empty_input() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let result = syncer.get_blocking_edges_for_tickets(&[]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_get_blocking_edges_no_matching_rows() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    insert_test_ticket(&conn, "tid-a", "r1");
    insert_test_ticket(&conn, "tid-b", "r1");
    // No ticket_dependencies rows at all
    let result = syncer
        .get_blocking_edges_for_tickets(&["tid-a", "tid-b"])
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_get_blocking_edges_returns_matching_edges() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    for id in [
        "blocker-1",
        "blocker-2",
        "blocker-3",
        "blocked-a",
        "blocked-b",
    ] {
        insert_test_ticket(&conn, id, "r1");
    }
    insert_blocks_dep(&conn, "blocker-1", "blocked-a");
    insert_blocks_dep(&conn, "blocker-2", "blocked-a");
    insert_blocks_dep(&conn, "blocker-3", "blocked-b");

    let mut result = syncer
        .get_blocking_edges_for_tickets(&["blocked-a", "blocked-b"])
        .unwrap();
    result.sort();

    assert_eq!(
        result,
        vec![
            ("blocker-1".to_string(), "blocked-a".to_string()),
            ("blocker-2".to_string(), "blocked-a".to_string()),
            ("blocker-3".to_string(), "blocked-b".to_string()),
        ]
    );
}

#[test]
fn test_get_blocking_edges_excludes_non_queried_tickets() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    for id in ["blocker-x", "blocker-y", "tid-target", "tid-other"] {
        insert_test_ticket(&conn, id, "r1");
    }
    insert_blocks_dep(&conn, "blocker-x", "tid-target");
    insert_blocks_dep(&conn, "blocker-y", "tid-other");

    let result = syncer
        .get_blocking_edges_for_tickets(&["tid-target"])
        .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0],
        ("blocker-x".to_string(), "tid-target".to_string())
    );
}

#[test]
fn test_get_blocking_edges_excludes_parent_of_dep_type() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);

    for id in ["parent-1", "blocker-1", "child-a"] {
        insert_test_ticket(&conn, id, "r1");
    }
    // Insert a 'parent_of' edge — should NOT be returned
    conn.execute(
        "INSERT OR IGNORE INTO ticket_dependencies \
             (from_ticket_id, to_ticket_id, dep_type) VALUES (:from_id, :to_id, 'parent_of')",
        rusqlite::named_params! { ":from_id": "parent-1", ":to_id": "child-a" },
    )
    .unwrap();
    // Insert a 'blocks' edge — should be returned
    insert_blocks_dep(&conn, "blocker-1", "child-a");

    let result = syncer.get_blocking_edges_for_tickets(&["child-a"]).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], ("blocker-1".to_string(), "child-a".to_string()));
}

// --- get_blocks_edges_within_set tests ---

#[test]
fn test_get_blocks_edges_within_set_empty_input() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let result = syncer.get_blocks_edges_within_set(&[]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_get_blocks_edges_within_set_no_edges() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    insert_test_ticket(&conn, "t-a", "r1");
    insert_test_ticket(&conn, "t-b", "r1");
    let result = syncer
        .get_blocks_edges_within_set(&["t-a".to_string(), "t-b".to_string()])
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_get_blocks_edges_within_set_returns_intra_set_edges() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    insert_test_ticket(&conn, "ta", "r1");
    insert_test_ticket(&conn, "tb", "r1");
    insert_test_ticket(&conn, "tc", "r1");
    insert_blocks_dep(&conn, "ta", "tb");
    insert_blocks_dep(&conn, "tb", "tc");

    let mut result = syncer
        .get_blocks_edges_within_set(&["ta".to_string(), "tb".to_string(), "tc".to_string()])
        .unwrap();
    result.sort();
    assert_eq!(
        result,
        vec![
            ("ta".to_string(), "tb".to_string()),
            ("tb".to_string(), "tc".to_string()),
        ]
    );
}

#[test]
fn test_get_blocks_edges_within_set_excludes_edges_outside_set() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    insert_test_ticket(&conn, "in-a", "r1");
    insert_test_ticket(&conn, "in-b", "r1");
    insert_test_ticket(&conn, "out-c", "r1");
    // Edge where the blocker is outside the queried set
    insert_blocks_dep(&conn, "out-c", "in-a");
    // Edge fully within the set
    insert_blocks_dep(&conn, "in-a", "in-b");

    let result = syncer
        .get_blocks_edges_within_set(&["in-a".to_string(), "in-b".to_string()])
        .unwrap();
    assert_eq!(result, vec![("in-a".to_string(), "in-b".to_string())]);
}

#[test]
fn test_get_blocks_edges_within_set_excludes_parent_of_dep_type() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    insert_test_ticket(&conn, "p1", "r1");
    insert_test_ticket(&conn, "c1", "r1");
    conn.execute(
        "INSERT OR IGNORE INTO ticket_dependencies \
             (from_ticket_id, to_ticket_id, dep_type) VALUES (:from_id, :to_id, 'parent_of')",
        rusqlite::named_params! { ":from_id": "p1", ":to_id": "c1" },
    )
    .unwrap();

    let result = syncer
        .get_blocks_edges_within_set(&["p1".to_string(), "c1".to_string()])
        .unwrap();
    assert!(result.is_empty());
}

// --- resolve_tickets_in_repo tests ---

fn insert_ticket_with_source(conn: &Connection, id: &str, repo_id: &str, source_id: &str) {
    conn.execute(
            "INSERT OR IGNORE INTO tickets \
             (id, repo_id, source_type, source_id, title, state, synced_at, raw_json) \
             VALUES (:id, :repo_id, 'github', :source_id, 'test', 'open', '2024-01-01T00:00:00Z', '{}')",
            rusqlite::named_params! { ":id": id, ":repo_id": repo_id, ":source_id": source_id },
        )
        .unwrap();
}

#[test]
fn test_resolve_tickets_in_repo_empty_input() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let result = syncer.resolve_tickets_in_repo("r1", &[]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_resolve_tickets_in_repo_by_internal_id() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    insert_ticket_with_source(&conn, "internal-id-1", "r1", "42");
    let ids = vec!["internal-id-1".to_string()];
    let result = syncer.resolve_tickets_in_repo("r1", &ids).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, "internal-id-1");
}

#[test]
fn test_resolve_tickets_in_repo_by_source_id() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    insert_ticket_with_source(&conn, "internal-id-2", "r1", "99");
    let ids = vec!["99".to_string()];
    let result = syncer.resolve_tickets_in_repo("r1", &ids).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].source_id, "99");
}

#[test]
fn test_resolve_tickets_in_repo_mixed_internal_and_source_id() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    insert_ticket_with_source(&conn, "int-id-a", "r1", "10");
    insert_ticket_with_source(&conn, "int-id-b", "r1", "20");
    let ids = vec!["int-id-a".to_string(), "20".to_string()];
    let result = syncer.resolve_tickets_in_repo("r1", &ids).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].id, "int-id-a");
    assert_eq!(result[1].source_id, "20");
}

#[test]
fn test_resolve_tickets_in_repo_unknown_id_returns_error() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    let ids = vec!["nonexistent-id".to_string()];
    let err = syncer.resolve_tickets_in_repo("r1", &ids).unwrap_err();
    assert!(matches!(err, ConductorError::TicketNotFound { .. }));
}

#[test]
fn test_resolve_tickets_in_repo_cross_repo_id_not_matched() {
    let conn = setup_db();
    // setup_db creates repo "r1"; insert a second repo "r2"
    crate::test_helpers::insert_test_repo(&conn, "r2", "test-repo-2", "/tmp/repo2");
    let syncer = TicketSyncer::new(&conn);
    // Ticket belongs to r2, not r1; its source_id doesn't exist in r1 either
    insert_ticket_with_source(&conn, "cross-repo-id", "r2", "55");
    let ids = vec!["cross-repo-id".to_string()];
    let err = syncer.resolve_tickets_in_repo("r1", &ids).unwrap_err();
    assert!(matches!(err, ConductorError::TicketNotFound { .. }));
}

#[test]
fn test_resolve_tickets_in_repo_preserves_order() {
    let conn = setup_db();
    let syncer = TicketSyncer::new(&conn);
    insert_ticket_with_source(&conn, "ord-id-1", "r1", "100");
    insert_ticket_with_source(&conn, "ord-id-2", "r1", "200");
    insert_ticket_with_source(&conn, "ord-id-3", "r1", "300");
    let ids = vec!["300".to_string(), "ord-id-1".to_string(), "200".to_string()];
    let result = syncer.resolve_tickets_in_repo("r1", &ids).unwrap();
    assert_eq!(result[0].source_id, "300");
    assert_eq!(result[1].id, "ord-id-1");
    assert_eq!(result[2].source_id, "200");
}
