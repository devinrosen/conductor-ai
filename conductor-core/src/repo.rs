use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::config::{Config, RepoConfig};
use crate::db::query_collect;
use crate::error::{ConductorError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: String,
    pub slug: String,
    pub local_path: String,
    pub remote_url: String,
    pub workspace_dir: String,
    pub created_at: String,
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
            workspace_dir: ws_dir,
            created_at: now,
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

        Ok(repo)
    }

    pub fn list(&self) -> Result<Vec<Repo>> {
        query_collect(
            self.conn,
            "SELECT id, slug, local_path, remote_url, workspace_dir, created_at, \
             COALESCE(allow_agent_issue_creation, 0) as allow_agent_issue_creation \
             FROM repos ORDER BY slug",
            [],
            map_repo_row,
        )
    }

    pub fn get_by_id(&self, id: &str) -> Result<Repo> {
        self.conn
            .query_row(
                "SELECT id, slug, local_path, remote_url, workspace_dir, created_at, \
                 COALESCE(allow_agent_issue_creation, 0) as allow_agent_issue_creation \
                 FROM repos WHERE id = ?1",
                params![id],
                map_repo_row,
            )
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
                "SELECT id, slug, local_path, remote_url, workspace_dir, created_at, \
                 COALESCE(allow_agent_issue_creation, 0) as allow_agent_issue_creation \
                 FROM repos WHERE slug = ?1",
                params![slug],
                map_repo_row,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ConductorError::RepoNotFound {
                    slug: slug.to_string(),
                },
                _ => ConductorError::Database(e),
            })
    }

    /// Set the per-repo model override via `.conductor/config.toml`.
    ///
    /// Encapsulates the load → patch → save cycle so callers don't duplicate it.
    pub fn set_model(&self, repo: &Repo, model: Option<String>) -> Result<()> {
        let repo_path = Path::new(&repo.local_path);
        let mut repo_config = RepoConfig::load(repo_path).unwrap_or_else(|e| {
            tracing::warn!(
                "Failed to load .conductor/config.toml for repo '{}': {e}; using defaults",
                repo.slug,
            );
            RepoConfig::default()
        });
        repo_config.defaults.model = model;
        repo_config.save_full(repo_path)
    }

    /// Load the per-repo config with warning fallback on error.
    pub fn load_repo_config(&self, repo: &Repo) -> RepoConfig {
        RepoConfig::load(Path::new(&repo.local_path)).unwrap_or_else(|e| {
            tracing::warn!(
                "Failed to load .conductor/config.toml for repo '{}': {e}; using defaults",
                repo.slug,
            );
            RepoConfig::default()
        })
    }

    /// Resolve the effective model for a repo: per-repo config → global config → None.
    pub fn resolve_model(&self, repo: &Repo) -> Option<String> {
        let rc = self.load_repo_config(repo);
        rc.defaults
            .model
            .or_else(|| self.config.general.model.clone())
    }

    /// Resolve the effective default branch: per-repo config → global config.
    pub fn resolve_default_branch(&self, repo: &Repo) -> String {
        let rc = self.load_repo_config(repo);
        rc.defaults
            .default_branch
            .unwrap_or_else(|| self.config.defaults.default_branch.clone())
    }

    /// Resolve the effective bot name: per-repo config → None.
    pub fn resolve_bot_name(&self, repo: &Repo) -> Option<String> {
        let rc = self.load_repo_config(repo);
        rc.defaults.bot_name
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

fn map_repo_row(row: &rusqlite::Row) -> rusqlite::Result<Repo> {
    Ok(Repo {
        id: row.get(0)?,
        slug: row.get(1)?,
        local_path: row.get(2)?,
        remote_url: row.get(3)?,
        workspace_dir: row.get(4)?,
        created_at: row.get(5)?,
        allow_agent_issue_creation: row.get::<_, i64>(6).map(|v| v != 0)?,
    })
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

    #[test]
    fn test_set_model_persists_and_clears() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = crate::test_helpers::create_test_conn();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);
        let repo = mgr
            .register(
                "set-model-test",
                tmp.path().to_str().unwrap(),
                "https://github.com/test/repo.git",
                None,
            )
            .unwrap();

        // Set a model and verify it persists to disk.
        mgr.set_model(&repo, Some("opus".to_string())).unwrap();
        let rc = RepoConfig::load(tmp.path()).unwrap();
        assert_eq!(rc.defaults.model.as_deref(), Some("opus"));

        // Clear the model and verify it's gone.
        mgr.set_model(&repo, None).unwrap();
        let rc = RepoConfig::load(tmp.path()).unwrap();
        assert!(rc.defaults.model.is_none());
    }

    #[test]
    fn test_resolve_bot_name_from_repo_config() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = crate::test_helpers::create_test_conn();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);
        let repo = mgr
            .register(
                "bot-name-test",
                tmp.path().to_str().unwrap(),
                "https://github.com/test/repo2.git",
                None,
            )
            .unwrap();

        // No config file → None.
        assert!(mgr.resolve_bot_name(&repo).is_none());

        // Write a config with bot_name.
        let mut rc = RepoConfig::default();
        rc.defaults.bot_name = Some("my-bot".to_string());
        rc.save(tmp.path()).unwrap();

        assert_eq!(mgr.resolve_bot_name(&repo).as_deref(), Some("my-bot"));
    }

    #[test]
    fn test_resolve_model_repo_and_global_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = crate::test_helpers::create_test_conn();

        // Global config with a model set.
        let mut config = Config::default();
        config.general.model = Some("global-model".to_string());
        let mgr = RepoManager::new(&conn, &config);
        let repo = mgr
            .register(
                "resolve-model-test",
                tmp.path().to_str().unwrap(),
                "https://github.com/test/resolve-model.git",
                None,
            )
            .unwrap();

        // No repo config → falls back to global.
        assert_eq!(mgr.resolve_model(&repo).as_deref(), Some("global-model"));

        // Write a per-repo config with model.
        let mut rc = RepoConfig::default();
        rc.defaults.model = Some("repo-model".to_string());
        rc.save(tmp.path()).unwrap();

        // Per-repo config takes precedence over global.
        assert_eq!(mgr.resolve_model(&repo).as_deref(), Some("repo-model"));
    }

    #[test]
    fn test_resolve_model_no_config() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = crate::test_helpers::create_test_conn();
        let config = Config::default(); // no global model
        let mgr = RepoManager::new(&conn, &config);
        let repo = mgr
            .register(
                "resolve-model-none",
                tmp.path().to_str().unwrap(),
                "https://github.com/test/resolve-model-none.git",
                None,
            )
            .unwrap();

        // No repo config, no global config → None.
        assert!(mgr.resolve_model(&repo).is_none());
    }

    #[test]
    fn test_resolve_default_branch_repo_and_global_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = crate::test_helpers::create_test_conn();
        let mut config = Config::default();
        config.defaults.default_branch = "develop".to_string();
        let mgr = RepoManager::new(&conn, &config);
        let repo = mgr
            .register(
                "resolve-branch-test",
                tmp.path().to_str().unwrap(),
                "https://github.com/test/resolve-branch.git",
                None,
            )
            .unwrap();

        // No repo config → falls back to global default_branch.
        assert_eq!(mgr.resolve_default_branch(&repo), "develop");

        // Write a per-repo config with default_branch.
        let mut rc = RepoConfig::default();
        rc.defaults.default_branch = Some("trunk".to_string());
        rc.save(tmp.path()).unwrap();

        // Per-repo config takes precedence.
        assert_eq!(mgr.resolve_default_branch(&repo), "trunk");
    }
}
