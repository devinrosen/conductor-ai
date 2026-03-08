use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::process::Command;

use crate::config::Config;
use crate::db::query_collect;
use crate::error::{ConductorError, Result};
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

    /// Resolve the effective base branch: the worktree's own base, or the repo default.
    pub fn effective_base<'a>(&'a self, repo_default: &'a str) -> &'a str {
        self.base_branch.as_deref().unwrap_or(repo_default)
    }
}

const WORKTREE_COLUMNS: &str =
    "id, repo_id, slug, branch, path, ticket_id, status, created_at, completed_at, model, base_branch";

const WORKTREE_COLUMNS_W: &str =
    "w.id, w.repo_id, w.slug, w.branch, w.path, w.ticket_id, w.status, w.created_at, w.completed_at, w.model, w.base_branch";

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
    pub fn create(
        &self,
        repo_slug: &str,
        name: &str,
        from_branch: Option<&str>,
        ticket_id: Option<&str>,
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

        // Resolve the base branch: explicit --from flag, or detect from repo
        let base = from_branch
            .map(|b| b.to_string())
            .unwrap_or_else(|| resolve_base_branch(&repo.local_path, &repo.default_branch));

        // Ensure the base branch is up to date with the remote
        let warnings = ensure_base_up_to_date(&repo.local_path, &base)?;

        let wt_path = Path::new(&repo.workspace_dir).join(&wt_slug);

        // Create git branch
        check_output(git_in(&repo.local_path).args(["branch", "--", &branch, &base]))?;

        // Create git worktree
        check_output(git_in(&repo.local_path).args([
            "worktree",
            "add",
            &wt_path.to_string_lossy(),
            &branch,
        ]))?;

        // Detect and install deps
        install_deps(&wt_path);

        let id = ulid::Ulid::new().to_string();
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
            base_branch: Some(base.clone()),
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
            .map_err(|_| ConductorError::WorktreeNotFound {
                slug: id.to_string(),
            })
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
            .map_err(|_| ConductorError::WorktreeNotFound {
                slug: slug.to_string(),
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
                    "SELECT {WORKTREE_COLUMNS_W} FROM worktrees w JOIN repos r ON r.id = w.repo_id WHERE r.slug = ?1{} ORDER BY CASE WHEN w.status = 'active' THEN 0 ELSE 1 END, w.created_at",
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
            .map_err(|_| ConductorError::WorktreeNotFound {
                slug: name.to_string(),
            })?;

        self.delete_internal(&repo, worktree, None)
    }

    pub fn delete_by_id(&self, worktree_id: &str) -> Result<Worktree> {
        let worktree = self.get_by_id(worktree_id)?;
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_id(&worktree.repo_id)?;
        self.delete_internal(&repo, worktree, None)
    }

    /// Like [`delete_by_id`] but skips the ticket-state DB query when the
    /// caller already knows the worktree was merged.
    pub(crate) fn delete_by_id_as_merged(&self, worktree_id: &str) -> Result<Worktree> {
        let worktree = self.get_by_id(worktree_id)?;
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_id(&worktree.repo_id)?;
        self.delete_internal(&repo, worktree, Some(true))
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
            || is_branch_merged(&repo.local_path, &worktree.branch, &repo.default_branch);
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

        let output = check_output(Command::new("gh").args(&args).current_dir(&worktree.path))?;

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
            .map_err(|_| ConductorError::WorktreeNotFound {
                slug: wt_slug.to_string(),
            })?;

        if !worktree.is_active() {
            return Err(ConductorError::Git(format!(
                "worktree '{}' is not active (status: {})",
                wt_slug, worktree.status
            )));
        }

        Ok((repo, worktree))
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

/// Check if a branch has been merged into the default branch.
fn is_branch_merged(repo_path: &str, branch: &str, default_branch: &str) -> bool {
    let output = git_in(repo_path)
        .args(["branch", "--merged", default_branch])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout
                .lines()
                .any(|line| line.trim().trim_start_matches("* ") == branch)
        }
        _ => false,
    }
}

/// Return a `Command` for `git` rooted at `dir`.
pub(crate) fn git_in(dir: impl AsRef<std::path::Path>) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    cmd
}

/// Run `cmd`, returning its `Output` on success or a `ConductorError::Git` on non-zero exit.
pub(crate) fn check_output(cmd: &mut Command) -> Result<std::process::Output> {
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(ConductorError::Git(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(output)
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
    fn test_delete_by_id_as_merged_updates_status() {
        // setup_db() inserts repo 'r1' (local_path = /tmp/repo) and worktree 'w1'
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        let mgr = WorktreeManager::new(&conn, &config);

        // git operations will fail on fake paths but are best-effort; should not propagate
        let wt = mgr.delete_by_id_as_merged("w1").unwrap();
        assert_eq!(wt.status, WorktreeStatus::Merged);
        assert!(wt.completed_at.is_some());

        // verify persisted in DB
        let persisted = mgr.get_by_id("w1").unwrap();
        assert_eq!(persisted.status, WorktreeStatus::Merged);
        assert!(persisted.completed_at.is_some());
    }

    #[test]
    fn test_delete_by_id_as_merged_unknown_id_returns_error() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        let mgr = WorktreeManager::new(&conn, &config);

        let result = mgr.delete_by_id_as_merged("nonexistent-id");
        assert!(result.is_err());
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
}
