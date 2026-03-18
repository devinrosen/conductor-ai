pub mod context;
pub mod db;
pub mod log_parsing;
pub mod manager;
pub mod status;
pub mod tmux;
pub mod types;

// Re-export everything that was public in the old agent.rs

pub use context::{build_startup_context, PR_REVIEW_SWARM_PROMPT_PREFIX};

pub use log_parsing::{
    count_turns_in_log, parse_agent_log, parse_events_from_line, parse_result_event,
};

pub use manager::AgentManager;

pub use status::{
    parse_feedback_marker, AgentRunStatus, FeedbackStatus, StepStatus, DEFAULT_AGENT_ERROR_MSG,
    FEEDBACK_MARKER, FEEDBACK_MAX_LEN,
};

pub(crate) use tmux::list_live_tmux_windows;

pub use types::{
    ActiveAgentCounts, AgentCreatedIssue, AgentEvent, AgentRun, AgentRunEvent, ClaudeJsonResult,
    CostPhase, FeedbackRequest, LogResult, PlanStep, RunTreeTotals, TicketAgentTotals,
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
    fn test_parse_events_from_line_system_init() {
        let line = r#"{"type":"system","subtype":"init","model":"claude-opus-4-5"}"#;
        let events = parse_events_from_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "system");
        assert!(events[0].summary.contains("claude-opus-4-5"));
    }

    #[test]
    fn test_parse_events_from_line_tool_use() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"description":"run tests"}}]}}"#;
        let events = parse_events_from_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "tool");
        assert!(events[0].summary.contains("Bash"));
        assert!(events[0].summary.contains("run tests"));
    }

    #[test]
    fn test_parse_events_from_line_unknown_type() {
        let line = r#"{"type":"rate_limit_event"}"#;
        let events = parse_events_from_line(line);
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_agent_log_uses_from_line() {
        let line1 = r#"{"type":"system","subtype":"init","model":"claude-3"}"#;
        let line2 =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}"#;
        let content = format!("{line1}\n{line2}\n");

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &content).unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        let events = parse_agent_log(&path);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "system");
        assert_eq!(events[1].kind, "text");
        assert_eq!(events[1].summary, "Hello");
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
