use std::fmt;
use std::str::FromStr;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::db::query_collect;
use crate::error::{ConductorError, Result};

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub id: String,
    pub repo_id: String,
    pub name: String,
    pub description: String,
    pub status: MilestoneStatus,
    pub target_date: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MilestoneStatus {
    Planned,
    InProgress,
    Completed,
    Blocked,
}

impl fmt::Display for MilestoneStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Planned => write!(f, "planned"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Completed => write!(f, "completed"),
            Self::Blocked => write!(f, "blocked"),
        }
    }
}

impl FromStr for MilestoneStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "planned" => Ok(Self::Planned),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            "blocked" => Ok(Self::Blocked),
            other => Err(format!("unknown milestone status: {other}")),
        }
    }
}

crate::impl_sql_enum!(MilestoneStatus);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deliverable {
    pub id: String,
    pub milestone_id: String,
    pub name: String,
    pub description: String,
    pub status: DeliverableStatus,
    pub feature_id: Option<String>,
    pub review_status: ReviewStatus,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliverableStatus {
    Planned,
    InProgress,
    Completed,
    Blocked,
}

impl fmt::Display for DeliverableStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Planned => write!(f, "planned"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Completed => write!(f, "completed"),
            Self::Blocked => write!(f, "blocked"),
        }
    }
}

impl FromStr for DeliverableStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "planned" => Ok(Self::Planned),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            "blocked" => Ok(Self::Blocked),
            other => Err(format!("unknown deliverable status: {other}")),
        }
    }
}

crate::impl_sql_enum!(DeliverableStatus);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewStatus {
    Pending,
    InReview,
    Approved,
    Rejected,
}

impl fmt::Display for ReviewStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::InReview => write!(f, "in_review"),
            Self::Approved => write!(f, "approved"),
            Self::Rejected => write!(f, "rejected"),
        }
    }
}

impl FromStr for ReviewStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "in_review" => Ok(Self::InReview),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            other => Err(format!("unknown review status: {other}")),
        }
    }
}

crate::impl_sql_enum!(ReviewStatus);

/// Summary of milestone progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MilestoneProgress {
    pub milestone: Milestone,
    pub total_deliverables: i64,
    pub completed_deliverables: i64,
    pub approved_deliverables: i64,
}

// ---------------------------------------------------------------------------
// Row mappers
// ---------------------------------------------------------------------------

fn map_milestone_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Milestone> {
    Ok(Milestone {
        id: row.get(0)?,
        repo_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        status: row.get(4)?,
        target_date: row.get(5)?,
        created_at: row.get(6)?,
        completed_at: row.get(7)?,
    })
}

fn map_deliverable_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Deliverable> {
    Ok(Deliverable {
        id: row.get(0)?,
        milestone_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        status: row.get(4)?,
        feature_id: row.get(5)?,
        review_status: row.get(6)?,
        created_at: row.get(7)?,
        completed_at: row.get(8)?,
    })
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

pub struct MilestoneManager<'a> {
    conn: &'a Connection,
    #[allow(dead_code)]
    config: &'a Config,
}

impl<'a> MilestoneManager<'a> {
    pub fn new(conn: &'a Connection, config: &'a Config) -> Self {
        Self { conn, config }
    }

    // -- Milestone CRUD ---------------------------------------------------

    pub fn create_milestone(
        &self,
        repo_id: &str,
        name: &str,
        description: &str,
        target_date: Option<&str>,
    ) -> Result<Milestone> {
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO milestones (id, repo_id, name, description, target_date, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, repo_id, name, description, target_date, now],
        )?;
        self.get_milestone_by_id(&id)
    }

    pub fn get_milestone_by_id(&self, id: &str) -> Result<Milestone> {
        self.conn
            .query_row(
                "SELECT id, repo_id, name, description, status, target_date, created_at, completed_at \
                 FROM milestones WHERE id = ?1",
                params![id],
                map_milestone_row,
            )
            .optional()?
            .ok_or_else(|| ConductorError::InvalidInput(format!("milestone not found: {id}")))
    }

    pub fn get_milestone_by_name(&self, repo_id: &str, name: &str) -> Result<Milestone> {
        self.conn
            .query_row(
                "SELECT id, repo_id, name, description, status, target_date, created_at, completed_at \
                 FROM milestones WHERE repo_id = ?1 AND name = ?2",
                params![repo_id, name],
                map_milestone_row,
            )
            .optional()?
            .ok_or_else(|| ConductorError::InvalidInput(format!("milestone not found: {name}")))
    }

    pub fn list_milestones(&self, repo_id: &str) -> Result<Vec<Milestone>> {
        query_collect(
            self.conn,
            "SELECT id, repo_id, name, description, status, target_date, created_at, completed_at \
             FROM milestones WHERE repo_id = ?1 ORDER BY created_at DESC",
            params![repo_id],
            map_milestone_row,
        )
    }

    pub fn update_milestone_status(&self, id: &str, status: MilestoneStatus) -> Result<()> {
        let completed_at = if status == MilestoneStatus::Completed {
            Some(Utc::now().to_rfc3339())
        } else {
            None
        };
        self.conn.execute(
            "UPDATE milestones SET status = ?1, completed_at = ?2 WHERE id = ?3",
            params![status, completed_at, id],
        )?;
        Ok(())
    }

    pub fn delete_milestone(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM milestones WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Get milestone progress summary.
    pub fn milestone_progress(&self, id: &str) -> Result<MilestoneProgress> {
        let milestone = self.get_milestone_by_id(id)?;
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM deliverables WHERE milestone_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        let completed: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM deliverables WHERE milestone_id = ?1 AND status = 'completed'",
            params![id],
            |r| r.get(0),
        )?;
        let approved: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM deliverables WHERE milestone_id = ?1 AND review_status = 'approved'",
            params![id],
            |r| r.get(0),
        )?;
        Ok(MilestoneProgress {
            milestone,
            total_deliverables: total,
            completed_deliverables: completed,
            approved_deliverables: approved,
        })
    }

    // -- Deliverable CRUD -------------------------------------------------

    pub fn create_deliverable(
        &self,
        milestone_id: &str,
        name: &str,
        description: &str,
        feature_id: Option<&str>,
    ) -> Result<Deliverable> {
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO deliverables (id, milestone_id, name, description, feature_id, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, milestone_id, name, description, feature_id, now],
        )?;
        self.get_deliverable_by_id(&id)
    }

    pub fn get_deliverable_by_id(&self, id: &str) -> Result<Deliverable> {
        self.conn
            .query_row(
                "SELECT id, milestone_id, name, description, status, feature_id, review_status, \
                 created_at, completed_at FROM deliverables WHERE id = ?1",
                params![id],
                map_deliverable_row,
            )
            .optional()?
            .ok_or_else(|| ConductorError::InvalidInput(format!("deliverable not found: {id}")))
    }

    pub fn list_deliverables(&self, milestone_id: &str) -> Result<Vec<Deliverable>> {
        query_collect(
            self.conn,
            "SELECT id, milestone_id, name, description, status, feature_id, review_status, \
             created_at, completed_at FROM deliverables WHERE milestone_id = ?1 \
             ORDER BY created_at ASC",
            params![milestone_id],
            map_deliverable_row,
        )
    }

    pub fn update_deliverable_status(&self, id: &str, status: DeliverableStatus) -> Result<()> {
        let completed_at = if status == DeliverableStatus::Completed {
            Some(Utc::now().to_rfc3339())
        } else {
            None
        };
        self.conn.execute(
            "UPDATE deliverables SET status = ?1, completed_at = ?2 WHERE id = ?3",
            params![status, completed_at, id],
        )?;
        Ok(())
    }

    pub fn update_deliverable_review_status(
        &self,
        id: &str,
        review_status: ReviewStatus,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE deliverables SET review_status = ?1 WHERE id = ?2",
            params![review_status, id],
        )?;
        Ok(())
    }

    pub fn delete_deliverable(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM deliverables WHERE id = ?1", params![id])?;
        Ok(())
    }

    // -- Deliverable-ticket links -----------------------------------------

    pub fn link_ticket(&self, deliverable_id: &str, ticket_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO deliverable_tickets (deliverable_id, ticket_id) \
             VALUES (?1, ?2)",
            params![deliverable_id, ticket_id],
        )?;
        Ok(())
    }

    pub fn unlink_ticket(&self, deliverable_id: &str, ticket_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM deliverable_tickets WHERE deliverable_id = ?1 AND ticket_id = ?2",
            params![deliverable_id, ticket_id],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        crate::test_helpers::setup_db()
    }

    #[test]
    fn test_milestone_crud() {
        let conn = setup_db();
        let repo_id = "r1"; // from test_helpers::setup_db()
        let config = Config::default();
        let mgr = MilestoneManager::new(&conn, &config);

        // Create
        let ms = mgr
            .create_milestone(
                repo_id,
                "Desktop App v1",
                "First desktop release",
                Some("2026-06-01"),
            )
            .unwrap();
        assert_eq!(ms.name, "Desktop App v1");
        assert_eq!(ms.status, MilestoneStatus::Planned);
        assert_eq!(ms.target_date.as_deref(), Some("2026-06-01"));

        // Get by id
        let ms2 = mgr.get_milestone_by_id(&ms.id).unwrap();
        assert_eq!(ms2.name, "Desktop App v1");

        // Get by name
        let ms3 = mgr
            .get_milestone_by_name(repo_id, "Desktop App v1")
            .unwrap();
        assert_eq!(ms3.id, ms.id);

        // List
        let list = mgr.list_milestones(repo_id).unwrap();
        assert_eq!(list.len(), 1);

        // Update status
        mgr.update_milestone_status(&ms.id, MilestoneStatus::InProgress)
            .unwrap();
        let ms4 = mgr.get_milestone_by_id(&ms.id).unwrap();
        assert_eq!(ms4.status, MilestoneStatus::InProgress);

        // Delete
        mgr.delete_milestone(&ms.id).unwrap();
        let list = mgr.list_milestones(repo_id).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_deliverable_crud() {
        let conn = setup_db();
        let repo_id = "r1";
        let config = Config::default();
        let mgr = MilestoneManager::new(&conn, &config);

        let ms = mgr.create_milestone(repo_id, "M1", "", None).unwrap();

        // Create deliverable
        let d = mgr
            .create_deliverable(&ms.id, "Tauri setup", "Initial Tauri crate", None)
            .unwrap();
        assert_eq!(d.name, "Tauri setup");
        assert_eq!(d.status, DeliverableStatus::Planned);
        assert_eq!(d.review_status, ReviewStatus::Pending);

        // List
        let list = mgr.list_deliverables(&ms.id).unwrap();
        assert_eq!(list.len(), 1);

        // Update status
        mgr.update_deliverable_status(&d.id, DeliverableStatus::InProgress)
            .unwrap();
        let d2 = mgr.get_deliverable_by_id(&d.id).unwrap();
        assert_eq!(d2.status, DeliverableStatus::InProgress);

        // Update review status
        mgr.update_deliverable_review_status(&d.id, ReviewStatus::Approved)
            .unwrap();
        let d3 = mgr.get_deliverable_by_id(&d.id).unwrap();
        assert_eq!(d3.review_status, ReviewStatus::Approved);

        // Delete
        mgr.delete_deliverable(&d.id).unwrap();
        let list = mgr.list_deliverables(&ms.id).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_milestone_progress() {
        let conn = setup_db();
        let repo_id = "r1";
        let config = Config::default();
        let mgr = MilestoneManager::new(&conn, &config);

        let ms = mgr.create_milestone(repo_id, "M1", "", None).unwrap();
        let d1 = mgr.create_deliverable(&ms.id, "D1", "", None).unwrap();
        let d2 = mgr.create_deliverable(&ms.id, "D2", "", None).unwrap();

        mgr.update_deliverable_status(&d1.id, DeliverableStatus::Completed)
            .unwrap();
        mgr.update_deliverable_review_status(&d1.id, ReviewStatus::Approved)
            .unwrap();

        let progress = mgr.milestone_progress(&ms.id).unwrap();
        assert_eq!(progress.total_deliverables, 2);
        assert_eq!(progress.completed_deliverables, 1);
        assert_eq!(progress.approved_deliverables, 1);

        // Suppress unused variable warnings
        let _ = d2;
    }
}
