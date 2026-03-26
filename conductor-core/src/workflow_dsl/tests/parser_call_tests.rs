use crate::workflow_dsl::*;

#[test]
fn test_parse_call_explicit_path() {
    let input =
        r#"workflow test { meta { targets = ["worktree"] } call ".claude/agents/review.md" }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 1);
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(
                c.agent,
                AgentRef::Path(".claude/agents/review.md".to_string())
            );
        }
        _ => panic!("Expected Call node"),
    }
}

#[test]
fn test_parse_call_mixed_name_and_path() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call plan
    call ".claude/agents/code-review.md"
    call implement { retries = 1  on_fail = ".claude/agents/diagnose.md" }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 3);
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.agent, AgentRef::Name("plan".to_string()));
        }
        _ => panic!("Expected Call node"),
    }
    match &def.body[1] {
        WorkflowNode::Call(c) => {
            assert_eq!(
                c.agent,
                AgentRef::Path(".claude/agents/code-review.md".to_string())
            );
        }
        _ => panic!("Expected Call node"),
    }
    match &def.body[2] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.agent, AgentRef::Name("implement".to_string()));
            assert_eq!(c.retries, 1);
            assert_eq!(
                c.on_fail,
                Some(AgentRef::Path(".claude/agents/diagnose.md".to_string()))
            );
        }
        _ => panic!("Expected Call node"),
    }
}

#[test]
fn test_parse_parallel_explicit_paths() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    parallel {
        call reviewer-security
        call ".claude/agents/code-review.md"
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Parallel(p) => {
            assert_eq!(
                p.calls,
                vec![
                    AgentRef::Name("reviewer-security".to_string()),
                    AgentRef::Path(".claude/agents/code-review.md".to_string()),
                ]
            );
        }
        _ => panic!("Expected Parallel node"),
    }
}

#[test]
fn test_parse_call_workflow_simple() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow lint-fix
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 1);
    match &def.body[0] {
        WorkflowNode::CallWorkflow(n) => {
            assert_eq!(n.workflow, "lint-fix");
            assert!(n.inputs.is_empty());
            assert_eq!(n.retries, 0);
            assert!(n.on_fail.is_none());
        }
        _ => panic!("Expected CallWorkflow node"),
    }
}

#[test]
fn test_parse_call_workflow_with_inputs() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow test-coverage {
        inputs {
            pr_url = "{{pr_url}}"
            branch = "main"
        }
        retries = 1
        on_fail = notify-lint-failure
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 1);
    match &def.body[0] {
        WorkflowNode::CallWorkflow(n) => {
            assert_eq!(n.workflow, "test-coverage");
            assert_eq!(n.inputs.get("pr_url").unwrap(), "{{pr_url}}");
            assert_eq!(n.inputs.get("branch").unwrap(), "main");
            assert_eq!(n.retries, 1);
            assert_eq!(
                n.on_fail,
                Some(AgentRef::Name("notify-lint-failure".to_string()))
            );
        }
        _ => panic!("Expected CallWorkflow node"),
    }
}

#[test]
fn test_parse_call_workflow_as_before_inputs() {
    // Regression: `as =` before `inputs { }` used to silently drop the workflow
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow ticket-to-pr {
        as = "developer"
        inputs {
            ticket_id = "{{ticket_id}}"
        }
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 1);
    match &def.body[0] {
        WorkflowNode::CallWorkflow(n) => {
            assert_eq!(n.workflow, "ticket-to-pr");
            assert_eq!(n.inputs.get("ticket_id").unwrap(), "{{ticket_id}}");
            assert_eq!(n.bot_name.as_deref(), Some("developer"));
        }
        _ => panic!("Expected CallWorkflow node"),
    }
}

#[test]
fn test_parse_call_workflow_no_block() {
    let input = "workflow parent { meta { targets = [\"worktree\"] } call workflow child }";
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 1);
    match &def.body[0] {
        WorkflowNode::CallWorkflow(n) => {
            assert_eq!(n.workflow, "child");
            assert!(n.inputs.is_empty());
        }
        _ => panic!("Expected CallWorkflow node"),
    }
}

#[test]
fn test_parse_mixed_call_and_call_workflow() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call plan
    call workflow lint-fix
    call implement
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 3);
    assert!(matches!(&def.body[0], WorkflowNode::Call(_)));
    assert!(matches!(&def.body[1], WorkflowNode::CallWorkflow(_)));
    assert!(matches!(&def.body[2], WorkflowNode::Call(_)));
}

#[test]
fn test_parse_call_workflow_in_if_block() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call analyze
    if analyze.needs_lint {
        call workflow lint-fix
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 2);
    match &def.body[1] {
        WorkflowNode::If(i) => {
            assert_eq!(i.body.len(), 1);
            assert!(matches!(&i.body[0], WorkflowNode::CallWorkflow(_)));
        }
        _ => panic!("Expected If node"),
    }
}

#[test]
fn test_parse_call_workflow_in_unless_block() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call analyze
    unless analyze.needs_lint {
        call workflow lint-fix
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 2);
    match &def.body[1] {
        WorkflowNode::Unless(u) => {
            assert_eq!(u.body.len(), 1);
            assert!(matches!(&u.body[0], WorkflowNode::CallWorkflow(_)));
        }
        _ => panic!("Expected Unless node"),
    }
}

#[test]
fn test_parse_call_workflow_in_while_block() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call analyze
    while analyze.needs_fixes {
        max_iterations = 3
        call workflow lint-fix
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 2);
    match &def.body[1] {
        WorkflowNode::While(w) => {
            assert_eq!(w.body.len(), 1);
            match &w.body[0] {
                WorkflowNode::CallWorkflow(n) => assert_eq!(n.workflow, "lint-fix"),
                _ => panic!("Expected CallWorkflow node inside while"),
            }
        }
        _ => panic!("Expected While node"),
    }
}

#[test]
fn test_parse_call_workflow_in_always_block() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call build
    always {
        call workflow notify
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.always.len(), 1);
    match &def.always[0] {
        WorkflowNode::CallWorkflow(n) => assert_eq!(n.workflow, "notify"),
        _ => panic!("Expected CallWorkflow node inside always"),
    }
}

#[test]
fn test_collect_workflow_refs() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call plan
    call workflow lint-fix
    if plan.needs_tests {
        call workflow test-coverage
    }
    always {
        call workflow notify
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let mut refs = collect_workflow_refs(&def.body);
    refs.extend(collect_workflow_refs(&def.always));
    refs.sort();
    assert_eq!(refs, vec!["lint-fix", "notify", "test-coverage"]);
}

#[test]
fn test_collect_workflow_refs_in_while() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call analyze
    while analyze.needs_fixes {
        max_iterations = 3
        call workflow lint-fix
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs = collect_workflow_refs(&def.body);
    assert_eq!(refs, vec!["lint-fix"]);
}

#[test]
fn test_collect_workflow_refs_in_always() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call build
    always {
        call workflow notify
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let body_refs = collect_workflow_refs(&def.body);
    let always_refs = collect_workflow_refs(&def.always);
    assert!(body_refs.is_empty());
    assert_eq!(always_refs, vec!["notify"]);
}

#[test]
fn test_collect_workflow_refs_in_do_while() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call analyze
    do {
        max_iterations = 3
        call workflow lint-fix
    } while analyze.needs_fixes
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs = collect_workflow_refs(&def.body);
    assert_eq!(refs, vec!["lint-fix"]);
}

#[test]
fn test_call_workflow_serialization_roundtrip() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow test-coverage {
        inputs { pr_url = "https://example.com" }
        retries = 2
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let json = serde_json::to_string(&def).unwrap();
    let restored: WorkflowDef = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.body.len(), 1);
    match &restored.body[0] {
        WorkflowNode::CallWorkflow(n) => {
            assert_eq!(n.workflow, "test-coverage");
            assert_eq!(n.inputs.get("pr_url").unwrap(), "https://example.com");
            assert_eq!(n.retries, 2);
        }
        _ => panic!("Expected CallWorkflow node after deserialization"),
    }
}

#[test]
fn test_collect_agent_names_call_workflow_on_fail() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow lint-fix {
        on_fail = recovery-agent
    }
    call workflow test-coverage
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs = collect_agent_names(&def.body);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0], AgentRef::Name("recovery-agent".to_string()));
}
