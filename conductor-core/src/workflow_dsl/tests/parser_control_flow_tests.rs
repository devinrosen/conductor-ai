use crate::workflow_dsl::*;

#[test]
fn test_while_requires_max_iterations() {
    let input = r#"
        workflow test {
            while step.marker {
                call something
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("max_iterations"));
}

#[test]
fn test_parse_unless() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call analyze
    unless analyze.has_errors {
        call deploy
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 2);
    match &def.body[1] {
        WorkflowNode::Unless(u) => {
            assert!(
                matches!(&u.condition, Condition::StepMarker { step, marker } if step == "analyze" && marker == "has_errors")
            );
            assert_eq!(u.body.len(), 1);
            assert!(matches!(&u.body[0], WorkflowNode::Call(_)));
        }
        _ => panic!("Expected Unless node"),
    }
}

#[test]
fn test_parse_do_while() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call analyze
    do {
        max_iterations = 3
        stuck_after    = 2
        on_max_iter    = continue
        call diagnose
        call fix
    } while analyze.needs_retry
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 2);
    match &def.body[1] {
        WorkflowNode::DoWhile(n) => {
            assert_eq!(n.step, "analyze");
            assert_eq!(n.marker, "needs_retry");
            assert_eq!(n.max_iterations, 3);
            assert_eq!(n.stuck_after, Some(2));
            assert_eq!(n.on_max_iter, OnMaxIter::Continue);
            assert_eq!(n.body.len(), 2);
        }
        _ => panic!("Expected DoWhile node"),
    }
}

#[test]
fn test_parse_do_while_requires_max_iterations() {
    // New syntax: missing max_iterations after `while` clause
    let input = r#"workflow test { do { call baz } while foo.bar }"#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("max_iterations"));
}

#[test]
fn test_parse_do_while_old_syntax_gives_hint() {
    // Old syntax (do x.y { ... }) must produce a clear error with a hint.
    let input = r#"workflow test { do foo.bar { call baz } }"#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("expected `{` after `do`"), "msg={msg}");
    assert!(msg.contains("do { ... } while x.y"), "msg={msg}");
}

#[test]
fn test_parse_do_while_invalid_on_max_iter() {
    let input = r#"workflow test { do { max_iterations = 3  on_max_iter = explode  call baz } while foo.bar }"#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Invalid on_max_iter"));
}

#[test]
fn test_parse_do_while_serde_roundtrip() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call check
    do {
        max_iterations = 2
        call fix
    } while check.has_issues
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let json = serde_json::to_string(&def).unwrap();
    assert!(json.contains("\"type\":\"do_while\""));
    // Deserialize and verify round-trip
    let def2: WorkflowDef = serde_json::from_str(&json).unwrap();
    assert_eq!(def2.body.len(), def.body.len());
    match &def2.body[1] {
        WorkflowNode::DoWhile(n) => {
            assert_eq!(n.step, "check");
            assert_eq!(n.marker, "has_issues");
            assert_eq!(n.max_iterations, 2);
        }
        _ => panic!("Expected DoWhile node after roundtrip"),
    }
}

#[test]
fn test_parse_plain_do_block() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    do {
        output = "review-result"
        with   = ["shared-context", "extra"]
        call reviewer_a
        call reviewer_b
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 1);
    match &def.body[0] {
        WorkflowNode::Do(n) => {
            assert_eq!(n.output.as_deref(), Some("review-result"));
            assert_eq!(n.with, vec!["shared-context", "extra"]);
            assert_eq!(n.body.len(), 2);
            // Verify body contains the expected calls
            match &n.body[0] {
                WorkflowNode::Call(c) => {
                    assert_eq!(c.agent, AgentRef::Name("reviewer_a".to_string()))
                }
                _ => panic!("Expected Call node"),
            }
        }
        _ => panic!("Expected Do node"),
    }
}

#[test]
fn test_parse_plain_do_block_minimal() {
    // Plain do block with no options — just grouping
    let input = r#"workflow test { meta { targets = ["worktree"] } do { call build } }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 1);
    match &def.body[0] {
        WorkflowNode::Do(n) => {
            assert!(n.output.is_none());
            assert!(n.with.is_empty());
            assert_eq!(n.body.len(), 1);
        }
        _ => panic!("Expected Do node"),
    }
}

#[test]
fn test_parse_plain_do_block_rejects_unknown_keys() {
    let input = r#"workflow test { do { max_iterations = 5 call build } }"#;
    let err_msg = parse_workflow_str(input, "test.wf")
        .unwrap_err()
        .to_string();
    assert!(
        err_msg.contains("unknown option"),
        "expected unknown option error, got: {err_msg}"
    );
}

#[test]
fn test_collect_agent_names_inside_do_while() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        do {
            max_iterations = 2
            call fix
            call verify
        } while check.has_issues
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs = collect_agent_names(&def.body);
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0], AgentRef::Name("fix".to_string()));
    assert_eq!(refs[1], AgentRef::Name("verify".to_string()));
}

#[test]
fn test_collect_snippet_refs_inside_do_while() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        do {
            max_iterations = 2
            call fix { with = ["do-while-context"] }
        } while check.has_issues
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs = collect_snippet_refs(&def.body);
    assert_eq!(refs, vec!["do-while-context".to_string()]);
}

#[test]
fn test_collect_snippet_refs_inside_do_block() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        do {
            with = ["block-snippet"]
            call fix { with = ["call-snippet"] }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs = collect_snippet_refs(&def.body);
    // Should include both the do-block's `with` and the inner call's `with`
    assert!(refs.contains(&"block-snippet".to_string()));
    assert!(refs.contains(&"call-snippet".to_string()));
    assert_eq!(refs.len(), 2);
}
