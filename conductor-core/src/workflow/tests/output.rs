#![allow(unused_imports)]

use super::*;

#[test]
fn test_parse_conductor_output() {
    let text = r#"Here is my analysis...

<<<CONDUCTOR_OUTPUT>>>
{"markers": ["has_review_issues", "has_critical_issues"], "context": "Found 2 issues in src/lib.rs"}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
    let output = parse_conductor_output(text).unwrap();
    assert_eq!(
        output.markers,
        vec!["has_review_issues", "has_critical_issues"]
    );
    assert_eq!(output.context, "Found 2 issues in src/lib.rs");
}

#[test]
fn test_parse_conductor_output_missing() {
    assert!(parse_conductor_output("no output block here").is_none());
}

#[test]
fn test_parse_conductor_output_no_markers() {
    let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"markers\": [], \"context\": \"All good\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
    let output = parse_conductor_output(text).unwrap();
    assert!(output.markers.is_empty());
    assert_eq!(output.context, "All good");
}

#[test]
fn test_parse_conductor_output_marker_in_field_value() {
    // The real block has the marker string inside a field value — must still parse correctly.
    let text = r#"The agent output block:
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["real"], "context": "actual output with <<<CONDUCTOR_OUTPUT>>> mentioned inside"}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
    let output = parse_conductor_output(text).unwrap();
    assert_eq!(output.markers, vec!["real"]);
    assert_eq!(
        output.context,
        "actual output with <<<CONDUCTOR_OUTPUT>>> mentioned inside"
    );
}

#[test]
fn test_substitute_variables() {
    use std::collections::HashMap;
    let mut vars = HashMap::new();
    vars.insert("ticket_id", "FEAT-123".to_string());
    vars.insert("prior_context", "Created PLAN.md".to_string());

    let prompt = "Fix ticket {{ticket_id}}. Context: {{prior_context}}. Unknown: {{unknown}}.";
    let result = substitute_variables(prompt, &vars);
    assert_eq!(
        result,
        "Fix ticket FEAT-123. Context: Created PLAN.md. Unknown: {{unknown}}."
    );
}

#[test]
fn test_workflow_run_status_roundtrip() {
    for status in [
        WorkflowRunStatus::Pending,
        WorkflowRunStatus::Running,
        WorkflowRunStatus::Completed,
        WorkflowRunStatus::Failed,
        WorkflowRunStatus::Cancelled,
        WorkflowRunStatus::Waiting,
    ] {
        let s = status.to_string();
        let parsed: WorkflowRunStatus = s.parse().unwrap();
        assert_eq!(parsed, status);
    }
}

#[test]
fn test_workflow_step_status_roundtrip() {
    for status in [
        WorkflowStepStatus::Pending,
        WorkflowStepStatus::Running,
        WorkflowStepStatus::Completed,
        WorkflowStepStatus::Failed,
        WorkflowStepStatus::Skipped,
        WorkflowStepStatus::Waiting,
    ] {
        let s = status.to_string();
        let parsed: WorkflowStepStatus = s.parse().unwrap();
        assert_eq!(parsed, status);
    }
}

#[test]
fn test_interpret_agent_output_schema_valid() {
    let schema = make_test_schema();
    let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"approved\": true, \"summary\": \"all good\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
    let (markers, context, json) = interpret_agent_output(Some(text), Some(&schema), true).unwrap();
    assert_eq!(context, "all good");
    assert!(json.is_some());
    // approved=true → no not_approved marker
    assert!(!markers.contains(&"not_approved".to_string()));
}

#[test]
fn test_interpret_agent_output_schema_validation_fails_succeeded() {
    let schema = make_test_schema();
    // Missing required field "approved"
    let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"summary\": \"oops\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
    let result = interpret_agent_output(Some(text), Some(&schema), true);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("structured output validation"));
}

#[test]
fn test_interpret_agent_output_schema_validation_fails_not_succeeded_falls_back() {
    let schema = make_test_schema();
    // Missing required field — but succeeded=false so it falls back
    let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"summary\": \"oops\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
    let (markers, context, json) =
        interpret_agent_output(Some(text), Some(&schema), false).unwrap();
    // Falls back to generic parse_conductor_output which doesn't find markers/context
    assert!(json.is_none());
    assert!(markers.is_empty());
    assert!(context.is_empty());
}

#[test]
fn test_interpret_agent_output_no_schema_generic_parsing() {
    let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"markers\": [\"done\"], \"context\": \"finished\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
    let (markers, context, json) = interpret_agent_output(Some(text), None, true).unwrap();
    assert_eq!(markers, vec!["done"]);
    assert_eq!(context, "finished");
    assert!(json.is_none());
}

#[test]
fn test_interpret_agent_output_no_text() {
    let schema = make_test_schema();
    // result_text is None with schema — falls back
    let (markers, context, json) = interpret_agent_output(None, Some(&schema), false).unwrap();
    assert!(markers.is_empty());
    assert!(context.is_empty());
    assert!(json.is_none());
}
