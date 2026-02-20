use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

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
}

pub struct TicketSyncer<'a> {
    conn: &'a Connection,
}

impl<'a> TicketSyncer<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Upsert a batch of tickets for a repo. Returns the number of tickets upserted.
    pub fn upsert_tickets(&self, repo_id: &str, tickets: &[TicketInput]) -> Result<usize> {
        let now = Utc::now().to_rfc3339();

        for ticket in tickets {
            let id = ulid::Ulid::new().to_string();
            self.conn.execute(
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
        }

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

        let mut stmt = self.conn.prepare(query)?;
        let rows = if let Some(rid) = repo_id {
            stmt.query_map(params![rid], map_ticket_row)?
        } else {
            stmt.query_map([], map_ticket_row)?
        };

        let tickets = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(tickets)
    }

    /// Link a ticket to a worktree.
    pub fn link_to_worktree(&self, ticket_id: &str, worktree_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE worktrees SET ticket_id = ?1 WHERE id = ?2",
            params![ticket_id, worktree_id],
        )?;
        Ok(())
    }
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
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE repos (
                id TEXT PRIMARY KEY,
                slug TEXT NOT NULL UNIQUE,
                local_path TEXT NOT NULL,
                remote_url TEXT NOT NULL,
                default_branch TEXT NOT NULL DEFAULT 'main',
                workspace_dir TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE tickets (
                id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL REFERENCES repos(id),
                source_type TEXT NOT NULL,
                source_id TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT NOT NULL DEFAULT '',
                state TEXT NOT NULL DEFAULT 'open',
                labels TEXT NOT NULL DEFAULT '[]',
                assignee TEXT,
                priority TEXT,
                url TEXT NOT NULL DEFAULT '',
                synced_at TEXT NOT NULL,
                raw_json TEXT NOT NULL DEFAULT '{}',
                UNIQUE(repo_id, source_type, source_id)
            );
            INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
            VALUES ('repo1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo', '/tmp/ws', '2024-01-01T00:00:00Z');",
        )
        .unwrap();
        conn
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
    fn test_close_missing_tickets_marks_absent_as_closed() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Sync #1: issues 1, 2, 3 are open
        let tickets = vec![
            make_ticket("1", "Issue 1"),
            make_ticket("2", "Issue 2"),
            make_ticket("3", "Issue 3"),
        ];
        syncer.upsert_tickets("repo1", &tickets).unwrap();

        // Sync #2: only issues 1, 3 are open (issue 2 was closed on GitHub)
        let tickets2 = vec![make_ticket("1", "Issue 1"), make_ticket("3", "Issue 3")];
        let synced_ids: Vec<&str> = tickets2.iter().map(|t| t.source_id.as_str()).collect();
        syncer.upsert_tickets("repo1", &tickets2).unwrap();
        let closed = syncer
            .close_missing_tickets("repo1", "github", &synced_ids)
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
        syncer.upsert_tickets("repo1", &tickets).unwrap();

        // Sync #2: only issue 1 open → issue 2 closed
        let synced_ids = vec!["1"];
        syncer
            .close_missing_tickets("repo1", "github", &synced_ids)
            .unwrap();
        assert_eq!(get_ticket_state(&conn, "2"), "closed");

        // Sync #3: still only issue 1 open → issue 2 already closed, count should be 0
        let closed = syncer
            .close_missing_tickets("repo1", "github", &synced_ids)
            .unwrap();
        assert_eq!(closed, 0);
    }

    #[test]
    fn test_close_missing_empty_sync_is_noop() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Sync existing tickets
        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("repo1", &tickets).unwrap();

        // Empty sync should not close anything (protects against API failures)
        let closed = syncer
            .close_missing_tickets("repo1", "github", &[])
            .unwrap();
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
        syncer.upsert_tickets("repo1", &tickets1).unwrap();
        syncer.upsert_tickets("repo2", &tickets2).unwrap();

        // Sync repo1 with no open issues → only repo1's ticket should close
        let closed = syncer
            .close_missing_tickets("repo1", "github", &["999"])
            .unwrap();
        assert_eq!(closed, 1);

        // repo1's ticket should be closed
        let repo1_state: String = conn
            .query_row(
                "SELECT state FROM tickets WHERE repo_id = 'repo1' AND source_id = '1'",
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
}
