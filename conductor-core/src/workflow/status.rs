use serde::{Deserialize, Serialize};

/// Status of a workflow run.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Waiting,
}

impl std::fmt::Display for WorkflowRunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Waiting => "waiting",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for WorkflowRunStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "waiting" => Ok(Self::Waiting),
            _ => Err(format!("unknown WorkflowRunStatus: {s}")),
        }
    }
}

impl WorkflowRunStatus {
    /// Canonical set of statuses that constitute an "active" run.
    pub const ACTIVE: [WorkflowRunStatus; 3] = [
        WorkflowRunStatus::Pending,
        WorkflowRunStatus::Running,
        WorkflowRunStatus::Waiting,
    ];

    /// Returns the SQL string representations of all active statuses.
    pub fn active_strings() -> Vec<String> {
        Self::ACTIVE.iter().map(|s| s.to_string()).collect()
    }

    /// Whether this status is terminal (no further transitions expected).
    /// Part of: fsm-state-specification-template@1.0.0
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Whether this status is active (run is in progress or waiting).
    /// Part of: fsm-state-specification-template@1.0.0
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Pending | Self::Running | Self::Waiting)
    }
}

crate::impl_sql_enum!(WorkflowRunStatus);

/// Status of a single workflow step execution.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStepStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
    Waiting,
    TimedOut,
}

impl std::fmt::Display for WorkflowStepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Waiting => "waiting",
            Self::TimedOut => "timed_out",
        };
        write!(f, "{s}")
    }
}

impl WorkflowStepStatus {
    /// Short display label used in summaries and status columns.
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::Completed => "ok",
            Self::Failed => "FAIL",
            Self::Skipped => "skip",
            Self::Running => "...",
            Self::Pending => "-",
            Self::Waiting => "wait",
            Self::TimedOut => "tout",
        }
    }
}

impl WorkflowStepStatus {
    /// Whether this status is terminal (no further transitions expected).
    /// Part of: fsm-state-specification-template@1.0.0
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Skipped | Self::TimedOut
        )
    }
}

impl std::str::FromStr for WorkflowStepStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "skipped" => Ok(Self::Skipped),
            "waiting" => Ok(Self::Waiting),
            "timed_out" => Ok(Self::TimedOut),
            _ => Err(format!("unknown WorkflowStepStatus: {s}")),
        }
    }
}

crate::impl_sql_enum!(WorkflowStepStatus);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_terminal_states() {
        assert!(WorkflowRunStatus::Completed.is_terminal());
        assert!(WorkflowRunStatus::Failed.is_terminal());
        assert!(WorkflowRunStatus::Cancelled.is_terminal());
        assert!(!WorkflowRunStatus::Pending.is_terminal());
        assert!(!WorkflowRunStatus::Running.is_terminal());
        assert!(!WorkflowRunStatus::Waiting.is_terminal());
    }

    #[test]
    fn run_active_states() {
        assert!(WorkflowRunStatus::Pending.is_active());
        assert!(WorkflowRunStatus::Running.is_active());
        assert!(WorkflowRunStatus::Waiting.is_active());
        assert!(!WorkflowRunStatus::Completed.is_active());
        assert!(!WorkflowRunStatus::Failed.is_active());
        assert!(!WorkflowRunStatus::Cancelled.is_active());
    }

    #[test]
    fn step_terminal_states() {
        assert!(WorkflowStepStatus::Completed.is_terminal());
        assert!(WorkflowStepStatus::Failed.is_terminal());
        assert!(WorkflowStepStatus::Skipped.is_terminal());
        assert!(WorkflowStepStatus::TimedOut.is_terminal());
        assert!(!WorkflowStepStatus::Pending.is_terminal());
        assert!(!WorkflowStepStatus::Running.is_terminal());
        assert!(!WorkflowStepStatus::Waiting.is_terminal());
    }

    #[test]
    fn run_terminal_and_active_are_mutually_exclusive() {
        let all = [
            WorkflowRunStatus::Pending,
            WorkflowRunStatus::Running,
            WorkflowRunStatus::Completed,
            WorkflowRunStatus::Failed,
            WorkflowRunStatus::Cancelled,
            WorkflowRunStatus::Waiting,
        ];
        for s in all {
            assert!(
                s.is_terminal() != s.is_active(),
                "{s} should be exactly one of terminal or active"
            );
        }
    }
}
