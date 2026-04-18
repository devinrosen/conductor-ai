use crate::agent::AgentRunStatus;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Typed representation of the three worktree lifecycle states stored in the DB.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

/// A `Worktree` augmented with the status of its latest agent run and linked ticket info.
/// Returned by `WorktreeManager::list_all_with_status` and the enriched GET methods.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeWithStatus {
    #[serde(flatten)]
    pub worktree: Worktree,
    pub agent_status: Option<AgentRunStatus>,
    pub ticket_title: Option<String>,
    pub ticket_number: Option<String>,
    pub ticket_url: Option<String>,
}

pub(super) fn map_worktree_row(row: &rusqlite::Row) -> rusqlite::Result<Worktree> {
    Ok(Worktree {
        id: row.get("id")?,
        repo_id: row.get("repo_id")?,
        slug: row.get("slug")?,
        branch: row.get("branch")?,
        path: row.get("path")?,
        ticket_id: row.get("ticket_id")?,
        status: row.get::<_, WorktreeStatus>("status")?,
        created_at: row.get("created_at")?,
        completed_at: row.get("completed_at")?,
        model: row.get("model")?,
        base_branch: row.get("base_branch")?,
    })
}
