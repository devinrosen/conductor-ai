use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub notes: Option<String>,
}

pub struct SessionTracker<'a> {
    conn: &'a Connection,
}

impl<'a> SessionTracker<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn start(&self) -> Result<Session> {
        let id = ulid::Ulid::new().to_string();
        let now = Utc::now().to_rfc3339();

        let session = Session {
            id: id.clone(),
            started_at: now.clone(),
            ended_at: None,
            notes: None,
        };

        self.conn.execute(
            "INSERT INTO sessions (id, started_at) VALUES (?1, ?2)",
            params![session.id, session.started_at],
        )?;

        Ok(session)
    }

    pub fn end(&self, session_id: &str, notes: Option<&str>) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?1, notes = ?2 WHERE id = ?3",
            params![now, notes, session_id],
        )?;
        Ok(())
    }

    pub fn current(&self) -> Result<Option<Session>> {
        let result = self.conn.query_row(
            "SELECT id, started_at, ended_at, notes FROM sessions WHERE ended_at IS NULL ORDER BY started_at DESC LIMIT 1",
            [],
            |row| {
                Ok(Session {
                    id: row.get(0)?,
                    started_at: row.get(1)?,
                    ended_at: row.get(2)?,
                    notes: row.get(3)?,
                })
            },
        );

        match result {
            Ok(session) => Ok(Some(session)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn add_worktree(&self, session_id: &str, worktree_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO session_worktrees (session_id, worktree_id) VALUES (?1, ?2)",
            params![session_id, worktree_id],
        )?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, ended_at, notes FROM sessions ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Session {
                id: row.get(0)?,
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
                notes: row.get(3)?,
            })
        })?;
        let sessions = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    pub fn get_worktrees(&self, session_id: &str) -> Result<Vec<crate::worktree::Worktree>> {
        let mut stmt = self.conn.prepare(
            "SELECT w.id, w.repo_id, w.slug, w.branch, w.path, w.ticket_id, w.status, w.created_at, w.completed_at
             FROM worktrees w
             JOIN session_worktrees sw ON sw.worktree_id = w.id
             WHERE sw.session_id = ?1
             ORDER BY w.created_at",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(crate::worktree::Worktree {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                slug: row.get(2)?,
                branch: row.get(3)?,
                path: row.get(4)?,
                ticket_id: row.get(5)?,
                status: row.get(6)?,
                created_at: row.get(7)?,
                completed_at: row.get(8)?,
            })
        })?;
        let worktrees = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(worktrees)
    }
}
