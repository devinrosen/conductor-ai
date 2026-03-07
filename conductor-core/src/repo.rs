use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::db::query_collect;
use crate::error::{ConductorError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: String,
    pub slug: String,
    pub local_path: String,
    pub remote_url: String,
    pub default_branch: String,
    pub workspace_dir: String,
    pub created_at: String,
    /// Per-repo default model for Claude agent runs.
    #[serde(default)]
    pub model: Option<String>,
    /// Whether agents are allowed to create issues in the issue tracker for this repo.
    pub allow_agent_issue_creation: bool,
}

pub struct RepoManager<'a> {
    conn: &'a Connection,
    config: &'a Config,
}

impl<'a> RepoManager<'a> {
    pub fn new(conn: &'a Connection, config: &'a Config) -> Self {
        Self { conn, config }
    }

    pub fn add(
        &self,
        slug: &str,
        local_path: &str,
        remote_url: &str,
        workspace_dir: Option<&str>,
    ) -> Result<Repo> {
        // Check for duplicates
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM repos WHERE slug = ?1)",
            params![slug],
            |row| row.get(0),
        )?;
        if exists {
            return Err(ConductorError::RepoAlreadyExists {
                slug: slug.to_string(),
            });
        }

        let id = ulid::Ulid::new().to_string();
        let now = Utc::now().to_rfc3339();
        let ws_dir = workspace_dir.map(|s| s.to_string()).unwrap_or_else(|| {
            self.config
                .general
                .workspace_root
                .join(slug)
                .to_string_lossy()
                .to_string()
        });

        let repo = Repo {
            id: id.clone(),
            slug: slug.to_string(),
            local_path: local_path.to_string(),
            remote_url: remote_url.to_string(),
            default_branch: self.config.defaults.default_branch.clone(),
            workspace_dir: ws_dir,
            created_at: now,
            model: None,
            allow_agent_issue_creation: false,
        };

        self.conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                repo.id,
                repo.slug,
                repo.local_path,
                repo.remote_url,
                repo.default_branch,
                repo.workspace_dir,
                repo.created_at,
            ],
        )?;

        Ok(repo)
    }

    pub fn list(&self) -> Result<Vec<Repo>> {
        query_collect(
            self.conn,
            "SELECT id, slug, local_path, remote_url, default_branch, workspace_dir, created_at, \
             COALESCE(model, NULL) as model, \
             COALESCE(allow_agent_issue_creation, 0) as allow_agent_issue_creation \
             FROM repos ORDER BY slug",
            [],
            |row| {
                Ok(Repo {
                    id: row.get(0)?,
                    slug: row.get(1)?,
                    local_path: row.get(2)?,
                    remote_url: row.get(3)?,
                    default_branch: row.get(4)?,
                    workspace_dir: row.get(5)?,
                    created_at: row.get(6)?,
                    model: row.get(7)?,
                    allow_agent_issue_creation: row.get::<_, i64>(8).map(|v| v != 0)?,
                })
            },
        )
    }

    pub fn get_by_id(&self, id: &str) -> Result<Repo> {
        self.conn
            .query_row(
                "SELECT id, slug, local_path, remote_url, default_branch, workspace_dir, \
                 created_at, \
                 COALESCE(model, NULL) as model, \
                 COALESCE(allow_agent_issue_creation, 0) as allow_agent_issue_creation \
                 FROM repos WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Repo {
                        id: row.get(0)?,
                        slug: row.get(1)?,
                        local_path: row.get(2)?,
                        remote_url: row.get(3)?,
                        default_branch: row.get(4)?,
                        workspace_dir: row.get(5)?,
                        created_at: row.get(6)?,
                        model: row.get(7)?,
                        allow_agent_issue_creation: row.get::<_, i64>(8).map(|v| v != 0)?,
                    })
                },
            )
            .map_err(|_| ConductorError::RepoNotFound {
                slug: id.to_string(),
            })
    }

    pub fn get_by_slug(&self, slug: &str) -> Result<Repo> {
        self.conn
            .query_row(
                "SELECT id, slug, local_path, remote_url, default_branch, workspace_dir, \
                 created_at, \
                 COALESCE(model, NULL) as model, \
                 COALESCE(allow_agent_issue_creation, 0) as allow_agent_issue_creation \
                 FROM repos WHERE slug = ?1",
                params![slug],
                |row| {
                    Ok(Repo {
                        id: row.get(0)?,
                        slug: row.get(1)?,
                        local_path: row.get(2)?,
                        remote_url: row.get(3)?,
                        default_branch: row.get(4)?,
                        workspace_dir: row.get(5)?,
                        created_at: row.get(6)?,
                        model: row.get(7)?,
                        allow_agent_issue_creation: row.get::<_, i64>(8).map(|v| v != 0)?,
                    })
                },
            )
            .map_err(|_| ConductorError::RepoNotFound {
                slug: slug.to_string(),
            })
    }

    /// Set (or clear) the per-repo default model.
    /// Pass `None` to clear the override and fall back to global config.
    pub fn set_model(&self, slug: &str, model: Option<&str>) -> Result<()> {
        let affected = self.conn.execute(
            "UPDATE repos SET model = ?1 WHERE slug = ?2",
            params![model, slug],
        )?;
        if affected == 0 {
            return Err(ConductorError::RepoNotFound {
                slug: slug.to_string(),
            });
        }
        Ok(())
    }

    /// Set whether agents can create issues for this repo.
    pub fn set_allow_agent_issue_creation(&self, repo_id: &str, allow: bool) -> Result<()> {
        let affected = self.conn.execute(
            "UPDATE repos SET allow_agent_issue_creation = ?1 WHERE id = ?2",
            params![allow as i64, repo_id],
        )?;
        if affected == 0 {
            return Err(ConductorError::RepoNotFound {
                slug: repo_id.to_string(),
            });
        }
        Ok(())
    }

    pub fn remove(&self, slug: &str) -> Result<()> {
        let affected = self
            .conn
            .execute("DELETE FROM repos WHERE slug = ?1", params![slug])?;
        if affected == 0 {
            return Err(ConductorError::RepoNotFound {
                slug: slug.to_string(),
            });
        }
        Ok(())
    }

    pub fn remove_by_id(&self, id: &str) -> Result<()> {
        let affected = self
            .conn
            .execute("DELETE FROM repos WHERE id = ?1", params![id])?;
        if affected == 0 {
            return Err(ConductorError::RepoNotFound {
                slug: id.to_string(),
            });
        }
        Ok(())
    }
}

/// Derive a repo slug from a remote URL (e.g. "https://github.com/org/repo.git" → "repo").
pub fn derive_slug_from_url(remote_url: &str) -> String {
    let last = remote_url.rsplit('/').next().unwrap_or("repo");
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

/// Derive a default local path for a repo from the config workspace root and slug.
pub fn derive_local_path(config: &Config, slug: &str) -> String {
    config
        .general
        .workspace_root
        .join(slug)
        .join("main")
        .to_string_lossy()
        .to_string()
}
