use crate::workflow_dsl::*;

#[test]
fn test_agent_ref_label() {
    assert_eq!(AgentRef::Name("plan".to_string()).label(), "plan");
    assert_eq!(
        AgentRef::Path(".claude/agents/plan.md".to_string()).label(),
        ".claude/agents/plan.md"
    );
}

#[test]
fn test_agent_ref_step_key() {
    // Name variants: step_key == label
    assert_eq!(AgentRef::Name("plan".to_string()).step_key(), "plan");

    // Path variants: step_key is the file stem (no extension)
    assert_eq!(
        AgentRef::Path(".claude/agents/plan.md".to_string()).step_key(),
        "plan"
    );
    assert_eq!(
        AgentRef::Path(".claude/agents/code-review.md".to_string()).step_key(),
        "code-review"
    );
    // Nested subdir — still just the stem
    assert_eq!(
        AgentRef::Path("custom/dir/my-agent.md".to_string()).step_key(),
        "my-agent"
    );
}

/// A quoted bare name (no `/`) in `on_fail` should produce `AgentRef::Name`,
/// not `AgentRef::Path` — quoting alone does not make a value a path.
#[test]
fn test_on_fail_quoted_bare_name_is_name() {
    let input =
        r#"workflow test { meta { targets = ["worktree"] } call agent { on_fail = "diagnose" } }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(
                c.on_fail,
                Some(OnFail::Agent(AgentRef::Name("diagnose".to_string()))),
                "quoted on_fail value without a slash should be AgentRef::Name"
            );
        }
        _ => panic!("Expected Call node"),
    }
}

/// A bare (unquoted) name in `on_fail` should produce `AgentRef::Name`.
#[test]
fn test_on_fail_bare_name_is_name() {
    let input =
        r#"workflow test { meta { targets = ["worktree"] } call agent { on_fail = diagnose } }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(
                c.on_fail,
                Some(OnFail::Agent(AgentRef::Name("diagnose".to_string())))
            );
        }
        _ => panic!("Expected Call node"),
    }
}

/// A quoted string without a `/` in `call` position should produce
/// `AgentRef::Path`, not `AgentRef::Name`.  In `call` position, quoting is
/// always a deliberate signal that the value is an explicit path, so the
/// slash-heuristic used by `KvValue::into_agent_ref` does not apply.
#[test]
fn test_call_quoted_bare_name_is_path() {
    let input = r#"workflow test { meta { targets = ["worktree"] } call "diagnose" }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(
                c.agent,
                AgentRef::Path("diagnose".to_string()),
                "quoted agent in call position should always be AgentRef::Path"
            );
        }
        _ => panic!("Expected Call node"),
    }
}
