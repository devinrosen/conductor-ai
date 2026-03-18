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

    #[error("issue source already exists for repo '{repo_slug}' with type '{source_type}'")]
    IssueSourceAlreadyExists {
        repo_slug: String,
        source_type: String,
    },

    #[error("ticket not found: {id}")]
    TicketNotFound { id: String },

    #[error("agent error: {0}")]
    Agent(String),

    #[error("feedback request {id} is not pending (current status: {status})")]
    FeedbackNotPending { id: String, status: String },

    #[error("worktree already has a linked ticket")]
    TicketAlreadyLinked,

    #[error("workflow error: {0}")]
    Workflow(String),

    #[error("agent config error: {0}")]
    AgentConfig(String),

    #[error("schema error: {0}")]
    Schema(String),

    #[error("worktree already has an active workflow run (\"{name}\") — wait for it to finish or cancel it before starting another")]
    WorkflowRunAlreadyActive { name: String },

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("feature not found: {name}")]
    FeatureNotFound { name: String },

    #[error("feature already exists: {name}")]
    FeatureAlreadyExists { name: String },
}

pub type Result<T> = std::result::Result<T, ConductorError>;
