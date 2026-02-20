use chrono::Utc;
use rusqlite::{params, Connection};
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

        // Check for duplicate
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM worktrees WHERE repo_id = ?1 AND slug = ?2)",
            params![repo.id, wt_slug],
            |row| row.get(0),
        )?;
        if exists {
            return Err(ConductorError::WorktreeAlreadyExists {
                slug: wt_slug.clone(),
            });
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
                "SELECT w.id, w.repo_id, w.slug, w.branch, w.path, w.ticket_id, w.status, w.created_at
                 FROM worktrees w
                 JOIN repos r ON r.id = w.repo_id
                 WHERE r.slug = ?1
                 ORDER BY w.created_at"
            }
            None => {
                "SELECT id, repo_id, slug, branch, path, ticket_id, status, created_at
                 FROM worktrees ORDER BY created_at"
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

    pub fn delete(&self, repo_slug: &str, name: &str) -> Result<()> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        let worktree = self
            .conn
            .query_row(
                "SELECT id, repo_id, slug, branch, path, ticket_id, status, created_at
                 FROM worktrees WHERE repo_id = ?1 AND slug = ?2",
                params![repo.id, name],
                map_worktree_row,
            )
            .map_err(|_| ConductorError::WorktreeNotFound {
                slug: name.to_string(),
            })?;

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

        self.conn
            .execute("DELETE FROM worktrees WHERE id = ?1", params![worktree.id])?;

        Ok(())
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
    })
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
