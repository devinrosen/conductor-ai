use serde::{Deserialize, Serialize};

use crate::agent::AgentRun;

/// Which resource a conversation is scoped to.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationScope {
    Repo,
    Worktree,
}

impl std::fmt::Display for ConversationScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConversationScope::Repo => write!(f, "repo"),
            ConversationScope::Worktree => write!(f, "worktree"),
        }
    }
}

impl std::str::FromStr for ConversationScope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "repo" => Ok(ConversationScope::Repo),
            "worktree" => Ok(ConversationScope::Worktree),
            other => Err(format!("unknown conversation scope: {other}")),
        }
    }
}

/// A persisted conversation thread (repo-scoped or worktree-scoped).
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub scope: ConversationScope,
    /// ID of the repo or worktree this conversation is scoped to.
    pub scope_id: String,
    /// Auto-set from the first 60 chars of the first prompt. `None` until the
    /// first message is sent.
    pub title: Option<String>,
    pub created_at: String,
    pub last_active_at: String,
}

/// A conversation together with all its associated agent runs (oldest first).
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationWithRuns {
    #[serde(flatten)]
    pub conversation: Conversation,
    pub runs: Vec<AgentRun>,
}
