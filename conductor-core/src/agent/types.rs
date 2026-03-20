use serde::{Deserialize, Serialize};

use super::status::{AgentRunStatus, FeedbackStatus, StepStatus};

/// A single step in an agent's two-phase execution plan.
/// Stored as individual records in the `agent_run_steps` table.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub worktree_id: Option<String>,
    pub claude_session_id: Option<String>,
    pub prompt: String,
    pub status: AgentRunStatus,
    pub result_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub tmux_window: Option<String>,
    pub log_file: Option<String>,
    /// The model used for this run (e.g. "claude-sonnet-4-6"). None means claude's default.
    pub model: Option<String>,
    /// Two-phase execution plan: JSON-serialized list of steps with completion state.
    pub plan: Option<Vec<PlanStep>>,
    /// If this is a child run, the ID of the parent (supervisor) run.
    pub parent_run_id: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    /// GitHub App bot identity used for this run (matches `[github.apps.<name>]`).
    pub bot_name: Option<String>,
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

    /// Returns true if this run ended (failed/cancelled) with incomplete plan steps
    /// and has a session_id available for resume.
    pub fn needs_resume(&self) -> bool {
        matches!(
            self.status,
            AgentRunStatus::Failed | AgentRunStatus::Cancelled
        ) && self.claude_session_id.is_some()
            && self.has_incomplete_plan_steps()
    }

    /// Returns true if the run has a plan with at least one incomplete step.
    pub fn has_incomplete_plan_steps(&self) -> bool {
        self.plan
            .as_ref()
            .is_some_and(|steps| steps.iter().any(|s| !s.done))
    }

    /// Returns the incomplete plan steps (not yet done).
    pub fn incomplete_plan_steps(&self) -> Vec<&PlanStep> {
        self.plan
            .as_ref()
            .map(|steps| steps.iter().filter(|s| !s.done).collect())
            .unwrap_or_default()
    }

    /// Build a resume prompt from the remaining plan steps.
    pub fn build_resume_prompt(&self) -> String {
        let incomplete = self.incomplete_plan_steps();
        if incomplete.is_empty() {
            return "Continue where you left off.".to_string();
        }

        let mut prompt = String::from(
            "Continue where you left off. The following plan steps remain incomplete:\n",
        );
        for (i, step) in incomplete.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, step.description));
        }
        prompt.push_str("\nPlease complete these remaining steps.");
        prompt
    }
}

/// Parsed JSON result from `claude -p --output-format json`.
#[derive(Debug, Deserialize)]
pub struct ClaudeJsonResult {
    pub session_id: Option<String>,
    pub result: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub is_error: Option<bool>,
}

/// A parsed display event from a stream-json agent log.
#[derive(Debug, Clone)]
pub struct AgentEvent {
    pub kind: String,
    pub summary: String,
    /// Optional JSON metadata (e.g. structured error details for `tool_error` events).
    pub metadata: Option<String>,
}

/// A persisted agent run event (trace/span model) stored in `agent_run_events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunEvent {
    pub id: String,
    pub run_id: String,
    pub kind: String,
    pub summary: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub metadata: Option<String>,
}

/// Event kind for tool errors captured from agent output.
pub const EVENT_KIND_TOOL_ERROR: &str = "tool_error";

/// Metadata JSON key that holds the error detail text.
pub const META_KEY_ERROR_TEXT: &str = "error_text";

impl AgentRunEvent {
    /// Duration in milliseconds, if both timestamps are present and parseable.
    pub fn duration_ms(&self) -> Option<i64> {
        let start = chrono::DateTime::parse_from_rfc3339(&self.started_at).ok()?;
        let end = chrono::DateTime::parse_from_rfc3339(self.ended_at.as_ref()?).ok()?;
        Some((end - start).num_milliseconds().max(0))
    }

    /// Extract the `error_text` field from metadata JSON for `tool_error` events.
    ///
    /// Returns `None` if this is not a `tool_error` event or if the metadata
    /// does not contain an `error_text` field.
    pub fn error_detail_text(&self) -> Option<String> {
        if self.kind != EVENT_KIND_TOOL_ERROR {
            return None;
        }
        let meta = self.metadata.as_ref()?;
        let parsed: serde_json::Value = serde_json::from_str(meta).ok()?;
        parsed
            .get(META_KEY_ERROR_TEXT)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
}

/// A GitHub issue (or other tracker issue) created by an agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCreatedIssue {
    pub id: String,
    pub agent_run_id: String,
    pub repo_id: String,
    pub source_type: String,
    pub source_id: String,
    pub title: String,
    pub url: String,
    pub created_at: String,
}

/// A human-in-the-loop feedback request created by an agent run.
/// The agent pauses execution and waits for the user to respond.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackRequest {
    pub id: String,
    pub run_id: String,
    /// The question or context the agent is asking about.
    pub prompt: String,
    /// The user's response (populated when status changes to `FeedbackStatus::Responded`).
    pub response: Option<String>,
    pub status: FeedbackStatus,
    pub created_at: String,
    pub responded_at: Option<String>,
}

/// Aggregated agent stats for a ticket (across all linked worktrees).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TicketAgentTotals {
    pub ticket_id: String,
    pub total_runs: i64,
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read_tokens: i64,
    pub total_cache_creation_tokens: i64,
}

/// Aggregated stats for a run tree (parent + all descendants).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunTreeTotals {
    pub total_runs: i64,
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}

/// A single phase in the cost breakdown (initial run, review fix #N, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostPhase {
    pub label: String,
    pub model: Option<String>,
    pub cost_usd: f64,
    pub duration_ms: i64,
}

/// Counts of active agent runs (running / waiting_for_feedback) for a single repo.
#[derive(Debug, Clone, Default)]
pub struct ActiveAgentCounts {
    pub running: u32,
    pub waiting: u32,
}

/// Parsed result event from an agent log file or streaming JSON.
pub struct LogResult {
    pub result_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub is_error: bool,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(kind: &str, metadata: Option<&str>) -> AgentRunEvent {
        AgentRunEvent {
            id: "ev1".into(),
            run_id: "run1".into(),
            kind: kind.into(),
            summary: "test".into(),
            started_at: "2025-01-01T00:00:00Z".into(),
            ended_at: None,
            metadata: metadata.map(String::from),
        }
    }

    #[test]
    fn test_error_detail_text_returns_text_for_tool_error() {
        let ev = make_event(
            EVENT_KIND_TOOL_ERROR,
            Some(r#"{"error_text":"something broke","tool_use_id":"t1"}"#),
        );
        assert_eq!(ev.error_detail_text().as_deref(), Some("something broke"));
    }

    #[test]
    fn test_error_detail_text_none_for_wrong_kind() {
        let ev = make_event("tool_use", Some(r#"{"error_text":"something broke"}"#));
        assert!(ev.error_detail_text().is_none());
    }

    #[test]
    fn test_error_detail_text_none_when_no_metadata() {
        let ev = make_event(EVENT_KIND_TOOL_ERROR, None);
        assert!(ev.error_detail_text().is_none());
    }

    #[test]
    fn test_error_detail_text_none_when_no_error_text_key() {
        let ev = make_event(EVENT_KIND_TOOL_ERROR, Some(r#"{"tool_use_id":"t1"}"#));
        assert!(ev.error_detail_text().is_none());
    }

    #[test]
    fn test_error_detail_text_none_for_invalid_json() {
        let ev = make_event(EVENT_KIND_TOOL_ERROR, Some("not json"));
        assert!(ev.error_detail_text().is_none());
    }
}
