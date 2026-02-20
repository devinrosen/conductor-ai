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
