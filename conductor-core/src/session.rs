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
}
