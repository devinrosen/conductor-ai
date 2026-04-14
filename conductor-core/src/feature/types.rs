use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    pub id: String,
    pub repo_id: String,
    pub name: String,
    pub branch: String,
    pub base_branch: String,
    pub status: FeatureStatus,
    pub created_at: String,
    pub merged_at: Option<String>,
    pub source_type: Option<String>,
    pub source_id: Option<String>,
    pub tickets_total: u32,
    pub tickets_merged: u32,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeatureStatus {
    InProgress,
    ReadyForReview,
    Approved,
    Merged,
    Closed,
}

impl fmt::Display for FeatureStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InProgress => write!(f, "in_progress"),
            Self::ReadyForReview => write!(f, "ready_for_review"),
            Self::Approved => write!(f, "approved"),
            Self::Merged => write!(f, "merged"),
            Self::Closed => write!(f, "closed"),
        }
    }
}

impl FromStr for FeatureStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "in_progress" | "active" => Ok(Self::InProgress),
            "ready_for_review" => Ok(Self::ReadyForReview),
            "approved" => Ok(Self::Approved),
            "merged" => Ok(Self::Merged),
            "closed" => Ok(Self::Closed),
            other => Err(format!("unknown feature status: {other}")),
        }
    }
}

crate::impl_sql_enum!(FeatureStatus);

/// Summary row returned by `list()`.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureRow {
    pub id: String,
    pub name: String,
    pub branch: String,
    pub base_branch: String,
    pub status: FeatureStatus,
    pub created_at: String,
    pub worktree_count: i64,
    pub ticket_count: i64,
    /// Cached timestamp of the most recent git commit on the feature branch.
    pub last_commit_at: Option<String>,
    /// Most recent worktree creation time targeting this feature branch (computed via subquery).
    pub last_worktree_activity: Option<String>,
}

/// A branch that has active worktrees but no matching feature record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnregisteredBranch {
    pub branch: String,
    pub worktree_count: i64,
    pub base_branch: Option<String>,
}
