use thiserror::Error;

/// Structured data from a failed subprocess invocation.
///
/// Preserves the command string, exit code, and captured output so callers can
/// programmatically classify failures (transient vs permanent, auth vs network, etc.)
/// instead of parsing opaque error messages.
///
/// Part of: semantic-exit-code-convention@1.0.0, bounded-retry-with-escalation@1.0.0
#[derive(Debug, Clone)]
pub struct SubprocessFailure {
    pub command: String,
    pub exit_code: Option<i32>,
    pub stderr: String,
    pub stdout: String,
}

impl SubprocessFailure {
    /// Convenience constructor for call sites that only have a pre-formatted message
    /// (e.g. spawn failures where no Output is available).
    pub fn from_message(command: &str, message: String) -> Self {
        Self {
            command: command.to_string(),
            exit_code: None,
            stderr: message,
            stdout: String::new(),
        }
    }
}

impl std::fmt::Display for SubprocessFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.stderr.is_empty() {
            write!(f, "{} failed: {}", self.command, self.stderr)
        } else if let Some(code) = self.exit_code {
            write!(f, "{} exited with code {}", self.command, code)
        } else {
            write!(f, "{} failed", self.command)
        }
    }
}

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
    Git(SubprocessFailure),

    #[error("gh cli error: {0}")]
    GhCli(SubprocessFailure),

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

    #[error("workflow run not found: {id}")]
    WorkflowRunNotFound { id: String },

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

    #[error("feature '{name}' is still active. Run `conductor feature close {repo} {name}` first")]
    FeatureStillActive { repo: String, name: String },

    #[error("unknown ticket source type: {0}")]
    UnknownSourceType(String),
}

impl ConductorError {
    /// Semantic exit code for this error.
    ///
    /// Ranges:
    ///   0      = success
    ///   1      = unspecified / anyhow fallthrough
    ///   10-19  = infrastructure (DB, I/O)
    ///   20-29  = user input / entity-not-found errors
    ///   30-39  = subprocess / external tool failures (33 = entity state / precondition error)
    ///   40-49  = configuration errors (43 = unknown/invalid config value type)
    ///   50-59  = agent subsystem
    ///   60-69  = workflow subsystem
    ///
    /// Part of: semantic-exit-code-convention@1.0.0
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Database(_) => 10,
            Self::Io(_) => 11,
            Self::RepoNotFound { .. } => 20,
            Self::RepoAlreadyExists { .. } => 21,
            Self::WorktreeNotFound { .. } => 22,
            Self::WorktreeAlreadyExists { .. } => 23,
            Self::IssueSourceAlreadyExists { .. } => 24,
            Self::TicketNotFound { .. } => 25,
            Self::TicketAlreadyLinked => 26,
            Self::InvalidInput(_) => 27,
            Self::FeatureNotFound { .. } => 28,
            Self::FeatureAlreadyExists { .. } => 29,
            Self::FeatureStillActive { .. } => 33,
            Self::Git(_) => 30,
            Self::GhCli(_) => 31,
            Self::TicketSync(_) => 32,
            Self::Config(_) => 40,
            Self::AgentConfig(_) => 41,
            Self::Schema(_) => 42,
            Self::Agent(_) => 50,
            Self::FeedbackNotPending { .. } => 51,
            Self::Workflow(_) => 60,
            Self::WorkflowRunAlreadyActive { .. } => 61,
            Self::WorkflowRunNotFound { .. } => 62,
            Self::UnknownSourceType(_) => 43,
        }
    }
}

pub type Result<T> = std::result::Result<T, ConductorError>;
