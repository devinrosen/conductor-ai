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

    #[error("agent run not found: {id}")]
    AgentRunNotFound { id: String },

    #[error("agent run {run_id} does not belong to conversation {conversation_id}")]
    AgentRunNotInConversation {
        run_id: String,
        conversation_id: String,
    },

    #[error("feedback request not found: {id}")]
    FeedbackNotFound { id: String },

    #[error("feedback {feedback_id} does not belong to run {run_id}")]
    FeedbackRunMismatch { feedback_id: String, run_id: String },

    #[error("no pending feedback request for run {run_id}")]
    NoPendingFeedbackForRun { run_id: String },

    #[error("feedback request {id} is not pending (current status: {status})")]
    FeedbackNotPending { id: String, status: String },

    #[error("worktree already has a linked ticket")]
    TicketAlreadyLinked,

    #[error("workflow error: {0}")]
    Workflow(String),

    #[error("workflow cancelled")]
    WorkflowCancelled,

    #[error("workflow run not found: {id}")]
    WorkflowRunNotFound { id: String },

    #[error("workflow step not found: {id}")]
    WorkflowStepNotFound { id: String },

    #[error("workflow step {step_id} does not belong to run {run_id}")]
    WorkflowStepNotInRun { step_id: String, run_id: String },

    #[error("agent config error: {0}")]
    AgentConfig(String),

    #[error("schema error: {0}")]
    Schema(String),

    #[error("worktree already has an active workflow run (\"{name}\") — wait for it to finish or cancel it before starting another")]
    WorkflowRunAlreadyActive { name: String },

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("unknown ticket source type: {0}")]
    UnknownSourceType(String),

    #[error("conversation not found: {id}")]
    ConversationNotFound { id: String },

    #[error("cannot delete conversation {id}: it has an active or waiting agent run")]
    ConversationHasActiveRun { id: String },

    #[error("notification error: {0}")]
    Notification(String),
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
            Self::Git(_) => 30,
            Self::GhCli(_) => 31,
            Self::TicketSync(_) => 32,
            Self::Config(_) => 40,
            Self::AgentConfig(_) => 41,
            Self::Schema(_) => 42,
            Self::Agent(_) => 50,
            Self::FeedbackNotPending { .. } => 51,
            Self::AgentRunNotFound { .. } => 52,
            Self::AgentRunNotInConversation { .. } => 53,
            Self::FeedbackNotFound { .. } => 54,
            Self::FeedbackRunMismatch { .. } => 55,
            Self::NoPendingFeedbackForRun { .. } => 56,
            Self::Workflow(_) => 60,
            Self::WorkflowCancelled => 65,
            Self::WorkflowRunAlreadyActive { .. } => 61,
            Self::WorkflowRunNotFound { .. } => 62,
            Self::WorkflowStepNotFound { .. } => 63,
            Self::WorkflowStepNotInRun { .. } => 64,
            Self::UnknownSourceType(_) => 43,
            Self::ConversationNotFound { .. } => 57,
            Self::ConversationHasActiveRun { .. } => 58,
            Self::Notification(_) => 70,
        }
    }
}

pub type Result<T> = std::result::Result<T, ConductorError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn all_variants() -> Vec<ConductorError> {
        vec![
            ConductorError::Database(rusqlite::Error::InvalidQuery),
            ConductorError::Io(std::io::Error::other("io")),
            ConductorError::RepoNotFound { slug: "r".into() },
            ConductorError::RepoAlreadyExists { slug: "r".into() },
            ConductorError::WorktreeNotFound { slug: "w".into() },
            ConductorError::WorktreeAlreadyExists { slug: "w".into() },
            ConductorError::IssueSourceAlreadyExists {
                repo_slug: "r".into(),
                source_type: "github".into(),
            },
            ConductorError::TicketNotFound { id: "t".into() },
            ConductorError::TicketAlreadyLinked,
            ConductorError::InvalidInput("bad".into()),
            ConductorError::Git(SubprocessFailure::from_message("git", "err".into())),
            ConductorError::GhCli(SubprocessFailure::from_message("gh", "err".into())),
            ConductorError::TicketSync("sync".into()),
            ConductorError::Config("cfg".into()),
            ConductorError::AgentConfig("acfg".into()),
            ConductorError::Schema("schema".into()),
            ConductorError::Agent("agent".into()),
            ConductorError::AgentRunNotFound { id: "id".into() },
            ConductorError::AgentRunNotInConversation {
                run_id: "r".into(),
                conversation_id: "c".into(),
            },
            ConductorError::FeedbackNotFound { id: "id".into() },
            ConductorError::FeedbackRunMismatch {
                feedback_id: "f".into(),
                run_id: "r".into(),
            },
            ConductorError::NoPendingFeedbackForRun { run_id: "r".into() },
            ConductorError::FeedbackNotPending {
                id: "id".into(),
                status: "done".into(),
            },
            ConductorError::Workflow("wf".into()),
            ConductorError::WorkflowCancelled,
            ConductorError::WorkflowRunAlreadyActive { name: "wf".into() },
            ConductorError::WorkflowRunNotFound { id: "id".into() },
            ConductorError::UnknownSourceType("jira".into()),
            ConductorError::ConversationNotFound { id: "id".into() },
            ConductorError::ConversationHasActiveRun { id: "id".into() },
            ConductorError::Notification("notif".into()),
        ]
    }

    #[test]
    fn exit_codes_are_unique() {
        let mut seen: HashMap<i32, String> = HashMap::new();
        for variant in all_variants() {
            let code = variant.exit_code();
            let name = format!("{:?}", variant);
            if let Some(existing) = seen.get(&code) {
                panic!("duplicate exit code {}: {} and {}", code, existing, name);
            }
            seen.insert(code, name);
        }
    }

    #[test]
    fn invalid_input_and_unknown_source_type_have_distinct_exit_codes() {
        let invalid_input = ConductorError::InvalidInput("x".into()).exit_code();
        let unknown_source = ConductorError::UnknownSourceType("x".into()).exit_code();
        assert_ne!(
            invalid_input, unknown_source,
            "InvalidInput (exit {}) and UnknownSourceType (exit {}) must have distinct exit codes",
            invalid_input, unknown_source
        );
        assert_eq!(invalid_input, 27);
        assert_eq!(unknown_source, 43);
    }
}
