use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::process::Command;

use crate::config::Config;
use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use crate::git::{check_gh_output, check_output, git_in};
use crate::repo::RepoManager;

/// Typed representation of the three worktree lifecycle states stored in the DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeStatus {
    Active,
    Merged,
    Abandoned,
}

impl WorktreeStatus {
    /// Returns `true` for terminal states (`Merged` or `Abandoned`).
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Merged | Self::Abandoned)
    }

    /// Return the canonical lowercase string stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            WorktreeStatus::Active => "active",
            WorktreeStatus::Merged => "merged",
            WorktreeStatus::Abandoned => "abandoned",
        }
    }
}

impl fmt::Display for WorktreeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for WorktreeStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "merged" => Ok(Self::Merged),
            "abandoned" => Ok(Self::Abandoned),
            _ => Err(format!("unknown WorktreeStatus: {s}")),
        }
    }
}

crate::impl_sql_enum!(WorktreeStatus);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub id: String,
    pub repo_id: String,
    pub slug: String,
    pub branch: String,
    pub path: String,
    pub ticket_id: Option<String>,
    pub status: WorktreeStatus,
    pub created_at: String,
    pub completed_at: Option<String>,
    /// Per-worktree default model override. Overrides global config; overridden by per-run.
    pub model: Option<String>,
    /// The branch this worktree was created from. NULL means the repo's default branch.
    pub base_branch: Option<String>,
}

impl Worktree {
    pub fn is_active(&self) -> bool {
        self.status == WorktreeStatus::Active
    }

    /// Returns true if this worktree is a child of the given feature
    /// (same repo and base_branch matches the feature branch).
    pub fn belongs_to_feature(&self, repo_id: &str, feature_branch: &str) -> bool {
        self.repo_id == repo_id && self.base_branch.as_deref() == Some(feature_branch)
    }

    /// Resolve the effective base branch: the worktree's own base, or the repo default.
    pub fn effective_base<'a>(&'a self, repo_default: &'a str) -> &'a str {
        self.base_branch.as_deref().unwrap_or(repo_default)
    }
}

const WORKTREE_COLUMNS: &str =
    "id, repo_id, slug, branch, path, ticket_id, status, created_at, completed_at, model, base_branch";

static WORKTREE_COLUMNS_W: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| crate::db::prefix_columns(WORKTREE_COLUMNS, "w."));

pub struct WorktreeManager<'a> {
    conn: &'a Connection,
    config: &'a Config,
}

impl<'a> WorktreeManager<'a> {
    pub fn new(conn: &'a Connection, config: &'a Config) -> Self {
        Self { conn, config }
    }

    /// Create a new worktree, ensuring the base branch is up to date first.
    ///
    /// Returns the created worktree and a list of non-fatal warnings
    /// (e.g., fetch failures, diverged base branch).
    ///
    /// When `from_pr` is `Some(n)`, the worktree is backed by the branch of PR #n
    /// instead of a newly-created branch.  `from_branch` is ignored in that case.
    pub fn create(
        &self,
        repo_slug: &str,
        name: &str,
        from_branch: Option<&str>,
        ticket_id: Option<&str>,
        from_pr: Option<u32>,
    ) -> Result<(Worktree, Vec<String>)> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        // Determine branch name and worktree slug
        let (wt_slug, branch) = if name.starts_with("fix-") {
            let clean = name.strip_prefix("fix-").unwrap();
            (format!("fix-{clean}"), format!("fix/{clean}"))
        } else {
            let clean = name.strip_prefix("feat-").unwrap_or(name);
            (format!("feat-{clean}"), format!("feat/{clean}"))
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
        let (branch, base_for_db, mut warnings) = if let Some(pr_number) = from_pr {
            // --from-pr path: fetch the PR branch and record the PR's base branch
            // so that create_pr can target the correct base.
            let (pr_branch, pr_base) = fetch_pr_branch(&repo.local_path, pr_number)?;
            (pr_branch, Some(pr_base), Vec::new())
        } else {
            // Normal path: resolve base, ensure it's up to date, create a new branch.
            let base = from_branch
                .map(|b| b.to_string())
                .unwrap_or_else(|| resolve_base_branch(&repo.local_path, &repo.default_branch));
            let warnings = ensure_base_up_to_date(&repo.local_path, &base)?;
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
            ticket_id: ticket_id.map(|s| s.to_string()),
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

        // Auto-register feature if targeting a non-default branch
        if let Some(ref base_branch) = worktree.base_branch {
            let fm = crate::feature::FeatureManager::new(self.conn, self.config);
            match fm.ensure_feature_for_branch(&repo, base_branch, None) {
                Ok(Some(feature)) => {
                    warnings.push(format!(
                        "Auto-registered feature '{}' for branch '{}'",
                        feature.name, feature.branch
                    ));
                }
                Ok(None) => {}
                Err(e) => {
                    warnings.push(format!(
                        "Warning: failed to auto-register feature for branch '{}': {}",
                        base_branch, e
                    ));
                }
            }
        }

        Ok((worktree, warnings))
    }

    pub fn get_by_id(&self, id: &str) -> Result<Worktree> {
        self.conn
            .query_row(
                &format!("SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE id = ?1"),
                params![id],
                map_worktree_row,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ConductorError::WorktreeNotFound {
                    slug: id.to_string(),
                },
                _ => ConductorError::Database(e),
            })
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
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ConductorError::WorktreeNotFound {
                    slug: slug.to_string(),
                },
                _ => ConductorError::Database(e),
            })
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
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ConductorError::WorktreeNotFound {
                    slug: branch.to_string(),
                },
                _ => ConductorError::Database(e),
            })
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

    pub fn list(&self, repo_slug: Option<&str>, active_only: bool) -> Result<Vec<Worktree>> {
        let status_filter = if active_only {
            " AND status = 'active'"
        } else {
            ""
        };

        let query = match repo_slug {
            Some(_) => {
                format!(
                    "SELECT {} FROM worktrees w JOIN repos r ON r.id = w.repo_id WHERE r.slug = ?1{} ORDER BY CASE WHEN w.status = 'active' THEN 0 ELSE 1 END, w.created_at",
                    &*WORKTREE_COLUMNS_W,
                    status_filter
                )
            }
            None => {
                format!(
                    "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE 1=1{} ORDER BY CASE WHEN status = 'active' THEN 0 ELSE 1 END, created_at",
                    status_filter
                )
            }
        };

        let worktrees = if let Some(slug) = repo_slug {
            query_collect(self.conn, &query, params![slug], map_worktree_row)?
        } else {
            query_collect(self.conn, &query, [], map_worktree_row)?
        };
        Ok(worktrees)
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
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ConductorError::WorktreeNotFound {
                    slug: name.to_string(),
                },
                _ => ConductorError::Database(e),
            })?;

        self.delete_internal(&repo, worktree, None)
    }

    pub fn delete_by_id(&self, worktree_id: &str) -> Result<Worktree> {
        let worktree = self.get_by_id(worktree_id)?;
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

        Ok(Worktree {
            status: new_status,
            completed_at: Some(now),
            ..worktree
        })
    }

    /// Remove the git worktree directory and delete the associated branch (best-effort).
    /// Failures are logged but not propagated. Delegates to the module-private
    /// `remove_git_artifacts` to keep the implementation detail encapsulated.
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
        // Auto-register feature if targeting a non-default branch
        if let Some(branch) = base_branch {
            let fm = crate::feature::FeatureManager::new(self.conn, self.config);
            if let Err(e) = fm.ensure_feature_for_branch(&repo, branch, None) {
                tracing::warn!(
                    repo_slug = repo_slug,
                    branch = branch,
                    error = %e,
                    "failed to auto-register feature for base branch"
                );
            }
        }
        Ok(())
    }

    /// Push the worktree branch to origin.
    pub fn push(&self, repo_slug: &str, name: &str) -> Result<String> {
        let (_repo, worktree) = self.get_active_worktree(repo_slug, name)?;

        check_output(git_in(&worktree.path).args(["push", "-u", "origin", &worktree.branch]))?;

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

        let worktree = self
            .conn
            .query_row(
                &format!(
                    "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE repo_id = ?1 AND slug = ?2"
                ),
                params![repo.id, wt_slug],
                map_worktree_row,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ConductorError::WorktreeNotFound {
                    slug: wt_slug.to_string(),
                },
                _ => ConductorError::Database(e),
            })?;

        if !worktree.is_active() {
            return Err(ConductorError::Git(format!(
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
                    let now = Utc::now().to_rfc3339();
                    self.conn.execute(
                        "UPDATE worktrees SET completed_at = ?1 WHERE id = ?2 AND completed_at IS NULL",
                        params![now, wt_id],
                    )?;
                    reaped += 1;
                }
                continue;
            }

            remove_git_artifacts(repo_path, wt_path, branch);
            pruned_repos.insert(repo_path.clone());

            // Backfill completed_at if NULL
            if completed_at.is_none() {
                let now = Utc::now().to_rfc3339();
                self.conn.execute(
                    "UPDATE worktrees SET completed_at = ?1 WHERE id = ?2 AND completed_at IS NULL",
                    params![now, wt_id],
                )?;
            }

            reaped += 1;
        }

        // Run git worktree prune on each affected repo
        for repo_path in &pruned_repos {
            let _ = git_in(repo_path).args(["worktree", "prune"]).output();
        }

        Ok(reaped)
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
}

fn map_worktree_row(row: &rusqlite::Row) -> rusqlite::Result<Worktree> {
    Ok(Worktree {
        id: row.get(0)?,
        repo_id: row.get(1)?,
        slug: row.get(2)?,
        branch: row.get(3)?,
        path: row.get(4)?,
        ticket_id: row.get(5)?,
        status: row.get::<_, WorktreeStatus>(6)?,
        created_at: row.get(7)?,
        completed_at: row.get(8)?,
        model: row.get(9)?,
        base_branch: row.get(10)?,
    })
}

/// Resolve the base branch for a repo using a priority order:
/// 1. The configured default branch (from DB) if it exists locally
/// 2. `git symbolic-ref refs/remotes/origin/HEAD` (remote default)
/// 3. Fall back to `main`, then `master`
/// 4. Final fallback: return the configured default regardless
fn resolve_base_branch(repo_path: &str, configured_default: &str) -> String {
    if branch_exists(repo_path, configured_default) {
        return configured_default.to_string();
    }

    if let Some(branch) = detect_remote_head(repo_path) {
        return branch;
    }

    for name in &["main", "master"] {
        if branch_exists(repo_path, name) {
            return name.to_string();
        }
    }

    configured_default.to_string()
}

/// Check if a local branch exists.
fn branch_exists(repo_path: &str, branch: &str) -> bool {
    git_in(repo_path)
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Detect the default branch from the remote's HEAD ref.
fn detect_remote_head(repo_path: &str) -> Option<String> {
    let output = git_in(repo_path)
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let refname = String::from_utf8_lossy(&output.stdout).trim().to_string();
    refname
        .strip_prefix("refs/remotes/origin/")
        .map(|s| s.to_string())
}

/// Ensure the base branch is up to date with the remote before creating a worktree.
///
/// Returns a list of non-fatal warnings (fetch failure, diverged branch, etc.).
/// Returns `Err` only for hard failures like a dirty working tree.
fn ensure_base_up_to_date(repo_path: &str, base_branch: &str) -> Result<Vec<String>> {
    let mut warnings = Vec::new();

    // 1. Check for uncommitted changes in the repo working tree
    let output = git_in(repo_path).args(["status", "--porcelain"]).output()?;
    if output.status.success() && !output.stdout.is_empty() {
        return Err(ConductorError::Git(
            "uncommitted changes on base branch, please commit or stash first".to_string(),
        ));
    }

    // 2. Fetch from remote (soft failure — warn and allow local-only creation)
    let fetch = git_in(repo_path).args(["fetch", "origin"]).output();
    match fetch {
        Ok(o) if o.status.success() => {}
        _ => {
            warnings.push(
                "could not fetch from origin; creating worktree from local state".to_string(),
            );
            return Ok(warnings);
        }
    }

    // 3. Check if the remote tracking branch exists
    let remote_ref = format!("refs/remotes/origin/{base_branch}");
    let has_remote = git_in(repo_path)
        .args(["rev-parse", "--verify", &remote_ref])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !has_remote {
        return Ok(warnings);
    }

    // 4. Determine which branch is currently checked out
    let current_branch = git_in(repo_path)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    // 5. Fast-forward the base branch
    let origin_ref = format!("origin/{base_branch}");
    if current_branch == base_branch {
        // Base is already checked out — merge directly
        let merge = git_in(repo_path)
            .args(["merge", "--ff-only", &origin_ref])
            .output();
        if !merge.map(|o| o.status.success()).unwrap_or(false) {
            warnings.push(format!(
                "base branch '{}' has diverged from origin; consider `git pull --rebase`",
                base_branch
            ));
        }
    } else {
        // Need to checkout base branch first (handles detached HEAD too)
        let checkout = git_in(repo_path).args(["checkout", base_branch]).output();
        match checkout {
            Ok(o) if o.status.success() => {
                let merge = git_in(repo_path)
                    .args(["merge", "--ff-only", &origin_ref])
                    .output();
                if !merge.map(|o| o.status.success()).unwrap_or(false) {
                    warnings.push(format!(
                        "base branch '{}' has diverged from origin; consider `git pull --rebase`",
                        base_branch
                    ));
                }
            }
            _ => {
                warnings.push(format!(
                    "could not checkout '{}'; creating worktree from local state",
                    base_branch
                ));
            }
        }
    }

    Ok(warnings)
}

/// Remove the git worktree directory and delete the associated branch.
/// Both operations are best-effort: failures are logged but not propagated because the
/// worktree or branch may already be gone (e.g. manually removed).
fn remove_git_artifacts(repo_path: &str, worktree_path: &str, branch: &str) {
    match git_in(repo_path)
        .args(["worktree", "remove", worktree_path, "--force"])
        .output()
    {
        Ok(o) if !o.status.success() => {
            tracing::warn!(
                repo = repo_path,
                worktree = worktree_path,
                stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                "git worktree remove failed"
            );
        }
        Err(e) => {
            tracing::warn!(
                repo = repo_path,
                worktree = worktree_path,
                error = %e,
                "failed to run git worktree remove"
            );
        }
        Ok(_) => {}
    }

    match git_in(repo_path)
        .args(["branch", "-D", "--", branch])
        .output()
    {
        Ok(o) if !o.status.success() => {
            tracing::warn!(
                repo = repo_path,
                branch = branch,
                stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                "git branch -D failed"
            );
        }
        Err(e) => {
            tracing::warn!(
                repo = repo_path,
                branch = branch,
                error = %e,
                "failed to run git branch -D"
            );
        }
        Ok(_) => {}
    }
}

/// Clone a remote repository into `local_path`.
/// Uses `git clone -- <remote_url> <local_path>` so that a `remote_url`
/// starting with `-` cannot be misinterpreted as a flag.
fn clone_repo(remote_url: &str, local_path: &str) -> Result<()> {
    check_output(Command::new("git").args(["clone", "--", remote_url, local_path]))?;
    Ok(())
}

/// Parse the raw output of the `gh pr view` jq expression used in
/// `fetch_pr_branch`.  The expected format is:
///
/// ```text
/// <head_branch>|<base_branch>|<head_owner>/<head_repo>|<true|false>
/// ```
///
/// The fourth field is the value of `isCrossRepository` (true for fork PRs).
///
/// Returns `(head_branch, base_branch, head_repo, is_fork)`.
fn parse_pr_view_output(raw: &str) -> Result<(String, String, String, bool)> {
    let raw = raw.trim();
    let parts: Vec<&str> = raw.splitn(4, '|').collect();
    if parts.len() < 4 {
        return Err(ConductorError::GhCli(format!(
            "unexpected gh pr view output: {raw}"
        )));
    }
    let head_branch = parts[0].to_string();
    let base_branch = parts[1].to_string();
    let head_repo = parts[2].to_string();
    let is_fork = parts[3].trim() == "true";
    Ok((head_branch, base_branch, head_repo, is_fork))
}

/// Fetch a PR's branch from the appropriate remote and return `(head_branch,
/// base_branch)`.
///
/// For same-repo PRs the branch is fetched from `origin`.  For fork PRs the
/// fork remote is added temporarily (or reused if it already exists) and the
/// branch is fetched from there.
fn fetch_pr_branch(repo_path: &str, pr_number: u32) -> Result<(String, String)> {
    // 1. Resolve the PR's head branch name, base branch, and repository info
    let output = check_gh_output(
        Command::new("gh")
            .args([
                "pr",
                "view",
                &pr_number.to_string(),
                "--json",
                "headRefName,baseRefName,headRepository,isCrossRepository",
                "--jq",
                ".headRefName + \"|\" + .baseRefName + \"|\" + .headRepository.owner.login + \"/\" + .headRepository.name + \"|\" + (.isCrossRepository | tostring)",
            ])
            .current_dir(repo_path),
    )?;

    let raw = String::from_utf8_lossy(&output.stdout);
    let (head_branch, base_branch, head_repo, is_fork) = parse_pr_view_output(&raw)?;

    if is_fork {
        // For fork PRs: add the fork as a named remote (using the owner login)
        // and fetch from it.
        let fork_owner = head_repo.split('/').next().unwrap_or(&head_repo);

        // Look up the fork's clone URL via gh api
        let url_output = check_gh_output(
            Command::new("gh")
                .args(["api", &format!("repos/{head_repo}"), "--jq", ".clone_url"])
                .current_dir(repo_path),
        )?;

        let fork_url = String::from_utf8_lossy(&url_output.stdout)
            .trim()
            .to_string();

        // Add the remote if it doesn't already exist (ignore failure if it does)
        let _ = git_in(repo_path)
            .args(["remote", "add", fork_owner, &fork_url])
            .output();

        // Fetch the branch from the fork remote
        check_output(git_in(repo_path).args([
            "fetch",
            fork_owner,
            &format!("{head_branch}:{head_branch}"),
        ]))?;
    } else {
        // Same-repo PR: fetch from origin
        check_output(git_in(repo_path).args([
            "fetch",
            "origin",
            &format!("{head_branch}:{head_branch}"),
        ]))?;
    }

    Ok((head_branch, base_branch))
}

/// Detect package manager and install dependencies if applicable.
fn install_deps(worktree_path: &Path) {
    if worktree_path.join("package.json").exists() {
        // Detect lockfile to choose the right package manager
        let pm = if worktree_path.join("bun.lockb").exists()
            || worktree_path.join("bun.lock").exists()
        {
            "bun"
        } else if worktree_path.join("pnpm-lock.yaml").exists() {
            "pnpm"
        } else if worktree_path.join("yarn.lock").exists() {
            "yarn"
        } else {
            "npm"
        };
        let _ = Command::new(pm)
            .arg("install")
            .current_dir(worktree_path)
            .output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: run a git command in a directory, panicking on failure.
    fn git(args: &[&str], dir: &std::path::Path) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Create a bare "remote" repo and a local clone that tracks it.
    /// Returns (tmp_dir, remote_path, local_path). TempDir must be kept alive.
    fn setup_repo_with_remote() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let remote_path = tmp.path().join("remote.git");
        let local_path = tmp.path().join("local");

        // Create bare remote with explicit main branch
        fs::create_dir_all(&remote_path).unwrap();
        git(&["init", "--bare", "-b", "main"], &remote_path);

        // Clone it
        git(
            &[
                "clone",
                &remote_path.to_string_lossy(),
                &local_path.to_string_lossy(),
            ],
            tmp.path(),
        );

        // Configure user for commits
        git(&["config", "user.email", "test@test.com"], &local_path);
        git(&["config", "user.name", "Test"], &local_path);

        // Ensure we're on the main branch (CI may not have init.defaultBranch=main)
        git(&["checkout", "-b", "main"], &local_path);

        // Create initial commit on main
        let file = local_path.join("README.md");
        fs::write(&file, "initial").unwrap();
        git(&["add", "README.md"], &local_path);
        git(&["commit", "-m", "initial"], &local_path);
        git(&["push", "-u", "origin", "main"], &local_path);

        (tmp, remote_path, local_path)
    }

    #[test]
    fn test_branch_exists() {
        let (_tmp, _, local) = setup_repo_with_remote();
        assert!(branch_exists(local.to_str().unwrap(), "main"));
        assert!(!branch_exists(local.to_str().unwrap(), "nonexistent"));
    }

    #[test]
    fn test_detect_remote_head() {
        let (_tmp, _, local) = setup_repo_with_remote();
        // Local clones don't auto-set origin/HEAD; set it explicitly (as GitHub does)
        git(&["remote", "set-head", "origin", "main"], &local);
        let detected = detect_remote_head(local.to_str().unwrap());
        assert_eq!(detected, Some("main".to_string()));
    }

    #[test]
    fn test_detect_remote_head_not_set() {
        let (_tmp, _, local) = setup_repo_with_remote();
        // Without setting origin/HEAD, detection returns None
        let detected = detect_remote_head(local.to_str().unwrap());
        assert_eq!(detected, None);
    }

    #[test]
    fn test_resolve_base_branch_uses_configured() {
        let (_tmp, _, local) = setup_repo_with_remote();
        let result = resolve_base_branch(local.to_str().unwrap(), "main");
        assert_eq!(result, "main");
    }

    #[test]
    fn test_resolve_base_branch_falls_back_to_detection() {
        let (_tmp, _, local) = setup_repo_with_remote();
        // Pass a non-existent configured default; should detect "main" via remote HEAD
        let result = resolve_base_branch(local.to_str().unwrap(), "nonexistent");
        assert_eq!(result, "main");
    }

    #[test]
    fn test_ensure_base_up_to_date_clean_fast_forward() {
        let (_tmp, remote, local) = setup_repo_with_remote();

        // Simulate a new commit on remote by cloning elsewhere and pushing
        let tmp2 = TempDir::new().unwrap();
        let other = tmp2.path().join("other");
        git(
            &["clone", &remote.to_string_lossy(), &other.to_string_lossy()],
            tmp2.path(),
        );
        git(&["config", "user.email", "test@test.com"], &other);
        git(&["config", "user.name", "Test"], &other);
        let file = other.join("new_file.txt");
        fs::write(&file, "new content").unwrap();
        git(&["add", "new_file.txt"], &other);
        git(&["commit", "-m", "remote commit"], &other);
        git(&["push", "origin", "main"], &other);

        // Local is now behind origin/main
        let warnings = ensure_base_up_to_date(local.to_str().unwrap(), "main").unwrap();
        assert!(warnings.is_empty(), "unexpected warnings: {:?}", warnings);

        // Verify local main now has the new file
        assert!(local.join("new_file.txt").exists());
    }

    #[test]
    fn test_ensure_base_up_to_date_dirty_working_tree() {
        let (_tmp, _, local) = setup_repo_with_remote();

        // Make the working tree dirty
        fs::write(local.join("dirty.txt"), "uncommitted").unwrap();

        let result = ensure_base_up_to_date(local.to_str().unwrap(), "main");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("uncommitted changes"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_ensure_base_up_to_date_diverged_branch() {
        let (_tmp, remote, local) = setup_repo_with_remote();

        // Push a commit from another clone
        let tmp2 = TempDir::new().unwrap();
        let other = tmp2.path().join("other");
        git(
            &["clone", &remote.to_string_lossy(), &other.to_string_lossy()],
            tmp2.path(),
        );
        git(&["config", "user.email", "test@test.com"], &other);
        git(&["config", "user.name", "Test"], &other);
        fs::write(other.join("remote.txt"), "from remote").unwrap();
        git(&["add", "remote.txt"], &other);
        git(&["commit", "-m", "remote diverge"], &other);
        git(&["push", "origin", "main"], &other);

        // Make a LOCAL commit on main that diverges
        fs::write(local.join("local.txt"), "from local").unwrap();
        git(&["add", "local.txt"], &local);
        git(&["commit", "-m", "local diverge"], &local);

        // Now ensure_base_up_to_date should warn about divergence
        let warnings = ensure_base_up_to_date(local.to_str().unwrap(), "main").unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("diverged")),
            "expected divergence warning, got: {:?}",
            warnings
        );
    }

    #[test]
    fn test_list_by_ticket() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();

        // Insert tickets referenced by worktrees
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'github', '1', 'Ticket 1', '', 'open', '[]', '', '2024-01-01T00:00:00Z', '{}')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t2', 'r1', 'github', '2', 'Ticket 2', '', 'open', '[]', '', '2024-01-01T00:00:00Z', '{}')",
            [],
        ).unwrap();

        // Insert worktrees with ticket_id
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at) \
             VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 't1', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at) \
             VALUES ('wt2', 'r1', 'feat-b', 'feat/b', '/tmp/ws/feat-b', 't1', 'merged', '2024-01-02T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at) \
             VALUES ('wt3', 'r1', 'feat-c', 'feat/c', '/tmp/ws/feat-c', 't2', 'active', '2024-01-03T00:00:00Z')",
            [],
        ).unwrap();

        let mgr = WorktreeManager::new(&conn, &config);

        // Should return 2 worktrees for ticket t1, ordered by created_at DESC
        let worktrees = mgr.list_by_ticket("t1").unwrap();
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].id, "wt2"); // newer first
        assert_eq!(worktrees[1].id, "wt1");

        // Should return 1 worktree for ticket t2
        let worktrees = mgr.list_by_ticket("t2").unwrap();
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].id, "wt3");

        // Should return empty for unknown ticket
        let worktrees = mgr.list_by_ticket("nonexistent").unwrap();
        assert!(worktrees.is_empty());
    }

    #[test]
    fn test_ensure_base_up_to_date_detached_head() {
        let (_tmp, remote, local) = setup_repo_with_remote();

        // Push a second commit from another clone so there's something to ff
        let tmp2 = TempDir::new().unwrap();
        let other = tmp2.path().join("other");
        git(
            &["clone", &remote.to_string_lossy(), &other.to_string_lossy()],
            tmp2.path(),
        );
        git(&["config", "user.email", "test@test.com"], &other);
        git(&["config", "user.name", "Test"], &other);
        fs::write(other.join("extra.txt"), "extra").unwrap();
        git(&["add", "extra.txt"], &other);
        git(&["commit", "-m", "extra commit"], &other);
        git(&["push", "origin", "main"], &other);

        // Detach HEAD in local
        git(&["checkout", "--detach", "HEAD"], &local);

        let warnings = ensure_base_up_to_date(local.to_str().unwrap(), "main").unwrap();
        // Should succeed (checkout main, then ff) with no warnings
        assert!(warnings.is_empty(), "unexpected warnings: {:?}", warnings);

        // Verify we're now on main and have the extra file
        assert!(local.join("extra.txt").exists());
    }

    #[test]
    fn test_worktree_status_is_done() {
        assert!(!WorktreeStatus::Active.is_done());
        assert!(WorktreeStatus::Merged.is_done());
        assert!(WorktreeStatus::Abandoned.is_done());
    }

    #[test]
    fn test_worktree_status_as_str() {
        assert_eq!(WorktreeStatus::Active.as_str(), "active");
        assert_eq!(WorktreeStatus::Merged.as_str(), "merged");
        assert_eq!(WorktreeStatus::Abandoned.as_str(), "abandoned");
    }

    #[test]
    fn test_worktree_status_display() {
        assert_eq!(WorktreeStatus::Active.to_string(), "active");
        assert_eq!(WorktreeStatus::Merged.to_string(), "merged");
        assert_eq!(WorktreeStatus::Abandoned.to_string(), "abandoned");
    }

    #[test]
    fn test_worktree_status_from_str_valid() {
        assert_eq!(
            "active".parse::<WorktreeStatus>().unwrap(),
            WorktreeStatus::Active
        );
        assert_eq!(
            "merged".parse::<WorktreeStatus>().unwrap(),
            WorktreeStatus::Merged
        );
        assert_eq!(
            "abandoned".parse::<WorktreeStatus>().unwrap(),
            WorktreeStatus::Abandoned
        );
    }

    #[test]
    fn test_worktree_status_from_str_invalid() {
        let err = "unknown_value".parse::<WorktreeStatus>().unwrap_err();
        assert_eq!(err, "unknown WorktreeStatus: unknown_value");
    }

    #[test]
    fn test_update_status_to_merged_sets_completed_at() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        mgr.update_status("wt1", WorktreeStatus::Merged).unwrap();

        let wt = mgr.get_by_id("wt1").unwrap();
        assert_eq!(wt.status, WorktreeStatus::Merged);
        assert!(wt.completed_at.is_some());
    }

    #[test]
    fn test_update_status_to_abandoned_sets_completed_at() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        mgr.update_status("wt1", WorktreeStatus::Abandoned).unwrap();

        let wt = mgr.get_by_id("wt1").unwrap();
        assert_eq!(wt.status, WorktreeStatus::Abandoned);
        assert!(wt.completed_at.is_some());
    }

    // ---- get_by_slug_or_branch tests ----

    fn insert_test_worktree(conn: &Connection, id: &str, repo_id: &str, slug: &str, branch: &str) {
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, '/tmp/ws', 'active', '2024-01-01T00:00:00Z')",
            params![id, repo_id, slug, branch],
        )
        .unwrap();
    }

    #[test]
    fn test_get_by_slug_or_branch_slug_match() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        insert_test_worktree(
            &conn,
            "wt1",
            "r1",
            "feat-123-my-feature",
            "feat/123-my-feature",
        );

        let mgr = WorktreeManager::new(&conn, &config);
        let wt = mgr
            .get_by_slug_or_branch("r1", "feat-123-my-feature")
            .unwrap();
        assert_eq!(wt.id, "wt1");
    }

    #[test]
    fn test_get_by_slug_or_branch_branch_match() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        insert_test_worktree(
            &conn,
            "wt1",
            "r1",
            "feat-123-my-feature",
            "feat/123-my-feature",
        );

        let mgr = WorktreeManager::new(&conn, &config);
        let wt = mgr
            .get_by_slug_or_branch("r1", "feat/123-my-feature")
            .unwrap();
        assert_eq!(wt.id, "wt1");
    }

    #[test]
    fn test_get_by_slug_or_branch_did_you_mean() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        insert_test_worktree(
            &conn,
            "wt1",
            "r1",
            "feat-123-my-feature",
            "feat/123-my-feature",
        );
        insert_test_worktree(&conn, "wt2", "r1", "fix-456-other", "fix/456-other");

        let mgr = WorktreeManager::new(&conn, &config);
        let err = mgr
            .get_by_slug_or_branch("r1", "totally-wrong")
            .unwrap_err()
            .to_string();
        assert!(err.contains("totally-wrong"), "error: {err}");
        assert!(err.contains("did you mean"), "error: {err}");
        assert!(err.contains("feat-123-my-feature"), "error: {err}");
    }

    #[test]
    fn test_get_by_slug_or_branch_empty_repo() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();

        // Use a repo ID that has no worktrees seeded in the test DB.
        let mgr = WorktreeManager::new(&conn, &config);
        let err = mgr
            .get_by_slug_or_branch("repo-with-no-worktrees", "anything")
            .unwrap_err()
            .to_string();
        assert!(err.contains("anything"), "error: {err}");
        // No "did you mean" hint when repo has no worktrees
        assert!(!err.contains("did you mean"), "error: {err}");
    }

    #[test]
    fn test_update_status_to_active_clears_completed_at() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, completed_at) \
             VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'merged', '2024-01-01T00:00:00Z', '2024-02-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        mgr.update_status("wt1", WorktreeStatus::Active).unwrap();

        let wt = mgr.get_by_id("wt1").unwrap();
        assert_eq!(wt.status, WorktreeStatus::Active);
        assert!(wt.completed_at.is_none());
    }

    #[test]
    fn test_remove_git_artifacts_success() {
        let (_tmp, _, local) = setup_repo_with_remote();
        let local_str = local.to_str().unwrap();

        // Create a branch and a worktree for it
        let wt_path = local.parent().unwrap().join("feat-test-wt");
        git(
            &[
                "worktree",
                "add",
                wt_path.to_str().unwrap(),
                "-b",
                "feat/test-wt",
            ],
            &local,
        );

        assert!(wt_path.exists());
        assert!(branch_exists(local_str, "feat/test-wt"));

        // remove_git_artifacts should cleanly remove both
        remove_git_artifacts(local_str, wt_path.to_str().unwrap(), "feat/test-wt");

        assert!(!wt_path.exists());
        assert!(!branch_exists(local_str, "feat/test-wt"));
    }

    #[test]
    fn test_remove_git_artifacts_nonexistent_does_not_panic() {
        let (_tmp, _, local) = setup_repo_with_remote();
        let local_str = local.to_str().unwrap();

        // Both the worktree path and branch are nonexistent; must not panic
        remove_git_artifacts(local_str, "/nonexistent/path/wt", "feat/no-such-branch");
    }

    #[test]
    #[tracing_test::traced_test]
    fn test_remove_git_artifacts_logs_warnings_on_git_failure() {
        let (_tmp, _, local) = setup_repo_with_remote();
        let local_str = local.to_str().unwrap();

        // Both are nonexistent so git will exit non-zero — the warn! arms fire
        remove_git_artifacts(local_str, "/nonexistent/path/wt", "feat/no-such-branch");

        assert!(logs_contain("git worktree remove failed"));
        assert!(logs_contain("git branch -D failed"));
    }

    #[test]
    fn test_reap_stale_worktrees_backfills_completed_at() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        // Insert a merged worktree with no completed_at and a nonexistent path
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-stale', 'r1', 'feat-stale', 'feat/stale', '/nonexistent/stale-wt', 'merged', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        let reaped = mgr.reap_stale_worktrees().unwrap();
        assert_eq!(reaped, 1);

        // completed_at should now be set
        let completed_at: Option<String> = conn
            .query_row(
                "SELECT completed_at FROM worktrees WHERE id = 'wt-stale'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(completed_at.is_some());
    }

    #[test]
    fn test_reap_stale_worktrees_skips_active() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        // w1 from setup_db is active — should not be reaped
        let mgr = WorktreeManager::new(&conn, &config);
        let reaped = mgr.reap_stale_worktrees().unwrap();
        assert_eq!(reaped, 0);
    }

    #[test]
    fn test_reap_stale_worktrees_skips_already_completed() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        // Insert a merged worktree that already has completed_at and nonexistent path
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, completed_at) \
             VALUES ('wt-done', 'r1', 'feat-done', 'feat/done', '/nonexistent/done-wt', 'merged', '2024-01-01T00:00:00Z', '2024-02-01T00:00:00Z')",
            [],
        ).unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        let reaped = mgr.reap_stale_worktrees().unwrap();
        // Path doesn't exist and completed_at is already set → not reaped
        assert_eq!(reaped, 0);
    }

    #[test]
    fn test_reap_stale_worktrees_removes_existing_path() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        let (_tmp, _, local) = setup_repo_with_remote();
        let local_str = local.to_str().unwrap();

        // Update repo to use real local path
        conn.execute(
            "UPDATE repos SET local_path = ?1 WHERE id = 'r1'",
            params![local_str],
        )
        .unwrap();

        // Create a real worktree
        let wt_path = local.parent().unwrap().join("stale-wt");
        git(&["branch", "feat/stale-wt"], &local);
        git(
            &[
                "worktree",
                "add",
                &wt_path.to_string_lossy(),
                "feat/stale-wt",
            ],
            &local,
        );
        assert!(wt_path.exists());

        // Insert as merged with no completed_at
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-real', 'r1', 'feat-stale-wt', 'feat/stale-wt', ?1, 'merged', '2024-01-01T00:00:00Z')",
            params![wt_path.to_str().unwrap()],
        ).unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        let reaped = mgr.reap_stale_worktrees().unwrap();
        assert_eq!(reaped, 1);

        // Worktree directory should be removed
        assert!(!wt_path.exists());

        // completed_at should be backfilled
        let completed_at: Option<String> = conn
            .query_row(
                "SELECT completed_at FROM worktrees WHERE id = 'wt-real'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(completed_at.is_some());
    }

    #[test]
    fn test_create_auto_clones_missing_local_path() {
        let (tmp, remote, _local) = setup_repo_with_remote();

        // Point local_path to a directory that does not yet exist
        let missing_local = tmp.path().join("not-yet-cloned");

        let conn = crate::test_helpers::setup_db();
        let mut config = Config::default();
        config.general.workspace_root = tmp.path().to_path_buf();

        let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
        let _repo = repo_mgr
            .register(
                "myrepo",
                missing_local.to_str().unwrap(),
                remote.to_str().unwrap(),
                Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
            )
            .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.create("myrepo", "feat-auto-clone", None, None, None);
        assert!(
            result.is_ok(),
            "expected Ok, got: {:?}",
            result.unwrap_err()
        );

        // The local repo should now exist on disk (cloned)
        assert!(missing_local.exists(), "local_path should have been cloned");

        // The worktree directory should also exist
        let (wt, _) = result.unwrap();
        assert!(
            Path::new(&wt.path).exists(),
            "worktree path should exist: {}",
            wt.path
        );
    }

    #[test]
    fn test_create_clone_fails_with_bad_remote() {
        let tmp = TempDir::new().unwrap();
        let missing_local = tmp.path().join("not-yet-cloned");

        let conn = crate::test_helpers::setup_db();
        let mut config = Config::default();
        config.general.workspace_root = tmp.path().to_path_buf();

        let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
        repo_mgr
            .register(
                "badrepo",
                missing_local.to_str().unwrap(),
                "file:///this/does/not/exist/at/all",
                Some(tmp.path().join("workspaces/badrepo").to_str().unwrap()),
            )
            .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.create("badrepo", "feat-should-fail", None, None, None);
        assert!(result.is_err(), "expected Err for bad remote");
        match result.unwrap_err() {
            ConductorError::Git(_) => {}
            other => panic!("expected ConductorError::Git, got: {other:?}"),
        }
    }

    #[test]
    fn test_reap_stale_worktrees_handles_abandoned() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-aband', 'r1', 'feat-aband', 'feat/aband', '/nonexistent/aband-wt', 'abandoned', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        let reaped = mgr.reap_stale_worktrees().unwrap();
        assert_eq!(reaped, 1);

        let completed_at: Option<String> = conn
            .query_row(
                "SELECT completed_at FROM worktrees WHERE id = 'wt-aband'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(completed_at.is_some());
    }

    // ── parse_pr_view_output tests ────────────────────────────────────────────

    #[test]
    fn test_parse_pr_view_output_same_repo() {
        let raw = "feat/my-feature|main|owner/repo|false";
        let (head, base, head_repo, is_fork) = parse_pr_view_output(raw).unwrap();
        assert_eq!(head, "feat/my-feature");
        assert_eq!(base, "main");
        assert_eq!(head_repo, "owner/repo");
        assert!(!is_fork);
    }

    #[test]
    fn test_parse_pr_view_output_fork() {
        let raw = "feat/my-feature|main|fork-user/repo|true";
        let (head, base, head_repo, is_fork) = parse_pr_view_output(raw).unwrap();
        assert_eq!(head, "feat/my-feature");
        assert_eq!(base, "main");
        assert_eq!(head_repo, "fork-user/repo");
        assert!(is_fork);
    }

    #[test]
    fn test_parse_pr_view_output_non_default_base() {
        // PR targeting a release branch rather than the repo default
        let raw = "feat/my-feature|release/v2|owner/repo|false";
        let (head, base, _head_repo, is_fork) = parse_pr_view_output(raw).unwrap();
        assert_eq!(head, "feat/my-feature");
        assert_eq!(base, "release/v2");
        assert!(!is_fork);
    }

    #[test]
    fn test_parse_pr_view_output_bad_format() {
        let raw = "incomplete|data";
        let result = parse_pr_view_output(raw);
        let err = result.unwrap_err();
        assert!(
            matches!(&err, crate::error::ConductorError::GhCli(_)),
            "expected GhCli variant, got: {err:?}"
        );
        assert!(err.to_string().contains("unexpected gh pr view output"));
    }

    #[test]
    fn test_parse_pr_view_output_empty() {
        let result = parse_pr_view_output("");
        assert!(result.is_err());
    }

    #[test]
    fn test_fetch_pr_branch_fails_without_github_remote() {
        // A local-only repo has no GitHub remote, so gh pr view will fail.
        // This exercises the error path of fetch_pr_branch.
        let (_tmp, _, local) = setup_repo_with_remote();
        let result = fetch_pr_branch(local.to_str().unwrap(), 999);
        let err = result.unwrap_err();
        assert!(
            matches!(err, ConductorError::GhCli(_)),
            "expected GhCli error, got: {err:?}"
        );
    }

    #[test]
    fn test_create_from_pr_propagates_fetch_error() {
        // Verify that create() with from_pr = Some(n) takes the from_pr branch,
        // calls fetch_pr_branch, and propagates the error when gh is unavailable.
        let (_tmp, _, local) = setup_repo_with_remote();
        let local_str = local.to_str().unwrap().to_string();

        let conn = crate::test_helpers::setup_db();
        let config = Config::default();

        // Point the test repo at the real local path so clone check passes
        conn.execute(
            "UPDATE repos SET local_path = ?1, workspace_dir = ?2 WHERE id = 'r1'",
            params![
                local_str,
                local.parent().unwrap().join("ws").to_str().unwrap()
            ],
        )
        .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.create("test-repo", "from-pr-test", None, None, Some(42));
        // fetch_pr_branch will fail because the local repo has no GitHub remote
        let err = result.unwrap_err();
        assert!(
            matches!(err, ConductorError::GhCli(_)),
            "expected GhCli error, got: {err:?}"
        );
    }

    #[test]
    fn test_find_by_cwd_no_match() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.find_by_cwd(Path::new("/tmp/other/path")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_find_by_cwd_exact_match() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.find_by_cwd(Path::new("/tmp/ws/feat-a")).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "wt1");
    }

    #[test]
    fn test_find_by_cwd_subdirectory_match() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr
            .find_by_cwd(Path::new("/tmp/ws/feat-a/src/lib"))
            .unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "wt1");
    }

    #[test]
    fn test_find_by_cwd_longest_prefix_wins() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        // wt1 is a prefix of wt2's path — wt2 should win when cwd is inside wt2
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt2', 'r1', 'feat-b', 'feat/b', '/tmp/ws/feat-a/nested', 'active', '2024-01-02T00:00:00Z')",
            [],
        )
        .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        // cwd is inside the nested worktree — should return wt2, not wt1
        let result = mgr
            .find_by_cwd(Path::new("/tmp/ws/feat-a/nested/src"))
            .unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "wt2");
    }

    #[test]
    fn test_worktree_columns_w_derivation() {
        // Every column in WORKTREE_COLUMNS must appear in WORKTREE_COLUMNS_W
        // with the "w." prefix, in the same order, with no extra whitespace.
        let expected: String = WORKTREE_COLUMNS
            .split(',')
            .map(|col| format!("w.{}", col.trim()))
            .collect::<Vec<_>>()
            .join(", ");

        assert_eq!(*WORKTREE_COLUMNS_W, expected);
    }

    #[test]
    fn test_get_by_ids_empty_returns_empty_vec() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        let mgr = WorktreeManager::new(&conn, &config);
        // Empty slice must not produce an `IN ()` SQL syntax error
        let result = mgr.get_by_ids(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_by_ids_returns_matching_worktrees() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();

        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt1', 'r1', 'feat-a', 'feat/a', '/tmp/ws/feat-a', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt2', 'r1', 'feat-b', 'feat/b', '/tmp/ws/feat-b', 'active', '2024-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt3', 'r1', 'feat-c', 'feat/c', '/tmp/ws/feat-c', 'active', '2024-01-03T00:00:00Z')",
            [],
        )
        .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);

        // Fetch two of the three; the third must not appear
        let mut result = mgr.get_by_ids(&["wt1", "wt2"]).unwrap();
        result.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "wt1");
        assert_eq!(result[1].id, "wt2");

        // Nonexistent ID returns nothing extra
        let result = mgr.get_by_ids(&["nonexistent"]).unwrap();
        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------------
    // Worktree::belongs_to_feature() tests
    // -----------------------------------------------------------------------

    fn make_worktree_with_base(repo_id: &str, base_branch: Option<&str>) -> Worktree {
        Worktree {
            id: "wt-test".into(),
            repo_id: repo_id.into(),
            slug: "test-wt".into(),
            branch: "feat/child".into(),
            path: "/tmp/test".into(),
            ticket_id: None,
            status: WorktreeStatus::Active,
            created_at: "2026-01-01T00:00:00Z".into(),
            completed_at: None,
            model: None,
            base_branch: base_branch.map(String::from),
        }
    }

    #[test]
    fn belongs_to_feature_matching_repo_and_branch() {
        let wt = make_worktree_with_base("repo1", Some("feat/parent"));
        assert!(wt.belongs_to_feature("repo1", "feat/parent"));
    }

    #[test]
    fn belongs_to_feature_mismatched_repo() {
        let wt = make_worktree_with_base("repo1", Some("feat/parent"));
        assert!(!wt.belongs_to_feature("repo2", "feat/parent"));
    }

    #[test]
    fn belongs_to_feature_mismatched_branch() {
        let wt = make_worktree_with_base("repo1", Some("feat/parent"));
        assert!(!wt.belongs_to_feature("repo1", "feat/other"));
    }

    #[test]
    fn belongs_to_feature_none_base_branch() {
        let wt = make_worktree_with_base("repo1", None);
        assert!(!wt.belongs_to_feature("repo1", "feat/parent"));
    }

    #[test]
    fn test_create_auto_registers_feature_for_non_default_base() {
        let (tmp, remote, local) = setup_repo_with_remote();

        // Create a feature branch in the repo to use as a non-default base
        git(&["checkout", "-b", "feat/parent"], &local);
        let file = local.join("feature.txt");
        fs::write(&file, "feature work").unwrap();
        git(&["add", "feature.txt"], &local);
        git(&["commit", "-m", "feature commit"], &local);
        git(&["push", "-u", "origin", "feat/parent"], &local);
        git(&["checkout", "main"], &local);

        let conn = crate::test_helpers::setup_db();
        let mut config = Config::default();
        config.general.workspace_root = tmp.path().to_path_buf();

        let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
        repo_mgr
            .register(
                "myrepo",
                local.to_str().unwrap(),
                remote.to_str().unwrap(),
                Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
            )
            .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        let (wt, _warnings) = mgr
            .create("myrepo", "feat-child", Some("feat/parent"), None, None)
            .expect("create should succeed");

        // Worktree should have feat/parent as its base branch
        assert_eq!(wt.base_branch.as_deref(), Some("feat/parent"));

        // Auto-registration should have happened inside create()
        let fm = crate::feature::FeatureManager::new(&conn, &config);
        let features = fm.list_active("myrepo").unwrap();
        assert!(
            features.iter().any(|f| f.branch == "feat/parent"),
            "expected a feature for 'feat/parent' to be auto-registered, got: {features:?}"
        );
    }

    #[test]
    fn test_create_skips_auto_registration_for_default_branch() {
        // Creating a worktree from the default branch should not trigger
        // auto-registration of a feature.
        let (tmp, remote, local) = setup_repo_with_remote();

        let conn = crate::test_helpers::setup_db();
        let mut config = Config::default();
        config.general.workspace_root = tmp.path().to_path_buf();

        let repo_mgr = crate::repo::RepoManager::new(&conn, &config);
        repo_mgr
            .register(
                "myrepo",
                local.to_str().unwrap(),
                remote.to_str().unwrap(),
                Some(tmp.path().join("workspaces/myrepo").to_str().unwrap()),
            )
            .unwrap();

        // Create a worktree from main (default branch)
        let mgr = WorktreeManager::new(&conn, &config);
        let (wt, _warnings) = mgr
            .create("myrepo", "feat-on-main", None, None, None)
            .expect("create should succeed");

        // base_branch should be "main" (default) — auto-registration should skip it
        assert!(
            wt.base_branch.is_none() || wt.base_branch.as_deref() == Some("main"),
            "expected no non-default base_branch"
        );
        let fm = crate::feature::FeatureManager::new(&conn, &config);
        let features = fm.list_active("myrepo").unwrap();
        assert!(
            features.is_empty(),
            "should not have any features for default branch, got: {features:?}"
        );
    }

    #[test]
    fn test_set_base_branch() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        let mgr = WorktreeManager::new(&conn, &config);

        // Initially base_branch should be NULL
        let wt: Option<String> = conn
            .query_row(
                "SELECT base_branch FROM worktrees WHERE slug = 'feat-test'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(wt.is_none(), "expected NULL base_branch initially");

        // Set base branch to a feature branch
        mgr.set_base_branch("test-repo", "feat-test", Some("develop"))
            .unwrap();
        let wt: Option<String> = conn
            .query_row(
                "SELECT base_branch FROM worktrees WHERE slug = 'feat-test'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(wt.as_deref(), Some("develop"));

        // Clear base branch (reset to repo default)
        mgr.set_base_branch("test-repo", "feat-test", None)
            .unwrap();
        let wt: Option<String> = conn
            .query_row(
                "SELECT base_branch FROM worktrees WHERE slug = 'feat-test'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(wt.is_none(), "expected NULL after clearing base_branch");
    }

    #[test]
    fn test_set_base_branch_not_found() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        let mgr = WorktreeManager::new(&conn, &config);

        let result = mgr.set_base_branch("test-repo", "nonexistent", Some("develop"));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConductorError::WorktreeNotFound { slug } => {
                assert_eq!(slug, "nonexistent");
            }
            other => panic!("expected WorktreeNotFound, got: {other}"),
        }
    }
}
