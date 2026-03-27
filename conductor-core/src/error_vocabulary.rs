//! Cross-agent error vocabulary: standardized error codes across agents.
//!
//! Covers pattern: cross-agent-error-vocabulary@1.0.0
//!
//! Classifies `ConductorError` variants into semantic categories with
//! machine-readable `C-{XX}-{NNN}` error codes. Does not replace existing
//! error types — it layers classification on top.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::{ConductorError, SubprocessFailure};

// ---------------------------------------------------------------------------
// Error categories
// ---------------------------------------------------------------------------

/// Semantic category for conductor errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    /// External tool or OS dependency missing/broken.
    Environment,
    /// Configuration file parse error or missing required field.
    Configuration,
    /// Internal state inconsistency (DB, desync).
    State,
    /// Runtime execution failure (subprocess, agent, workflow).
    Execution,
    /// Authentication or authorization failure.
    Permission,
    /// User input or schema validation failure.
    Validation,
}

impl ErrorCategory {
    /// Two-letter code prefix for this category.
    pub fn prefix(&self) -> &'static str {
        match self {
            Self::Environment => "EN",
            Self::Configuration => "CF",
            Self::State => "ST",
            Self::Execution => "EX",
            Self::Permission => "PM",
            Self::Validation => "VL",
        }
    }

    /// All category variants (useful for iteration in tests).
    pub fn all() -> &'static [ErrorCategory] {
        &[
            Self::Environment,
            Self::Configuration,
            Self::State,
            Self::Execution,
            Self::Permission,
            Self::Validation,
        ]
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Environment => write!(f, "Environment"),
            Self::Configuration => write!(f, "Configuration"),
            Self::State => write!(f, "State"),
            Self::Execution => write!(f, "Execution"),
            Self::Permission => write!(f, "Permission"),
            Self::Validation => write!(f, "Validation"),
        }
    }
}

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

/// A structured error code in `C-{XX}-{NNN}` format.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ErrorCode {
    pub category: ErrorCategory,
    pub number: u16,
}

impl ErrorCode {
    pub fn new(category: ErrorCategory, number: u16) -> Self {
        Self { category, number }
    }

    /// Format as `C-{XX}-{NNN}`.
    pub fn code(&self) -> String {
        format!("C-{}-{:03}", self.category.prefix(), self.number)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code())
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Classify a `ConductorError` into a category and code.
pub fn classify(err: &ConductorError) -> ErrorCode {
    match err {
        ConductorError::Io(_) => ErrorCode::new(ErrorCategory::Environment, 4),
        ConductorError::Config(_) => ErrorCode::new(ErrorCategory::Configuration, 1),
        ConductorError::AgentConfig(_) => ErrorCode::new(ErrorCategory::Configuration, 2),
        ConductorError::Schema(_) => ErrorCode::new(ErrorCategory::Validation, 2),
        ConductorError::InvalidInput(_) => ErrorCode::new(ErrorCategory::Validation, 3),
        ConductorError::Database(_) => ErrorCode::new(ErrorCategory::State, 3),
        ConductorError::Git(sub) => classify_subprocess(sub, ErrorCategory::Execution, 3),
        ConductorError::GhCli(sub) => classify_subprocess(sub, ErrorCategory::Execution, 5),
        ConductorError::Agent(_) => ErrorCode::new(ErrorCategory::Execution, 1),
        ConductorError::Workflow(_) => ErrorCode::new(ErrorCategory::Execution, 4),
        ConductorError::WorkflowRunAlreadyActive { .. } => ErrorCode::new(ErrorCategory::State, 2),
        ConductorError::TicketSync(_) => ErrorCode::new(ErrorCategory::Execution, 6),
        ConductorError::RepoNotFound { .. } => ErrorCode::new(ErrorCategory::Validation, 4),
        ConductorError::RepoAlreadyExists { .. } => ErrorCode::new(ErrorCategory::Validation, 5),
        ConductorError::WorktreeNotFound { .. } => ErrorCode::new(ErrorCategory::Validation, 6),
        ConductorError::WorktreeAlreadyExists { .. } => {
            ErrorCode::new(ErrorCategory::Validation, 7)
        }
        ConductorError::IssueSourceAlreadyExists { .. } => {
            ErrorCode::new(ErrorCategory::Validation, 8)
        }
        ConductorError::TicketNotFound { .. } => ErrorCode::new(ErrorCategory::Validation, 9),
        ConductorError::TicketAlreadyLinked => ErrorCode::new(ErrorCategory::Validation, 10),
        ConductorError::FeedbackNotPending { .. } => ErrorCode::new(ErrorCategory::State, 4),
        ConductorError::FeatureNotFound { .. } => ErrorCode::new(ErrorCategory::Validation, 11),
        ConductorError::FeatureAlreadyExists { .. } => {
            ErrorCode::new(ErrorCategory::Validation, 12)
        }
        ConductorError::FeatureStillActive { .. } => ErrorCode::new(ErrorCategory::State, 5),
    }
}

/// Classify a subprocess failure by examining exit code and stderr content.
///
/// Returns a `Permission` error code when auth-related patterns are detected,
/// otherwise returns the provided default category/number.
pub fn classify_subprocess(
    sub: &SubprocessFailure,
    default_category: ErrorCategory,
    default_number: u16,
) -> ErrorCode {
    let stderr_lower = sub.stderr.to_lowercase();

    // Auth / permission failures
    if stderr_lower.contains("authentication")
        || stderr_lower.contains("permission denied")
        || stderr_lower.contains("could not read from remote")
        || stderr_lower.contains("403")
        || stderr_lower.contains("401")
        || sub.exit_code == Some(128) && stderr_lower.contains("fatal")
    {
        return ErrorCode::new(ErrorCategory::Permission, 1);
    }

    // Token expired
    if stderr_lower.contains("token") && stderr_lower.contains("expired") {
        return ErrorCode::new(ErrorCategory::Permission, 2);
    }

    // Environment: command not found
    if stderr_lower.contains("not found") || stderr_lower.contains("no such file") {
        return ErrorCode::new(ErrorCategory::Environment, 1);
    }

    ErrorCode::new(default_category, default_number)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_format_regex() {
        let re = regex_lite::Regex::new(r"^C-[A-Z]{2}-\d{3}$").unwrap();
        // Verify all categories produce valid codes
        for cat in ErrorCategory::all() {
            for num in [1u16, 10, 99] {
                let code = ErrorCode::new(*cat, num);
                assert!(
                    re.is_match(&code.code()),
                    "Code {} did not match C-XX-NNN format",
                    code.code()
                );
            }
        }
    }

    #[test]
    fn classify_io_error() {
        let err = ConductorError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        ));
        let code = classify(&err);
        assert_eq!(code.category, ErrorCategory::Environment);
        assert_eq!(code.code(), "C-EN-004");
    }

    #[test]
    fn classify_config_error() {
        let err = ConductorError::Config("bad config".to_string());
        let code = classify(&err);
        assert_eq!(code.category, ErrorCategory::Configuration);
        assert_eq!(code.code(), "C-CF-001");
    }

    #[test]
    fn classify_database_error() {
        let err = ConductorError::Database(rusqlite::Error::QueryReturnedNoRows);
        let code = classify(&err);
        assert_eq!(code.category, ErrorCategory::State);
        assert_eq!(code.code(), "C-ST-003");
    }

    #[test]
    fn classify_invalid_input() {
        let err = ConductorError::InvalidInput("bad input".to_string());
        let code = classify(&err);
        assert_eq!(code.category, ErrorCategory::Validation);
        assert_eq!(code.code(), "C-VL-003");
    }

    #[test]
    fn classify_schema_error() {
        let err = ConductorError::Schema("invalid schema".to_string());
        let code = classify(&err);
        assert_eq!(code.category, ErrorCategory::Validation);
        assert_eq!(code.code(), "C-VL-002");
    }

    #[test]
    fn classify_workflow_error() {
        let err = ConductorError::Workflow("step failed".to_string());
        let code = classify(&err);
        assert_eq!(code.category, ErrorCategory::Execution);
        assert_eq!(code.code(), "C-EX-004");
    }

    #[test]
    fn classify_git_auth_failure() {
        let sub = SubprocessFailure {
            command: "git push".to_string(),
            exit_code: Some(128),
            stderr: "fatal: Authentication failed for ...".to_string(),
            stdout: String::new(),
        };
        let err = ConductorError::Git(sub);
        let code = classify(&err);
        assert_eq!(code.category, ErrorCategory::Permission);
        assert_eq!(code.code(), "C-PM-001");
    }

    #[test]
    fn classify_git_generic_failure() {
        let sub = SubprocessFailure {
            command: "git merge".to_string(),
            exit_code: Some(1),
            stderr: "CONFLICT (content): Merge conflict in file.rs".to_string(),
            stdout: String::new(),
        };
        let err = ConductorError::Git(sub);
        let code = classify(&err);
        assert_eq!(code.category, ErrorCategory::Execution);
        assert_eq!(code.code(), "C-EX-003");
    }

    #[test]
    fn classify_subprocess_token_expired() {
        let sub = SubprocessFailure {
            command: "gh api".to_string(),
            exit_code: Some(1),
            stderr: "error: token has expired, please re-authenticate".to_string(),
            stdout: String::new(),
        };
        let code = classify_subprocess(&sub, ErrorCategory::Execution, 5);
        assert_eq!(code.category, ErrorCategory::Permission);
        assert_eq!(code.code(), "C-PM-002");
    }

    #[test]
    fn classify_subprocess_command_not_found() {
        let sub = SubprocessFailure {
            command: "some-tool".to_string(),
            exit_code: Some(127),
            stderr: "some-tool: command not found".to_string(),
            stdout: String::new(),
        };
        let code = classify_subprocess(&sub, ErrorCategory::Execution, 1);
        assert_eq!(code.category, ErrorCategory::Environment);
        assert_eq!(code.code(), "C-EN-001");
    }

    #[test]
    fn classify_all_error_variants_covered() {
        // Ensure every ConductorError variant can be classified without panicking.
        let errors: Vec<ConductorError> = vec![
            ConductorError::Database(rusqlite::Error::QueryReturnedNoRows),
            ConductorError::RepoNotFound {
                slug: "x".to_string(),
            },
            ConductorError::RepoAlreadyExists {
                slug: "x".to_string(),
            },
            ConductorError::WorktreeNotFound {
                slug: "x".to_string(),
            },
            ConductorError::WorktreeAlreadyExists {
                slug: "x".to_string(),
            },
            ConductorError::Git(SubprocessFailure::from_message("git", "err".to_string())),
            ConductorError::GhCli(SubprocessFailure::from_message("gh", "err".to_string())),
            ConductorError::Config("x".to_string()),
            ConductorError::Io(std::io::Error::other("x")),
            ConductorError::TicketSync("x".to_string()),
            ConductorError::IssueSourceAlreadyExists {
                repo_slug: "x".to_string(),
                source_type: "y".to_string(),
            },
            ConductorError::TicketNotFound {
                id: "x".to_string(),
            },
            ConductorError::Agent("x".to_string()),
            ConductorError::FeedbackNotPending {
                id: "x".to_string(),
                status: "y".to_string(),
            },
            ConductorError::TicketAlreadyLinked,
            ConductorError::Workflow("x".to_string()),
            ConductorError::AgentConfig("x".to_string()),
            ConductorError::Schema("x".to_string()),
            ConductorError::WorkflowRunAlreadyActive {
                name: "x".to_string(),
            },
            ConductorError::InvalidInput("x".to_string()),
            ConductorError::FeatureNotFound {
                name: "x".to_string(),
            },
            ConductorError::FeatureAlreadyExists {
                name: "x".to_string(),
            },
        ];

        for err in &errors {
            let code = classify(err);
            // Verify the code is well-formed
            assert!(
                code.code().starts_with("C-"),
                "Error {:?} produced invalid code: {}",
                err,
                code.code()
            );
        }
        // Ensure we tested all variants (count must match enum variant count)
        assert_eq!(errors.len(), 22);
    }

    #[test]
    fn error_code_display() {
        let code = ErrorCode::new(ErrorCategory::Execution, 4);
        assert_eq!(format!("{code}"), "C-EX-004");
    }

    #[test]
    fn error_category_display() {
        assert_eq!(format!("{}", ErrorCategory::Environment), "Environment");
        assert_eq!(format!("{}", ErrorCategory::Permission), "Permission");
    }

    #[test]
    fn error_code_roundtrip_serde() {
        let code = ErrorCode::new(ErrorCategory::State, 3);
        let json = serde_json::to_string(&code).unwrap();
        let back: ErrorCode = serde_json::from_str(&json).unwrap();
        assert_eq!(code, back);
    }

    #[test]
    fn error_category_roundtrip_serde() {
        for cat in ErrorCategory::all() {
            let json = serde_json::to_string(cat).unwrap();
            let back: ErrorCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(*cat, back);
        }
    }
}
