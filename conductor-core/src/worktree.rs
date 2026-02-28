use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::repo::RepoManager;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub id: String,
    pub repo_id: String,
    pub slug: String,
    pub branch: String,
    pub path: String,
    pub ticket_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
}

impl Worktree {
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }
}

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
        let existing_status: Option<String> = self
            .conn
            .query_row(
                "SELECT status FROM worktrees WHERE repo_id = ?1 AND slug = ?2",
                params![repo.id, wt_slug],
                |row| row.get(0),
            )
            .optional()?;

        match existing_status {
            Some(ref s) if s == "active" => {
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
        let output = Command::new("git")
            .args(["branch", &branch, &base])
            .current_dir(&repo.local_path)
            .output()?;
        if !output.status.success() {
            return Err(ConductorError::Git(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        // Create git worktree
        let output = Command::new("git")
            .args(["worktree", "add", &wt_path.to_string_lossy(), &branch])
            .current_dir(&repo.local_path)
            .output()?;
        if !output.status.success() {
            return Err(ConductorError::Git(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

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
            status: "active".to_string(),
            created_at: now,
            completed_at: None,
        };

        self.conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                worktree.id,
                worktree.repo_id,
                worktree.slug,
                worktree.branch,
                worktree.path,
                worktree.ticket_id,
                worktree.status,
                worktree.created_at,
            ],
        )?;

        Ok((worktree, warnings))
    }

    pub fn get_by_id(&self, id: &str) -> Result<Worktree> {
        self.conn
            .query_row(
                "SELECT id, repo_id, slug, branch, path, ticket_id, status, created_at, completed_at
                 FROM worktrees WHERE id = ?1",
                params![id],
                map_worktree_row,
            )
            .map_err(|_| ConductorError::WorktreeNotFound {
                slug: id.to_string(),
            })
    }

    pub fn list_by_repo_id(&self, repo_id: &str, active_only: bool) -> Result<Vec<Worktree>> {
        let status_filter = if active_only {
            " AND status = 'active'"
        } else {
            ""
        };
        let query = format!(
            "SELECT id, repo_id, slug, branch, path, ticket_id, status, created_at, completed_at
             FROM worktrees WHERE repo_id = ?1{}
             ORDER BY CASE WHEN status = 'active' THEN 0 ELSE 1 END, created_at",
            status_filter
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params![repo_id], map_worktree_row)?;
        let worktrees = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(worktrees)
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
                    "SELECT w.id, w.repo_id, w.slug, w.branch, w.path, w.ticket_id, w.status, w.created_at, w.completed_at
                     FROM worktrees w
                     JOIN repos r ON r.id = w.repo_id
                     WHERE r.slug = ?1{}
                     ORDER BY CASE WHEN w.status = 'active' THEN 0 ELSE 1 END, w.created_at",
                    status_filter
                )
            }
            None => {
                format!(
                    "SELECT id, repo_id, slug, branch, path, ticket_id, status, created_at, completed_at
                     FROM worktrees
                     WHERE 1=1{}
                     ORDER BY CASE WHEN status = 'active' THEN 0 ELSE 1 END, created_at",
                    status_filter
                )
            }
        };

        let mut stmt = self.conn.prepare(&query)?;
        let rows = if let Some(slug) = repo_slug {
            stmt.query_map(params![slug], map_worktree_row)?
        } else {
            stmt.query_map([], map_worktree_row)?
        };

        let worktrees = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(worktrees)
    }

    pub fn delete(&self, repo_slug: &str, name: &str) -> Result<Worktree> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        let worktree = self
            .conn
            .query_row(
                "SELECT id, repo_id, slug, branch, path, ticket_id, status, created_at, completed_at
                 FROM worktrees WHERE repo_id = ?1 AND slug = ?2",
                params![repo.id, name],
                map_worktree_row,
            )
            .map_err(|_| ConductorError::WorktreeNotFound {
                slug: name.to_string(),
            })?;

        self.delete_internal(&repo, worktree)
    }

    pub fn delete_by_id(&self, worktree_id: &str) -> Result<Worktree> {
        let worktree = self.get_by_id(worktree_id)?;
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_id(&worktree.repo_id)?;
        self.delete_internal(&repo, worktree)
    }

    fn delete_internal(&self, repo: &crate::repo::Repo, worktree: Worktree) -> Result<Worktree> {
        // Determine merged vs abandoned:
        // 1. Check if the linked ticket is closed (covers squash merges that git can't detect)
        // 2. Fall back to git branch --merged (covers cases without a linked ticket)
        let ticket_closed = worktree
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
            .unwrap_or(false);
        let is_merged = ticket_closed
            || is_branch_merged(&repo.local_path, &worktree.branch, &repo.default_branch);
        let new_status = if is_merged { "merged" } else { "abandoned" };
        let now = Utc::now().to_rfc3339();

        // Remove git worktree
        let _ = Command::new("git")
            .args(["worktree", "remove", &worktree.path, "--force"])
            .current_dir(&repo.local_path)
            .output();

        // Delete git branch
        let _ = Command::new("git")
            .args(["branch", "-D", &worktree.branch])
            .current_dir(&repo.local_path)
            .output();

        // Soft-delete: update status + completed_at instead of deleting the row
        self.conn.execute(
            "UPDATE worktrees SET status = ?1, completed_at = ?2 WHERE id = ?3",
            params![new_status, now, worktree.id],
        )?;

        Ok(Worktree {
            status: new_status.to_string(),
            completed_at: Some(now),
            ..worktree
        })
    }

    pub fn update_status(&self, worktree_id: &str, status: &str) -> Result<()> {
        let completed_at = if status != "active" {
            Some(Utc::now().to_rfc3339())
        } else {
            None
        };
        self.conn.execute(
            "UPDATE worktrees SET status = ?1, completed_at = ?2 WHERE id = ?3",
            params![status, completed_at, worktree_id],
        )?;
        Ok(())
    }

    /// Push the worktree branch to origin.
    pub fn push(&self, repo_slug: &str, name: &str) -> Result<String> {
        let (_repo, worktree) = self.get_active_worktree(repo_slug, name)?;

        let output = Command::new("git")
            .args(["push", "-u", "origin", &worktree.branch])
            .current_dir(&worktree.path)
            .output()?;

        if !output.status.success() {
            return Err(ConductorError::Git(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(format!(
            "Pushed {} to origin/{}",
            worktree.slug, worktree.branch
        ))
    }

    /// Create a pull request for the worktree branch using `gh`.
    pub fn create_pr(&self, repo_slug: &str, name: &str, draft: bool) -> Result<String> {
        let (_repo, worktree) = self.get_active_worktree(repo_slug, name)?;

        let mut args = vec!["pr", "create", "--fill", "--head", &worktree.branch];
        if draft {
            args.push("--draft");
        }

        let output = Command::new("gh")
            .args(&args)
            .current_dir(&worktree.path)
            .output()?;

        if !output.status.success() {
            return Err(ConductorError::Git(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

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
                "SELECT id, repo_id, slug, branch, path, ticket_id, status, created_at, completed_at
                 FROM worktrees WHERE repo_id = ?1 AND slug = ?2",
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
        status: row.get(6)?,
        created_at: row.get(7)?,
        completed_at: row.get(8)?,
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
    Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .current_dir(repo_path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Detect the default branch from the remote's HEAD ref.
fn detect_remote_head(repo_path: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .current_dir(repo_path)
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
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_path)
        .output()?;
    if output.status.success() && !output.stdout.is_empty() {
        return Err(ConductorError::Git(
            "uncommitted changes on base branch, please commit or stash first".to_string(),
        ));
    }

    // 2. Fetch from remote (soft failure — warn and allow local-only creation)
    let fetch = Command::new("git")
        .args(["fetch", "origin"])
        .current_dir(repo_path)
        .output();
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
    let has_remote = Command::new("git")
        .args(["rev-parse", "--verify", &remote_ref])
        .current_dir(repo_path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !has_remote {
        return Ok(warnings);
    }

    // 4. Determine which branch is currently checked out
    let current_branch = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_path)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    // 5. Fast-forward the base branch
    let origin_ref = format!("origin/{base_branch}");
    if current_branch == base_branch {
        // Base is already checked out — merge directly
        let merge = Command::new("git")
            .args(["merge", "--ff-only", &origin_ref])
            .current_dir(repo_path)
            .output();
        if !merge.map(|o| o.status.success()).unwrap_or(false) {
            warnings.push(format!(
                "base branch '{}' has diverged from origin; consider `git pull --rebase`",
                base_branch
            ));
        }
    } else {
        // Need to checkout base branch first (handles detached HEAD too)
        let checkout = Command::new("git")
            .args(["checkout", base_branch])
            .current_dir(repo_path)
            .output();
        match checkout {
            Ok(o) if o.status.success() => {
                let merge = Command::new("git")
                    .args(["merge", "--ff-only", &origin_ref])
                    .current_dir(repo_path)
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

/// Check if a branch has been merged into the default branch.
fn is_branch_merged(repo_path: &str, branch: &str, default_branch: &str) -> bool {
    let output = Command::new("git")
        .args(["branch", "--merged", default_branch])
        .current_dir(repo_path)
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
}
