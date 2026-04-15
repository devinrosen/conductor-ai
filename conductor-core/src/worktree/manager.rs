use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::process::Command;

use crate::config::Config;
use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use crate::git::{check_gh_output, check_output, git_in};
use crate::repo::RepoManager;
use crate::tickets::TicketSyncer;

use super::git_helpers::*;
use super::types::{map_worktree_row, Worktree, WorktreeStatus, WorktreeWithStatus};
use super::{WORKTREE_COLUMNS, WORKTREE_COLUMNS_W, WORKTREE_COLUMN_COUNT};

/// Map a ticket label to the conventional-commit branch prefix it implies.
///
/// Matching is case-insensitive and exact (no substring matching).
/// First matching label wins. Returns `"feat"` when no label matches.
pub fn label_to_branch_prefix(labels: &[&str]) -> &'static str {
    for label in labels {
        match label.to_lowercase().as_str() {
            "bug" | "fix" | "security" => return "fix",
            "chore" | "maintenance" => return "chore",
            "documentation" | "docs" => return "docs",
            "refactor" => return "refactor",
            "test" | "testing" => return "test",
            "ci" | "build" => return "ci",
            "perf" | "performance" => return "perf",
            _ => {}
        }
    }
    "feat"
}

fn worktree_not_found(slug: impl Into<String>) -> impl FnOnce(rusqlite::Error) -> ConductorError {
    let slug = slug.into();
    move |e| match e {
        rusqlite::Error::QueryReturnedNoRows => ConductorError::WorktreeNotFound { slug },
        _ => ConductorError::Database(e),
    }
}

/// SQL fragment that LEFT JOINs the latest agent run per worktree.
///
/// Adds one extra column: `latest.status AS agent_status` (at index `WORKTREE_COLUMN_COUNT`).
/// Must be used together with `map_enriched_row`.
const AGENT_LATEST_JOIN: &str = "LEFT JOIN (\
        SELECT a.worktree_id, a.status \
        FROM agent_runs a \
        INNER JOIN (\
            SELECT worktree_id, MAX(started_at) AS max_started \
            FROM agent_runs \
            WHERE worktree_id IS NOT NULL \
            GROUP BY worktree_id\
        ) top ON a.worktree_id = top.worktree_id AND a.started_at = top.max_started \
        GROUP BY a.worktree_id\
    ) latest ON latest.worktree_id = w.id";

/// Returns the base SELECT+FROM+JOIN fragment for enriched worktree queries.
/// This includes all worktree columns plus ticket info and latest agent status.
/// To be used with `map_enriched_row`.
fn enriched_worktree_base() -> String {
    format!(
        "SELECT {cols}, latest.status AS agent_status, \
         t.title AS ticket_title, t.source_id AS ticket_number, t.url AS ticket_url \
         FROM worktrees w \
         {agent_join} \
         LEFT JOIN tickets t ON t.id = w.ticket_id",
        cols = &*WORKTREE_COLUMNS_W,
        agent_join = AGENT_LATEST_JOIN,
    )
}

/// Map a row that contains the standard worktree columns followed by
/// `agent_status`, `ticket_title`, `ticket_number`, and `ticket_url` (in that order).
///
/// Column layout:
/// - `[0 .. WORKTREE_COLUMN_COUNT)`: mapped by `map_worktree_row`
/// - `WORKTREE_COLUMN_COUNT + 0`: `agent_status`
/// - `WORKTREE_COLUMN_COUNT + 1`: `ticket_title`
/// - `WORKTREE_COLUMN_COUNT + 2`: `ticket_number`
/// - `WORKTREE_COLUMN_COUNT + 3`: `ticket_url`
fn map_enriched_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeWithStatus> {
    let worktree = map_worktree_row(row)?;
    let agent_status: Option<crate::agent::AgentRunStatus> = row.get(WORKTREE_COLUMN_COUNT)?;
    let ticket_title: Option<String> = row.get(WORKTREE_COLUMN_COUNT + 1)?;
    let ticket_number: Option<String> = row.get(WORKTREE_COLUMN_COUNT + 2)?;
    let ticket_url: Option<String> = row.get(WORKTREE_COLUMN_COUNT + 3)?;
    Ok(WorktreeWithStatus {
        worktree,
        agent_status,
        ticket_title,
        ticket_number,
        ticket_url,
    })
}

/// Look up a ticket's dependencies and return the branch of the first parent that has
/// an active worktree.  Returns `None` if the ticket has no resolvable parent branch
/// (no dependency metadata for its source type, no deps, or no parent worktree).
///
/// Dependency IDs are extracted via [`crate::ticket_source::get_dependency_ids`];
/// swap to a `ticket_dependencies` table query once RFC 009 lands.
fn resolve_parent_branch(conn: &Connection, ticket_id: &str, repo_id: &str) -> Option<String> {
    let syncer = TicketSyncer::new(conn);
    let ticket = match syncer.get_by_id(ticket_id) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("resolve_parent_branch: failed to look up ticket {ticket_id}: {e}");
            return None;
        }
    };

    let dep_ids = crate::ticket_source::get_dependency_ids(&ticket.raw_json, &ticket.source_type);
    if dep_ids.is_empty() {
        return None;
    }

    // Query 1: Get all parent tickets and their worktrees in a single JOIN query
    // This reduces N+1 queries to just 2 total: one to get the child ticket, one to get all relevant data
    let placeholders = dep_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT w.branch, t.source_id \
         FROM tickets t \
         LEFT JOIN worktrees w ON w.ticket_id = t.id AND w.status = 'active' \
         WHERE t.repo_id = ?1 AND t.source_id IN ({placeholders}) \
         ORDER BY w.created_at DESC"
    );

    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&repo_id];
    for dep_id in &dep_ids {
        params.push(dep_id);
    }

    let results: Vec<(Option<String>, String)> =
        match query_collect(conn, &sql, params.as_slice(), |row| {
            Ok((
                row.get::<_, Option<String>>(0)?, // branch (nullable if no active worktree)
                row.get::<_, String>(1)?,         // source_id
            ))
        }) {
            Ok(results) => results,
            Err(e) => {
                tracing::warn!("resolve_parent_branch: batch query failed: {e}");
                return None;
            }
        };

    // Return the first active worktree branch we find, respecting dependency order
    for dep_id in &dep_ids {
        for (branch_opt, source_id) in &results {
            if source_id == dep_id {
                if let Some(branch) = branch_opt {
                    return Some(branch.clone());
                }
                break; // Found the ticket but no active worktree, move to next dependency
            }
        }
    }

    None
}

/// Options for creating a new worktree.
///
/// Passed to [`WorktreeManager::create`] to avoid a long positional argument list.
/// All fields are optional and default to `None` / `false`.
#[derive(Debug, Default)]
pub struct WorktreeCreateOptions {
    /// When `Some(n)`, the worktree is backed by the branch of PR #n instead
    /// of a newly-created branch. `from_branch` is ignored in that case.
    pub from_pr: Option<u32>,
    /// Start the worktree from an existing branch name instead of creating a
    /// new one.  Ignored when `from_pr` is set.
    pub from_branch: Option<String>,
    /// Associate the new worktree with this ticket ID.
    pub ticket_id: Option<String>,
    /// When `true`, skip the dirty-state check. Use only after the caller has
    /// explicitly confirmed the user wants to proceed with uncommitted changes.
    pub force_dirty: bool,
    /// Pre-computed health status from a prior `check_main_health()` call.
    /// When `Some` and the working tree is clean, the redundant `git status`
    /// inside `ensure_base_up_to_date()` is skipped.
    pub pre_health: Option<super::git_helpers::MainHealthStatus>,
}

pub struct WorktreeManager<'a> {
    conn: &'a Connection,
    config: &'a Config,
}

impl<'a> WorktreeManager<'a> {
    pub fn new(conn: &'a Connection, config: &'a Config) -> Self {
        Self { conn, config }
    }

    /// Run a read-only health check on the base branch of `repo_slug`.
    ///
    /// Resolves the base branch in the same priority order as `create()`.
    /// Returns a `MainHealthStatus` describing dirty state and staleness.
    pub fn check_main_health(
        &self,
        repo_slug: &str,
        base_branch: Option<&str>,
    ) -> Result<super::git_helpers::MainHealthStatus> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;
        let base = base_branch
            .map(|b| b.to_string())
            .unwrap_or_else(|| resolve_base_branch(&repo.local_path, &repo.default_branch));
        Ok(check_main_health(&repo.local_path, &base))
    }

    /// Create a new worktree, ensuring the base branch is up to date first.
    ///
    /// Returns the created worktree and a list of non-fatal warnings
    /// (e.g., fetch failures, diverged base branch).
    ///
    /// When `from_pr` is `Some(n)`, the worktree is backed by the branch of PR #n
    /// instead of a newly-created branch.  `from_branch` is ignored in that case.
    ///
    /// When `force_dirty` is `true`, the dirty-state check inside
    /// `ensure_base_up_to_date()` is skipped. Use this only after the caller has
    /// explicitly confirmed the user wants to proceed with uncommitted changes.
    ///
    /// When `opts.pre_health` is `Some` and the health status shows a clean working tree,
    /// the redundant `git status --porcelain` call inside `ensure_base_up_to_date()` is
    /// skipped. Callers that already ran `check_main_health()` should pass the result here.
    pub fn create(
        &self,
        repo_slug: &str,
        name: &str,
        opts: WorktreeCreateOptions,
    ) -> Result<(Worktree, Vec<String>)> {
        let WorktreeCreateOptions {
            from_pr,
            from_branch,
            ticket_id,
            force_dirty,
            pre_health,
        } = opts;
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        // Determine branch name and worktree slug.
        // "bug-" slugs are preserved as-is but map to "fix/" in git.
        const SLUG_PREFIXES: &[(&str, &str)] = &[
            ("fix-", "fix"),
            ("bug-", "fix"),
            ("feat-", "feat"),
            ("release-", "release"),
            ("chore-", "chore"),
            ("docs-", "docs"),
            ("refactor-", "refactor"),
            ("test-", "test"),
            ("ci-", "ci"),
            ("perf-", "perf"),
        ];
        let (wt_slug, branch) =
            if let Some(&(dash, slash)) = SLUG_PREFIXES.iter().find(|(d, _)| name.starts_with(d)) {
                let clean = name.strip_prefix(dash).unwrap();
                (format!("{dash}{clean}"), format!("{slash}/{clean}"))
            } else {
                (format!("feat-{name}"), format!("feat/{name}"))
            };

        // Check for existing worktree with same slug
        let existing_status: Option<WorktreeStatus> = self
            .conn
            .query_row(
                "SELECT status FROM worktrees WHERE repo_id = ?1 AND slug = ?2",
                params![repo.id, wt_slug],
                |row| row.get(0),
            )
            .optional()?;

        match existing_status {
            Some(WorktreeStatus::Active) => {
                return Err(ConductorError::WorktreeAlreadyExists {
                    slug: wt_slug.clone(),
                });
            }
            Some(_) => {
                // Purge the completed record to allow slug reuse
                self.conn.execute(
                    "DELETE FROM worktrees WHERE repo_id = ?1 AND slug = ?2",
                    params![repo.id, wt_slug],
                )?;
            }
            None => {}
        }

        // Auto-clone if the local path doesn't exist on disk yet
        if !Path::new(&repo.local_path).exists() {
            clone_repo(&repo.remote_url, &repo.local_path)?;
        }

        let wt_path = Path::new(&repo.workspace_dir).join(&wt_slug);

        // Ensure the per-repo workspace directory exists
        std::fs::create_dir_all(&repo.workspace_dir)?;

        // (branch_name, base_branch_for_db, warnings)
        let (branch, base_for_db, warnings) = if let Some(pr_number) = from_pr {
            // --from-pr path: fetch the PR branch and record the PR's base branch
            // so that create_pr can target the correct base.
            let (pr_branch, pr_base) = fetch_pr_branch(&repo.local_path, pr_number)?;
            (pr_branch, Some(pr_base), Vec::new())
        } else {
            // Normal path: resolve base, ensure it's up to date, create a new branch.
            //
            // `resolve_and_update_base` handles:
            //   - explicit from_branch with prefix fallback (feat/, fix/)
            //   - auto-creating a local tracking branch from remote
            //   - non-checkout fast-forward updates
            let explicit_base = if let Some(b) = from_branch {
                Some(b)
            } else {
                ticket_id
                    .as_deref()
                    .and_then(|tid| resolve_parent_branch(self.conn, tid, &repo.id))
            };
            let pre_verified_clean = pre_health
                .map(|h| !h.is_dirty && !h.status_check_failed)
                .unwrap_or(false);
            let (base, warnings) = resolve_and_update_base(
                &repo.local_path,
                explicit_base.as_deref(),
                &repo.default_branch,
                force_dirty,
                pre_verified_clean,
            )?;
            check_output(git_in(&repo.local_path).args([
                "branch",
                "--",
                &branch,
                &format!("refs/heads/{base}"),
            ]))?;
            (branch, Some(base), warnings)
        };

        // Create git worktree
        check_output(git_in(&repo.local_path).args([
            "worktree",
            "add",
            &wt_path.to_string_lossy(),
            &branch,
        ]))?;

        // Detect and install deps
        install_deps(&wt_path);

        // Create isolated DB for the worktree (runs migrations + seeds)
        let wt_db_path = wt_path.join(".conductor.db");
        let wt_conn = crate::db::open_database(&wt_db_path)?;
        crate::db::seed::seed_database(&wt_conn)?;

        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        let worktree = Worktree {
            id: id.clone(),
            repo_id: repo.id.clone(),
            slug: wt_slug,
            branch,
            path: wt_path.to_string_lossy().to_string(),
            ticket_id,
            status: WorktreeStatus::Active,
            created_at: now,
            completed_at: None,
            model: None,
            base_branch: base_for_db.clone(),
        };

        self.conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at, base_branch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                worktree.id,
                worktree.repo_id,
                worktree.slug,
                worktree.branch,
                worktree.path,
                worktree.ticket_id,
                worktree.status,
                worktree.created_at,
                worktree.base_branch,
            ],
        )?;

        Ok((worktree, warnings))
    }

    pub fn get_by_id(&self, id: &str) -> Result<Worktree> {
        self.conn
            .query_row(
                &format!("SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE id = ?1"),
                params![id],
                map_worktree_row,
            )
            .map_err(worktree_not_found(id))
    }

    /// Fetch a worktree by ID, returning `WorktreeNotFound` if it does not exist
    /// or does not belong to `repo_id`.
    pub fn get_by_id_for_repo(&self, id: &str, repo_id: &str) -> Result<Worktree> {
        self.conn
            .query_row(
                &format!("SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE id = ?1 AND repo_id = ?2"),
                params![id, repo_id],
                map_worktree_row,
            )
            .map_err(worktree_not_found(id))
    }

    /// Fetch multiple worktrees by their IDs in a single query.
    /// Returns an empty Vec when `ids` is empty (avoids a syntax-error `IN ()` clause).
    pub fn get_by_ids(&self, ids: &[&str]) -> Result<Vec<Worktree>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let placeholders = crate::db::sql_placeholders(ids.len());
        let sql = format!("SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE id IN ({placeholders})");
        query_collect(
            self.conn,
            &sql,
            rusqlite::params_from_iter(ids.iter()),
            map_worktree_row,
        )
    }

    pub fn get_by_slug(&self, repo_id: &str, slug: &str) -> Result<Worktree> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE repo_id = ?1 AND slug = ?2"
                ),
                params![repo_id, slug],
                map_worktree_row,
            )
            .map_err(worktree_not_found(slug))
    }

    pub fn get_by_branch(&self, repo_id: &str, branch: &str) -> Result<Worktree> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE repo_id = ?1 AND branch = ?2"
                ),
                params![repo_id, branch],
                map_worktree_row,
            )
            .map_err(worktree_not_found(branch))
    }

    /// Try to resolve a worktree by slug first, then by branch name.
    /// If neither matches, returns a "did you mean" error listing available slugs.
    pub fn get_by_slug_or_branch(&self, repo_id: &str, slug_or_branch: &str) -> Result<Worktree> {
        match self.get_by_slug(repo_id, slug_or_branch) {
            Ok(wt) => return Ok(wt),
            Err(ConductorError::WorktreeNotFound { .. }) => {}
            Err(e) => return Err(e),
        }

        match self.get_by_branch(repo_id, slug_or_branch) {
            Ok(wt) => return Ok(wt),
            Err(ConductorError::WorktreeNotFound { .. }) => {}
            Err(e) => return Err(e),
        }

        // Neither slug nor branch matched — build a "did you mean" error.
        let available = self.list_by_repo_id(repo_id, false).unwrap_or_default();
        let suggestions: Vec<&str> = available.iter().take(5).map(|w| w.slug.as_str()).collect();
        let hint = if suggestions.is_empty() {
            String::new()
        } else {
            format!(" — did you mean one of: {}", suggestions.join(", "))
        };
        Err(ConductorError::WorktreeNotFound {
            slug: format!("{slug_or_branch}{hint}"),
        })
    }

    pub fn list_by_ticket(&self, ticket_id: &str) -> Result<Vec<Worktree>> {
        query_collect(
            self.conn,
            &format!("SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE ticket_id = ?1 ORDER BY created_at DESC"),
            params![ticket_id],
            map_worktree_row,
        )
    }

    pub fn list_by_repo_id(&self, repo_id: &str, active_only: bool) -> Result<Vec<Worktree>> {
        let status_filter = if active_only {
            " AND status = 'active'"
        } else {
            ""
        };
        let query = format!(
            "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE repo_id = ?1{} ORDER BY CASE WHEN status = 'active' THEN 0 ELSE 1 END, created_at",
            status_filter
        );
        query_collect(self.conn, &query, params![repo_id], map_worktree_row)
    }

    /// Shared query builder for [`list`] and [`list_paginated`].
    ///
    /// `pagination` is `Some((limit, offset))` to add `LIMIT ?N OFFSET ?M`; `None` for unbounded.
    fn list_inner(
        &self,
        repo_slug: Option<&str>,
        active_only: bool,
        pagination: Option<(usize, usize)>,
    ) -> Result<Vec<Worktree>> {
        let status_filter = if active_only {
            " AND status = 'active'"
        } else {
            ""
        };

        let base_query = match repo_slug {
            Some(_) => format!(
                "SELECT {} FROM worktrees w JOIN repos r ON r.id = w.repo_id WHERE r.slug = ?1{} ORDER BY CASE WHEN w.status = 'active' THEN 0 ELSE 1 END, w.created_at",
                &*WORKTREE_COLUMNS_W,
                status_filter,
            ),
            None => format!(
                "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE 1=1{} ORDER BY CASE WHEN status = 'active' THEN 0 ELSE 1 END, created_at",
                status_filter,
            ),
        };

        match (repo_slug, pagination) {
            (Some(slug), Some((limit, offset))) => {
                let query = format!("{base_query} LIMIT ?2 OFFSET ?3");
                query_collect(
                    self.conn,
                    &query,
                    params![slug, limit as i64, offset as i64],
                    map_worktree_row,
                )
            }
            (Some(slug), None) => {
                query_collect(self.conn, &base_query, params![slug], map_worktree_row)
            }
            (None, Some((limit, offset))) => {
                let query = format!("{base_query} LIMIT ?1 OFFSET ?2");
                query_collect(
                    self.conn,
                    &query,
                    params![limit as i64, offset as i64],
                    map_worktree_row,
                )
            }
            (None, None) => query_collect(self.conn, &base_query, [], map_worktree_row),
        }
    }

    pub fn list(&self, repo_slug: Option<&str>, active_only: bool) -> Result<Vec<Worktree>> {
        self.list_inner(repo_slug, active_only, None)
    }

    pub fn list_paginated(
        &self,
        repo_slug: Option<&str>,
        active_only: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Worktree>> {
        self.list_inner(repo_slug, active_only, Some((limit, offset)))
    }

    /// List all worktrees joined with the status of each worktree's latest agent run.
    ///
    /// Uses a LEFT JOIN so worktrees with no agent runs still appear (with `agent_status = None`).
    /// Uses the INNER JOIN subquery pattern to avoid duplicate rows when two runs share
    /// the same MAX(started_at) timestamp.
    pub fn list_all_with_status(&self, active_only: bool) -> Result<Vec<WorktreeWithStatus>> {
        let status_filter = if active_only {
            " AND w.status = 'active'"
        } else {
            ""
        };
        let sql = format!(
            "{base} \
             WHERE 1=1{status_filter} \
             ORDER BY CASE WHEN w.status = 'active' THEN 0 ELSE 1 END, w.created_at",
            base = enriched_worktree_base(),
            status_filter = status_filter,
        );
        query_collect(self.conn, &sql, [], map_enriched_row)
    }

    /// Fetch a worktree by ID, returning a `WorktreeWithStatus` with ticket info populated.
    pub fn get_by_id_enriched(&self, id: &str) -> Result<WorktreeWithStatus> {
        self.conn
            .query_row(
                &format!("{base} WHERE w.id = ?1", base = enriched_worktree_base(),),
                params![id],
                map_enriched_row,
            )
            .map_err(worktree_not_found(id))
    }

    /// Fetch a worktree by ID and repo, returning a `WorktreeWithStatus` with ticket info.
    /// Returns `WorktreeNotFound` if the worktree does not exist or belongs to a different repo.
    pub fn get_by_id_for_repo_enriched(
        &self,
        id: &str,
        repo_id: &str,
    ) -> Result<WorktreeWithStatus> {
        self.conn
            .query_row(
                &format!(
                    "{base} WHERE w.id = ?1 AND w.repo_id = ?2",
                    base = enriched_worktree_base(),
                ),
                params![id, repo_id],
                map_enriched_row,
            )
            .map_err(worktree_not_found(id))
    }

    /// List worktrees for a repo with ticket info populated.
    /// Does not modify `list_by_repo_id` to avoid breaking internal callers.
    pub fn list_by_repo_id_enriched(
        &self,
        repo_id: &str,
        active_only: bool,
    ) -> Result<Vec<WorktreeWithStatus>> {
        let status_filter = if active_only {
            " AND w.status = 'active'"
        } else {
            ""
        };
        let sql = format!(
            "{base} \
             WHERE w.repo_id = ?1{status_filter} \
             ORDER BY CASE WHEN w.status = 'active' THEN 0 ELSE 1 END, w.created_at",
            base = enriched_worktree_base(),
            status_filter = status_filter,
        );
        query_collect(self.conn, &sql, params![repo_id], map_enriched_row)
    }

    /// Walk up from `cwd` and return the worktree whose `path` is a prefix of (or equals) `cwd`.
    ///
    /// When multiple worktrees match (nested paths), the one with the longest path wins,
    /// ensuring the most-specific worktree is returned.
    ///
    /// Returns `None` when no registered worktree matches.
    pub fn find_by_cwd(&self, cwd: &Path) -> Result<Option<Worktree>> {
        let worktrees = self.list(None, false)?;
        let found = worktrees
            .into_iter()
            .filter(|wt| cwd.starts_with(Path::new(&wt.path)))
            .max_by_key(|wt| wt.path.len());
        Ok(found)
    }

    pub fn delete(&self, repo_slug: &str, name: &str) -> Result<Worktree> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        let worktree = self
            .conn
            .query_row(
                &format!(
                    "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE repo_id = ?1 AND slug = ?2"
                ),
                params![repo.id, name],
                map_worktree_row,
            )
            .map_err(worktree_not_found(name))?;

        self.delete_internal(&repo, worktree, None)
    }

    pub fn delete_by_id(&self, worktree_id: &str) -> Result<Worktree> {
        let worktree = self.get_by_id(worktree_id)?;
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_id(&worktree.repo_id)?;
        self.delete_internal(&repo, worktree, None)
    }

    /// Delete a worktree by ID, enforcing that it belongs to `repo_id`.
    /// Returns `WorktreeNotFound` if the worktree does not exist or belongs to a different repo.
    pub fn delete_by_id_for_repo(&self, id: &str, repo_id: &str) -> Result<Worktree> {
        let worktree = self.get_by_id_for_repo(id, repo_id)?;
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_id(&worktree.repo_id)?;
        self.delete_internal(&repo, worktree, None)
    }

    /// `ticket_closed_hint`: when `Some(true)` the caller already knows the
    /// linked ticket is closed; the per-ticket DB query is skipped.
    fn delete_internal(
        &self,
        repo: &crate::repo::Repo,
        worktree: Worktree,
        ticket_closed_hint: Option<bool>,
    ) -> Result<Worktree> {
        // Determine merged vs abandoned:
        // 1. Check if the linked ticket is closed (covers squash merges that git can't detect)
        // 2. Fall back to git branch --merged (covers cases without a linked ticket)
        let ticket_closed = ticket_closed_hint.unwrap_or_else(|| {
            worktree
                .ticket_id
                .as_ref()
                .map(|tid| {
                    self.conn
                        .query_row(
                            "SELECT state = 'closed' FROM tickets WHERE id = ?1",
                            params![tid],
                            |row| row.get::<_, bool>(0),
                        )
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        });
        let is_merged = ticket_closed
            || crate::git::is_branch_merged_local(
                &repo.local_path,
                &worktree.branch,
                &repo.default_branch,
            );
        let new_status = if is_merged {
            WorktreeStatus::Merged
        } else {
            WorktreeStatus::Abandoned
        };
        let now = Utc::now().to_rfc3339();

        remove_git_artifacts(&repo.local_path, &worktree.path, &worktree.branch);

        // Soft-delete: update status + completed_at instead of deleting the row
        self.conn.execute(
            "UPDATE worktrees SET status = ?1, completed_at = ?2 WHERE id = ?3",
            params![new_status.as_str(), now, worktree.id],
        )?;

        let deleted_wt = Worktree {
            status: new_status,
            completed_at: Some(now),
            ..worktree
        };

        // Auto-close orphaned feature if the branch is gone.
        // Best-effort: log but don't propagate errors so the delete itself succeeds.
        let fm = crate::feature::FeatureManager::new(self.conn, self.config);
        if let Err(e) = fm.auto_close_after_worktree_delete(
            &deleted_wt.repo_id,
            deleted_wt.base_branch.as_deref(),
        ) {
            tracing::warn!(error = %e, "failed to auto-close orphaned feature");
        }

        Ok(deleted_wt)
    }

    /// Remove the git worktree directory and delete the associated branch (best-effort).
    /// Failures are logged but not propagated. Delegates to the module-private
    /// `remove_git_artifacts` to keep the implementation detail encapsulated.
    ///
    /// NOTE: This is a static method by design — it's a cross-manager utility called from
    /// TicketSyncer that doesn't require WorktreeManager instance state (db connection, config).
    /// Making it an instance method would create circular dependency issues.
    pub fn remove_artifacts(repo_path: &str, worktree_path: &str, branch: &str) {
        remove_git_artifacts(repo_path, worktree_path, branch);
    }

    pub fn update_status(&self, worktree_id: &str, status: WorktreeStatus) -> Result<()> {
        let completed_at = if status != WorktreeStatus::Active {
            Some(Utc::now().to_rfc3339())
        } else {
            None
        };
        self.conn.execute(
            "UPDATE worktrees SET status = ?1, completed_at = ?2 WHERE id = ?3",
            params![status.as_str(), completed_at, worktree_id],
        )?;
        Ok(())
    }

    /// Set (or clear) the per-worktree default model.
    /// Pass `None` to clear the override and fall back to the global config.
    pub fn set_model(&self, repo_slug: &str, name: &str, model: Option<&str>) -> Result<()> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;
        let updated = self.conn.execute(
            "UPDATE worktrees SET model = ?1 WHERE repo_id = ?2 AND slug = ?3",
            params![model, repo.id, name],
        )?;
        if updated == 0 {
            return Err(ConductorError::WorktreeNotFound {
                slug: name.to_string(),
            });
        }
        Ok(())
    }

    /// Set (or clear) the worktree's base branch.
    /// Pass `None` to reset to the repo default branch.
    /// When setting to a non-default branch, auto-registers a feature for that branch.
    pub fn set_base_branch(
        &self,
        repo_slug: &str,
        name: &str,
        base_branch: Option<&str>,
    ) -> Result<()> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;
        let updated = self.conn.execute(
            "UPDATE worktrees SET base_branch = ?1 WHERE repo_id = ?2 AND slug = ?3",
            params![base_branch, repo.id, name],
        )?;
        if updated == 0 {
            return Err(ConductorError::WorktreeNotFound {
                slug: name.to_string(),
            });
        }
        Ok(())
    }

    /// Push the worktree branch to origin.
    pub fn push(&self, repo_slug: &str, name: &str) -> Result<String> {
        let (_repo, worktree) = self.get_active_worktree(repo_slug, name)?;

        check_output(git_in(&worktree.path).args(["push", "-u", "origin", &worktree.branch]))?;

        // If this worktree targets a feature branch, refresh its last_commit_at
        // cache so staleness detection stays up to date on the most common write path.
        if let Some(ref base_branch) = worktree.base_branch {
            let feat_mgr = crate::feature::FeatureManager::new(self.conn, self.config);
            if let Some(fid) =
                feat_mgr.get_active_id_by_repo_and_branch(&worktree.repo_id, base_branch)?
            {
                if let Err(e) = feat_mgr.refresh_last_commit(&fid) {
                    tracing::warn!("failed to refresh last_commit_at for feature {fid}: {e}");
                }
            }
        }

        Ok(format!(
            "Pushed {} to origin/{}",
            worktree.slug, worktree.branch
        ))
    }

    /// Create a pull request for the worktree branch using `gh`.
    pub fn create_pr(&self, repo_slug: &str, name: &str, draft: bool) -> Result<String> {
        let (repo, worktree) = self.get_active_worktree(repo_slug, name)?;

        let base = worktree.effective_base(&repo.default_branch);
        let mut args = vec![
            "pr",
            "create",
            "--fill",
            "--head",
            &worktree.branch,
            "--base",
            base,
        ];
        if draft {
            args.push("--draft");
        }

        let output = check_gh_output(Command::new("gh").args(&args).current_dir(&worktree.path))?;

        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(url)
    }

    /// Look up a repo and its active worktree by slugs.
    fn get_active_worktree(
        &self,
        repo_slug: &str,
        wt_slug: &str,
    ) -> Result<(crate::repo::Repo, Worktree)> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        let worktree = self.get_by_slug(&repo.id, wt_slug)?;

        if !worktree.is_active() {
            return Err(ConductorError::InvalidInput(format!(
                "worktree '{}' is not active (status: {})",
                wt_slug, worktree.status
            )));
        }

        Ok((repo, worktree))
    }

    /// Reap stale worktrees whose status is `merged` or `abandoned` but whose
    /// filesystem artifacts still exist. For each stale worktree:
    /// 1. Remove git worktree directory and branch (best-effort)
    /// 2. Run `git worktree prune` on the parent repo
    /// 3. Backfill `completed_at` if NULL
    ///
    /// Returns the number of worktrees cleaned up.
    pub fn reap_stale_worktrees(&self) -> Result<usize> {
        let stale: Vec<(String, String, String, String, Option<String>)> = query_collect(
            self.conn,
            "SELECT w.id, r.local_path, w.path, w.branch, w.completed_at
             FROM worktrees w
             JOIN repos r ON r.id = w.repo_id
             WHERE w.status IN ('merged', 'abandoned')",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;

        let mut reaped = 0;
        let mut pruned_repos = std::collections::HashSet::new();

        for (wt_id, repo_path, wt_path, branch, completed_at) in &stale {
            if !Path::new(wt_path).exists() {
                // Backfill completed_at if missing even when path is already gone
                if completed_at.is_none() {
                    self.backfill_completed_at(wt_id)?;
                    reaped += 1;
                }
                continue;
            }

            remove_git_artifacts(repo_path, wt_path, branch);
            pruned_repos.insert(repo_path.clone());

            // Backfill completed_at if NULL
            if completed_at.is_none() {
                self.backfill_completed_at(wt_id)?;
            }

            reaped += 1;
        }

        // Run git worktree prune on each affected repo
        for repo_path in &pruned_repos {
            let _ = git_in(repo_path).args(["worktree", "prune"]).output();
        }

        Ok(reaped)
    }

    fn backfill_completed_at(&self, wt_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE worktrees SET completed_at = ?1 WHERE id = ?2 AND completed_at IS NULL",
            params![now, wt_id],
        )?;
        Ok(())
    }

    /// Permanently delete completed (merged/abandoned) worktree records from the database.
    pub fn purge(&self, repo_slug: &str, name: Option<&str>) -> Result<usize> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        let count = if let Some(slug) = name {
            self.conn.execute(
                "DELETE FROM worktrees WHERE repo_id = ?1 AND slug = ?2 AND status != 'active'",
                params![repo.id, slug],
            )?
        } else {
            self.conn.execute(
                "DELETE FROM worktrees WHERE repo_id = ?1 AND status != 'active'",
                params![repo.id],
            )?
        };

        Ok(count)
    }

    /// Scan all active worktrees for merged PRs and clean them up.
    ///
    /// For each active worktree whose PR has been merged:
    /// 1. Mark status as `merged` with `completed_at`
    /// 2. Remove local git artifacts (worktree dir + local branch)
    /// 3. Delete the remote branch (best-effort)
    /// 4. Auto-close orphaned features
    ///
    /// When `repo_slug` is `Some`, only worktrees for that repo are checked.
    /// Returns the number of worktrees cleaned up.
    pub fn cleanup_merged_worktrees(&self, repo_slug: Option<&str>) -> Result<usize> {
        self.cleanup_merged_worktrees_with_merge_check(
            repo_slug,
            crate::github::merged_branches_for_repo,
            pull_ff_only,
        )
    }

    pub(crate) fn cleanup_merged_worktrees_with_merge_check(
        &self,
        repo_slug: Option<&str>,
        merge_check: impl Fn(&str, &[String]) -> std::collections::HashSet<String>,
        pull_fn: impl Fn(&str, &str) -> std::result::Result<(), String>,
    ) -> Result<usize> {
        let base_query =
            "SELECT w.id, w.branch, w.path, r.local_path, r.remote_url, w.repo_id, w.base_branch
                 FROM worktrees w
                 JOIN repos r ON r.id = w.repo_id
                 WHERE w.status = 'active'";
        let query = match repo_slug {
            Some(_) => format!("{base_query} AND r.slug = ?1"),
            None => base_query.to_string(),
        };

        let mapper = |row: &rusqlite::Row| -> rusqlite::Result<[String; 7]> {
            Ok([
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get::<_, Option<String>>(6)?.unwrap_or_default(),
            ])
        };
        let rows: Vec<[String; 7]> = match repo_slug {
            Some(slug) => query_collect(self.conn, &query, params![slug], mapper)?,
            None => query_collect(self.conn, &query, [], mapper)?,
        };

        // Group branches by remote_url and batch-check merged status per repo.
        let mut branches_by_remote: std::collections::HashMap<&str, Vec<String>> =
            std::collections::HashMap::new();
        for row in &rows {
            branches_by_remote
                .entry(row[4].as_str())
                .or_default()
                .push(row[1].clone());
        }
        let mut merged_branches: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for (remote_url, branches) in &branches_by_remote {
            merged_branches.extend(merge_check(remote_url, branches));
        }

        let now = Utc::now().to_rfc3339();
        let mut cleaned = 0usize;
        let mut pruned_repos: std::collections::HashSet<&str> = std::collections::HashSet::new();
        // Track (repo_id, base_branch) pairs already pulled to avoid redundant subprocesses
        let mut pulled_bases: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        // Track (repo_id, base_branch) pairs already checked for auto-ready-for-review to avoid N+1
        let mut checked_ready: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();

        for row in &rows {
            let [wt_id, branch, wt_path, repo_path, _remote_url, repo_id, base_branch] = row;
            if !merged_branches.contains(branch) {
                continue;
            }

            tracing::info!(
                worktree = %wt_id,
                branch = %branch,
                "merged PR detected — cleaning up worktree"
            );

            // Mark as merged
            self.conn.execute(
                "UPDATE worktrees SET status = 'merged', completed_at = ?1 WHERE id = ?2",
                params![now, wt_id],
            )?;

            // Remove local git artifacts
            remove_git_artifacts(repo_path, wt_path, branch);

            // Delete remote branch (best-effort)
            delete_remote_branch(repo_path, branch);

            pruned_repos.insert(repo_path.as_str());

            // Auto-close orphaned features
            let fm = crate::feature::FeatureManager::new(self.conn, self.config);
            let base = if base_branch.is_empty() {
                None
            } else {
                Some(base_branch.as_str())
            };
            if let Err(e) = fm.auto_close_after_worktree_delete(repo_id, base) {
                tracing::warn!(error = %e, "failed to auto-close orphaned feature during cleanup");
            }

            // Auto-transition feature to ready_for_review when last worktree merges.
            // Skip pairs already processed this loop iteration to avoid N+1 queries.
            if self.config.general.auto_ready_for_review && !base_branch.is_empty() {
                let ready_key = (repo_id.clone(), base_branch.clone());
                if checked_ready.insert(ready_key) {
                    if let Err(e) = fm.auto_ready_for_review_if_complete(repo_id, base_branch) {
                        tracing::warn!(error = %e, "failed to auto-transition feature to ready_for_review");
                    }
                }
            }

            // Auto-pull base branch worktree if tracked and active
            let pull_key = (repo_id.clone(), base_branch.clone());
            if !base_branch.is_empty() && !pulled_bases.contains(&pull_key) {
                match self.get_by_branch(repo_id, base_branch) {
                    Ok(base_wt) if base_wt.status == WorktreeStatus::Active => {
                        if !base_wt.path.is_empty() {
                            if let Err(e) = pull_fn(&base_wt.path, base_branch) {
                                tracing::warn!(
                                    base_branch = %base_branch,
                                    error = %e,
                                    "auto-pull of base branch failed after sub-PR merge"
                                );
                            }
                        }
                        pulled_bases.insert(pull_key);
                    }
                    Ok(_) => {
                        // base worktree exists but is not active — skip, but record to avoid
                        // repeated DB lookups if other sub-PRs target the same base
                        pulled_bases.insert(pull_key);
                    }
                    Err(ConductorError::WorktreeNotFound { .. }) => {} // not tracked — skip
                    Err(e) => {
                        tracing::warn!(error = %e, base_branch = %base_branch, "failed to look up base branch worktree");
                    }
                }
            }

            cleaned += 1;
        }

        // Run git worktree prune once per unique repo path
        for repo_path in &pruned_repos {
            let _ = git_in(repo_path).args(["worktree", "prune"]).output();
        }

        Ok(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::create_test_conn;

    fn insert_ticket(conn: &Connection, id: &str, repo_id: &str, source_id: &str, raw_json: &str) {
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES (?1, ?2, 'vantage', ?3, 'Test', '', 'open', '[]', '', '2024-01-01T00:00:00Z', ?4)",
            rusqlite::params![id, repo_id, source_id, raw_json],
        ).unwrap();
    }

    fn insert_worktree_with_ticket(
        conn: &Connection,
        id: &str,
        repo_id: &str,
        ticket_id: &str,
        status: &str,
    ) {
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, ticket_id, created_at) \
             VALUES (?1, ?2, ?1, 'feat/dep', '/tmp/dep', ?3, ?4, '2024-01-01T00:00:00Z')",
            rusqlite::params![id, repo_id, status, ticket_id],
        ).unwrap();
    }

    fn insert_repo(conn: &Connection) {
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('r1','test-repo','/tmp/repo','https://github.com/x/y.git','/tmp/ws','2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
    }

    fn insert_wt(conn: &Connection, id: &str, slug: &str, created_at: &str) {
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES (?1, 'r1', ?2, 'feat/test', '/tmp/ws', 'active', ?3)",
            rusqlite::params![id, slug, created_at],
        )
        .unwrap();
    }

    #[test]
    fn list_pagination_limit_truncates_results() {
        let conn = crate::test_helpers::create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "slug-a", "2024-01-01T00:00:00Z");
        insert_wt(&conn, "wt2", "slug-b", "2024-01-02T00:00:00Z");
        insert_wt(&conn, "wt3", "slug-c", "2024-01-03T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        let results = mgr.list_paginated(Some("test-repo"), false, 2, 0).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn list_pagination_offset_skips_results() {
        let conn = crate::test_helpers::create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "slug-a", "2024-01-01T00:00:00Z");
        insert_wt(&conn, "wt2", "slug-b", "2024-01-02T00:00:00Z");
        insert_wt(&conn, "wt3", "slug-c", "2024-01-03T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        // Offset 2 should return only the last row (ordered by active-first, then created_at)
        let results = mgr.list_paginated(Some("test-repo"), false, 10, 2).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn list_pagination_limit_offset_second_page() {
        let conn = crate::test_helpers::create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "slug-a", "2024-01-01T00:00:00Z");
        insert_wt(&conn, "wt2", "slug-b", "2024-01-02T00:00:00Z");
        insert_wt(&conn, "wt3", "slug-c", "2024-01-03T00:00:00Z");
        insert_wt(&conn, "wt4", "slug-d", "2024-01-04T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        let page1 = mgr.list_paginated(Some("test-repo"), false, 2, 0).unwrap();
        let page2 = mgr.list_paginated(Some("test-repo"), false, 2, 2).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page2.len(), 2);
        // Pages must not overlap
        let ids1: Vec<_> = page1.iter().map(|w| w.id.clone()).collect();
        let ids2: Vec<_> = page2.iter().map(|w| w.id.clone()).collect();
        assert!(ids1.iter().all(|id| !ids2.contains(id)));
    }

    #[test]
    fn list_no_pagination_returns_all() {
        let conn = crate::test_helpers::create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "slug-a", "2024-01-01T00:00:00Z");
        insert_wt(&conn, "wt2", "slug-b", "2024-01-02T00:00:00Z");
        insert_wt(&conn, "wt3", "slug-c", "2024-01-03T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        let results = mgr.list(Some("test-repo"), false).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn list_pagination_no_repo_slug_uses_all_repos() {
        let conn = crate::test_helpers::create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "slug-a", "2024-01-01T00:00:00Z");
        insert_wt(&conn, "wt2", "slug-b", "2024-01-02T00:00:00Z");
        insert_wt(&conn, "wt3", "slug-c", "2024-01-03T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        let results = mgr.list_paginated(None, false, 2, 0).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn resolve_parent_branch_returns_none_for_non_vantage_ticket() {
        let conn = create_test_conn();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) VALUES ('r1','repo','/p','u','/w','2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'github', '42', 'Issue', '', 'open', '[]', '', '2024-01-01T00:00:00Z', '{}')",
            [],
        ).unwrap();
        assert!(resolve_parent_branch(&conn, "t1", "r1").is_none());
    }

    #[test]
    fn resolve_parent_branch_returns_none_when_no_dependencies() {
        let conn = create_test_conn();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) VALUES ('r1','repo','/p','u','/w','2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        insert_ticket(&conn, "t1", "r1", "D-001", r#"{"id":"D-001"}"#);
        assert!(resolve_parent_branch(&conn, "t1", "r1").is_none());
    }

    #[test]
    fn resolve_parent_branch_finds_active_parent_worktree() {
        let conn = create_test_conn();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) VALUES ('r1','repo','/p','u','/w','2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        // Parent ticket (dep D-000) with an active worktree
        insert_ticket(&conn, "parent", "r1", "D-000", r#"{"id":"D-000"}"#);
        insert_worktree_with_ticket(&conn, "wt-parent", "r1", "parent", "active");
        // Child ticket depending on D-000
        insert_ticket(
            &conn,
            "child",
            "r1",
            "D-001",
            r#"{"id":"D-001","dependencies":["D-000"]}"#,
        );
        let branch = resolve_parent_branch(&conn, "child", "r1");
        assert_eq!(branch, Some("feat/dep".to_string()));
    }

    #[test]
    fn resolve_parent_branch_returns_none_when_parent_worktree_not_active() {
        let conn = create_test_conn();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) VALUES ('r1','repo','/p','u','/w','2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        insert_ticket(&conn, "parent", "r1", "D-000", r#"{"id":"D-000"}"#);
        insert_worktree_with_ticket(&conn, "wt-parent", "r1", "parent", "merged");
        insert_ticket(
            &conn,
            "child",
            "r1",
            "D-001",
            r#"{"id":"D-001","dependencies":["D-000"]}"#,
        );
        assert!(resolve_parent_branch(&conn, "child", "r1").is_none());
    }

    // ── cleanup_merged_worktrees pull_fn tests ──────────────────────────────

    /// Returns a merge_check closure that marks only "feat/sub-task" as merged.
    fn merged_sub_task_check() -> impl Fn(&str, &[String]) -> std::collections::HashSet<String> {
        |_remote_url: &str, _branches: &[String]| {
            let mut set = std::collections::HashSet::new();
            set.insert("feat/sub-task".to_string());
            set
        }
    }

    fn insert_worktree_with_base(
        conn: &Connection,
        id: &str,
        branch: &str,
        path: &str,
        status: &str,
        base_branch: &str,
    ) {
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, base_branch, created_at) \
             VALUES (?1, 'r1', ?1, ?2, ?3, ?4, ?5, '2024-01-01T00:00:00Z')",
            rusqlite::params![id, branch, path, status, base_branch],
        ).unwrap();
    }

    #[test]
    fn test_cleanup_pulls_base_branch_worktree() {
        use std::sync::{Arc, Mutex};

        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);

        // Sub-PR worktree (will be detected as merged)
        insert_worktree_with_base(
            &conn,
            "wt-sub",
            "feat/sub-task",
            "/tmp/sub",
            "active",
            "feat/epic",
        );

        // Base worktree (active, should be pulled)
        insert_worktree_with_base(&conn, "wt-base", "feat/epic", "/tmp/epic", "active", "");

        let pulled: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let pulled_clone = pulled.clone();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.cleanup_merged_worktrees_with_merge_check(
            Some("test-repo"),
            merged_sub_task_check(),
            move |path, branch| {
                pulled_clone
                    .lock()
                    .unwrap()
                    .push((path.to_string(), branch.to_string()));
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        let calls = pulled.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "/tmp/epic");
        assert_eq!(calls[0].1, "feat/epic");
    }

    #[test]
    fn test_cleanup_skips_pull_when_base_not_tracked() {
        use std::sync::{Arc, Mutex};

        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);

        // Sub-PR worktree with a base_branch that has no worktree entry in DB
        insert_worktree_with_base(
            &conn,
            "wt-sub",
            "feat/sub-task",
            "/tmp/sub",
            "active",
            "feat/untracked-epic",
        );

        let pulled: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let pulled_clone = pulled.clone();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.cleanup_merged_worktrees_with_merge_check(
            Some("test-repo"),
            merged_sub_task_check(),
            move |path, branch| {
                pulled_clone
                    .lock()
                    .unwrap()
                    .push((path.to_string(), branch.to_string()));
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        // pull_fn must never have been called
        assert!(pulled.lock().unwrap().is_empty());
    }

    #[test]
    fn test_cleanup_pull_failure_does_not_block() {
        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);

        // Sub-PR worktree
        insert_worktree_with_base(
            &conn,
            "wt-sub",
            "feat/sub-task",
            "/tmp/sub",
            "active",
            "feat/epic",
        );

        // Base worktree (active)
        insert_worktree_with_base(&conn, "wt-base", "feat/epic", "/tmp/epic", "active", "");

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.cleanup_merged_worktrees_with_merge_check(
            Some("test-repo"),
            merged_sub_task_check(),
            |_path, _branch| Err("simulated pull failure".to_string()),
        );

        // Cleanup must still succeed and report 1 worktree cleaned
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        // Sub worktree must be marked merged
        let status: String = conn
            .query_row(
                "SELECT status FROM worktrees WHERE id = 'wt-sub'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "merged");
    }

    #[test]
    fn test_cleanup_skips_pull_when_base_worktree_not_active() {
        // Verifies the Ok(_) arm: base worktree is in DB but not active → pull_fn must not fire.
        use std::sync::{Arc, Mutex};

        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);

        // Sub-PR worktree targeting a base that exists but is already merged
        insert_worktree_with_base(
            &conn,
            "wt-sub",
            "feat/sub-task",
            "/tmp/sub",
            "active",
            "feat/epic",
        );

        // Base worktree present in DB but status = merged (not active)
        insert_worktree_with_base(&conn, "wt-base", "feat/epic", "/tmp/epic", "merged", "");

        let pulled: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let pulled_clone = pulled.clone();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.cleanup_merged_worktrees_with_merge_check(
            Some("test-repo"),
            merged_sub_task_check(),
            move |path, branch| {
                pulled_clone
                    .lock()
                    .unwrap()
                    .push((path.to_string(), branch.to_string()));
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        // pull_fn must never have been called — base worktree is not active
        assert!(pulled.lock().unwrap().is_empty());
    }
}
