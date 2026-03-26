use crate::workflow_dsl::*;

/// Minimal workflow header that satisfies the parser's required fields.
const WF_HEADER: &str = r#"meta { targets = ["worktree"] }"#;

fn make_wf(body: &str) -> String {
    format!("workflow w {{\n  {WF_HEADER}\n{body}\n}}")
}

// ---------------------------------------------------------------------------
// script parsing
// ---------------------------------------------------------------------------

#[test]
fn test_parse_script_happy_path() {
    let src = make_wf(r#"  script my-step { run = "scripts/build.sh" }"#);
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    assert_eq!(def.body.len(), 1);
    match &def.body[0] {
        WorkflowNode::Script(s) => {
            assert_eq!(s.name, "my-step");
            assert_eq!(s.run, "scripts/build.sh");
            assert!(s.env.is_empty());
            assert!(s.timeout.is_none());
            assert_eq!(s.retries, 0);
            assert!(s.on_fail.is_none());
        }
        other => panic!("expected Script node, got {other:?}"),
    }
}

#[test]
fn test_parse_script_with_all_fields() {
    let src = make_wf(
        r#"  script build {
    run = "ci/build.sh"
    timeout = "120"
    retries = "2"
    env = { CI = "true" BRANCH = "main" }
  }"#,
    );
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Script(s) => {
            assert_eq!(s.run, "ci/build.sh");
            assert_eq!(s.timeout, Some(120));
            assert_eq!(s.retries, 2);
            assert_eq!(s.env.get("CI").map(|s| s.as_str()), Some("true"));
            assert_eq!(s.env.get("BRANCH").map(|s| s.as_str()), Some("main"));
        }
        other => panic!("expected Script node, got {other:?}"),
    }
}

#[test]
fn test_parse_script_with_bot_name() {
    let src = make_wf(
        r#"  script deploy {
    run = "scripts/deploy.sh"
    as = "deploy-bot"
  }"#,
    );
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Script(s) => {
            assert_eq!(s.name, "deploy");
            assert_eq!(s.run, "scripts/deploy.sh");
            assert_eq!(s.bot_name.as_deref(), Some("deploy-bot"));
        }
        other => panic!("expected Script node, got {other:?}"),
    }
}

#[test]
fn test_parse_script_without_bot_name_defaults_to_none() {
    let src = make_wf(r#"  script simple { run = "scripts/run.sh" }"#);
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Script(s) => {
            assert!(
                s.bot_name.is_none(),
                "bot_name should be None when `as` is not specified"
            );
        }
        other => panic!("expected Script node, got {other:?}"),
    }
}

#[test]
fn test_parse_script_missing_run_field() {
    let src = make_wf(r#"  script my-step { timeout = "30" }"#);
    let err = parse_workflow_str(&src, "w.wf").unwrap_err();
    assert!(
        err.to_string().contains("missing required `run` field"),
        "expected 'missing required `run` field', got: {err}"
    );
}

#[test]
fn test_parse_script_invalid_timeout() {
    let src = make_wf(r#"  script my-step { run = "x.sh" timeout = "not-a-number" }"#);
    let err = parse_workflow_str(&src, "w.wf").unwrap_err();
    assert!(
        err.to_string().contains("invalid timeout"),
        "expected 'invalid timeout', got: {err}"
    );
}

#[test]
fn test_parse_script_invalid_retries() {
    let src = make_wf(r#"  script my-step { run = "x.sh" retries = "bad" }"#);
    let err = parse_workflow_str(&src, "w.wf").unwrap_err();
    assert!(
        err.to_string().contains("invalid retries"),
        "expected 'invalid retries', got: {err}"
    );
}

// ---------------------------------------------------------------------------
// bot_name on call / call_workflow
// ---------------------------------------------------------------------------

#[test]
fn test_parse_call_with_bot_name() {
    let input = r#"
        workflow test {
            meta { targets = ["worktree"] }
            call my_agent { as = "developer" }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let node = def.body.first().unwrap();
    match node {
        WorkflowNode::Call(c) => {
            assert_eq!(c.bot_name.as_deref(), Some("developer"));
        }
        other => panic!("Expected Call node, got {other:?}"),
    }
}

#[test]
fn test_parse_call_without_bot_name() {
    let input = r#"
        workflow test {
            meta { targets = ["worktree"] }
            call my_agent {}
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let node = def.body.first().unwrap();
    match node {
        WorkflowNode::Call(c) => {
            assert!(c.bot_name.is_none(), "bot_name should be None when omitted");
        }
        other => panic!("Expected Call node, got {other:?}"),
    }
}

#[test]
fn test_parse_call_workflow_with_bot_name() {
    let input = r#"
        workflow test {
            meta { targets = ["worktree"] }
            call workflow sub-workflow { as = "reviewer" }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let node = def.body.first().unwrap();
    match node {
        WorkflowNode::CallWorkflow(cw) => {
            assert_eq!(cw.bot_name.as_deref(), Some("reviewer"));
        }
        other => panic!("Expected CallWorkflow node, got {other:?}"),
    }
}

#[test]
fn test_parse_call_workflow_without_bot_name() {
    let input = r#"
        workflow test {
            meta { targets = ["worktree"] }
            call workflow sub-workflow {}
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let node = def.body.first().unwrap();
    match node {
        WorkflowNode::CallWorkflow(cw) => {
            assert!(
                cw.bot_name.is_none(),
                "bot_name should be None when omitted"
            );
        }
        other => panic!("Expected CallWorkflow node, got {other:?}"),
    }
}

#[test]
fn test_parse_call_bot_name_serde_roundtrip() {
    let input = r#"
        workflow test {
            meta { targets = ["worktree"] }
            call my_agent { as = "developer" }
            call workflow sub { as = "reviewer" }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let json = serde_json::to_string(&def).unwrap();
    let restored: WorkflowDef = serde_json::from_str(&json).unwrap();
    match restored.body.first().unwrap() {
        WorkflowNode::Call(c) => assert_eq!(c.bot_name.as_deref(), Some("developer")),
        other => panic!("Expected Call, got {other:?}"),
    }
    match restored.body.get(1).unwrap() {
        WorkflowNode::CallWorkflow(cw) => {
            assert_eq!(cw.bot_name.as_deref(), Some("reviewer"))
        }
        other => panic!("Expected CallWorkflow, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// collect_schema_refs / collect_bot_names
// ---------------------------------------------------------------------------

#[test]
fn test_collect_schema_refs_empty() {
    assert!(collect_schema_refs(&[]).is_empty());
}

#[test]
fn test_collect_schema_refs_call_node() {
    let src = make_wf(
        r#"  call plan { output = "review-findings" }
  call build"#,
    );
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    let refs = collect_schema_refs(&def.body);
    assert_eq!(refs, vec!["review-findings"]);
}

#[test]
fn test_collect_schema_refs_nested_if() {
    let src = make_wf(
        r#"  call plan { output = "plan-output" }
  if plan.ready {
    call implement { output = "impl-result" }
  }"#,
    );
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    let refs = collect_schema_refs(&def.body);
    assert!(refs.contains(&"plan-output".to_string()));
    assert!(refs.contains(&"impl-result".to_string()));
}

#[test]
fn test_collect_schema_refs_parallel_node() {
    let src = make_wf(
        r#"  parallel {
    output = "shared-schema"
    call reviewer_a
    call reviewer_b
  }"#,
    );
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    let refs = collect_schema_refs(&def.body);
    assert!(refs.contains(&"shared-schema".to_string()));
}

#[test]
fn test_collect_all_schema_refs_includes_always_block() {
    let src = make_wf(
        r#"  call plan { output = "plan-schema" }
  always {
    call notify { output = "notify-schema" }
  }"#,
    );
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    let refs = def.collect_all_schema_refs();
    assert!(
        refs.contains(&"plan-schema".to_string()),
        "body schema missing"
    );
    assert!(
        refs.contains(&"notify-schema".to_string()),
        "always schema missing"
    );
}

#[test]
fn test_collect_all_schema_refs_deduplicates() {
    let src = make_wf(
        r#"  call step_a { output = "shared" }
  call step_b { output = "shared" }"#,
    );
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    let refs = def.collect_all_schema_refs();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0], "shared");
}

#[test]
fn test_collect_bot_names_empty() {
    assert!(collect_bot_names(&[]).is_empty());
}

#[test]
fn test_collect_bot_names_call_node() {
    let src = make_wf(
        r#"  call plan { as = "conductor-ai" }
  call build"#,
    );
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    let names = collect_bot_names(&def.body);
    assert_eq!(names, vec!["conductor-ai"]);
}

#[test]
fn test_collect_bot_names_nested_blocks() {
    let src = make_wf(
        r#"  if step.marker {
    call act { as = "my-bot" }
  }
  while step.marker {
    max_iterations = 3
    on_max_iter = fail
    call retry { as = "my-bot" }
  }"#,
    );
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    let names = collect_bot_names(&def.body);
    // both calls have the same bot name — raw list has two entries
    assert_eq!(names.len(), 2);
    assert!(names.iter().all(|n| n == "my-bot"));
}

#[test]
fn test_collect_all_bot_names_includes_always_block() {
    let src = make_wf(
        r#"  call plan { as = "main-bot" }
  always {
    call cleanup { as = "always-bot" }
  }"#,
    );
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    let names = def.collect_all_bot_names();
    assert!(names.contains(&"main-bot".to_string()), "body bot missing");
    assert!(
        names.contains(&"always-bot".to_string()),
        "always bot missing"
    );
}

#[test]
fn test_collect_all_bot_names_deduplicates() {
    let src = make_wf(
        r#"  call step_a { as = "shared-bot" }
  call step_b { as = "shared-bot" }"#,
    );
    let def = parse_workflow_str(&src, "w.wf").unwrap();
    let names = def.collect_all_bot_names();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0], "shared-bot");
}

// ---------------------------------------------------------------------------
// collect_agent_names / collect_all_agent_refs
// ---------------------------------------------------------------------------

#[test]
fn test_collect_all_agent_refs_deduplicates_across_body_and_always() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call shared-agent {}
        call body-only-agent {}
        always {
            call shared-agent {}
            call always-only-agent {}
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs: Vec<String> = def
        .collect_all_agent_refs()
        .into_iter()
        .map(|r| r.label().to_string())
        .collect();
    // Should be sorted and deduplicated: "shared-agent" appears in both blocks
    assert_eq!(
        refs,
        vec![
            "always-only-agent".to_string(),
            "body-only-agent".to_string(),
            "shared-agent".to_string(),
        ]
    );
}
