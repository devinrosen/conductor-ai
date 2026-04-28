use serde::{Deserialize, Serialize};

/// Status of an agent run.
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Running,
    WaitingForFeedback,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for AgentRunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Running => "running",
            Self::WaitingForFeedback => "waiting_for_feedback",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for AgentRunStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "running" => Ok(Self::Running),
            "waiting_for_feedback" => Ok(Self::WaitingForFeedback),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(format!("unknown AgentRunStatus: {s}")),
        }
    }
}

#[cfg(feature = "rusqlite")]
macro_rules! impl_rusqlite_string_enum {
    ($ty:ty) => {
        impl rusqlite::types::ToSql for $ty {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::from(self.to_string()))
            }
        }

        impl rusqlite::types::FromSql for $ty {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let s = String::column_result(value)?;
                s.parse().map_err(|e: String| {
                    rusqlite::types::FromSqlError::Other(Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        e,
                    )))
                })
            }
        }
    };
}

#[cfg(feature = "rusqlite")]
mod rusqlite_impl {
    use super::AgentRunStatus;
    impl_rusqlite_string_enum!(AgentRunStatus);
}

/// Status of a single plan step.
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl std::fmt::Display for StepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for StepStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            _ => Err(format!("unknown StepStatus: {s}")),
        }
    }
}

#[cfg(feature = "rusqlite")]
mod rusqlite_impl_step {
    use super::StepStatus;
    impl_rusqlite_string_enum!(StepStatus);
}

/// A single step in an agent's two-phase execution plan.
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    /// ULID primary key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub description: String,
    /// Backward-compat flag derived from `status == StepStatus::Completed`.
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub status: StepStatus,
    /// Ordering within the run's plan (0-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

impl Default for PlanStep {
    fn default() -> Self {
        Self {
            id: None,
            description: String::new(),
            done: false,
            status: StepStatus::Pending,
            position: None,
            started_at: None,
            completed_at: None,
        }
    }
}

/// A single agent run.
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub worktree_id: Option<String>,
    pub repo_id: Option<String>,
    pub claude_session_id: Option<String>,
    pub prompt: String,
    pub status: AgentRunStatus,
    pub result_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub log_file: Option<String>,
    pub model: Option<String>,
    pub plan: Option<Vec<PlanStep>>,
    pub parent_run_id: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub bot_name: Option<String>,
    pub conversation_id: Option<String>,
    pub subprocess_pid: Option<i64>,
    #[serde(default = "default_runtime_field")]
    pub runtime: String,
}

fn default_runtime_field() -> String {
    "claude".to_string()
}

impl AgentRun {
    /// Returns true if this run is currently active (running or waiting for feedback).
    pub fn is_active(&self) -> bool {
        matches!(
            self.status,
            AgentRunStatus::Running | AgentRunStatus::WaitingForFeedback
        )
    }

    /// Returns true if this run is waiting for human feedback.
    pub fn is_waiting_for_feedback(&self) -> bool {
        self.status == AgentRunStatus::WaitingForFeedback
    }
}
