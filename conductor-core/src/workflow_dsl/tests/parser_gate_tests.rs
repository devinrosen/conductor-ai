use crate::workflow_dsl::{types::GateOptions, *};

#[test]
fn test_unknown_gate_type() {
    let input = r#"
        workflow test {
            gate unknown_type {
                timeout = "1h"
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("Unknown gate type"));
}

#[test]
fn test_parse_gate_review_decision_mode() {
    let input = r#"
        workflow test {
            meta { targets = ["worktree"] }
            gate pr_approval {
                mode = "review_decision"
                timeout = "1h"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let node = def.body.first().unwrap();
    match node {
        WorkflowNode::Gate(g) => {
            assert_eq!(g.approval_mode, ApprovalMode::ReviewDecision);
            assert_eq!(g.gate_type, GateType::PrApproval);
        }
        other => panic!("Expected Gate node, got {other:?}"),
    }
}

#[test]
fn test_parse_gate_min_approvals_mode_explicit() {
    let input = r#"
        workflow test {
            meta { targets = ["worktree"] }
            gate pr_approval {
                mode = "min_approvals"
                min_approvals = 2
                timeout = "1h"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let node = def.body.first().unwrap();
    match node {
        WorkflowNode::Gate(g) => {
            assert_eq!(g.approval_mode, ApprovalMode::MinApprovals);
            assert_eq!(g.min_approvals, 2);
        }
        other => panic!("Expected Gate node, got {other:?}"),
    }
}

#[test]
fn test_parse_gate_invalid_mode_rejected() {
    let input = r#"
        workflow test {
            gate pr_approval {
                mode = "banana"
                timeout = "1h"
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("Invalid mode"), "got: {err}");
}

#[test]
fn test_parse_gate_review_decision_with_min_approvals_rejected() {
    let input = r#"
        workflow test {
            gate pr_approval {
                mode = "review_decision"
                min_approvals = 2
                timeout = "1h"
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("Cannot specify both"),
        "expected conflict error, got: {err}"
    );
}

#[test]
fn test_parse_gate_with_bot_name() {
    let input = r#"
        workflow test {
            meta { targets = ["worktree"] }
            gate pr_approval {
                mode = "review_decision"
                timeout = "1h"
                as = "reviewer"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let node = def.body.first().unwrap();
    match node {
        WorkflowNode::Gate(g) => {
            assert_eq!(g.bot_name.as_deref(), Some("reviewer"));
        }
        other => panic!("Expected Gate node, got {other:?}"),
    }
}

#[test]
fn test_parse_gate_without_bot_name() {
    let input = r#"
        workflow test {
            meta { targets = ["worktree"] }
            gate pr_checks {
                timeout = "30m"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let node = def.body.first().unwrap();
    match node {
        WorkflowNode::Gate(g) => {
            assert!(g.bot_name.is_none(), "bot_name should be None when omitted");
        }
        other => panic!("Expected Gate node, got {other:?}"),
    }
}

#[test]
fn test_parse_quality_gate() {
    let input = r#"
workflow review {
    meta { targets = ["worktree"] }
    call rt-aggregator { output = "roundtable-verdict" }
    gate quality_gate {
        source    = "rt-aggregator"
        threshold = 85
        on_fail   = fail
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.body.len(), 2);
    match &def.body[1] {
        WorkflowNode::Gate(g) => {
            assert_eq!(g.gate_type, GateType::QualityGate);
            assert_eq!(g.name, "quality_gate");
            let qg = g
                .quality_gate
                .as_ref()
                .expect("quality_gate config should be present");
            assert_eq!(qg.source, "rt-aggregator");
            assert_eq!(qg.threshold, 85);
            assert_eq!(qg.on_fail_action, crate::workflow_dsl::OnFailAction::Fail);
        }
        _ => panic!("Expected Gate node"),
    }
}

#[test]
fn test_parse_quality_gate_continue() {
    let input = r#"
workflow review {
    meta { targets = ["worktree"] }
    call aggregator
    gate quality_gate {
        source    = "aggregator"
        threshold = 70
        on_fail   = continue
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[1] {
        WorkflowNode::Gate(g) => {
            assert_eq!(g.gate_type, GateType::QualityGate);
            let qg = g
                .quality_gate
                .as_ref()
                .expect("quality_gate config should be present");
            assert_eq!(qg.threshold, 70);
            assert_eq!(
                qg.on_fail_action,
                crate::workflow_dsl::OnFailAction::Continue
            );
        }
        _ => panic!("Expected Gate node"),
    }
}

#[test]
fn test_parse_quality_gate_missing_source() {
    let input = r#"
workflow review {
    meta { targets = ["worktree"] }
    gate quality_gate {
        threshold = 85
    }
}
"#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("source"), "error should mention source: {msg}");
}

#[test]
fn test_parse_quality_gate_missing_threshold() {
    let input = r#"
workflow review {
    meta { targets = ["worktree"] }
    gate quality_gate {
        source = "aggregator"
    }
}
"#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("threshold"),
        "error should mention threshold: {msg}"
    );
}

#[test]
fn test_parse_quality_gate_threshold_out_of_range() {
    let input = r#"
workflow review {
    meta { targets = ["worktree"] }
    gate quality_gate {
        source    = "aggregator"
        threshold = 150
    }
}
"#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("0-100"), "error should mention range: {msg}");
}

#[test]
fn test_parse_quality_gate_invalid_on_fail() {
    let input = r#"
workflow review {
    meta { targets = ["worktree"] }
    gate quality_gate {
        source    = "aggregator"
        threshold = 85
        on_fail   = garbage
    }
}
"#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("Invalid on_fail"),
        "error should mention invalid on_fail: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Multi-select gate options — regression tests for dotted step-field refs
// ---------------------------------------------------------------------------

#[test]
fn test_parse_gate_options_static_array() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    gate human_approval {
        timeout = "1h"
        options = ["approve", "request changes", "defer"]
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Gate(g) => match g.options.as_ref().unwrap() {
            GateOptions::Static(items) => {
                assert_eq!(items, &["approve", "request changes", "defer"]);
            }
            other => panic!("Expected Static options, got {other:?}"),
        },
        other => panic!("Expected Gate node, got {other:?}"),
    }
}

#[test]
fn test_parse_gate_options_step_field_ref() {
    // Regression test: `options = step.field` must parse as GateOptions::StepRef,
    // not fail — this was broken before the dotted-ref fix in parser.rs:158.
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call generate-choices { output = "choices" }
    gate human_approval {
        timeout = "1h"
        options = choices.items
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[1] {
        WorkflowNode::Gate(g) => match g.options.as_ref().unwrap() {
            GateOptions::StepRef(s) => {
                assert_eq!(s, "choices.items");
            }
            other => panic!("Expected StepRef options, got {other:?}"),
        },
        other => panic!("Expected Gate node, got {other:?}"),
    }
}

#[test]
fn test_parse_gate_options_on_wrong_gate_type_rejected() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    gate pr_approval {
        timeout = "1h"
        options = ["a", "b"]
    }
}
"#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("only valid on human_approval"),
        "error should mention gate type restriction: {msg}"
    );
}

#[test]
fn test_parse_gate_options_bare_value_without_dot_rejected() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    gate human_approval {
        timeout = "1h"
        options = choices
    }
}
"#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("step field reference"),
        "error should mention step field reference: {msg}"
    );
}
