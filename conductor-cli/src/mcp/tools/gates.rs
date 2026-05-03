use conductor_core::Conductor;
use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{get_arg, tool_err, tool_ok};

pub(super) fn tool_approve_gate(
    conductor: &Conductor,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    let run_id = require_arg!(args, "run_id");
    let feedback = get_arg(args, "feedback");

    // Optional selections: JSON array of strings, e.g. ["finding-1","finding-2"]
    let selections: Option<Vec<String>> =
        args.get("selections")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });

    let conn = &conductor.conn;
    let step = match conductor_core::workflow::find_waiting_gate(conn, run_id) {
        Ok(Some(s)) => s,
        Ok(None) => return tool_err(format!("No waiting gate found for run {run_id}")),
        Err(e) => return tool_err(e),
    };
    let context_out = selections
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(conductor_core::workflow::helpers::format_gate_selection_context);
    match conductor_core::workflow::approve_gate(
        conn,
        &step.id,
        "mcp",
        feedback,
        selections.as_deref(),
        context_out,
    ) {
        Ok(()) => tool_ok(format!("Gate approved for run {run_id}.")),
        Err(e) => tool_err(e),
    }
}

pub(super) fn tool_reject_gate(
    conductor: &Conductor,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    let run_id = require_arg!(args, "run_id");
    let conn = &conductor.conn;
    let feedback = get_arg(args, "feedback");
    let step = match conductor_core::workflow::find_waiting_gate(conn, run_id) {
        Ok(Some(s)) => s,
        Ok(None) => return tool_err(format!("No waiting gate found for run {run_id}")),
        Err(e) => return tool_err(e),
    };
    match conductor_core::workflow::reject_gate(conn, &step.id, "mcp", feedback) {
        Ok(()) => tool_ok(format!("Gate rejected for run {run_id}.")),
        Err(e) => tool_err(e),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::test_helpers::make_test_conductor;
    use super::*;
    use serde_json::Value;

    fn empty_args() -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }

    fn args_with(key: &str, val: &str) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert(key.to_string(), Value::String(val.to_string()));
        m
    }

    /// Helper: set up a workflow run with a waiting gate step. Returns (run_id, step_id).
    fn make_waiting_gate(conductor: &Conductor) -> (String, String) {
        use conductor_core::agent::AgentManager;
        use conductor_core::workflow::{GateType, WorkflowStepStatus};

        let conn = &conductor.conn;

        // FK: workflow_runs.parent_run_id references agent_runs.id
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr
            .create_run(None, "workflow", None)
            .expect("create agent run");

        let run = conductor_core::workflow::create_workflow_run(
            conn, "test-wf", None, &parent.id, false, "manual", None,
        )
        .expect("create run");

        let step_id = conductor_core::workflow::insert_step(
            conn,
            &run.id,
            "human_review",
            "reviewer",
            false,
            0,
            0,
        )
        .expect("insert step");

        conductor_core::workflow::set_step_gate_info(
            conn,
            &step_id,
            GateType::HumanApproval,
            Some("Approve?"),
            "24h",
        )
        .expect("set gate info");

        conductor_core::workflow::update_step_status(
            conn,
            &step_id,
            WorkflowStepStatus::Waiting,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("set waiting status");

        (run.id, step_id)
    }

    #[test]
    fn test_dispatch_approve_gate_missing_run_id_arg() {
        let (_f, conductor) = make_test_conductor();
        let result = tool_approve_gate(&conductor, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_reject_gate_missing_run_id_arg() {
        let (_f, conductor) = make_test_conductor();
        let result = tool_reject_gate(&conductor, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_approve_gate_no_waiting_gate() {
        let (_f, conductor) = make_test_conductor();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_approve_gate(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_reject_gate_no_waiting_gate() {
        let (_f, conductor) = make_test_conductor();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_reject_gate(&conductor, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_approve_gate_success() {
        let (_f, conductor) = make_test_conductor();
        let (run_id, _step_id) = make_waiting_gate(&conductor);

        let args = args_with("run_id", &run_id);
        let result = tool_approve_gate(&conductor, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "approve_gate should succeed; got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("approved"), "got: {text}");
    }

    #[test]
    fn test_dispatch_reject_gate_success() {
        let (_f, conductor) = make_test_conductor();
        let (run_id, _step_id) = make_waiting_gate(&conductor);

        let args = args_with("run_id", &run_id);
        let result = tool_reject_gate(&conductor, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "reject_gate should succeed; got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("rejected"), "got: {text}");
    }

    #[test]
    fn test_dispatch_approve_gate_with_feedback() {
        let (_f, conductor) = make_test_conductor();
        let (run_id, _step_id) = make_waiting_gate(&conductor);

        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id.clone()));
        args.insert("feedback".to_string(), Value::String("LGTM".to_string()));
        let result = tool_approve_gate(&conductor, &args);
        assert_ne!(result.is_error, Some(true));

        // Verify the feedback was persisted
        let steps = conductor_core::workflow::get_workflow_steps(&conductor.conn, &run_id)
            .expect("get steps");
        assert_eq!(steps[0].gate_feedback.as_deref(), Some("LGTM"));
        assert_eq!(steps[0].gate_approved_by.as_deref(), Some("mcp"));
    }

    #[test]
    fn test_dispatch_reject_gate_with_feedback() {
        let (_f, conductor) = make_test_conductor();
        let (run_id, _step_id) = make_waiting_gate(&conductor);

        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id.clone()));
        args.insert(
            "feedback".to_string(),
            Value::String("Needs more work".to_string()),
        );
        let result = tool_reject_gate(&conductor, &args);
        assert_ne!(result.is_error, Some(true));

        // Verify the feedback was persisted
        let steps = conductor_core::workflow::get_workflow_steps(&conductor.conn, &run_id)
            .expect("get steps");
        assert_eq!(steps[0].gate_feedback.as_deref(), Some("Needs more work"));
        assert_eq!(steps[0].gate_approved_by.as_deref(), Some("mcp"));
    }
}
