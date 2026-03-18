use std::fmt;
use std::process::Command;
use std::str::FromStr;

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use crate::repo::RepoManager;
use crate::worktree::{check_output, git_in};

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    pub id: String,
    pub repo_id: String,
    pub name: String,
    pub branch: String,
    pub base_branch: String,
    pub status: FeatureStatus,
    pub created_at: String,
    pub merged_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeatureStatus {
    Active,
    Merged,
    Closed,
}

impl fmt::Display for FeatureStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Merged => write!(f, "merged"),
            Self::Closed => write!(f, "closed"),
        }
    }
}

impl FromStr for FeatureStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "merged" => Ok(Self::Merged),
            "closed" => Ok(Self::Closed),
            other => Err(format!("unknown feature status: {other}")),
        }
    }
}

crate::impl_sql_enum!(FeatureStatus);

/// Summary row returned by `list()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureRow {
    pub id: String,
    pub name: String,
    pub branch: String,
    pub base_branch: String,
    pub status: FeatureStatus,
    pub created_at: String,
    pub worktree_count: i64,
    pub ticket_count: i64,
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

pub struct FeatureManager<'a> {
    conn: &'a Connection,
    config: &'a Config,
}

impl<'a> FeatureManager<'a> {
    pub fn new(conn: &'a Connection, config: &'a Config) -> Self {
        Self { conn, config }
    }

    /// Create a feature: insert DB record, create git branch, push to origin,
    /// and optionally link tickets.
    pub fn create(
        &self,
        repo_slug: &str,
        name: &str,
        from_branch: Option<&str>,
        ticket_ids: &[String],
    ) -> Result<Feature> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;

        // Check for duplicate
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM features WHERE repo_id = ?1 AND name = ?2)",
            params![repo.id, name],
            |row| row.get(0),
        )?;
        if exists {
            return Err(ConductorError::FeatureAlreadyExists {
                name: name.to_string(),
            });
        }

        // Derive branch name
        let branch = if name.contains('/') {
            name.to_string()
        } else {
            format!("feat/{name}")
        };

        let base = from_branch
            .map(|b| b.to_string())
            .unwrap_or_else(|| repo.default_branch.clone());

        // Create git branch and push
        check_output(git_in(&repo.local_path).args([
            "branch",
            "--",
            &branch,
            &format!("refs/heads/{base}"),
        ]))?;
        check_output(git_in(&repo.local_path).args(["push", "-u", "origin", &branch]))?;

        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        let feature = Feature {
            id: id.clone(),
            repo_id: repo.id.clone(),
            name: name.to_string(),
            branch,
            base_branch: base,
            status: FeatureStatus::Active,
            created_at: now,
            merged_at: None,
        };

        self.conn.execute(
            "INSERT INTO features (id, repo_id, name, branch, base_branch, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                feature.id,
                feature.repo_id,
                feature.name,
                feature.branch,
                feature.base_branch,
                feature.status,
                feature.created_at,
            ],
        )?;

        // Link tickets if provided
        if !ticket_ids.is_empty() {
            self.link_tickets_internal(&repo.id, &feature.id, ticket_ids)?;
        }

        Ok(feature)
    }

    /// List features for a repo with worktree and ticket counts.
    pub fn list(&self, repo_slug: &str) -> Result<Vec<FeatureRow>> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;

        query_collect(
            self.conn,
            "SELECT f.id, f.name, f.branch, f.base_branch, f.status, f.created_at,
                    (SELECT COUNT(*) FROM worktrees w WHERE w.repo_id = f.repo_id AND w.base_branch = f.branch) AS wt_count,
                    (SELECT COUNT(*) FROM feature_tickets ft WHERE ft.feature_id = f.id) AS ticket_count
             FROM features f
             WHERE f.repo_id = ?1
             ORDER BY f.created_at DESC",
            params![repo.id],
            |row| {
                Ok(FeatureRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    branch: row.get(2)?,
                    base_branch: row.get(3)?,
                    status: row.get(4)?,
                    created_at: row.get(5)?,
                    worktree_count: row.get(6)?,
                    ticket_count: row.get(7)?,
                })
            },
        )
    }

    /// Look up a single feature by repo slug + feature name.
    pub fn get_by_name(&self, repo_slug: &str, name: &str) -> Result<Feature> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;

        self.conn
            .query_row(
                "SELECT id, repo_id, name, branch, base_branch, status, created_at, merged_at
                 FROM features WHERE repo_id = ?1 AND name = ?2",
                params![repo.id, name],
                map_feature_row,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ConductorError::FeatureNotFound {
                    name: name.to_string(),
                },
                _ => ConductorError::Database(e),
            })
    }

    /// Link tickets (by source_id) to a feature.
    pub fn link_tickets(
        &self,
        repo_slug: &str,
        feature_name: &str,
        ticket_source_ids: &[String],
    ) -> Result<()> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, feature_name)?;
        let ticket_ids = self.resolve_ticket_ids(&repo.id, ticket_source_ids)?;
        self.link_tickets_internal(&repo.id, &feature.id, &ticket_ids)
    }

    /// Unlink tickets (by source_id) from a feature.
    pub fn unlink_tickets(
        &self,
        repo_slug: &str,
        feature_name: &str,
        ticket_source_ids: &[String],
    ) -> Result<()> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, feature_name)?;
        let ticket_ids = self.resolve_ticket_ids(&repo.id, ticket_source_ids)?;

        for tid in &ticket_ids {
            self.conn.execute(
                "DELETE FROM feature_tickets WHERE feature_id = ?1 AND ticket_id = ?2",
                params![feature.id, tid],
            )?;
        }
        Ok(())
    }

    /// Create a PR for the feature branch via `gh pr create`.
    pub fn create_pr(&self, repo_slug: &str, feature_name: &str, draft: bool) -> Result<String> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, feature_name)?;

        let mut args = vec![
            "pr",
            "create",
            "--fill",
            "--head",
            &feature.branch,
            "--base",
            &feature.base_branch,
        ];
        if draft {
            args.push("--draft");
        }

        let output = check_output(Command::new("gh").args(&args).current_dir(&repo.local_path))?;
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(url)
    }

    /// Close a feature (set status to closed, or merged if the branch was merged).
    pub fn close(&self, repo_slug: &str, feature_name: &str) -> Result<()> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, feature_name)?;

        // Check if the branch was merged on the remote
        let merged = is_branch_merged(&repo.local_path, &feature.branch, &feature.base_branch);

        let now = Utc::now().to_rfc3339();
        if merged {
            self.conn.execute(
                "UPDATE features SET status = 'merged', merged_at = ?1 WHERE id = ?2",
                params![now, feature.id],
            )?;
        } else {
            self.conn.execute(
                "UPDATE features SET status = 'closed' WHERE id = ?1",
                params![feature.id],
            )?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn get_feature_by_repo_id(&self, repo_id: &str, name: &str) -> Result<Feature> {
        self.conn
            .query_row(
                "SELECT id, repo_id, name, branch, base_branch, status, created_at, merged_at
                 FROM features WHERE repo_id = ?1 AND name = ?2",
                params![repo_id, name],
                map_feature_row,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ConductorError::FeatureNotFound {
                    name: name.to_string(),
                },
                _ => ConductorError::Database(e),
            })
    }

    /// Resolve ticket source_ids (e.g. "1262") to internal ULID ticket IDs.
    fn resolve_ticket_ids(&self, repo_id: &str, source_ids: &[String]) -> Result<Vec<String>> {
        let mut ids = Vec::with_capacity(source_ids.len());
        for sid in source_ids {
            let ticket_id: String = self
                .conn
                .query_row(
                    "SELECT id FROM tickets WHERE repo_id = ?1 AND source_id = ?2",
                    params![repo_id, sid],
                    |row| row.get(0),
                )
                .map_err(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => {
                        ConductorError::TicketNotFound { id: sid.clone() }
                    }
                    _ => ConductorError::Database(e),
                })?;
            ids.push(ticket_id);
        }
        Ok(ids)
    }

    fn link_tickets_internal(
        &self,
        _repo_id: &str,
        feature_id: &str,
        ticket_ids: &[String],
    ) -> Result<()> {
        for tid in ticket_ids {
            self.conn.execute(
                "INSERT OR IGNORE INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
                params![feature_id, tid],
            )?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn map_feature_row(row: &rusqlite::Row) -> rusqlite::Result<Feature> {
    Ok(Feature {
        id: row.get(0)?,
        repo_id: row.get(1)?,
        name: row.get(2)?,
        branch: row.get(3)?,
        base_branch: row.get(4)?,
        status: row.get(5)?,
        created_at: row.get(6)?,
        merged_at: row.get(7)?,
    })
}

/// Check whether `branch` has been merged into `base` by looking at the remote.
fn is_branch_merged(repo_path: &str, branch: &str, base: &str) -> bool {
    // Fetch latest remote state (best-effort)
    let _ = git_in(repo_path)
        .args(["fetch", "origin", base, branch])
        .output();

    // Check if the branch is an ancestor of the base
    git_in(repo_path)
        .args([
            "merge-base",
            "--is-ancestor",
            &format!("origin/{branch}"),
            &format!("origin/{base}"),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::migrations;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn insert_repo(conn: &Connection) -> String {
        let id = crate::new_id();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at)
             VALUES (?1, 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', 'main', '/tmp/ws', '2024-01-01T00:00:00Z')",
            params![id],
        ).unwrap();
        id
    }

    fn insert_feature(conn: &Connection, repo_id: &str, name: &str, branch: &str) -> String {
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO features (id, repo_id, name, branch, base_branch, status, created_at)
             VALUES (?1, ?2, ?3, ?4, 'main', 'active', ?5)",
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
    fn test_create_feature_duplicate() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        insert_feature(
            &conn,
            &repo_id,
            "notif-improvements",
            "feat/notif-improvements",
        );

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        // Inserting directly (bypassing git) to test the DB constraint
        let result = conn.execute(
            "INSERT INTO features (id, repo_id, name, branch, base_branch, status, created_at)
             VALUES (?1, ?2, 'notif-improvements', 'feat/notif-improvements', 'main', 'active', '2024-01-01')",
            params![crate::new_id(), repo_id],
        );
        assert!(result.is_err());

        // Also test via get_by_name that the original is found
        let f = mgr.get_by_name("test-repo", "notif-improvements").unwrap();
        assert_eq!(f.name, "notif-improvements");
    }

    #[test]
    fn test_list_features() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        insert_feature(&conn, &repo_id, "feature-a", "feat/feature-a");
        insert_feature(&conn, &repo_id, "feature-b", "feat/feature-b");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        let features = mgr.list("test-repo").unwrap();
        assert_eq!(features.len(), 2);
    }

    #[test]
    fn test_link_unlink_tickets() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let feature_id = insert_feature(&conn, &repo_id, "notif", "feat/notif");
        let ticket_id_a = insert_ticket(&conn, &repo_id, "100");
        let ticket_id_b = insert_ticket(&conn, &repo_id, "101");

        // Link
        conn.execute(
            "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
            params![feature_id, ticket_id_a],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
            params![feature_id, ticket_id_b],
        )
        .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM feature_tickets WHERE feature_id = ?1",
                params![feature_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);

        // Unlink one
        conn.execute(
            "DELETE FROM feature_tickets WHERE feature_id = ?1 AND ticket_id = ?2",
            params![feature_id, ticket_id_a],
        )
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
    fn test_close_feature() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let feature_id = insert_feature(&conn, &repo_id, "done-feature", "feat/done-feature");

        // Simulate close by updating DB directly (git ops not available in tests)
        conn.execute(
            "UPDATE features SET status = 'closed' WHERE id = ?1",
            params![feature_id],
        )
        .unwrap();

        let status: String = conn
            .query_row(
                "SELECT status FROM features WHERE id = ?1",
                params![feature_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "closed");
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
    fn test_branch_name_derivation() {
        // Simple name gets feat/ prefix
        let name = "notification-improvements";
        let branch = if name.contains('/') {
            name.to_string()
        } else {
            format!("feat/{name}")
        };
        assert_eq!(branch, "feat/notification-improvements");

        // Name with slash is used as-is
        let name2 = "release/2.0";
        let branch2 = if name2.contains('/') {
            name2.to_string()
        } else {
            format!("feat/{name2}")
        };
        assert_eq!(branch2, "release/2.0");
    }
}
