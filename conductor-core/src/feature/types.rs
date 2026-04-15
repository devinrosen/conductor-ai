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
    /// Denormalized total number of tickets linked to this feature (from the features table).
    pub tickets_total: u32,
    /// Denormalized number of merged tickets (from the features table).
    pub tickets_merged: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_status_display_round_trip() {
        let cases = [
            (FeatureStatus::InProgress, "in_progress"),
            (FeatureStatus::ReadyForReview, "ready_for_review"),
            (FeatureStatus::Approved, "approved"),
            (FeatureStatus::Merged, "merged"),
            (FeatureStatus::Closed, "closed"),
        ];
        for (status, expected) in &cases {
            assert_eq!(status.to_string(), *expected);
            let parsed: FeatureStatus = expected.parse().expect("parse should succeed");
            assert_eq!(parsed, *status);
        }
    }

    #[test]
    fn feature_status_legacy_active_maps_to_in_progress() {
        let parsed: FeatureStatus = "active".parse().expect("legacy 'active' should parse");
        assert_eq!(parsed, FeatureStatus::InProgress);
    }

    #[test]
    fn feature_status_unknown_returns_error() {
        let result = "unknown_status".parse::<FeatureStatus>();
        assert!(result.is_err());
    }
}

/// A branch that has active worktrees but no matching feature record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnregisteredBranch {
    pub branch: String,
    pub worktree_count: i64,
    pub base_branch: Option<String>,
}

/// Result returned by `FeatureManager::sync_from_milestone`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    /// Number of tickets newly linked to the feature.
    pub added: usize,
    /// Number of tickets unlinked from the feature (ticket records are preserved).
    pub removed: usize,
}

/// Result returned by `FeatureManager::run()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    /// Number of tickets for which worktrees + agents were dispatched.
    pub dispatched: u32,
    /// Number of tickets that failed to dispatch (worktree or agent spawn error).
    pub failed: u32,
}
