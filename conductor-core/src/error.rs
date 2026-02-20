use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConductorError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("repo not found: {slug}")]
    RepoNotFound { slug: String },

    #[error("repo already exists: {slug}")]
    RepoAlreadyExists { slug: String },

    #[error("worktree not found: {slug}")]
    WorktreeNotFound { slug: String },

    #[error("worktree already exists: {slug}")]
    WorktreeAlreadyExists { slug: String },

    #[error("git error: {0}")]
    Git(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ticket sync error: {0}")]
    TicketSync(String),
}

pub type Result<T> = std::result::Result<T, ConductorError>;
