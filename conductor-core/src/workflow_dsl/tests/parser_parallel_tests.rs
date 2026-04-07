use crate::workflow_dsl::*;

#[test]
fn test_parallel_requires_calls() {
    let input = r#"
        workflow test {
            parallel {
                fail_fast = true
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("at least one call"));
}

#[test]
fn test_call_with_output_option() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call review-security { output = "review-findings" }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.agent, AgentRef::Name("review-security".to_string()));
            assert_eq!(c.output.as_deref(), Some("review-findings"));
        }
        _ => panic!("Expected Call node"),
    }
}

#[test]
fn test_call_with_output_and_retries() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call review { output = "review-findings" retries = 2 }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.output.as_deref(), Some("review-findings"));
            assert_eq!(c.retries, 2);
        }
        _ => panic!("Expected Call node"),
    }
}

#[test]
fn test_call_without_output() {
    let input = r#"workflow test { meta { targets = ["worktree"] } call plan }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert!(c.output.is_none());
        }
        _ => panic!("Expected Call node"),
    }
}

#[test]
fn test_parallel_with_block_level_output() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        parallel {
            output = "review-findings"
            fail_fast = false
            call review-security
            call review-style
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Parallel(p) => {
            assert_eq!(p.output.as_deref(), Some("review-findings"));
            assert_eq!(p.calls.len(), 2);
            assert!(!p.fail_fast);
        }
        _ => panic!("Expected Parallel node"),
    }
}

#[test]
fn test_parallel_with_per_call_output_override() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        parallel {
            output = "review-findings"
            call review-security
            call lint-check { output = "lint-results" }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Parallel(p) => {
            assert_eq!(p.output.as_deref(), Some("review-findings"));
            assert_eq!(p.calls.len(), 2);
            assert!(p.call_outputs.is_empty() || !p.call_outputs.contains_key("0"));
            assert_eq!(
                p.call_outputs.get("1").map(|s| s.as_str()),
                Some("lint-results")
            );
        }
        _ => panic!("Expected Parallel node"),
    }
}

#[test]
fn test_parallel_if_parsed() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call detect-db-migrations
        parallel {
            fail_fast = false
            call review-security    { retries = 1 }
            call review-db-migrations { retries = 1 if = "detect-db-migrations.has_db_migrations" }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[1] {
        WorkflowNode::Parallel(p) => {
            assert_eq!(p.calls.len(), 2);
            assert!(!p.call_if.contains_key("0"));
            assert_eq!(
                p.call_if.get("1"),
                Some(&(
                    "detect-db-migrations".to_string(),
                    "has_db_migrations".to_string()
                ))
            );
        }
        _ => panic!("Expected Parallel node"),
    }
}

#[test]
fn test_parallel_call_if_snapshot_roundtrip() {
    // Regression test: HashMap<String, (String, String)> must survive serde_json
    // serialize → deserialize. Previously the key type was HashMap<usize, ...> which
    // caused "invalid type: string "6", expected usize" on resume because JSON object
    // keys are always strings and serde_json's MapKeyDeserializer does not coerce
    // string keys to integer types.
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call detect-db-migrations
        call detect-file-types
        parallel {
            fail_fast = false
            call review-architecture    { retries = 1 }
            call review-dry-abstraction { retries = 1 }
            call review-security        { retries = 1 if = "detect-file-types.has_code_changes" }
            call review-performance     { retries = 1 if = "detect-file-types.has_code_changes" }
            call review-error-handling  { retries = 1 if = "detect-file-types.has_code_changes" }
            call review-test-coverage   { retries = 1 if = "detect-file-types.has_code_changes" }
            call review-db-migrations   { retries = 1 if = "detect-db-migrations.has_db_migrations" }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    // Serialize to JSON (as stored in the DB snapshot) and deserialize back.
    let json = serde_json::to_string(&def).expect("serialize failed");
    let def2: WorkflowDef = serde_json::from_str(&json).expect(
        "deserialize failed — HashMap<String, (String, String)> must round-trip through JSON",
    );
    match &def2.body[2] {
        WorkflowNode::Parallel(p) => {
            assert_eq!(p.calls.len(), 7);
            // call_if should survive the round-trip with correct string keys
            assert_eq!(
                p.call_if.get("6"),
                Some(&(
                    "detect-db-migrations".to_string(),
                    "has_db_migrations".to_string()
                ))
            );
            assert_eq!(
                p.call_if.get("2"),
                Some(&(
                    "detect-file-types".to_string(),
                    "has_code_changes".to_string()
                ))
            );
        }
        _ => panic!("Expected Parallel node at index 2"),
    }
}

#[test]
fn test_parallel_if_malformed_no_dot() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        parallel {
            call review-db-migrations { if = "no-dot-here" }
        }
    }"#;
    let err = parse_workflow_str(input, "test.wf").unwrap_err();
    assert!(
        err.to_string().contains("step.marker"),
        "Expected error about step.marker format, got: {err}"
    );
}

#[test]
fn test_parallel_if_with_output_and_with() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call detect-check
        parallel {
            output = "findings"
            with   = ["scope"]
            fail_fast = false
            call agent-a { retries = 1 }
            call agent-b { output = "b-out" with = ["extra"] if = "detect-check.flag" }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[1] {
        WorkflowNode::Parallel(p) => {
            assert_eq!(p.output.as_deref(), Some("findings"));
            assert_eq!(p.with, vec!["scope".to_string()]);
            assert!(!p.call_if.contains_key("0"));
            assert_eq!(
                p.call_if.get("1"),
                Some(&("detect-check".to_string(), "flag".to_string()))
            );
            assert_eq!(p.call_outputs.get("1").map(|s| s.as_str()), Some("b-out"));
            assert_eq!(p.call_with.get("1"), Some(&vec!["extra".to_string()]));
        }
        _ => panic!("Expected Parallel node"),
    }
}
