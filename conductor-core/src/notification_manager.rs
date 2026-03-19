use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Severity levels for in-app notifications.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSeverity {
    Info,
    Warning,
    ActionRequired,
}

impl fmt::Display for NotificationSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Warning => write!(f, "warning"),
            Self::ActionRequired => write!(f, "action_required"),
        }
    }
}

impl FromStr for NotificationSeverity {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "info" => Ok(Self::Info),
            "warning" => Ok(Self::Warning),
            "action_required" => Ok(Self::ActionRequired),
            other => Err(format!("unknown notification severity: {other}")),
        }
    }
}

impl_sql_enum!(NotificationSeverity);

/// A persistent in-app notification record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub severity: NotificationSeverity,
    pub entity_id: Option<String>,
    pub entity_type: Option<String>,
    pub read: bool,
    pub created_at: String,
    pub read_at: Option<String>,
}

/// Parameters for creating a new notification.
pub struct CreateNotification<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub severity: NotificationSeverity,
    pub entity_id: Option<&'a str>,
    pub entity_type: Option<&'a str>,
}

/// Manages persistent in-app notification records.
pub struct NotificationManager<'a> {
    conn: &'a Connection,
}

impl<'a> NotificationManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Create a new notification record and return its ID.
    pub fn create_notification(&self, params: &CreateNotification<'_>) -> Result<String, String> {
        let id = crate::new_id();
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO notifications (id, kind, title, body, severity, entity_id, entity_type, read, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)",
                rusqlite::params![
                    id,
                    params.kind,
                    params.title,
                    params.body,
                    params.severity.to_string(),
                    params.entity_id,
                    params.entity_type,
                    now,
                ],
            )
            .map_err(|e| format!("create_notification failed: {e}"))?;
        Ok(id)
    }

    /// List unread notifications, most recent first.
    pub fn list_unread(&self) -> Result<Vec<Notification>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind, title, body, severity, entity_id, entity_type, read, created_at, read_at
                 FROM notifications WHERE read = 0 ORDER BY created_at DESC",
            )
            .map_err(|e| format!("list_unread prepare failed: {e}"))?;
        let rows = stmt
            .query_map([], row_to_notification)
            .map_err(|e| format!("list_unread query failed: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("list_unread collect failed: {e}"))
    }

    /// List recent notifications (read and unread), most recent first.
    pub fn list_recent(&self, limit: usize, offset: usize) -> Result<Vec<Notification>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind, title, body, severity, entity_id, entity_type, read, created_at, read_at
                 FROM notifications ORDER BY created_at DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| format!("list_recent prepare failed: {e}"))?;
        let rows = stmt
            .query_map(
                rusqlite::params![limit as i64, offset as i64],
                row_to_notification,
            )
            .map_err(|e| format!("list_recent query failed: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("list_recent collect failed: {e}"))
    }

    /// Mark a single notification as read.
    pub fn mark_read(&self, id: &str) -> Result<(), String> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "UPDATE notifications SET read = 1, read_at = ?1 WHERE id = ?2",
                rusqlite::params![now, id],
            )
            .map_err(|e| format!("mark_read failed: {e}"))?;
        Ok(())
    }

    /// Mark all unread notifications as read.
    pub fn mark_all_read(&self) -> Result<(), String> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "UPDATE notifications SET read = 1, read_at = ?1 WHERE read = 0",
                rusqlite::params![now],
            )
            .map_err(|e| format!("mark_all_read failed: {e}"))?;
        Ok(())
    }

    /// Count unread notifications.
    pub fn unread_count(&self) -> Result<usize, String> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM notifications WHERE read = 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c as usize)
            .map_err(|e| format!("unread_count failed: {e}"))
    }
}

fn row_to_notification(row: &rusqlite::Row<'_>) -> rusqlite::Result<Notification> {
    let read_int: i64 = row.get(7)?;
    Ok(Notification {
        id: row.get(0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        body: row.get(3)?,
        severity: row.get(4)?,
        entity_id: row.get(5)?,
        entity_type: row.get(6)?,
        read: read_int != 0,
        created_at: row.get(8)?,
        read_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE notifications (
                id          TEXT PRIMARY KEY,
                kind        TEXT NOT NULL,
                title       TEXT NOT NULL,
                body        TEXT NOT NULL,
                severity    TEXT NOT NULL DEFAULT 'info',
                entity_id   TEXT,
                entity_type TEXT,
                read        INTEGER NOT NULL DEFAULT 0,
                created_at  TEXT NOT NULL,
                read_at     TEXT
            );
            CREATE INDEX idx_notifications_read ON notifications(read);
            CREATE INDEX idx_notifications_created_at ON notifications(created_at);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn create_and_list_unread() {
        let conn = test_db();
        let mgr = NotificationManager::new(&conn);
        let id = mgr
            .create_notification(&CreateNotification {
                kind: "workflow_completed",
                title: "Workflow Finished",
                body: "deploy completed",
                severity: NotificationSeverity::Info,
                entity_id: Some("run-1"),
                entity_type: Some("workflow_run"),
            })
            .unwrap();
        assert!(!id.is_empty());

        let unread = mgr.list_unread().unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].kind, "workflow_completed");
        assert!(!unread[0].read);
    }

    #[test]
    fn mark_read_removes_from_unread() {
        let conn = test_db();
        let mgr = NotificationManager::new(&conn);
        let id = mgr
            .create_notification(&CreateNotification {
                kind: "workflow_failed",
                title: "Workflow Failed",
                body: "deploy failed",
                severity: NotificationSeverity::ActionRequired,
                entity_id: None,
                entity_type: None,
            })
            .unwrap();

        mgr.mark_read(&id).unwrap();

        let unread = mgr.list_unread().unwrap();
        assert_eq!(unread.len(), 0);

        let recent = mgr.list_recent(10, 0).unwrap();
        assert_eq!(recent.len(), 1);
        assert!(recent[0].read);
        assert!(recent[0].read_at.is_some());
    }

    #[test]
    fn mark_all_read() {
        let conn = test_db();
        let mgr = NotificationManager::new(&conn);
        for i in 0..3 {
            mgr.create_notification(&CreateNotification {
                kind: "test",
                title: &format!("Test {i}"),
                body: "body",
                severity: NotificationSeverity::Info,
                entity_id: None,
                entity_type: None,
            })
            .unwrap();
        }

        assert_eq!(mgr.unread_count().unwrap(), 3);
        mgr.mark_all_read().unwrap();
        assert_eq!(mgr.unread_count().unwrap(), 0);
    }

    #[test]
    fn unread_count() {
        let conn = test_db();
        let mgr = NotificationManager::new(&conn);
        assert_eq!(mgr.unread_count().unwrap(), 0);

        let id = mgr
            .create_notification(&CreateNotification {
                kind: "test",
                title: "Test",
                body: "body",
                severity: NotificationSeverity::Warning,
                entity_id: None,
                entity_type: None,
            })
            .unwrap();
        assert_eq!(mgr.unread_count().unwrap(), 1);

        mgr.mark_read(&id).unwrap();
        assert_eq!(mgr.unread_count().unwrap(), 0);
    }

    #[test]
    fn list_recent_with_limit_and_offset() {
        let conn = test_db();
        let mgr = NotificationManager::new(&conn);
        for i in 0..5 {
            mgr.create_notification(&CreateNotification {
                kind: "test",
                title: &format!("N{i}"),
                body: "body",
                severity: NotificationSeverity::Info,
                entity_id: None,
                entity_type: None,
            })
            .unwrap();
        }

        let page1 = mgr.list_recent(2, 0).unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = mgr.list_recent(2, 2).unwrap();
        assert_eq!(page2.len(), 2);

        let page3 = mgr.list_recent(2, 4).unwrap();
        assert_eq!(page3.len(), 1);
    }

    #[test]
    fn severity_roundtrip() {
        assert_eq!(
            "info".parse::<NotificationSeverity>().unwrap(),
            NotificationSeverity::Info
        );
        assert_eq!(
            "warning".parse::<NotificationSeverity>().unwrap(),
            NotificationSeverity::Warning
        );
        assert_eq!(
            "action_required".parse::<NotificationSeverity>().unwrap(),
            NotificationSeverity::ActionRequired
        );
        assert!("unknown".parse::<NotificationSeverity>().is_err());
    }
}
