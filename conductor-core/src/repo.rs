use crate::config::{Config, RepoConfig};
use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: String,
    pub slug: String,
    pub local_path: String,
    pub remote_url: String,
    /// Effective default branch, resolved from per-repo `.conductor/config.toml`
    /// then global config. Not stored in DB — computed on load.
    pub default_branch: String,
    pub workspace_dir: String,
    pub created_at: String,
    /// Per-repo model from `.conductor/config.toml`. Not stored in DB — computed on load.
    #[serde(default)]
    pub model: Option<String>,
    /// Whether agents are allowed to create issues in the issue tracker for this repo.
    pub allow_agent_issue_creation: bool,
}

pub struct RepoManager<'a> {
    conn: &'a Connection,
    config: &'a Config,
}

impl Repo {
    /// Populate the computed `default_branch` and `model` fields from
    /// the per-repo `.conductor/config.toml`, falling back to global config.
    ///
    /// Loads `RepoConfig` once to resolve both fields, avoiding redundant disk reads.
    fn enrich(mut self, global_config: &Config) -> Self {
        let repo_config = RepoConfig::load(Path::new(&self.local_path)).unwrap_or_else(|e| {
            tracing::warn!(
                repo = %self.slug,
                path = %self.local_path,
                "failed to load .conductor/config.toml, using defaults: {e}"
            );
            RepoConfig::default()
        });
        self.default_branch = repo_config
            .defaults
            .default_branch
            .unwrap_or_else(|| global_config.defaults.default_branch.clone());
        self.model = repo_config.defaults.model;
        self
    }
}

impl<'a> RepoManager<'a> {
    pub fn new(conn: &'a Connection, config: &'a Config) -> Self {
        Self { conn, config }
    }

    pub fn register(
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

        let id = crate::new_id();
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
            default_branch: String::new(),
            workspace_dir: ws_dir,
            created_at: now,
            model: None,
            allow_agent_issue_creation: false,
        };

        self.conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                repo.id,
                repo.slug,
                repo.local_path,
                repo.remote_url,
                repo.workspace_dir,
                repo.created_at,
            ],
        )?;

        Ok(repo.enrich(self.config))
    }

    pub fn list(&self) -> Result<Vec<Repo>> {
        let repos = query_collect(
            self.conn,
            "SELECT id, slug, local_path, remote_url, workspace_dir, created_at, \
             COALESCE(allow_agent_issue_creation, 0) as allow_agent_issue_creation \
             FROM repos ORDER BY slug",
            [],
            |row| {
                Ok(Repo {
                    id: row.get(0)?,
                    slug: row.get(1)?,
                    local_path: row.get(2)?,
                    remote_url: row.get(3)?,
                    default_branch: String::new(),
                    workspace_dir: row.get(4)?,
                    created_at: row.get(5)?,
                    model: None,
                    allow_agent_issue_creation: row.get::<_, i64>(6).map(|v| v != 0)?,
                })
            },
        )?;
        Ok(repos.into_iter().map(|r| r.enrich(self.config)).collect())
    }

    pub fn get_by_id(&self, id: &str) -> Result<Repo> {
        self.conn
            .query_row(
                "SELECT id, slug, local_path, remote_url, workspace_dir, \
                 created_at, \
                 COALESCE(allow_agent_issue_creation, 0) as allow_agent_issue_creation \
                 FROM repos WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Repo {
                        id: row.get(0)?,
                        slug: row.get(1)?,
                        local_path: row.get(2)?,
                        remote_url: row.get(3)?,
                        default_branch: String::new(),
                        workspace_dir: row.get(4)?,
                        created_at: row.get(5)?,
                        model: None,
                        allow_agent_issue_creation: row.get::<_, i64>(6).map(|v| v != 0)?,
                    })
                },
            )
            .map(|r| r.enrich(self.config))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ConductorError::RepoNotFound {
                    slug: id.to_string(),
                },
                _ => ConductorError::Database(e),
            })
    }

    pub fn get_by_slug(&self, slug: &str) -> Result<Repo> {
        self.conn
            .query_row(
                "SELECT id, slug, local_path, remote_url, workspace_dir, \
                 created_at, \
                 COALESCE(allow_agent_issue_creation, 0) as allow_agent_issue_creation \
                 FROM repos WHERE slug = ?1",
                params![slug],
                |row| {
                    Ok(Repo {
                        id: row.get(0)?,
                        slug: row.get(1)?,
                        local_path: row.get(2)?,
                        remote_url: row.get(3)?,
                        default_branch: String::new(),
                        workspace_dir: row.get(4)?,
                        created_at: row.get(5)?,
                        model: None,
                        allow_agent_issue_creation: row.get::<_, i64>(6).map(|v| v != 0)?,
                    })
                },
            )
            .map(|r| r.enrich(self.config))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ConductorError::RepoNotFound {
                    slug: slug.to_string(),
                },
                _ => ConductorError::Database(e),
            })
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

    /// Set the per-repo model override in `.conductor/config.toml`.
    /// Pass `None` to clear the override.
    pub fn set_model(&self, slug: &str, model: Option<&str>) -> Result<()> {
        let repo = self.get_by_slug(slug)?;
        let repo_path = Path::new(&repo.local_path);
        let mut repo_config = RepoConfig::load(repo_path)?;
        repo_config.defaults.model = model.map(|s| s.to_string());
        repo_config.save(repo_path)?;
        Ok(())
    }

    pub fn unregister(&self, slug: &str) -> Result<()> {
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

    pub fn unregister_by_id(&self, id: &str) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        conn
    }

    #[test]
    fn test_set_model_writes_repo_config() {
        let dir = tempfile::tempdir().unwrap();
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        // Register a repo pointing at the temp dir
        mgr.register(
            "test-repo",
            dir.path().to_str().unwrap(),
            "https://example.com/repo.git",
            None,
        )
        .unwrap();

        // Set model
        mgr.set_model("test-repo", Some("opus")).unwrap();
        let rc = RepoConfig::load(dir.path()).unwrap();
        assert_eq!(rc.defaults.model.as_deref(), Some("opus"));

        // Clear model
        mgr.set_model("test-repo", None).unwrap();
        let rc = RepoConfig::load(dir.path()).unwrap();
        assert!(rc.defaults.model.is_none(), "model should be cleared");
    }

    #[test]
    fn test_set_model_not_found() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let result = mgr.set_model("nonexistent", Some("opus"));
        assert!(result.is_err());
    }
}
