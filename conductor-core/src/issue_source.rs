use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::db::query_collect;
use crate::error::{ConductorError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSource {
    pub id: String,
    pub repo_id: String,
    pub source_type: String,
    pub config_json: String,
}

/// Configuration for a GitHub issue source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubConfig {
    pub owner: String,
    pub repo: String,
}

/// Configuration for a Jira issue source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraConfig {
    pub jql: String,
    pub url: String,
}

/// Configuration for a Vantage (SDLC) issue source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VantageConfig {
    pub project_id: String,
    pub sdlc_root: String,
}

pub struct IssueSourceManager<'a> {
    conn: &'a Connection,
}

impl<'a> IssueSourceManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Add an issue source for a repo. Rejects duplicates (same repo + source_type).
    pub fn add(
        &self,
        repo_id: &str,
        source_type: &str,
        config_json: &str,
        repo_slug: &str,
    ) -> Result<IssueSource> {
        // Check for existing source of same type for this repo
        let exists: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM repo_issue_sources WHERE repo_id = ?1 AND source_type = ?2",
            params![repo_id, source_type],
            |row| row.get(0),
        )?;

        if exists {
            return Err(ConductorError::IssueSourceAlreadyExists {
                repo_slug: repo_slug.to_string(),
                source_type: source_type.to_string(),
            });
        }

        let id = crate::new_id();
        self.conn.execute(
            "INSERT INTO repo_issue_sources (id, repo_id, source_type, config_json) VALUES (?1, ?2, ?3, ?4)",
            params![id, repo_id, source_type, config_json],
        )?;

        Ok(IssueSource {
            id,
            repo_id: repo_id.to_string(),
            source_type: source_type.to_string(),
            config_json: config_json.to_string(),
        })
    }

    /// List all issue sources for a repo.
    pub fn list(&self, repo_id: &str) -> Result<Vec<IssueSource>> {
        query_collect(
            self.conn,
            "SELECT id, repo_id, source_type, config_json FROM repo_issue_sources WHERE repo_id = ?1",
            params![repo_id],
            |row| {
                Ok(IssueSource {
                    id: row.get(0)?,
                    repo_id: row.get(1)?,
                    source_type: row.get(2)?,
                    config_json: row.get(3)?,
                })
            },
        )
    }

    /// Remove an issue source by ID.
    pub fn remove(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM repo_issue_sources WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Remove an issue source by repo_id and source_type.
    pub fn remove_by_type(&self, repo_id: &str, source_type: &str) -> Result<bool> {
        let count = self.conn.execute(
            "DELETE FROM repo_issue_sources WHERE repo_id = ?1 AND source_type = ?2",
            params![repo_id, source_type],
        )?;
        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        crate::test_helpers::setup_db()
    }

    #[test]
    fn test_add_and_list_source() {
        let conn = setup_db();
        let mgr = IssueSourceManager::new(&conn);

        let source = mgr
            .add(
                "r1",
                "github",
                r#"{"owner":"test","repo":"repo"}"#,
                "test-repo",
            )
            .unwrap();

        assert_eq!(source.repo_id, "r1");
        assert_eq!(source.source_type, "github");

        let sources = mgr.list("r1").unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_type, "github");
    }

    #[test]
    fn test_reject_duplicate_source_type() {
        let conn = setup_db();
        let mgr = IssueSourceManager::new(&conn);

        mgr.add(
            "r1",
            "github",
            r#"{"owner":"test","repo":"repo"}"#,
            "test-repo",
        )
        .unwrap();

        let result = mgr.add(
            "r1",
            "github",
            r#"{"owner":"other","repo":"other"}"#,
            "test-repo",
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_source() {
        let conn = setup_db();
        let mgr = IssueSourceManager::new(&conn);

        let source = mgr
            .add(
                "r1",
                "github",
                r#"{"owner":"test","repo":"repo"}"#,
                "test-repo",
            )
            .unwrap();

        mgr.remove(&source.id).unwrap();

        let sources = mgr.list("r1").unwrap();
        assert!(sources.is_empty());
    }

    #[test]
    fn test_remove_by_type() {
        let conn = setup_db();
        let mgr = IssueSourceManager::new(&conn);

        mgr.add(
            "r1",
            "github",
            r#"{"owner":"test","repo":"repo"}"#,
            "test-repo",
        )
        .unwrap();

        let removed = mgr.remove_by_type("r1", "github").unwrap();
        assert!(removed);

        let sources = mgr.list("r1").unwrap();
        assert!(sources.is_empty());

        // Removing again returns false
        let removed = mgr.remove_by_type("r1", "github").unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_list_empty() {
        let conn = setup_db();
        let mgr = IssueSourceManager::new(&conn);

        let sources = mgr.list("r1").unwrap();
        assert!(sources.is_empty());
    }

    #[test]
    fn test_different_source_types_allowed() {
        let conn = setup_db();
        let mgr = IssueSourceManager::new(&conn);

        mgr.add(
            "r1",
            "github",
            r#"{"owner":"test","repo":"repo"}"#,
            "test-repo",
        )
        .unwrap();

        mgr.add(
            "r1",
            "jira",
            r#"{"jql":"project = TEST","url":"https://jira.example.com"}"#,
            "test-repo",
        )
        .unwrap();

        let sources = mgr.list("r1").unwrap();
        assert_eq!(sources.len(), 2);
    }
}
