mod git_helpers;
mod manager;
mod types;

#[cfg(test)]
mod tests;

pub use git_helpers::MainHealthStatus;
pub use manager::{
    get_ticket_id_by_branch, label_to_branch_prefix, SetBaseBranchOptions, WorktreeCreateOptions,
    WorktreeManager,
};
pub use types::{Worktree, WorktreeStatus, WorktreeWithStatus};

// Column constants used by both types.rs and manager.rs — live here to avoid circular deps.
const WORKTREE_COLUMNS: &str =
    "id, repo_id, slug, branch, path, ticket_id, status, created_at, completed_at, model, base_branch";

static WORKTREE_COLUMNS_W: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| crate::db::prefix_columns(WORKTREE_COLUMNS, "w."));
