use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use crate::worktree::WorktreeManager;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    pub repo_id: String,
    pub source_type: String,
    pub source_id: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: String,
    pub assignee: Option<String>,
    pub priority: Option<String>,
    pub url: String,
    pub synced_at: String,
    pub raw_json: String,
}

/// A normalized ticket from any source, ready to be upserted into the database.
pub struct TicketInput {
    pub source_type: String,
    pub source_id: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: String,
    pub assignee: Option<String>,
    pub priority: Option<String>,
    pub url: String,
    pub raw_json: String,
    /// Label details (name + color) for populating the ticket_labels join table.
    /// Pass `vec![]` for sources that do not supply color data.
    pub label_details: Vec<TicketLabelInput>,
}

/// Label detail passed in during sync. Carries color alongside the name.
pub struct TicketLabelInput {
    pub name: String,
    pub color: Option<String>,
}

/// A label row from the ticket_labels table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketLabel {
    pub ticket_id: String,
    pub label: String,
    pub color: Option<String>,
}

impl Ticket {
    pub fn matches_filter(&self, query: &str) -> bool {
        self.title.to_lowercase().contains(query)
            || self.source_id.contains(query)
            || self.labels.to_lowercase().contains(query)
    }
}

pub struct TicketSyncer<'a> {
    conn: &'a Connection,
}

const CLOSED_TICKET_ARTIFACTS_SQL: &str = "SELECT r.local_path, w.path, w.branch
     FROM worktrees w
     JOIN repos r ON r.id = w.repo_id
     WHERE w.repo_id = ?1
       AND w.status != 'merged'
       AND w.ticket_id IS NOT NULL
       AND w.ticket_id IN (SELECT id FROM tickets WHERE state = 'closed')";

impl<'a> TicketSyncer<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Upsert a batch of tickets for a repo. Returns the number of tickets upserted.
    pub fn upsert_tickets(&self, repo_id: &str, tickets: &[TicketInput]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let now = Utc::now().to_rfc3339();

        for ticket in tickets {
            let id = ulid::Ulid::new().to_string();
            tx.execute(
                "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(repo_id, source_type, source_id) DO UPDATE SET
                     title = excluded.title,
                     body = excluded.body,
                     state = excluded.state,
                     labels = excluded.labels,
                     assignee = excluded.assignee,
                     priority = excluded.priority,
                     url = excluded.url,
                     synced_at = excluded.synced_at,
                     raw_json = excluded.raw_json",
                params![
                    id,
                    repo_id,
                    ticket.source_type,
                    ticket.source_id,
                    ticket.title,
                    ticket.body,
                    ticket.state,
                    ticket.labels,
                    ticket.assignee,
                    ticket.priority,
                    ticket.url,
                    now,
                    ticket.raw_json,
                ],
            )?;

            let ticket_id: String = tx.query_row(
                "SELECT id FROM tickets WHERE repo_id = ?1 AND source_type = ?2 AND source_id = ?3",
                params![repo_id, ticket.source_type, ticket.source_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "DELETE FROM ticket_labels WHERE ticket_id = ?1",
                params![ticket_id],
            )?;
            for ld in &ticket.label_details {
                tx.execute(
                    "INSERT OR REPLACE INTO ticket_labels (ticket_id, label, color) VALUES (?1, ?2, ?3)",
                    params![ticket_id, ld.name, ld.color],
                )?;
            }
        }

        tx.commit()?;
        Ok(tickets.len())
    }

    /// Mark tickets as closed if they were not in the latest sync batch.
    /// After upserting open tickets, any ticket for the same repo+source_type
    /// that wasn't in the synced set is presumed closed.
    /// Returns the number of tickets marked closed.
    pub fn close_missing_tickets(
        &self,
        repo_id: &str,
        source_type: &str,
        synced_source_ids: &[&str],
    ) -> Result<usize> {
        if synced_source_ids.is_empty() {
            // Nothing was synced — don't mark everything as closed
            return Ok(0);
        }

        let now = Utc::now().to_rfc3339();
        let placeholders: Vec<String> = (0..synced_source_ids.len())
            .map(|i| format!("?{}", i + 4))
            .collect();
        let sql = format!(
            "UPDATE tickets SET state = 'closed', synced_at = ?1
             WHERE repo_id = ?2 AND source_type = ?3
             AND state != 'closed'
             AND source_id NOT IN ({})",
            placeholders.join(", ")
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        param_values.push(Box::new(now));
        param_values.push(Box::new(repo_id.to_string()));
        param_values.push(Box::new(source_type.to_string()));
        for id in synced_source_ids {
            param_values.push(Box::new(id.to_string()));
        }
        let params: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let count = stmt.execute(params.as_slice())?;

        Ok(count)
    }

    /// List tickets, optionally filtered by repo.
    pub fn list(&self, repo_id: Option<&str>) -> Result<Vec<Ticket>> {
        let query = match repo_id {
            Some(_) => {
                "SELECT id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json
                 FROM tickets WHERE repo_id = ?1 ORDER BY synced_at DESC"
            }
            None => {
                "SELECT id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json
                 FROM tickets ORDER BY synced_at DESC"
            }
        };

        let tickets = if let Some(rid) = repo_id {
            query_collect(self.conn, query, params![rid], map_ticket_row)?
        } else {
            query_collect(self.conn, query, [], map_ticket_row)?
        };
        Ok(tickets)
    }

    /// Link a ticket to a worktree.
    /// Returns an error if the worktree already has a linked ticket.
    pub fn link_to_worktree(&self, ticket_id: &str, worktree_id: &str) -> Result<()> {
        let existing: Option<String> = self.conn.query_row(
            "SELECT ticket_id FROM worktrees WHERE id = ?1",
            params![worktree_id],
            |row| row.get(0),
        )?;
        if existing.is_some() {
            return Err(ConductorError::TicketAlreadyLinked);
        }
        self.conn.execute(
            "UPDATE worktrees SET ticket_id = ?1 WHERE id = ?2",
            params![ticket_id, worktree_id],
        )?;
        Ok(())
    }

    /// Fetch a single ticket by its internal (ULID) ID.
    pub fn get_by_id(&self, ticket_id: &str) -> Result<Ticket> {
        self.conn
            .query_row(
                "SELECT id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json
                 FROM tickets WHERE id = ?1",
                params![ticket_id],
                map_ticket_row,
            )
            .map_err(|_| ConductorError::TicketNotFound {
                id: ticket_id.to_string(),
            })
    }

    /// Upsert a batch of synced tickets, close any missing ones, and mark their
    /// worktrees. Returns `(synced, closed)` counts. Errors from the close and
    /// mark steps are logged as warnings rather than propagated, matching the
    /// intent that one source failure should not abort the entire sync.
    pub fn sync_and_close_tickets(
        &self,
        repo_id: &str,
        source_type: &str,
        tickets: &[TicketInput],
    ) -> (usize, usize) {
        let warn_and_default = |result: Result<usize>, ctx: &str| {
            result.unwrap_or_else(|e| {
                warn!("{ctx} failed for {repo_id}: {e}");
                0
            })
        };
        let synced_ids: Vec<&str> = tickets.iter().map(|t| t.source_id.as_str()).collect();
        let synced = warn_and_default(self.upsert_tickets(repo_id, tickets), "upsert_tickets");
        let closed = warn_and_default(
            self.close_missing_tickets(repo_id, source_type, &synced_ids),
            "close_missing_tickets",
        );
        warn_and_default(
            self.mark_worktrees_for_closed_tickets(repo_id),
            "mark_worktrees_for_closed_tickets",
        );
        (synced, closed)
    }

    /// Query the normalized labels for a ticket by its internal (ULID) ID.
    pub fn get_labels(&self, ticket_id: &str) -> Result<Vec<TicketLabel>> {
        query_collect(
            self.conn,
            "SELECT ticket_id, label, color FROM ticket_labels WHERE ticket_id = ?1 ORDER BY label",
            params![ticket_id],
            |row| {
                Ok(TicketLabel {
                    ticket_id: row.get(0)?,
                    label: row.get(1)?,
                    color: row.get(2)?,
                })
            },
        )
    }

    /// After syncing tickets, mark any linked worktrees whose ticket is now
    /// closed by setting their status to `'merged'`. Also removes the git
    /// worktree directory and branch for each affected worktree (best-effort).
    /// Called as part of the ticket sync flow, typically after
    /// [`TicketSyncer::close_missing_tickets`].
    /// Returns the number of worktrees updated.
    pub fn mark_worktrees_for_closed_tickets(&self, repo_id: &str) -> Result<usize> {
        // Collect git paths before updating so we can clean up worktree dirs and branches.
        let artifacts: Vec<(String, String, String)> = query_collect(
            self.conn,
            CLOSED_TICKET_ARTIFACTS_SQL,
            params![repo_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        let now = Utc::now().to_rfc3339();
        let count = self.conn.execute(
            "UPDATE worktrees SET status = 'merged', completed_at = ?2
             WHERE repo_id = ?1
             AND status != 'merged'
             AND ticket_id IS NOT NULL
             AND ticket_id IN (SELECT id FROM tickets WHERE state = 'closed')",
            params![repo_id, now],
        )?;

        for (repo_path, worktree_path, branch) in artifacts {
            WorktreeManager::remove_artifacts(&repo_path, &worktree_path, &branch);
        }

        Ok(count)
    }
}

/// Build a rich agent prompt from a ticket's context.
pub fn build_agent_prompt(ticket: &Ticket) -> String {
    let labels_display = if ticket.labels.is_empty() || ticket.labels == "[]" {
        "None".to_string()
    } else {
        ticket.labels.clone()
    };

    let body_display = if ticket.body.is_empty() {
        "(No description provided)".to_string()
    } else {
        ticket.body.clone()
    };

    format!(
        "Work on the following GitHub issue in this repository.\n\
         \n\
         Issue: #{source_id} — {title}\n\
         State: {state}\n\
         Labels: {labels}\n\
         \n\
         Description:\n\
         {body}\n\
         \n\
         Implement the changes described in the issue. Follow existing code conventions and patterns. Write tests if appropriate.",
        source_id = ticket.source_id,
        title = ticket.title,
        state = ticket.state,
        labels = labels_display,
        body = body_display,
    )
}

fn map_ticket_row(row: &rusqlite::Row) -> rusqlite::Result<Ticket> {
    Ok(Ticket {
        id: row.get(0)?,
        repo_id: row.get(1)?,
        source_type: row.get(2)?,
        source_id: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        state: row.get(6)?,
        labels: row.get(7)?,
        assignee: row.get(8)?,
        priority: row.get(9)?,
        url: row.get(10)?,
        synced_at: row.get(11)?,
        raw_json: row.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

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
            labels: "[]".to_string(),
            assignee: None,
            priority: None,
            url: String::new(),
            raw_json: "{}".to_string(),
            label_details: vec![],
        }
    }

    fn get_ticket_state(conn: &Connection, source_id: &str) -> String {
        conn.query_row(
            "SELECT state FROM tickets WHERE source_id = ?1",
            params![source_id],
            |row| row.get(0),
        )
        .unwrap()
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
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");

        // Second sync: only issue 2 remains open → issue 1 closed, worktree merged
        let second = vec![make_ticket("2", "Issue 2")];
        let (synced2, closed2) = syncer.sync_and_close_tickets("r1", "github", &second);
        assert_eq!(synced2, 1);
        assert_eq!(closed2, 1);
        assert_eq!(get_ticket_state(&conn, "1"), "closed");
        assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
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
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repo1_state, "closed");

        // repo2's ticket should still be open (different repo, unaffected)
        let repo2_state: String = conn
            .query_row(
                "SELECT state FROM tickets WHERE repo_id = 'repo2' AND source_id = '1'",
                [],
                |row| row.get(0),
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
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                repo_id,
                format!("wt-{id}"),
                format!("feat/{id}"),
                format!("/tmp/wt-{id}"),
                ticket_id,
                status,
                "2024-01-01T00:00:00Z",
            ],
        )
        .unwrap();
    }

    fn get_worktree_status(conn: &Connection, id: &str) -> String {
        conn.query_row(
            "SELECT status FROM worktrees WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn test_mark_worktrees_active_to_merged() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");

        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        let count = syncer.mark_worktrees_for_closed_tickets("r1").unwrap();
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
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "abandoned");
        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        let count = syncer.mark_worktrees_for_closed_tickets("r1").unwrap();
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
                row.get(0)
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
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");
        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        // Use the same constant the implementation uses so this test stays in sync.
        let artifacts: Vec<(String, String, String)> = conn
            .prepare(CLOSED_TICKET_ARTIFACTS_SQL)
            .unwrap()
            .query_map(rusqlite::params!["r1"], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
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
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "merged");
        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        // Use the same constant the implementation uses so this test stays in sync.
        let artifacts: Vec<(String, String, String)> = conn
            .prepare(CLOSED_TICKET_ARTIFACTS_SQL)
            .unwrap()
            .query_map(rusqlite::params!["r1"], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
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
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");
        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        let count = syncer.mark_worktrees_for_closed_tickets("r1").unwrap();
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
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");

        // Verify completed_at starts as NULL
        let before: Option<String> = conn
            .query_row(
                "SELECT completed_at FROM worktrees WHERE id = 'wt1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(before.is_none());

        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();
        syncer.mark_worktrees_for_closed_tickets("r1").unwrap();

        let after: Option<String> = conn
            .query_row(
                "SELECT completed_at FROM worktrees WHERE id = 'wt1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            after.is_some(),
            "completed_at must be set when marking as merged"
        );
    }

    #[test]
    fn test_mark_worktrees_idempotent() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
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
                row.get(0)
            })
            .unwrap();
        let tid2: String = conn
            .query_row(
                "SELECT id FROM tickets WHERE repo_id = 'repo2'",
                [],
                |row| row.get(0),
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

        let count = syncer.mark_worktrees_for_closed_tickets("r1").unwrap();
        assert_eq!(count, 1);
        assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
        assert_eq!(get_worktree_status(&conn, "wt2"), "active");
    }

    #[test]
    fn test_link_to_worktree_success() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);
        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", None, "active");

        syncer.link_to_worktree(&ticket_id, "wt1").unwrap();

        let linked: Option<String> = conn
            .query_row(
                "SELECT ticket_id FROM worktrees WHERE id = 'wt1'",
                [],
                |row| row.get(0),
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
                row.get(0)
            })
            .unwrap();
        let tid2: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '2'", [], |row| {
                row.get(0)
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
                row.get(0)
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
        ticket.labels = r#"["bug","enhancement"]"#.to_string();
        syncer.upsert_tickets("r1", &[ticket]).unwrap();

        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
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
                row.get(0)
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
                row.get(0)
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
        };

        let prompt = build_agent_prompt(&ticket);
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
        };

        let prompt = build_agent_prompt(&ticket);
        assert!(prompt.contains("Issue: #7 — Fix typo"));
        assert!(prompt.contains("Labels: None"));
        assert!(prompt.contains("(No description provided)"));
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
}
