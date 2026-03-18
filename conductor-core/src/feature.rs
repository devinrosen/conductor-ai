use std::fmt;
use std::process::Command;
use std::str::FromStr;

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use crate::git::{check_output, git_in};
use crate::repo::RepoManager;

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
        ticket_source_ids: &[String],
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

        // Resolve ticket source_ids to internal ULID IDs before doing anything else
        let ticket_ids = if !ticket_source_ids.is_empty() {
            self.resolve_ticket_ids(&repo.id, ticket_source_ids)?
        } else {
            Vec::new()
        };

        let branch = derive_branch_name(name);

        let base = from_branch
            .map(|b| b.to_string())
            .unwrap_or_else(|| repo.default_branch.clone());

        // Create git branch and push — clean up local branch on push failure
        check_output(git_in(&repo.local_path).args([
            "branch",
            "--",
            &branch,
            &format!("refs/heads/{base}"),
        ]))?;
        if let Err(e) =
            check_output(git_in(&repo.local_path).args(["push", "-u", "origin", "--", &branch]))
        {
            // Best-effort cleanup of the local branch so the command is retriable
            let _ = git_in(&repo.local_path)
                .args(["branch", "-D", "--", &branch])
                .output();
            return Err(e);
        }

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

        // Link tickets if provided (already resolved to internal IDs)
        if !ticket_ids.is_empty() {
            self.link_tickets_internal(&feature.id, &ticket_ids)?;
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
        self.get_feature_by_repo_id(&repo.id, name)
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
        self.link_tickets_internal(&feature.id, &ticket_ids)
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

        if !ticket_ids.is_empty() {
            let (sql, param_values) = build_in_clause(
                "DELETE FROM feature_tickets WHERE feature_id = ?1 AND ticket_id IN",
                &feature.id,
                &ticket_ids,
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let params: Vec<&dyn rusqlite::types::ToSql> =
                param_values.iter().map(|p| p.as_ref()).collect();
            stmt.execute(params.as_slice())?;
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
        let merged = crate::git::is_branch_merged_remote(
            &repo.local_path,
            &feature.branch,
            &feature.base_branch,
        );

        let now = Utc::now().to_rfc3339();
        if merged {
            self.conn.execute(
                "UPDATE features SET status = ?1, merged_at = ?2 WHERE id = ?3",
                params![FeatureStatus::Merged, now, feature.id],
            )?;
        } else {
            self.conn.execute(
                "UPDATE features SET status = ?1 WHERE id = ?2",
                params![FeatureStatus::Closed, feature.id],
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
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }

        let (sql, param_values) = build_in_clause(
            "SELECT id, source_id FROM tickets WHERE repo_id = ?1 AND source_id IN",
            repo_id,
            source_ids,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut rows = stmt.query(params.as_slice())?;
        let mut map = std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let source_id: String = row.get(1)?;
            map.insert(source_id, id);
        }

        // Verify all requested source_ids were found, preserving order
        let mut ids = Vec::with_capacity(source_ids.len());
        for sid in source_ids {
            match map.get(sid) {
                Some(id) => ids.push(id.clone()),
                None => {
                    return Err(ConductorError::TicketNotFound { id: sid.clone() });
                }
            }
        }
        Ok(ids)
    }

    fn link_tickets_internal(&self, feature_id: &str, ticket_ids: &[String]) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT OR IGNORE INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        )?;
        for tid in ticket_ids {
            stmt.execute(params![feature_id, tid])?;
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

/// Build a parameterised IN-clause query.
///
/// `prefix` is everything before the `IN (...)` — e.g.
/// `"SELECT id FROM tickets WHERE repo_id = ?1 AND source_id IN"`.
/// `first_param` is bound to `?1`; `items` are bound to `?2`, `?3`, …
fn build_in_clause(
    prefix: &str,
    first_param: &str,
    items: &[String],
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let placeholders: Vec<String> = (0..items.len()).map(|i| format!("?{}", i + 2)).collect();
    let sql = format!(
        "{prefix} ({placeholders})",
        placeholders = placeholders.join(", ")
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(first_param.to_string())];
    for item in items {
        params.push(Box::new(item.clone()));
    }
    (sql, params)
}

/// Derive a git branch name from a feature name.
/// Names containing `/` are used as-is; otherwise `feat/` is prepended.
fn derive_branch_name(name: &str) -> String {
    if name.contains('/') {
        name.to_string()
    } else {
        format!("feat/{name}")
    }
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
    fn test_create_feature_duplicate_via_manager() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);

        // Insert the first feature directly (bypassing git)
        insert_feature(
            &conn,
            &repo_id,
            "notif-improvements",
            "feat/notif-improvements",
        );

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        // The manager's duplicate check should fire before any git ops
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM features WHERE repo_id = ?1 AND name = ?2)",
                params![repo_id, "notif-improvements"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "feature should already exist in DB");

        // Also test via get_by_name that the original is found
        let f = mgr.get_by_name("test-repo", "notif-improvements").unwrap();
        assert_eq!(f.name, "notif-improvements");
        assert!(matches!(f.status, FeatureStatus::Active));
    }

    #[test]
    fn test_list_features() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let feat_a_id = insert_feature(&conn, &repo_id, "feature-a", "feat/feature-a");
        insert_feature(&conn, &repo_id, "feature-b", "feat/feature-b");

        // Create a worktree record whose base_branch matches feature-a's branch
        let wt_id = crate::new_id();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, created_at)
             VALUES (?1, ?2, 'wt-a', 'wt-branch', 'feat/feature-a', '/tmp/wt', '2024-01-01T00:00:00Z')",
            params![wt_id, repo_id],
        ).unwrap();

        // Link a ticket to feature-a
        let ticket_id = insert_ticket(&conn, &repo_id, "42");
        conn.execute(
            "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
            params![feat_a_id, ticket_id],
        )
        .unwrap();

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        let features = mgr.list("test-repo").unwrap();
        assert_eq!(features.len(), 2);

        // Features are ordered by created_at DESC, so feature-b is first
        let feat_a = features.iter().find(|f| f.name == "feature-a").unwrap();
        let feat_b = features.iter().find(|f| f.name == "feature-b").unwrap();
        assert_eq!(feat_a.worktree_count, 1);
        assert_eq!(feat_a.ticket_count, 1);
        assert_eq!(feat_b.worktree_count, 0);
        assert_eq!(feat_b.ticket_count, 0);
    }

    #[test]
    fn test_link_unlink_tickets_via_manager() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let feature_id = insert_feature(&conn, &repo_id, "notif", "feat/notif");
        let _ticket_id_a = insert_ticket(&conn, &repo_id, "100");
        let _ticket_id_b = insert_ticket(&conn, &repo_id, "101");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        // Link via manager (using source_ids)
        mgr.link_tickets("test-repo", "notif", &["100".into(), "101".into()])
            .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM feature_tickets WHERE feature_id = ?1",
                params![feature_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);

        // Unlink one via manager
        mgr.unlink_tickets("test-repo", "notif", &["100".into()])
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
    fn test_resolve_ticket_not_found() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let _feature_id = insert_feature(&conn, &repo_id, "notif", "feat/notif");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        let result = mgr.link_tickets("test-repo", "notif", &["999".into()]);
        assert!(matches!(result, Err(ConductorError::TicketNotFound { .. })));
    }

    /// Create a temp git repo with "origin" remote (bare) and a default "main" branch.
    /// Returns (repo_dir, bare_dir) as TempDir handles (drop cleans up).
    fn setup_git_repo() -> (tempfile::TempDir, tempfile::TempDir) {
        use std::process::Command;

        let bare = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(bare.path())
            .output()
            .unwrap();

        let work = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(work.path())
            .output()
            .unwrap();
        // Configure user for commits
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(work.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(work.path())
            .output()
            .unwrap();
        // Create initial commit on main
        Command::new("git")
            .args(["checkout", "-b", "main"])
            .current_dir(work.path())
            .output()
            .unwrap();
        std::fs::write(work.path().join("README"), "init").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(work.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(work.path())
            .output()
            .unwrap();
        // Add bare as origin and push
        Command::new("git")
            .args(["remote", "add", "origin", bare.path().to_str().unwrap()])
            .current_dir(work.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(work.path())
            .output()
            .unwrap();

        (work, bare)
    }

    fn insert_repo_at(conn: &Connection, local_path: &str) -> String {
        let id = crate::new_id();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at)
             VALUES (?1, 'test-repo', ?2, 'https://github.com/test/repo.git', 'main', '/tmp/ws', '2024-01-01T00:00:00Z')",
            params![id, local_path],
        ).unwrap();
        id
    }

    #[test]
    fn test_close_feature_sets_closed_status() {
        let (work, _bare) = setup_git_repo();
        let conn = setup_db();
        let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

        // Create a feature branch with an extra commit NOT merged into main
        std::process::Command::new("git")
            .args(["checkout", "-b", "feat/done-feature", "main"])
            .current_dir(work.path())
            .output()
            .unwrap();
        std::fs::write(work.path().join("unmerged.txt"), "unmerged work").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(work.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "unmerged commit"])
            .current_dir(work.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "origin", "feat/done-feature"])
            .current_dir(work.path())
            .output()
            .unwrap();
        // Switch back to main
        std::process::Command::new("git")
            .args(["checkout", "main"])
            .current_dir(work.path())
            .output()
            .unwrap();

        insert_feature(&conn, &repo_id, "done-feature", "feat/done-feature");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        mgr.close("test-repo", "done-feature").unwrap();

        let f = mgr.get_by_name("test-repo", "done-feature").unwrap();
        assert_eq!(f.status, FeatureStatus::Closed);
        assert!(f.merged_at.is_none());
    }

    #[test]
    fn test_close_feature_sets_merged_status() {
        let (work, _bare) = setup_git_repo();
        let conn = setup_db();
        let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

        // Create a feature branch, make a commit, merge it into main, push both
        std::process::Command::new("git")
            .args(["checkout", "-b", "feat/merged-feature", "main"])
            .current_dir(work.path())
            .output()
            .unwrap();
        std::fs::write(work.path().join("feature.txt"), "feature work").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(work.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "feature commit"])
            .current_dir(work.path())
            .output()
            .unwrap();
        // Merge into main
        std::process::Command::new("git")
            .args(["checkout", "main"])
            .current_dir(work.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["merge", "--no-ff", "feat/merged-feature", "-m", "merge"])
            .current_dir(work.path())
            .output()
            .unwrap();
        // Push both branches
        std::process::Command::new("git")
            .args(["push", "origin", "main", "feat/merged-feature"])
            .current_dir(work.path())
            .output()
            .unwrap();

        insert_feature(&conn, &repo_id, "merged-feature", "feat/merged-feature");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        mgr.close("test-repo", "merged-feature").unwrap();

        let f = mgr.get_by_name("test-repo", "merged-feature").unwrap();
        assert_eq!(f.status, FeatureStatus::Merged);
        assert!(f.merged_at.is_some());
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
    fn test_create_feature_happy_path() {
        let (work, _bare) = setup_git_repo();
        let conn = setup_db();
        let _repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        let feature = mgr.create("test-repo", "my-feature", None, &[]).unwrap();

        assert_eq!(feature.name, "my-feature");
        assert_eq!(feature.branch, "feat/my-feature");
        assert_eq!(feature.base_branch, "main");
        assert!(matches!(feature.status, FeatureStatus::Active));
        assert!(feature.merged_at.is_none());

        // Verify the branch exists in git
        let output = std::process::Command::new("git")
            .args(["branch", "--list", "feat/my-feature"])
            .current_dir(work.path())
            .output()
            .unwrap();
        let branches = String::from_utf8_lossy(&output.stdout);
        assert!(
            branches.contains("feat/my-feature"),
            "branch should exist in git"
        );

        // Verify DB record via get_by_name
        let fetched = mgr.get_by_name("test-repo", "my-feature").unwrap();
        assert_eq!(fetched.id, feature.id);
    }

    #[test]
    fn test_branch_name_derivation() {
        // Simple name gets feat/ prefix
        assert_eq!(
            derive_branch_name("notification-improvements"),
            "feat/notification-improvements"
        );

        // Name with slash is used as-is
        assert_eq!(derive_branch_name("release/2.0"), "release/2.0");
    }
}
