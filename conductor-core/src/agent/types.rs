use std::path::{Component, PathBuf};

use serde::{Deserialize, Serialize};

use super::status::{AgentRunStatus, FeedbackStatus, FeedbackType, StepStatus};
use crate::error::Result;

/// A single step in an agent's two-phase execution plan.
///
/// Defined natively in conductor-core (not re-exported from runkon-runtimes)
/// since plan-step semantics are conductor's two-phase agent execution
/// model, not a portable runtime concept.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

/// A single agent run as conductor persists it.
///
/// Defined natively here (not re-exported from runkon-runtimes) so that
/// conductor-domain fields (`worktree_id`, `repo_id`, `conversation_id`,
/// `parent_run_id`, `bot_name`, `plan`) and the conductor-only
/// `WaitingForFeedback` status stay out of the portable crate. The runtime
/// layer sees a [`runkon_runtimes::RunHandle`] subset; conversion happens at
/// the boundary in [`SqliteHostAdapter`](crate::runtime::adapter::SqliteHostAdapter).
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

    /// Project this conductor record into the portable [`runkon_runtimes::RunHandle`]
    /// subset consumed by the runtime layer (`AgentRuntime` / `RunTracker` traits).
    /// Drops conductor-domain fields (`worktree_id`, `repo_id`, `prompt`, `plan`,
    /// `parent_run_id`, `bot_name`, `conversation_id`) and collapses
    /// `WaitingForFeedback` to `Running`.
    pub fn to_run_handle(&self) -> runkon_runtimes::RunHandle {
        runkon_runtimes::RunHandle {
            id: self.id.clone(),
            status: self.status.into(),
            subprocess_pid: self.subprocess_pid,
            runtime: self.runtime.clone(),
            session_id: self.claude_session_id.clone(),
            result_text: self.result_text.clone(),
            started_at: self.started_at.clone(),
            ended_at: self.ended_at.clone(),
            log_file: self.log_file.clone(),
            model: self.model.clone(),
            cost_usd: self.cost_usd,
            num_turns: self.num_turns,
            duration_ms: self.duration_ms,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
        }
    }
}

/// Resolves `..` and `.` components without touching the filesystem so that
/// `starts_with` checks cannot be bypassed by paths like
/// `/log/dir/../../../etc/passwd`.
fn lexical_normalize(path: PathBuf) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            c => out.push(c),
        }
    }
    out.iter().collect()
}

/// Extension trait for `AgentRun` that provides conductor-specific functionality.
pub trait AgentRunExt {
    /// Returns the log file path for this run.
    fn log_path(&self) -> Result<PathBuf>;

    /// Returns true if this run ended (failed/cancelled) with incomplete plan steps
    /// and has a session_id available for resume.
    fn needs_resume(&self) -> bool;

    /// Returns true if the run has a plan with at least one incomplete step.
    fn has_incomplete_plan_steps(&self) -> bool;

    /// Returns the incomplete plan steps (not yet done).
    fn incomplete_plan_steps(&self) -> Vec<&PlanStep>;

    /// Build a resume prompt from the remaining plan steps.
    fn build_resume_prompt(&self) -> String;
}

impl AgentRunExt for AgentRun {
    fn log_path(&self) -> Result<PathBuf> {
        match self.log_file.as_deref() {
            Some(path) => {
                let resolved = lexical_normalize(PathBuf::from(path));
                let log_dir = lexical_normalize(crate::config::agent_log_dir());
                if resolved.starts_with(&log_dir) {
                    Ok(resolved)
                } else {
                    Err(crate::error::ConductorError::Agent(format!(
                        "log_file path is outside agent log directory: {path}"
                    )))
                }
            }
            None => crate::config::agent_log_path(&self.id),
        }
    }

    fn needs_resume(&self) -> bool {
        matches!(
            self.status,
            AgentRunStatus::Failed | AgentRunStatus::Cancelled
        ) && self.claude_session_id.is_some()
            && self.has_incomplete_plan_steps()
    }

    fn has_incomplete_plan_steps(&self) -> bool {
        self.plan
            .as_ref()
            .is_some_and(|steps| steps.iter().any(|s| !s.done))
    }

    fn incomplete_plan_steps(&self) -> Vec<&PlanStep> {
        self.plan
            .as_ref()
            .map(|steps| steps.iter().filter(|s| !s.done).collect())
            .unwrap_or_default()
    }

    fn build_resume_prompt(&self) -> String {
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

/// A selectable option for `SingleSelect` / `MultiSelect` feedback types.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackOption {
    /// Machine-readable value sent back as the response.
    pub value: String,
    /// Human-readable label shown to the user.
    pub label: String,
}

/// A human-in-the-loop feedback request created by an agent run.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// The kind of input requested (text, confirm, single_select, multi_select).
    #[serde(default)]
    pub feedback_type: FeedbackType,
    /// Selectable options for `SingleSelect` / `MultiSelect` types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<FeedbackOption>>,
    /// Per-request timeout in seconds. `None` means wait indefinitely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
}

/// Parameters for creating a new feedback request (builder-style).
#[derive(Debug, Clone, Default)]
pub struct FeedbackRequestParams {
    pub feedback_type: FeedbackType,
    pub options: Option<Vec<FeedbackOption>>,
    pub timeout_secs: Option<i64>,
}

/// Aggregated agent stats for a ticket (across all linked worktrees).
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    pub session_id: Option<String>,
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
    use crate::agent::status::AgentRunStatus;

    fn make_run(id: &str, log_file: Option<&str>) -> AgentRun {
        AgentRun {
            id: id.to_string(),
            worktree_id: None,
            repo_id: None,
            claude_session_id: None,
            prompt: String::new(),
            status: AgentRunStatus::Running,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: "2025-01-01T00:00:00Z".into(),
            ended_at: None,
            log_file: log_file.map(String::from),
            model: None,
            plan: None,
            parent_run_id: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            bot_name: None,
            conversation_id: None,
            subprocess_pid: None,
            runtime: "claude".into(),
        }
    }

    /// Build a fully-populated `AgentRun` so `to_run_handle` projection can be
    /// asserted field-by-field.
    fn make_full_run() -> AgentRun {
        AgentRun {
            id: "01JVFJT9K7XPPQ9MH6JV7XRM3M".into(),
            worktree_id: Some("wt-1".into()),
            repo_id: Some("repo-1".into()),
            claude_session_id: Some("sess-abc".into()),
            prompt: "do the thing".into(),
            status: AgentRunStatus::Completed,
            result_text: Some("done".into()),
            cost_usd: Some(0.42),
            num_turns: Some(7),
            duration_ms: Some(1234),
            started_at: "2025-01-01T00:00:00Z".into(),
            ended_at: Some("2025-01-01T00:01:00Z".into()),
            log_file: Some("/tmp/log".into()),
            model: Some("sonnet".into()),
            plan: Some(vec![PlanStep::default()]),
            parent_run_id: Some("parent-1".into()),
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_input_tokens: Some(20),
            cache_creation_input_tokens: Some(10),
            bot_name: Some("conductor-bot".into()),
            conversation_id: Some("conv-1".into()),
            subprocess_pid: Some(12345),
            runtime: "claude".into(),
        }
    }

    #[test]
    fn is_active_true_for_running_and_waiting_for_feedback() {
        let mut run = make_full_run();
        run.status = AgentRunStatus::Running;
        assert!(run.is_active(), "Running must be active");
        run.status = AgentRunStatus::WaitingForFeedback;
        assert!(run.is_active(), "WaitingForFeedback must be active");
    }

    #[test]
    fn is_active_false_for_terminal_statuses() {
        let mut run = make_full_run();
        for status in [
            AgentRunStatus::Completed,
            AgentRunStatus::Failed,
            AgentRunStatus::Cancelled,
        ] {
            run.status = status;
            assert!(
                !run.is_active(),
                "{status:?} is terminal and must not be active"
            );
        }
    }

    #[test]
    fn is_waiting_for_feedback_only_true_for_that_variant() {
        let mut run = make_full_run();
        run.status = AgentRunStatus::WaitingForFeedback;
        assert!(run.is_waiting_for_feedback());
        for status in [
            AgentRunStatus::Running,
            AgentRunStatus::Completed,
            AgentRunStatus::Failed,
            AgentRunStatus::Cancelled,
        ] {
            run.status = status;
            assert!(
                !run.is_waiting_for_feedback(),
                "{status:?} is not WaitingForFeedback"
            );
        }
    }

    #[test]
    fn to_run_handle_projects_portable_fields() {
        let run = make_full_run();
        let handle = run.to_run_handle();

        assert_eq!(handle.id, run.id);
        assert_eq!(handle.subprocess_pid, run.subprocess_pid);
        assert_eq!(handle.runtime, run.runtime);
        // claude_session_id maps to the generic `session_id` on the portable handle.
        assert_eq!(handle.session_id, run.claude_session_id);
        assert_eq!(handle.result_text, run.result_text);
        assert_eq!(handle.started_at, run.started_at);
        assert_eq!(handle.ended_at, run.ended_at);
        assert_eq!(handle.log_file, run.log_file);
        assert_eq!(handle.model, run.model);
        assert_eq!(handle.cost_usd, run.cost_usd);
        assert_eq!(handle.num_turns, run.num_turns);
        assert_eq!(handle.duration_ms, run.duration_ms);
        assert_eq!(handle.input_tokens, run.input_tokens);
        assert_eq!(handle.output_tokens, run.output_tokens);
        assert_eq!(handle.cache_read_input_tokens, run.cache_read_input_tokens);
        assert_eq!(
            handle.cache_creation_input_tokens,
            run.cache_creation_input_tokens
        );
        assert_eq!(handle.status, runkon_runtimes::RunStatus::Completed);
    }

    #[test]
    fn to_run_handle_collapses_waiting_for_feedback_to_running() {
        let mut run = make_full_run();
        run.status = AgentRunStatus::WaitingForFeedback;
        let handle = run.to_run_handle();
        // The runtime layer doesn't model paused-for-feedback; it appears as
        // "still active" — i.e. Running — to AgentRuntime callers.
        assert_eq!(handle.status, runkon_runtimes::RunStatus::Running);
    }

    #[test]
    fn to_run_handle_status_mapping_for_terminal_states() {
        for (input, expected) in [
            (AgentRunStatus::Running, runkon_runtimes::RunStatus::Running),
            (
                AgentRunStatus::Completed,
                runkon_runtimes::RunStatus::Completed,
            ),
            (AgentRunStatus::Failed, runkon_runtimes::RunStatus::Failed),
            (
                AgentRunStatus::Cancelled,
                runkon_runtimes::RunStatus::Cancelled,
            ),
        ] {
            let mut run = make_full_run();
            run.status = input;
            assert_eq!(
                run.to_run_handle().status,
                expected,
                "AgentRunStatus::{input:?} must project to RunStatus::{expected:?}"
            );
        }
    }

    #[test]
    fn log_path_rejects_log_file_outside_log_dir() {
        let run = make_run("01JVFJT9K7XPPQ9MH6JV7XRM3M", Some("/tmp/custom.log"));
        assert!(run.log_path().is_err());
    }

    #[test]
    fn log_path_accepts_log_file_inside_log_dir() {
        let inside = crate::config::agent_log_dir().join("foo.log");
        let run = make_run("01JVFJT9K7XPPQ9MH6JV7XRM3M", Some(inside.to_str().unwrap()));
        assert_eq!(run.log_path().unwrap(), inside);
    }

    #[test]
    fn log_path_falls_back_to_ulid_derived_path() {
        let run = make_run("01JVFJT9K7XPPQ9MH6JV7XRM3M", None);
        let path = run.log_path().unwrap();
        assert!(path
            .to_string_lossy()
            .ends_with("01JVFJT9K7XPPQ9MH6JV7XRM3M.log"));
    }

    #[test]
    fn log_path_rejects_non_ulid_id() {
        let run = make_run("../../etc/passwd", None);
        assert!(run.log_path().is_err());
    }

    #[test]
    fn log_path_rejects_dotdot_traversal_that_starts_with_log_dir() {
        let log_dir = crate::config::agent_log_dir();
        let traversal = log_dir.join("../../../etc/passwd");
        let run = make_run(
            "01JVFJT9K7XPPQ9MH6JV7XRM3M",
            Some(traversal.to_str().unwrap()),
        );
        assert!(
            run.log_path().is_err(),
            "path with .. components that escape log_dir must be rejected"
        );
    }

    #[test]
    fn log_path_accepts_path_with_harmless_dotdot_inside_log_dir() {
        let log_dir = crate::config::agent_log_dir();
        let inside = log_dir.join("sub/../valid.log");
        let run = make_run("01JVFJT9K7XPPQ9MH6JV7XRM3M", Some(inside.to_str().unwrap()));
        let result = run.log_path().unwrap();
        assert_eq!(result, log_dir.join("valid.log"));
    }

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
