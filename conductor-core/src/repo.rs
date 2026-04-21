use crate::config::{Config, RepoConfig};
use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use chrono::Utc;
use rusqlite::{named_params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// JSON-serialized per-repo runtime overrides (RFC 007). None means use global config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_overrides: Option<String>,
}

const REPO_SELECT: &str = "SELECT id, slug, local_path, remote_url, workspace_dir, created_at, \
     COALESCE(allow_agent_issue_creation, 0) as allow_agent_issue_creation, \
     runtime_overrides FROM repos";

fn row_to_repo(row: &rusqlite::Row) -> rusqlite::Result<Repo> {
    Ok(Repo {
        id: row.get("id")?,
        slug: row.get("slug")?,
        local_path: row.get("local_path")?,
        remote_url: row.get("remote_url")?,
        default_branch: String::new(),
        workspace_dir: row.get("workspace_dir")?,
        created_at: row.get("created_at")?,
        model: None,
        allow_agent_issue_creation: row
            .get::<_, i64>("allow_agent_issue_creation")
            .map(|v| v != 0)?,
        runtime_overrides: row.get("runtime_overrides")?,
    })
}

fn repo_not_found(slug: impl Into<String>) -> impl FnOnce(rusqlite::Error) -> ConductorError {
    let slug = slug.into();
    move |e| match e {
        rusqlite::Error::QueryReturnedNoRows => ConductorError::RepoNotFound { slug },
        _ => ConductorError::Database(e),
    }
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
            "SELECT EXISTS(SELECT 1 FROM repos WHERE slug = :slug)",
            named_params! { ":slug": slug },
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
            runtime_overrides: None,
        };

        self.conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
             VALUES (:id, :slug, :local_path, :remote_url, :workspace_dir, :created_at)",
            named_params! {
                ":id": repo.id,
                ":slug": repo.slug,
                ":local_path": repo.local_path,
                ":remote_url": repo.remote_url,
                ":workspace_dir": repo.workspace_dir,
                ":created_at": repo.created_at,
            },
        )?;

        Ok(repo.enrich(self.config))
    }

    pub fn list(&self) -> Result<Vec<Repo>> {
        let repos = query_collect(
            self.conn,
            &format!("{REPO_SELECT} ORDER BY slug"),
            [],
            row_to_repo,
        )?;
        Ok(repos.into_iter().map(|r| r.enrich(self.config)).collect())
    }

    pub fn get_by_id(&self, id: &str) -> Result<Repo> {
        self.conn
            .query_row(
                &format!("{REPO_SELECT} WHERE id = :id"),
                named_params! { ":id": id },
                row_to_repo,
            )
            .map(|r| r.enrich(self.config))
            .map_err(repo_not_found(id))
    }

    pub fn get_by_slug(&self, slug: &str) -> Result<Repo> {
        self.conn
            .query_row(
                &format!("{REPO_SELECT} WHERE slug = :slug"),
                named_params! { ":slug": slug },
                row_to_repo,
            )
            .map(|r| r.enrich(self.config))
            .map_err(repo_not_found(slug))
    }

    /// Set whether agents can create issues for this repo.
    pub fn set_allow_agent_issue_creation(&self, repo_id: &str, allow: bool) -> Result<()> {
        let affected = self.conn.execute(
            "UPDATE repos SET allow_agent_issue_creation = :allow WHERE id = :id",
            named_params! { ":allow": allow as i64, ":id": repo_id },
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

    /// Returns the `.conductor/` directory inside the repo's local path.
    /// Used to locate per-repo runtime configs (RFC 007).
    pub fn runtime_config_dir(repo: &Repo) -> std::path::PathBuf {
        std::path::PathBuf::from(&repo.local_path).join(".conductor")
    }

    pub fn unregister(&self, slug: &str) -> Result<()> {
        let affected = self.conn.execute(
            "DELETE FROM repos WHERE slug = :slug",
            named_params! { ":slug": slug },
        )?;
        if affected == 0 {
            return Err(ConductorError::RepoNotFound {
                slug: slug.to_string(),
            });
        }
        Ok(())
    }

    pub fn unregister_by_id(&self, id: &str) -> Result<()> {
        let affected = self.conn.execute(
            "DELETE FROM repos WHERE id = :id",
            named_params! { ":id": id },
        )?;
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

    // ── register ──────────────────────────────────────────────────────

    #[test]
    fn test_register_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let repo = mgr
            .register(
                "my-repo",
                dir.path().to_str().unwrap(),
                "https://github.com/org/my-repo.git",
                None,
            )
            .unwrap();

        assert_eq!(repo.slug, "my-repo");
        assert_eq!(repo.local_path, dir.path().to_str().unwrap());
        assert_eq!(repo.remote_url, "https://github.com/org/my-repo.git");
        assert!(!repo.id.is_empty());
        assert!(!repo.created_at.is_empty());
    }

    #[test]
    fn test_register_duplicate_slug_error() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        mgr.register("dup-repo", "/tmp/a", "https://github.com/org/a.git", None)
            .unwrap();
        let err = mgr
            .register("dup-repo", "/tmp/b", "https://github.com/org/b.git", None)
            .unwrap_err();
        assert!(matches!(err, ConductorError::RepoAlreadyExists { slug } if slug == "dup-repo"));
    }

    #[test]
    fn test_register_default_workspace_dir() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let repo = mgr
            .register("ws-repo", "/tmp/ws", "https://github.com/org/ws.git", None)
            .unwrap();

        let expected = config
            .general
            .workspace_root
            .join("ws-repo")
            .to_string_lossy()
            .to_string();
        assert_eq!(repo.workspace_dir, expected);
    }

    #[test]
    fn test_register_explicit_workspace_dir() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let repo = mgr
            .register(
                "custom-repo",
                "/tmp/custom",
                "https://github.com/org/custom.git",
                Some("/custom/workspace"),
            )
            .unwrap();

        assert_eq!(repo.workspace_dir, "/custom/workspace");
    }

    // ── list ──────────────────────────────────────────────────────────

    #[test]
    fn test_list_empty() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let repos = mgr.list().unwrap();
        assert!(repos.is_empty());
    }

    #[test]
    fn test_list_returns_repos_sorted_by_slug() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        mgr.register("b-repo", "/tmp/b", "https://github.com/org/b.git", None)
            .unwrap();
        mgr.register("a-repo", "/tmp/a", "https://github.com/org/a.git", None)
            .unwrap();

        let repos = mgr.list().unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].slug, "a-repo");
        assert_eq!(repos[1].slug, "b-repo");
    }

    // ── get_by_id ─────────────────────────────────────────────────────

    #[test]
    fn test_get_by_id_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let registered = mgr
            .register(
                "id-repo",
                dir.path().to_str().unwrap(),
                "https://github.com/org/id-repo.git",
                None,
            )
            .unwrap();

        let fetched = mgr.get_by_id(&registered.id).unwrap();
        assert_eq!(fetched.id, registered.id);
        assert_eq!(fetched.slug, "id-repo");
    }

    #[test]
    fn test_get_by_id_not_found() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let err = mgr.get_by_id("nonexistent-id").unwrap_err();
        assert!(matches!(err, ConductorError::RepoNotFound { .. }));
    }

    // ── get_by_slug ───────────────────────────────────────────────────

    #[test]
    fn test_get_by_slug_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let registered = mgr
            .register(
                "slug-repo",
                dir.path().to_str().unwrap(),
                "https://github.com/org/slug-repo.git",
                None,
            )
            .unwrap();

        let fetched = mgr.get_by_slug("slug-repo").unwrap();
        assert_eq!(fetched.id, registered.id);
        assert_eq!(fetched.slug, "slug-repo");
    }

    #[test]
    fn test_get_by_slug_not_found() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let err = mgr.get_by_slug("no-such-slug").unwrap_err();
        assert!(matches!(err, ConductorError::RepoNotFound { .. }));
    }

    // ── unregister ────────────────────────────────────────────────────

    #[test]
    fn test_unregister_happy_path() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        mgr.register(
            "del-repo",
            "/tmp/del",
            "https://github.com/org/del.git",
            None,
        )
        .unwrap();

        mgr.unregister("del-repo").unwrap();
        let err = mgr.get_by_slug("del-repo").unwrap_err();
        assert!(matches!(err, ConductorError::RepoNotFound { .. }));
    }

    #[test]
    fn test_unregister_not_found() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let err = mgr.unregister("ghost").unwrap_err();
        assert!(matches!(err, ConductorError::RepoNotFound { .. }));
    }

    // ── unregister_by_id ──────────────────────────────────────────────

    #[test]
    fn test_unregister_by_id_happy_path() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let repo = mgr
            .register(
                "del-id-repo",
                "/tmp/del-id",
                "https://github.com/org/del-id.git",
                None,
            )
            .unwrap();

        mgr.unregister_by_id(&repo.id).unwrap();
        let err = mgr.get_by_id(&repo.id).unwrap_err();
        assert!(matches!(err, ConductorError::RepoNotFound { .. }));
    }

    #[test]
    fn test_unregister_by_id_not_found() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let err = mgr.unregister_by_id("fake-id").unwrap_err();
        assert!(matches!(err, ConductorError::RepoNotFound { .. }));
    }

    // ── set_allow_agent_issue_creation ─────────────────────────────────

    #[test]
    fn test_set_allow_agent_issue_creation_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let repo = mgr
            .register(
                "issue-repo",
                dir.path().to_str().unwrap(),
                "https://github.com/org/issue-repo.git",
                None,
            )
            .unwrap();

        // Default is false
        assert!(!repo.allow_agent_issue_creation);

        // Set to true
        mgr.set_allow_agent_issue_creation(&repo.id, true).unwrap();
        let updated = mgr.get_by_id(&repo.id).unwrap();
        assert!(updated.allow_agent_issue_creation);

        // Set back to false
        mgr.set_allow_agent_issue_creation(&repo.id, false).unwrap();
        let updated = mgr.get_by_id(&repo.id).unwrap();
        assert!(!updated.allow_agent_issue_creation);
    }

    #[test]
    fn test_set_allow_agent_issue_creation_not_found() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = RepoManager::new(&conn, &config);

        let err = mgr
            .set_allow_agent_issue_creation("fake-repo-id", true)
            .unwrap_err();
        assert!(matches!(err, ConductorError::RepoNotFound { .. }));
    }

    // ── derive_slug_from_url ──────────────────────────────────────────

    #[test]
    fn test_derive_slug_from_url_various_formats() {
        assert_eq!(
            derive_slug_from_url("https://github.com/org/repo.git"),
            "repo"
        );
        assert_eq!(derive_slug_from_url("https://github.com/org/repo"), "repo");
        assert_eq!(derive_slug_from_url("git@github.com:org/repo.git"), "repo");
        // No slash — returns the whole string stripped of .git
        assert_eq!(derive_slug_from_url("repo.git"), "repo");
        // Empty string
        assert_eq!(derive_slug_from_url(""), "");
    }

    // ── derive_local_path ─────────────────────────────────────────────

    #[test]
    fn test_derive_local_path() {
        let config = Config::default();
        let result = derive_local_path(&config, "my-repo");
        let expected = config
            .general
            .workspace_root
            .join("my-repo")
            .join("main")
            .to_string_lossy()
            .to_string();
        assert_eq!(result, expected);
    }
}
