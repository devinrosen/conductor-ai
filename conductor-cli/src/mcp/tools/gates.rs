use std::path::Path;

use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{get_arg, open_db_and_config, tool_err, tool_ok};

pub(super) fn tool_approve_gate(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::workflow::WorkflowManager;

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

    let (conn, _config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let step = match wf_mgr.find_waiting_gate(run_id) {
        Ok(Some(s)) => s,
        Ok(None) => return tool_err(format!("No waiting gate found for run {run_id}")),
        Err(e) => return tool_err(e),
    };
    match wf_mgr.approve_gate(&step.id, "mcp", feedback, selections.as_deref()) {
        Ok(()) => tool_ok(format!("Gate approved for run {run_id}.")),
        Err(e) => tool_err(e),
    }
}

pub(super) fn tool_reject_gate(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::workflow::WorkflowManager;

    let run_id = require_arg!(args, "run_id");
    let (conn, _config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let feedback = get_arg(args, "feedback");
    let step = match wf_mgr.find_waiting_gate(run_id) {
        Ok(Some(s)) => s,
        Ok(None) => return tool_err(format!("No waiting gate found for run {run_id}")),
        Err(e) => return tool_err(e),
    };
    match wf_mgr.reject_gate(&step.id, "mcp", feedback) {
        Ok(()) => tool_ok(format!("Gate rejected for run {run_id}.")),
        Err(e) => tool_err(e),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn make_test_db() -> (tempfile::NamedTempFile, std::path::PathBuf) {
        use conductor_core::db::open_database;
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        open_database(&path).expect("open_database");
        (file, path)
    }

    fn empty_args() -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }

    fn args_with(key: &str, val: &str) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert(key.to_string(), Value::String(val.to_string()));
        m
    }

    /// Helper: set up a workflow run with a waiting gate step. Returns (run_id, step_id).
    fn make_waiting_gate(db_path: &std::path::Path) -> (String, String) {
        use conductor_core::agent::AgentManager;
        use conductor_core::db::open_database;
        use conductor_core::workflow::{GateType, WorkflowManager, WorkflowStepStatus};

        let conn = open_database(db_path).expect("open db");

        // FK: workflow_runs.parent_run_id references agent_runs.id
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(None, "workflow", None, None)
            .expect("create agent run");

        let mgr = WorkflowManager::new(&conn);

        let run = mgr
            .create_workflow_run("test-wf", None, &parent.id, false, "manual", None)
            .expect("create run");

        let step_id = mgr
            .insert_step(&run.id, "human_review", "reviewer", false, 0, 0)
            .expect("insert step");

        mgr.set_step_gate_info(&step_id, GateType::HumanApproval, Some("Approve?"), "24h")
            .expect("set gate info");

        mgr.update_step_status(
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
        let (_f, db) = make_test_db();
        let result = tool_approve_gate(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_reject_gate_missing_run_id_arg() {
        let (_f, db) = make_test_db();
        let result = tool_reject_gate(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_approve_gate_no_waiting_gate() {
        let (_f, db) = make_test_db();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_approve_gate(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_reject_gate_no_waiting_gate() {
        let (_f, db) = make_test_db();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = tool_reject_gate(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_approve_gate_success() {
        let (_f, db) = make_test_db();
        let (run_id, _step_id) = make_waiting_gate(&db);

        let args = args_with("run_id", &run_id);
        let result = tool_approve_gate(&db, &args);
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
        let (_f, db) = make_test_db();
        let (run_id, _step_id) = make_waiting_gate(&db);

        let args = args_with("run_id", &run_id);
        let result = tool_reject_gate(&db, &args);
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
        let (_f, db) = make_test_db();
        let (run_id, _step_id) = make_waiting_gate(&db);

        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id.clone()));
        args.insert("feedback".to_string(), Value::String("LGTM".to_string()));
        let result = tool_approve_gate(&db, &args);
        assert_ne!(result.is_error, Some(true));

        // Verify the feedback was persisted
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowManager;
        let conn = open_database(&db).expect("open db");
        let mgr = WorkflowManager::new(&conn);
        let steps = mgr.get_workflow_steps(&run_id).expect("get steps");
        assert_eq!(steps[0].gate_feedback.as_deref(), Some("LGTM"));
        assert_eq!(steps[0].gate_approved_by.as_deref(), Some("mcp"));
    }

    #[test]
    fn test_dispatch_reject_gate_with_feedback() {
        let (_f, db) = make_test_db();
        let (run_id, _step_id) = make_waiting_gate(&db);

        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id.clone()));
        args.insert(
            "feedback".to_string(),
            Value::String("Needs more work".to_string()),
        );
        let result = tool_reject_gate(&db, &args);
        assert_ne!(result.is_error, Some(true));

        // Verify the feedback was persisted
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowManager;
        let conn = open_database(&db).expect("open db");
        let mgr = WorkflowManager::new(&conn);
        let steps = mgr.get_workflow_steps(&run_id).expect("get steps");
        assert_eq!(steps[0].gate_feedback.as_deref(), Some("Needs more work"));
        assert_eq!(steps[0].gate_approved_by.as_deref(), Some("mcp"));
    }
}
