//! Per-repo review configuration for multi-agent PR review swarms.
//!
//! Each repo can define a set of reviewer roles (architecture, security, etc.)
//! along with configuration for retry limits, auto-merge, and PR comment posting.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// A single reviewer role in a PR review swarm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewerRole {
    /// Short identifier, e.g. "architecture", "security".
    pub name: String,
    /// Human-readable focus area description.
    pub focus: String,
    /// System prompt injected into the reviewer agent.
    pub system_prompt: String,
    /// If true, blocking findings from this reviewer prevent auto-merge.
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

/// Build a reviewer system prompt from an intro, focus points, and a "no issues" phrase.
fn reviewer_system_prompt(intro: &str, focus_points: &str, no_issues_phrase: &str) -> String {
    format!(
        "{intro}\n\
         Focus exclusively on:\n\
         {focus_points}\n\n\
         For each issue found, report:\n\
         - **Issue**: one-line description\n\
         - **Severity**: critical | warning | suggestion\n\
         - **Location**: file:line reference\n\
         - **Details**: explanation and recommended fix\n\n\
         If you find no issues, state \"{no_issues_phrase}\" and explain what you reviewed."
    )
}

/// Default reviewer roles used when no per-repo config exists.
pub fn default_reviewer_roles() -> Vec<ReviewerRole> {
    vec![
        ReviewerRole {
            name: "architecture".to_string(),
            focus: "Coupling, cohesion, layer violations, design patterns".to_string(),
            system_prompt: reviewer_system_prompt(
                "You are a senior software architect reviewing a pull request.",
                "- Coupling and cohesion between modules\n\
                 - Layer violations (e.g. UI code calling DB directly)\n\
                 - Design pattern misuse or missed opportunities\n\
                 - API surface consistency",
                "No architectural issues found",
            ),
            required: true,
        },
        ReviewerRole {
            name: "dry-abstraction".to_string(),
            focus: "Duplication, premature abstraction, missing helpers".to_string(),
            system_prompt: reviewer_system_prompt(
                "You are a code quality reviewer focused on DRY principles and abstraction.",
                "- Code duplication (copy-pasted logic)\n\
                 - Premature or over-engineered abstractions\n\
                 - Missing helper functions that would reduce repetition\n\
                 - Unnecessary indirection",
                "No DRY/abstraction issues found",
            ),
            required: false,
        },
        ReviewerRole {
            name: "security".to_string(),
            focus: "Input validation, auth gaps, injection risks, secrets in code".to_string(),
            system_prompt: reviewer_system_prompt(
                "You are a security-focused code reviewer.",
                "- Input validation gaps\n\
                 - Authentication and authorization issues\n\
                 - Injection risks (SQL, command, XSS)\n\
                 - Secrets, credentials, or tokens in code\n\
                 - Unsafe deserialization",
                "No security issues found",
            ),
            required: true,
        },
        ReviewerRole {
            name: "performance".to_string(),
            focus: "Unnecessary allocations, N+1 queries, blocking calls".to_string(),
            system_prompt: reviewer_system_prompt(
                "You are a performance-focused code reviewer.",
                "- Unnecessary memory allocations or copies\n\
                 - N+1 query patterns\n\
                 - Blocking calls in hot paths\n\
                 - Missing caching opportunities\n\
                 - Algorithmic complexity issues",
                "No performance issues found",
            ),
            required: false,
        },
    ]
}

/// Per-repo review swarm configuration stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewConfig {
    pub id: String,
    pub repo_id: String,
    pub roles: Vec<ReviewerRole>,
    pub post_to_pr: bool,
    pub auto_merge: bool,
    pub created_at: String,
    pub updated_at: String,
}

pub struct ReviewConfigManager<'a> {
    conn: &'a Connection,
}

impl<'a> ReviewConfigManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Get the review config for a repo, or None if not configured.
    pub fn get_for_repo(&self, repo_id: &str) -> Result<Option<ReviewConfig>> {
        let result = self
            .conn
            .query_row(
                "SELECT id, repo_id, roles_json, post_to_pr, auto_merge, created_at, \
                 updated_at FROM review_configs WHERE repo_id = ?1",
                params![repo_id],
                |row| {
                    let roles_json: String = row.get(2)?;
                    let roles: Vec<ReviewerRole> =
                        serde_json::from_str(&roles_json).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?;
                    Ok(ReviewConfig {
                        id: row.get(0)?,
                        repo_id: row.get(1)?,
                        roles,
                        post_to_pr: row.get::<_, bool>(3)?,
                        auto_merge: row.get::<_, bool>(4)?,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(result)
    }

    /// Get the review config for a repo, falling back to defaults if not configured.
    pub fn get_or_default(&self, repo_id: &str) -> Result<ReviewConfig> {
        if let Some(config) = self.get_for_repo(repo_id)? {
            return Ok(config);
        }
        let now = Utc::now().to_rfc3339();
        Ok(ReviewConfig {
            id: String::new(),
            repo_id: repo_id.to_string(),
            roles: default_reviewer_roles(),
            post_to_pr: true,
            auto_merge: true,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Create or update the review config for a repo.
    pub fn upsert(
        &self,
        repo_id: &str,
        roles: &[ReviewerRole],
        post_to_pr: bool,
        auto_merge: bool,
    ) -> Result<ReviewConfig> {
        let now = Utc::now().to_rfc3339();
        let roles_json = serde_json::to_string(roles)
            .map_err(|e| crate::error::ConductorError::Config(e.to_string()))?;

        let existing = self.get_for_repo(repo_id)?;
        let id = existing
            .map(|c| c.id)
            .unwrap_or_else(|| ulid::Ulid::new().to_string());

        self.conn.execute(
            "INSERT INTO review_configs (id, repo_id, roles_json, post_to_pr, auto_merge, \
             created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(repo_id) DO UPDATE SET
                roles_json = excluded.roles_json,
                post_to_pr = excluded.post_to_pr,
                auto_merge = excluded.auto_merge,
                updated_at = excluded.updated_at",
            params![id, repo_id, roles_json, post_to_pr, auto_merge, now],
        )?;

        Ok(ReviewConfig {
            id,
            repo_id: repo_id.to_string(),
            roles: roles.to_vec(),
            post_to_pr,
            auto_merge,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Delete the review config for a repo.
    pub fn delete_for_repo(&self, repo_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM review_configs WHERE repo_id = ?1",
            params![repo_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use tempfile::NamedTempFile;

    fn setup() -> (Connection, String) {
        let tmp = NamedTempFile::new().unwrap();
        let conn = db::open_database(tmp.path()).unwrap();
        let repo_id = ulid::Ulid::new().to_string();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at)
             VALUES (?1, 'test-repo', '/tmp/test', 'https://github.com/test/repo', 'main', '/tmp/ws', ?2)",
            params![repo_id, now],
        ).unwrap();
        (conn, repo_id)
    }

    #[test]
    fn test_default_reviewer_roles() {
        let roles = default_reviewer_roles();
        assert_eq!(roles.len(), 4);
        assert_eq!(roles[0].name, "architecture");
        assert!(roles[0].required);
        assert_eq!(roles[1].name, "dry-abstraction");
        assert!(!roles[1].required);
        assert_eq!(roles[2].name, "security");
        assert!(roles[2].required);
        assert_eq!(roles[3].name, "performance");
        assert!(!roles[3].required);
    }

    #[test]
    fn test_get_or_default_without_config() {
        let (conn, repo_id) = setup();
        let mgr = ReviewConfigManager::new(&conn);
        let config = mgr.get_or_default(&repo_id).unwrap();
        assert_eq!(config.roles.len(), 4);
        assert!(config.post_to_pr);
        assert!(config.auto_merge);
    }

    #[test]
    fn test_upsert_and_get() {
        let (conn, repo_id) = setup();
        let mgr = ReviewConfigManager::new(&conn);

        let roles = vec![ReviewerRole {
            name: "security".to_string(),
            focus: "Security review".to_string(),
            system_prompt: "Review for security".to_string(),
            required: true,
        }];

        let config = mgr.upsert(&repo_id, &roles, false, true).unwrap();
        assert_eq!(config.roles.len(), 1);
        assert!(!config.post_to_pr);
        assert!(config.auto_merge);

        let fetched = mgr.get_for_repo(&repo_id).unwrap().unwrap();
        assert_eq!(fetched.roles.len(), 1);
        assert_eq!(fetched.roles[0].name, "security");
    }

    #[test]
    fn test_upsert_overwrites() {
        let (conn, repo_id) = setup();
        let mgr = ReviewConfigManager::new(&conn);

        let roles1 = vec![ReviewerRole {
            name: "a".to_string(),
            focus: "A".to_string(),
            system_prompt: "A".to_string(),
            required: true,
        }];
        mgr.upsert(&repo_id, &roles1, true, true).unwrap();

        let roles2 = vec![
            ReviewerRole {
                name: "b".to_string(),
                focus: "B".to_string(),
                system_prompt: "B".to_string(),
                required: false,
            },
            ReviewerRole {
                name: "c".to_string(),
                focus: "C".to_string(),
                system_prompt: "C".to_string(),
                required: true,
            },
        ];
        mgr.upsert(&repo_id, &roles2, false, false).unwrap();

        let config = mgr.get_for_repo(&repo_id).unwrap().unwrap();
        assert_eq!(config.roles.len(), 2);
        assert!(!config.post_to_pr);
        assert!(!config.auto_merge);
    }

    #[test]
    fn test_delete_for_repo() {
        let (conn, repo_id) = setup();
        let mgr = ReviewConfigManager::new(&conn);

        let roles = default_reviewer_roles();
        mgr.upsert(&repo_id, &roles, true, true).unwrap();
        assert!(mgr.get_for_repo(&repo_id).unwrap().is_some());

        mgr.delete_for_repo(&repo_id).unwrap();
        assert!(mgr.get_for_repo(&repo_id).unwrap().is_none());
    }

    #[test]
    fn test_reviewer_role_serialization() {
        let role = ReviewerRole {
            name: "test".to_string(),
            focus: "Testing".to_string(),
            system_prompt: "Review tests".to_string(),
            required: false,
        };
        let json = serde_json::to_string(&role).unwrap();
        let deserialized: ReviewerRole = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "test");
        assert!(!deserialized.required);
    }

    #[test]
    fn test_reviewer_role_default_required() {
        let json = r#"{"name":"x","focus":"X","system_prompt":"X"}"#;
        let role: ReviewerRole = serde_json::from_str(json).unwrap();
        assert!(role.required);
    }
}
