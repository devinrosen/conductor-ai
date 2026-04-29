pub(crate) mod context;
pub(crate) mod db;
pub(crate) mod log_parsing;
pub(crate) mod manager;
pub(crate) mod status;
pub(crate) mod types;

// Re-export everything that was public in the old agent.rs

pub use context::{build_startup_context, PR_REVIEW_SWARM_PROMPT_PREFIX};

pub use log_parsing::{
    count_turns_in_log, count_turns_incremental, parse_agent_log, parse_events_from_line,
    parse_result_event,
};

pub use manager::feedback::normalize_feedback_response;
pub use manager::AgentManager;

pub use status::{
    parse_feedback_marker, parse_feedback_marker_structured, AgentRunStatus, FeedbackStatus,
    FeedbackType, ParsedFeedbackMarker, StepStatus, DEFAULT_AGENT_ERROR_MSG, FEEDBACK_MARKER,
    FEEDBACK_MAX_LEN,
};

pub use types::{
    ActiveAgentCounts, AgentCreatedIssue, AgentEvent, AgentRun, AgentRunEvent, ClaudeJsonResult,
    CostPhase, FeedbackOption, FeedbackRequest, FeedbackRequestParams, LogResult, PlanStep,
    RunTreeTotals, TicketAgentTotals, EVENT_KIND_TOOL_ERROR, META_KEY_ERROR_TEXT,
};

#[cfg(test)]
mod tests {
    use super::log_parsing::scan_log_for_result_at;
    use super::*;

    #[test]
    fn test_claude_json_result_deserialization() {
        let json = r#"{"session_id":"sess-abc","result":"Final output","cost_usd":0.05,"num_turns":3,"duration_ms":15000,"is_error":false}"#;
        let result: ClaudeJsonResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(result.cost_usd, Some(0.05));
        assert_eq!(result.num_turns, Some(3));
        assert_eq!(result.duration_ms, Some(15000));
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn test_scan_log_for_result_success() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test-scan-success.log");
        std::fs::write(
            &log_path,
            r#"{"type":"init","session_id":"s1"}
{"type":"text","text":"working..."}
{"result":"All done","total_cost_usd":0.05,"num_turns":3,"duration_ms":5000}
"#,
        )
        .unwrap();

        let result = scan_log_for_result_at(&log_path).unwrap();
        assert_eq!(result.result_text.as_deref(), Some("All done"));
        assert_eq!(result.cost_usd, Some(0.05));
        assert_eq!(result.num_turns, Some(3));
        assert_eq!(result.duration_ms, Some(5000));
        assert!(!result.is_error);
    }

    #[test]
    fn test_scan_log_for_result_error() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test-scan-error.log");
        std::fs::write(
            &log_path,
            r#"{"result":"Something went wrong","is_error":true,"total_cost_usd":0.01,"num_turns":1,"duration_ms":1000}
"#,
        )
        .unwrap();

        let result = scan_log_for_result_at(&log_path).unwrap();
        assert!(result.is_error);
        assert_eq!(result.result_text.as_deref(), Some("Something went wrong"));
    }

    #[test]
    fn test_scan_log_no_result() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test-no-result.log");
        std::fs::write(
            &log_path,
            r#"{"type":"init","session_id":"s1"}
{"type":"text","text":"still working..."}
"#,
        )
        .unwrap();

        assert!(scan_log_for_result_at(&log_path).is_none());
    }
}
