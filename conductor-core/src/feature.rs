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
use crate::tickets::TicketSyncer;
use crate::worktree::WorktreeManager;

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

        if let Err(e) = self.conn.execute(
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
        ) {
            // Best-effort cleanup of branches created above so the command is retriable
            let _ = git_in(&repo.local_path)
                .args(["push", "origin", "--delete", "--", &feature.branch])
                .output();
            let _ = git_in(&repo.local_path)
                .args(["branch", "-D", "--", &feature.branch])
                .output();
            return Err(e.into());
        }

        // Link tickets if provided (already resolved to internal IDs)
        if !ticket_ids.is_empty() {
            self.link_tickets_internal(&feature.id, &ticket_ids)?;
        }

        Ok(feature)
    }

    /// List features for a repo with worktree and ticket counts.
    pub fn list(&self, repo_slug: &str) -> Result<Vec<FeatureRow>> {
        self.list_with_status_filter(repo_slug, None)
    }

    /// List only active features for a repo (with worktree and ticket counts).
    pub fn list_active(&self, repo_slug: &str) -> Result<Vec<FeatureRow>> {
        self.list_with_status_filter(repo_slug, Some(FeatureStatus::Active))
    }

    /// List active features for all repos in a single query, keyed by repo_id.
    pub fn list_all_active(&self) -> Result<std::collections::HashMap<String, Vec<FeatureRow>>> {
        let row_mapper = |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, String>(0)?, // repo_id
                FeatureRow {
                    id: row.get(1)?,
                    name: row.get(2)?,
                    branch: row.get(3)?,
                    base_branch: row.get(4)?,
                    status: row.get(5)?,
                    created_at: row.get(6)?,
                    worktree_count: row.get(7)?,
                    ticket_count: row.get(8)?,
                },
            ))
        };

        let sql = "\
            SELECT f.repo_id, f.id, f.name, f.branch, f.base_branch, f.status, f.created_at, \
                   (SELECT COUNT(*) FROM worktrees w WHERE w.repo_id = f.repo_id AND w.base_branch = f.branch) AS wt_count, \
                   (SELECT COUNT(*) FROM feature_tickets ft WHERE ft.feature_id = f.id) AS ticket_count \
            FROM features f \
            WHERE f.status = ?1 \
            ORDER BY f.created_at DESC";

        let pairs: Vec<(String, FeatureRow)> =
            query_collect(self.conn, sql, params![FeatureStatus::Active], row_mapper)?;

        let mut map = std::collections::HashMap::new();
        for (repo_id, row) in pairs {
            map.entry(repo_id).or_insert_with(Vec::new).push(row);
        }
        Ok(map)
    }

    /// Shared helper: list features with an optional status filter.
    ///
    /// Worktree count uses an implicit join via branch name
    /// (w.base_branch = f.branch) rather than an FK join table. This is
    /// intentional: worktrees are created independently of features and
    /// linked by which branch they're based on, while ticket-feature links
    /// are explicit user actions stored in the `feature_tickets` table.
    fn list_with_status_filter(
        &self,
        repo_slug: &str,
        status: Option<FeatureStatus>,
    ) -> Result<Vec<FeatureRow>> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;

        let row_mapper = |row: &rusqlite::Row<'_>| {
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
        };

        const SELECT: &str = "\
            SELECT f.id, f.name, f.branch, f.base_branch, f.status, f.created_at, \
                   (SELECT COUNT(*) FROM worktrees w WHERE w.repo_id = f.repo_id AND w.base_branch = f.branch) AS wt_count, \
                   (SELECT COUNT(*) FROM feature_tickets ft WHERE ft.feature_id = f.id) AS ticket_count \
            FROM features f";
        const ORDER: &str = " ORDER BY f.created_at DESC";

        match status {
            Some(s) => {
                let sql = format!("{SELECT} WHERE f.repo_id = ?1 AND f.status = ?2{ORDER}");
                query_collect(self.conn, &sql, params![repo.id, s], row_mapper)
            }
            None => {
                let sql = format!("{SELECT} WHERE f.repo_id = ?1{ORDER}");
                query_collect(self.conn, &sql, params![repo.id], row_mapper)
            }
        }
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

    /// Look up a single feature by its internal ULID ID.
    pub fn get_by_id(&self, id: &str) -> Result<Feature> {
        self.conn
            .query_row(
                "SELECT id, repo_id, name, branch, base_branch, status, created_at, merged_at
                 FROM features WHERE id = ?1",
                params![id],
                map_feature_row,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ConductorError::FeatureNotFound {
                    name: id.to_string(),
                },
                _ => ConductorError::Database(e),
            })
    }

    /// Look up a feature by repo slug + name and verify it is active.
    ///
    /// Returns `ConductorError::Workflow` if the feature exists but is not active.
    pub fn resolve_active_feature(&self, repo_slug: &str, name: &str) -> Result<Feature> {
        let f = self.get_by_name(repo_slug, name)?;
        if f.status != FeatureStatus::Active {
            return Err(ConductorError::Workflow(format!(
                "Feature '{}' is {} — only active features can be used.",
                name, f.status
            )));
        }
        Ok(f)
    }

    /// Find the active feature linked to a ticket, if any.
    ///
    /// Returns `None` when the ticket is not linked to any feature or when all
    /// linked features are closed/merged. Returns an error if the ticket is linked
    /// to multiple *active* features (ambiguous).
    pub fn find_feature_for_ticket(&self, ticket_id: &str) -> Result<Option<Feature>> {
        let features: Vec<Feature> = query_collect(
            self.conn,
            "SELECT f.id, f.repo_id, f.name, f.branch, f.base_branch, f.status, f.created_at, f.merged_at
             FROM features f
             INNER JOIN feature_tickets ft ON ft.feature_id = f.id
             WHERE ft.ticket_id = ?1 AND f.status = 'active'",
            params![ticket_id],
            map_feature_row,
        )?;
        match features.len() {
            0 => Ok(None),
            1 => Ok(Some(features.into_iter().next().unwrap())),
            n => Err(ConductorError::Workflow(format!(
                "Ticket is linked to {n} active features ({}) — specify which feature to use.",
                features
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// Resolve a feature ID for a workflow run.
    ///
    /// This is an intentional cross-domain convenience orchestrator: it touches
    /// `TicketSyncer` and `WorktreeManager` so that callers (CLI, TUI, web, MCP)
    /// share a single resolution path instead of each re-implementing the lookup
    /// chain. The coupling is accepted because feature resolution inherently
    /// requires ticket and worktree context.
    ///
    /// Resolution order:
    /// 1. Explicit feature name → look up by repo slug + name, verify active.
    /// 2. Ticket ID provided → auto-detect from feature_tickets table.
    /// 3. Repo + worktree slugs provided → look up worktree's linked ticket, then auto-detect.
    /// 4. None of the above → `Ok(None)`.
    ///
    /// When `feature_name` is `Some`, a repo slug is required — it is derived from
    /// `repo_slug`, or by looking up the ticket's repo when only `ticket_id` is given.
    /// Returns an error if no repo context can be determined.
    pub fn resolve_feature_id_for_run(
        &self,
        feature_name: Option<&str>,
        repo_slug: Option<&str>,
        ticket_id: Option<&str>,
        worktree_slug: Option<&str>,
    ) -> Result<Option<String>> {
        if let Some(feat_name) = feature_name {
            // Explicit feature — need a repo slug.
            let slug = if let Some(s) = repo_slug {
                s.to_string()
            } else if let Some(tid) = ticket_id {
                let t = TicketSyncer::new(self.conn).get_by_id(tid)?;
                let r = RepoManager::new(self.conn, self.config).get_by_id(&t.repo_id)?;
                r.slug
            } else {
                return Err(ConductorError::Workflow(
                    "Feature resolution requires a repo context (provide a repo, ticket, or worktree)"
                        .to_string(),
                ));
            };
            let f = self.resolve_active_feature(&slug, feat_name)?;
            return Ok(Some(f.id));
        }

        if let Some(tid) = ticket_id {
            return Ok(self.find_feature_for_ticket(tid)?.map(|f| f.id));
        }

        if let (Some(rs), Some(ws)) = (repo_slug, worktree_slug) {
            let r = RepoManager::new(self.conn, self.config).get_by_slug(rs)?;
            let wt = WorktreeManager::new(self.conn, self.config).get_by_slug(&r.id, ws)?;
            if let Some(ref tid) = wt.ticket_id {
                return Ok(self.find_feature_for_ticket(tid)?.map(|f| f.id));
            }
        }

        Ok(None)
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
            with_in_clause(
                "DELETE FROM feature_tickets WHERE feature_id = ?1 AND ticket_id IN",
                &feature.id,
                &ticket_ids,
                |sql, params| -> Result<()> {
                    self.conn.prepare(sql)?.execute(params)?;
                    Ok(())
                },
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

        let output = Command::new("gh")
            .args(&args)
            .current_dir(&repo.local_path)
            .output()
            .map_err(|e| ConductorError::GhCli(format!("failed to run `gh`: {e}")))?;
        if !output.status.success() {
            return Err(ConductorError::GhCli(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(url)
    }

    /// Close a feature (set status to closed, or merged if the branch was merged).
    pub fn close(&self, repo_slug: &str, feature_name: &str) -> Result<()> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, feature_name)?;

        // Check if the branch was merged on the remote. Fall back to local
        // merge check — the remote branch may have been auto-deleted after merge.
        let merged = crate::git::is_branch_merged_remote(
            &repo.local_path,
            &feature.branch,
            &feature.base_branch,
        ) || crate::git::is_branch_merged_local(
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

        let map = with_in_clause(
            "SELECT id, source_id FROM tickets WHERE repo_id = ?1 AND source_id IN",
            repo_id,
            source_ids,
            |sql, params| -> Result<std::collections::HashMap<String, String>> {
                let mut stmt = self.conn.prepare(sql)?;
                let mut rows = stmt.query(params)?;
                let mut map = std::collections::HashMap::new();
                while let Some(row) = rows.next()? {
                    let id: String = row.get(0)?;
                    let source_id: String = row.get(1)?;
                    map.insert(source_id, id);
                }
                Ok(map)
            },
        )?;

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

/// Build a parameterised IN-clause query and execute a closure with the
/// prepared params slice.
///
/// `prefix` is everything before the `IN (...)` — e.g.
/// `"SELECT id FROM tickets WHERE repo_id = ?1 AND source_id IN"`.
/// `first_param` is bound to `?1`; `items` are bound to `?2`, `?3`, …
///
/// The closure receives `(&str, &[&dyn ToSql])` — the SQL string and a
/// ready-to-use params slice — so callers never need to manually convert
/// boxed params.
fn with_in_clause<T>(
    prefix: &str,
    first_param: &str,
    items: &[String],
    f: impl FnOnce(&str, &[&dyn rusqlite::types::ToSql]) -> T,
) -> T {
    debug_assert!(
        !items.is_empty(),
        "with_in_clause called with empty items — produces invalid SQL `IN ()`"
    );
    let placeholders = crate::db::sql_placeholders_from(items.len(), 2);
    let sql = format!("{prefix} ({placeholders})");
    let first = first_param.to_string();
    let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&first];
    for item in items {
        params.push(item);
    }
    f(&sql, &params)
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
        let (work, _bare) = setup_git_repo();
        let conn = setup_db();
        let _repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        // First create succeeds
        let feature = mgr
            .create("test-repo", "notif-improvements", None, &[])
            .unwrap();
        assert_eq!(feature.name, "notif-improvements");

        // Second create with the same name should return FeatureAlreadyExists
        let err = mgr
            .create("test-repo", "notif-improvements", None, &[])
            .unwrap_err();
        assert!(
            matches!(err, ConductorError::FeatureAlreadyExists { .. }),
            "expected FeatureAlreadyExists, got: {err:?}"
        );
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
    fn test_list_active_filters_by_status() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        insert_feature(&conn, &repo_id, "active-feat", "feat/active-feat");
        let closed_id = insert_feature(&conn, &repo_id, "closed-feat", "feat/closed-feat");
        // Mark one feature as closed.
        conn.execute(
            "UPDATE features SET status = 'closed' WHERE id = ?1",
            params![closed_id],
        )
        .unwrap();

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        // list() returns both; list_active() returns only the active one.
        let all = mgr.list("test-repo").unwrap();
        assert_eq!(all.len(), 2);

        let active = mgr.list_active("test-repo").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "active-feat");
        assert_eq!(active[0].status, FeatureStatus::Active);
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
    fn test_create_feature_with_custom_base_branch() {
        let (work, _bare) = setup_git_repo();

        // Create a "develop" branch and push it so it can be used as base
        std::process::Command::new("git")
            .args(["branch", "develop", "main"])
            .current_dir(work.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "origin", "develop"])
            .current_dir(work.path())
            .output()
            .unwrap();

        let conn = setup_db();
        let _repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        let feature = mgr
            .create("test-repo", "custom-base", Some("develop"), &[])
            .unwrap();

        assert_eq!(feature.name, "custom-base");
        assert_eq!(feature.branch, "feat/custom-base");
        assert_eq!(feature.base_branch, "develop");

        // Verify the branch was created from develop
        let output = std::process::Command::new("git")
            .args(["branch", "--list", "feat/custom-base"])
            .current_dir(work.path())
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("feat/custom-base"));
    }

    #[test]
    fn test_create_feature_with_ticket_source_ids() {
        let (work, _bare) = setup_git_repo();
        let conn = setup_db();
        let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

        // Pre-create tickets with known source_ids
        let ticket_a = insert_ticket(&conn, &repo_id, "42");
        let ticket_b = insert_ticket(&conn, &repo_id, "43");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        let feature = mgr
            .create(
                "test-repo",
                "with-tickets",
                None,
                &["42".into(), "43".into()],
            )
            .unwrap();

        // Verify tickets were linked
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM feature_tickets WHERE feature_id = ?1",
                params![feature.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);

        // Verify the correct tickets were linked
        let linked: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT ticket_id FROM feature_tickets WHERE feature_id = ?1 ORDER BY ticket_id")
                .unwrap();
            stmt.query_map(params![feature.id], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        let mut expected = vec![ticket_a, ticket_b];
        expected.sort();
        assert_eq!(linked, expected);
    }

    #[test]
    fn test_close_feature_merged_when_remote_branch_deleted() {
        let (work, _bare) = setup_git_repo();
        let conn = setup_db();
        let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

        // Create a feature branch, commit, merge into main, push main, then delete the remote branch
        std::process::Command::new("git")
            .args(["checkout", "-b", "feat/auto-deleted", "main"])
            .current_dir(work.path())
            .output()
            .unwrap();
        std::fs::write(work.path().join("ad.txt"), "work").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(work.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "feature work"])
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
            .args(["merge", "--no-ff", "feat/auto-deleted", "-m", "merge"])
            .current_dir(work.path())
            .output()
            .unwrap();
        // Push main only (simulate remote branch auto-deletion)
        std::process::Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(work.path())
            .output()
            .unwrap();

        insert_feature(&conn, &repo_id, "auto-deleted", "feat/auto-deleted");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        mgr.close("test-repo", "auto-deleted").unwrap();

        let f = mgr.get_by_name("test-repo", "auto-deleted").unwrap();
        assert_eq!(
            f.status,
            FeatureStatus::Merged,
            "should detect merge via local fallback when remote branch is deleted"
        );
        assert!(f.merged_at.is_some());
    }

    #[test]
    fn test_with_in_clause_generates_valid_sql() {
        // Single item
        let (sql, _) = with_in_clause(
            "SELECT id FROM t WHERE repo_id = ?1 AND source_id IN",
            "repo1",
            &["a".to_string()],
            |sql, params| (sql.to_string(), params.len()),
        );
        assert_eq!(
            sql,
            "SELECT id FROM t WHERE repo_id = ?1 AND source_id IN (?2)"
        );

        // Multiple items
        let (sql, param_count) = with_in_clause(
            "DELETE FROM ft WHERE fid = ?1 AND tid IN",
            "f1",
            &["a".to_string(), "b".to_string(), "c".to_string()],
            |sql, params| (sql.to_string(), params.len()),
        );
        assert_eq!(sql, "DELETE FROM ft WHERE fid = ?1 AND tid IN (?2, ?3, ?4)");
        assert_eq!(param_count, 4); // first_param + 3 items
    }

    #[test]
    fn test_create_pr_feature_not_found() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        let err = mgr.create_pr("test-repo", "nonexistent", false);
        assert!(err.is_err(), "create_pr should fail for missing feature");
    }

    #[test]
    fn test_create_pr_gh_failure() {
        let (work, _bare) = setup_git_repo();
        let conn = setup_db();
        let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());
        insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        // gh pr create will fail because there's no GitHub remote configured,
        // exercising the non-zero exit / GhCli error path
        let result = mgr.create_pr("test-repo", "my-feat", false);
        assert!(result.is_err(), "create_pr should fail when gh errors");
        let err_msg = format!("{}", result.unwrap_err());
        // Should be a GhCli error, not a generic git error
        assert!(
            err_msg.contains("gh") || err_msg.contains("Gh"),
            "error should reference gh CLI: {err_msg}"
        );
    }

    #[test]
    fn test_create_pr_draft_flag() {
        let (work, _bare) = setup_git_repo();
        let conn = setup_db();
        let repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());
        insert_feature(&conn, &repo_id, "draft-feat", "feat/draft-feat");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        // With draft=true, gh will also fail (no remote) but exercises the draft code path
        let result = mgr.create_pr("test-repo", "draft-feat", true);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_feature_cleans_up_branches_on_db_failure() {
        let (work, _bare) = setup_git_repo();
        let conn = setup_db();
        let _repo_id = insert_repo_at(&conn, work.path().to_str().unwrap());

        // Add a trigger that makes INSERT INTO features fail, simulating a DB
        // error after git branch + push have already succeeded.
        conn.execute_batch(
            "CREATE TRIGGER fail_feature_insert BEFORE INSERT ON features
             BEGIN SELECT RAISE(ABORT, 'simulated DB failure'); END;",
        )
        .unwrap();

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        let result = mgr.create("test-repo", "cleanup-test", None, &[]);
        assert!(result.is_err(), "create should fail due to trigger");

        // Verify the local branch was cleaned up
        let output = std::process::Command::new("git")
            .args(["branch", "--list", "feat/cleanup-test"])
            .current_dir(work.path())
            .output()
            .unwrap();
        let branches = String::from_utf8_lossy(&output.stdout);
        assert!(
            !branches.contains("feat/cleanup-test"),
            "local branch should be cleaned up after DB failure"
        );

        // Verify the remote branch was cleaned up
        let output = std::process::Command::new("git")
            .args(["ls-remote", "--heads", "origin", "feat/cleanup-test"])
            .current_dir(work.path())
            .output()
            .unwrap();
        let remote_refs = String::from_utf8_lossy(&output.stdout);
        assert!(
            !remote_refs.contains("feat/cleanup-test"),
            "remote branch should be cleaned up after DB failure"
        );
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

    #[test]
    fn test_find_feature_for_ticket_none() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let ticket_id = insert_ticket(&conn, &repo_id, "100");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        let result = mgr.find_feature_for_ticket(&ticket_id).unwrap();
        assert!(result.is_none(), "no feature linked to ticket");
    }

    #[test]
    fn test_find_feature_for_ticket_found() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let ticket_id = insert_ticket(&conn, &repo_id, "200");
        let feature_id = insert_feature(&conn, &repo_id, "notif", "feat/notif");

        // Link ticket to feature
        conn.execute(
            "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
            params![feature_id, ticket_id],
        )
        .unwrap();

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        let result = mgr.find_feature_for_ticket(&ticket_id).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "notif");
    }

    #[test]
    fn test_find_feature_for_ticket_skips_closed() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let ticket_id = insert_ticket(&conn, &repo_id, "300");
        let feature_id = insert_feature(&conn, &repo_id, "closed-feat", "feat/closed-feat");

        // Close the feature
        conn.execute(
            "UPDATE features SET status = 'closed' WHERE id = ?1",
            params![feature_id],
        )
        .unwrap();

        // Link ticket
        conn.execute(
            "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
            params![feature_id, ticket_id],
        )
        .unwrap();

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        let result = mgr.find_feature_for_ticket(&ticket_id).unwrap();
        assert!(result.is_none(), "closed feature should not be returned");
    }

    #[test]
    fn test_find_feature_for_ticket_ambiguous() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let ticket_id = insert_ticket(&conn, &repo_id, "400");
        let feat_a = insert_feature(&conn, &repo_id, "feat-a", "feat/feat-a");
        let feat_b = insert_feature(&conn, &repo_id, "feat-b", "feat/feat-b");

        // Link ticket to both features
        conn.execute(
            "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
            params![feat_a, ticket_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
            params![feat_b, ticket_id],
        )
        .unwrap();

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        let result = mgr.find_feature_for_ticket(&ticket_id);
        assert!(result.is_err(), "should error when ambiguous");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("specify which feature"),
            "error should mention disambiguation: {err_msg}"
        );
    }

    #[test]
    fn test_get_by_id() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        let result = mgr.get_by_id(&feature_id).unwrap();
        assert_eq!(result.name, "my-feat");
        assert_eq!(result.id, feature_id);
    }

    #[test]
    fn test_resolve_active_feature_returns_active() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        let f = mgr.resolve_active_feature("test-repo", "my-feat").unwrap();
        assert_eq!(f.name, "my-feat");
        assert_eq!(f.status, FeatureStatus::Active);
    }

    // -----------------------------------------------------------------------
    // resolve_feature_id_for_run tests (4 code paths)
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_feature_id_for_run_none_inputs() {
        let conn = setup_db();
        let _repo_id = insert_repo(&conn);
        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        // No feature name, no ticket, no worktree → Ok(None)
        let result = mgr
            .resolve_feature_id_for_run(None, None, None, None)
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_feature_id_for_run_explicit_name() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");
        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        let result = mgr
            .resolve_feature_id_for_run(Some("my-feat"), Some("test-repo"), None, None)
            .unwrap();
        assert_eq!(result, Some(feature_id));
    }

    #[test]
    fn test_resolve_feature_id_for_run_explicit_name_no_repo_errors() {
        let conn = setup_db();
        let _repo_id = insert_repo(&conn);
        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        // Feature name without repo context should error
        let err = mgr
            .resolve_feature_id_for_run(Some("my-feat"), None, None, None)
            .unwrap_err();
        assert!(
            matches!(err, ConductorError::Workflow(ref msg) if msg.contains("requires a repo context")),
            "expected Workflow error about repo context, got: {err:?}"
        );
    }

    #[test]
    fn test_resolve_feature_id_for_run_explicit_name_via_ticket_repo() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");
        let ticket_id = insert_ticket(&conn, &repo_id, "77");

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        // feature_name provided, repo_slug absent, ticket_id used to derive the repo
        let result = mgr
            .resolve_feature_id_for_run(Some("my-feat"), None, Some(&ticket_id), None)
            .unwrap();
        assert_eq!(result, Some(feature_id));
    }

    #[test]
    fn test_resolve_feature_id_for_run_via_ticket() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");
        let ticket_id = insert_ticket(&conn, &repo_id, "42");
        conn.execute(
            "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
            params![feature_id, ticket_id],
        )
        .unwrap();

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        let result = mgr
            .resolve_feature_id_for_run(None, None, Some(&ticket_id), None)
            .unwrap();
        assert_eq!(result, Some(feature_id));
    }

    #[test]
    fn test_resolve_feature_id_for_run_via_worktree() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let feature_id = insert_feature(&conn, &repo_id, "my-feat", "feat/my-feat");
        let ticket_id = insert_ticket(&conn, &repo_id, "99");
        conn.execute(
            "INSERT INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
            params![feature_id, ticket_id],
        )
        .unwrap();
        // Create a worktree linked to the ticket
        let wt_id = crate::new_id();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, ticket_id, created_at)
             VALUES (?1, ?2, 'wt-slug', 'wt-branch', 'main', '/tmp/wt', ?3, '2024-01-01T00:00:00Z')",
            params![wt_id, repo_id, ticket_id],
        )
        .unwrap();

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        let result = mgr
            .resolve_feature_id_for_run(None, Some("test-repo"), None, Some("wt-slug"))
            .unwrap();
        assert_eq!(result, Some(feature_id));
    }

    #[test]
    fn test_resolve_feature_id_for_run_worktree_no_ticket() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        // Create a worktree with no linked ticket (ticket_id is NULL)
        let wt_id = crate::new_id();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, base_branch, path, created_at)
             VALUES (?1, ?2, 'wt-no-ticket', 'feat/no-ticket', 'main', '/tmp/wt', '2024-01-01T00:00:00Z')",
            params![wt_id, repo_id],
        )
        .unwrap();

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);

        // Should return Ok(None) — no ticket means no feature can be resolved
        let result = mgr
            .resolve_feature_id_for_run(None, Some("test-repo"), None, Some("wt-no-ticket"))
            .unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_active_feature_rejects_closed() {
        let conn = setup_db();
        let repo_id = insert_repo(&conn);
        let fid = insert_feature(&conn, &repo_id, "done-feat", "feat/done-feat");
        conn.execute(
            "UPDATE features SET status = 'closed' WHERE id = ?1",
            params![fid],
        )
        .unwrap();

        let config = Config::default();
        let mgr = FeatureManager::new(&conn, &config);
        let err = mgr
            .resolve_active_feature("test-repo", "done-feat")
            .unwrap_err();
        assert!(
            matches!(err, ConductorError::Workflow(ref msg) if msg.contains("only active features")),
            "expected Workflow error about active features, got: {err:?}"
        );
    }
}
