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

    pub fn create(
        &self,
        repo_slug: &str,
        name: &str,
        from_branch: Option<&str>,
        ticket_id: Option<&str>,
    ) -> Result<Worktree> {
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

        let base = from_branch.unwrap_or(&repo.default_branch);
        let wt_path = Path::new(&repo.workspace_dir).join(&wt_slug);

        // Create git branch
        let output = Command::new("git")
            .args(["branch", &branch, base])
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

        Ok(worktree)
    }

    pub fn list(&self, repo_slug: Option<&str>) -> Result<Vec<Worktree>> {
        let query = match repo_slug {
            Some(_) => {
                "SELECT w.id, w.repo_id, w.slug, w.branch, w.path, w.ticket_id, w.status, w.created_at, w.completed_at
                 FROM worktrees w
                 JOIN repos r ON r.id = w.repo_id
                 WHERE r.slug = ?1
                 ORDER BY CASE WHEN w.status = 'active' THEN 0 ELSE 1 END, w.created_at"
            }
            None => {
                "SELECT id, repo_id, slug, branch, path, ticket_id, status, created_at, completed_at
                 FROM worktrees
                 ORDER BY CASE WHEN status = 'active' THEN 0 ELSE 1 END, created_at"
            }
        };

        let mut stmt = self.conn.prepare(query)?;
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

        // Detect if the branch was merged into the default branch (before removing it)
        let is_merged = is_branch_merged(&repo.local_path, &worktree.branch, &repo.default_branch);
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
