use crate::workflow_dsl::*;

const FULL_WORKFLOW: &str = r#"
workflow ticket-to-pr {
  meta {
    description = "Full development cycle"
    trigger     = "manual"
    targets     = ["worktree"]
  }

  inputs {
    ticket_id  required
    skip_tests default = "false"
  }

  call plan

  call implement {
    retries = 2
    on_fail = diagnose
  }

  call push_and_pr
  call review

  while review.has_review_issues {
    max_iterations = 10
    stuck_after    = 3
    on_max_iter    = fail

    call address_reviews
    call push
    call review
  }

  parallel {
    fail_fast   = false
    min_success = 1
    call reviewer_security
    call reviewer_tests
    call reviewer_style
  }

  gate human_review {
    prompt     = "Review agent findings before merging. Add notes if needed."
    timeout    = "48h"
    on_timeout = fail
  }

  gate pr_checks {
    timeout    = "2h"
    on_timeout = fail
  }

  if review.has_critical_issues {
    call escalate
  }

  unless review.has_critical_issues {
    call fast-path
  }

  always {
    call notify_result
  }
}
"#;

#[test]
fn test_parse_full_workflow() {
    let def = parse_workflow_str(FULL_WORKFLOW, "test.wf").unwrap();
    assert_eq!(def.name, "ticket-to-pr");
    assert_eq!(def.description, "Full development cycle");
    assert_eq!(def.trigger, WorkflowTrigger::Manual);

    // Inputs
    assert_eq!(def.inputs.len(), 2);
    assert_eq!(def.inputs[0].name, "ticket_id");
    assert!(def.inputs[0].required);
    assert_eq!(def.inputs[1].name, "skip_tests");
    assert!(!def.inputs[1].required);
    assert_eq!(def.inputs[1].default.as_deref(), Some("false"));

    // Body nodes: call plan, call implement, call push_and_pr, call review,
    //             while, parallel, gate human_review, gate pr_checks, if, unless
    assert_eq!(def.body.len(), 10);

    // call plan
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.agent, AgentRef::Name("plan".to_string()));
            assert_eq!(c.retries, 0);
            assert!(c.on_fail.is_none());
        }
        _ => panic!("Expected Call node"),
    }

    // call implement with retries
    match &def.body[1] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.agent, AgentRef::Name("implement".to_string()));
            assert_eq!(c.retries, 2);
            assert_eq!(
                c.on_fail,
                Some(OnFail::Agent(AgentRef::Name("diagnose".to_string())))
            );
        }
        _ => panic!("Expected Call node"),
    }

    // while loop
    match &def.body[4] {
        WorkflowNode::While(w) => {
            assert_eq!(w.step, "review");
            assert_eq!(w.marker, "has_review_issues");
            assert_eq!(w.max_iterations, 10);
            assert_eq!(w.stuck_after, Some(3));
            assert_eq!(w.on_max_iter, OnMaxIter::Fail);
            assert_eq!(w.body.len(), 3);
        }
        _ => panic!("Expected While node"),
    }

    // parallel
    match &def.body[5] {
        WorkflowNode::Parallel(p) => {
            assert!(!p.fail_fast);
            assert_eq!(p.min_success, Some(1));
            assert_eq!(
                p.calls,
                vec![
                    AgentRef::Name("reviewer_security".to_string()),
                    AgentRef::Name("reviewer_tests".to_string()),
                    AgentRef::Name("reviewer_style".to_string()),
                ]
            );
        }
        _ => panic!("Expected Parallel node"),
    }

    // gate human_review
    match &def.body[6] {
        WorkflowNode::Gate(g) => {
            assert_eq!(g.gate_type, GateType::HumanReview);
            assert!(g.prompt.as_ref().unwrap().contains("Review agent findings"));
            assert_eq!(g.timeout_secs, 48 * 3600);
            assert_eq!(g.on_timeout, OnTimeout::Fail);
        }
        _ => panic!("Expected Gate node"),
    }

    // gate pr_checks
    match &def.body[7] {
        WorkflowNode::Gate(g) => {
            assert_eq!(g.gate_type, GateType::PrChecks);
            assert_eq!(g.timeout_secs, 2 * 3600);
        }
        _ => panic!("Expected Gate node"),
    }

    // if block
    match &def.body[8] {
        WorkflowNode::If(i) => {
            assert!(
                matches!(&i.condition, Condition::StepMarker { step, marker } if step == "review" && marker == "has_critical_issues")
            );
            assert_eq!(i.body.len(), 1);
        }
        _ => panic!("Expected If node"),
    }

    // unless block
    match &def.body[9] {
        WorkflowNode::Unless(u) => {
            assert!(
                matches!(&u.condition, Condition::StepMarker { step, marker } if step == "review" && marker == "has_critical_issues")
            );
            assert_eq!(u.body.len(), 1);
        }
        _ => panic!("Expected Unless node"),
    }

    // always
    assert_eq!(def.always.len(), 1);
    match &def.always[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.agent, AgentRef::Name("notify_result".to_string()))
        }
        _ => panic!("Expected Call node in always"),
    }
}

#[test]
fn test_parse_minimal_workflow() {
    let input = "workflow simple { meta { targets = [\"worktree\"] } call build }";
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.name, "simple");
    assert_eq!(def.body.len(), 1);
    assert!(def.always.is_empty());
    assert!(def.inputs.is_empty());
}

#[test]
fn test_parse_duration() {
    assert_eq!(parse_duration_str("2h").unwrap(), 7200);
    assert_eq!(parse_duration_str("48h").unwrap(), 172800);
    assert_eq!(parse_duration_str("30m").unwrap(), 1800);
    assert_eq!(parse_duration_str("60s").unwrap(), 60);
    assert_eq!(parse_duration_str("3600").unwrap(), 3600);
}

#[test]
fn test_comments_ignored() {
    let input = r#"
        // This is a comment
        workflow test {
            meta { targets = ["worktree"] }
            // Another comment
            call build // inline comment
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.name, "test");
    assert_eq!(def.body.len(), 1);
}

#[test]
fn test_collect_agent_names() {
    let def = parse_workflow_str(FULL_WORKFLOW, "test.wf").unwrap();
    let mut refs = collect_agent_names(&def.body);
    refs.extend(collect_agent_names(&def.always));
    assert!(refs.contains(&AgentRef::Name("plan".to_string())));
    assert!(refs.contains(&AgentRef::Name("implement".to_string())));
    assert!(refs.contains(&AgentRef::Name("diagnose".to_string()))); // on_fail
    assert!(refs.contains(&AgentRef::Name("reviewer_security".to_string())));
    assert!(refs.contains(&AgentRef::Name("notify_result".to_string())));
}

#[test]
fn test_count_nodes() {
    let def = parse_workflow_str(FULL_WORKFLOW, "test.wf").unwrap();
    let body_count = count_nodes(&def.body);
    // 10 top-level + 3 in while + 3 in parallel + 1 in if + 1 in unless = 18
    assert_eq!(body_count, 18);
    // total_nodes covers body + always
    assert_eq!(def.total_nodes(), body_count + count_nodes(&def.always));
}

#[test]
fn test_serialization_roundtrip() {
    let def = parse_workflow_str(FULL_WORKFLOW, "test.wf").unwrap();
    let json = serde_json::to_string(&def).unwrap();
    let restored: WorkflowDef = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.name, def.name);
    assert_eq!(restored.body.len(), def.body.len());
}

#[test]
fn test_parse_boolean_input_declaration() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    inputs {
        dry_run boolean
        label
    }
    call step
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.inputs.len(), 2);
    let dry_run = def.inputs.iter().find(|i| i.name == "dry_run").unwrap();
    assert_eq!(dry_run.input_type, InputType::Boolean);
    let label = def.inputs.iter().find(|i| i.name == "label").unwrap();
    assert_eq!(label.input_type, InputType::String);
}

#[test]
fn test_input_with_description_remains_required() {
    // Regression: a description modifier must not silently change required→optional.
    let src = r#"
workflow w {
    meta { trigger = "manual" targets = ["worktree"] }
    inputs {
        bare_required
        explicit_required required
        with_description description = "some help text"
        with_desc_and_required required description = "help"
        with_default default = "x"
    }
    call agent
}
"#;
    let def = parse_workflow_str(src, "test.wf").unwrap();
    assert_eq!(def.inputs.len(), 5);

    // bare identifier → required
    assert_eq!(def.inputs[0].name, "bare_required");
    assert!(def.inputs[0].required, "bare input should be required");
    assert!(def.inputs[0].default.is_none());
    assert!(def.inputs[0].description.is_none());

    // explicit `required` keyword
    assert_eq!(def.inputs[1].name, "explicit_required");
    assert!(def.inputs[1].required);

    // description alone must NOT make the input optional
    assert_eq!(def.inputs[2].name, "with_description");
    assert!(
        def.inputs[2].required,
        "input with only a description must still be required"
    );
    assert_eq!(def.inputs[2].description.as_deref(), Some("some help text"));
    assert!(def.inputs[2].default.is_none());

    // explicit required + description
    assert_eq!(def.inputs[3].name, "with_desc_and_required");
    assert!(def.inputs[3].required);
    assert_eq!(def.inputs[3].description.as_deref(), Some("help"));

    // default makes it optional
    assert_eq!(def.inputs[4].name, "with_default");
    assert!(!def.inputs[4].required);
    assert_eq!(def.inputs[4].default.as_deref(), Some("x"));
}

#[test]
fn test_gate_type_from_str_error_path() {
    use std::str::FromStr;
    let result = GateType::from_str("unknown_gate");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("unknown gate type"), "unexpected error: {err}");
}

#[test]
fn test_parse_ticket_to_pr_wf() {
    let input = r#"
workflow ticket-to-pr {
  meta {
    description = "Full development cycle — plan from ticket, implement, push PR, run review swarm, iterate until clean"
    trigger     = "manual"
    targets     = ["worktree"]
  }

  inputs {
    ticket_id required
  }

  call plan { output = "task-plan" }

  call implement {
    retries = 2
  }

  call push-and-pr

  parallel {
    output    = "review-findings"
    with      = ["review-diff-scope"]
    fail_fast = false
    call review-architecture
    call review-security
    call review-performance
    call review-dry-abstraction
    call review-error-handling
    call review-test-coverage
    call review-db-migrations
  }

  call review-aggregator { output = "review-aggregator" }

  while review-aggregator.has_review_issues {
    max_iterations = 3
    stuck_after    = 2
    on_max_iter    = fail

    call address-reviews

    parallel {
      output    = "review-findings"
      with      = ["review-diff-scope"]
      fail_fast = false
      call review-architecture
      call review-security
      call review-performance
      call review-dry-abstraction
      call review-error-handling
      call review-test-coverage
      call review-db-migrations
    }

    call review-aggregator { output = "review-aggregator" }
  }
}
"#;
    let def = parse_workflow_str(input, "ticket-to-pr.wf").unwrap();
    assert_eq!(def.name, "ticket-to-pr");
    assert_eq!(def.trigger, WorkflowTrigger::Manual);
    assert_eq!(def.inputs.len(), 1);
    assert!(def.inputs[0].required);
    // call plan, call implement, call push-and-pr, parallel, call review-aggregator, while
    assert_eq!(def.body.len(), 6);

    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.agent, AgentRef::Name("plan".to_string()));
            assert_eq!(c.output.as_deref(), Some("task-plan"));
        }
        _ => panic!("Expected Call node for plan"),
    }

    match &def.body[1] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.agent, AgentRef::Name("implement".to_string()));
            assert_eq!(c.retries, 2);
        }
        _ => panic!("Expected Call node"),
    }

    match &def.body[3] {
        WorkflowNode::Parallel(p) => {
            assert_eq!(p.calls.len(), 7);
            assert!(!p.fail_fast);
            assert_eq!(p.with, vec!["review-diff-scope".to_string()]);
            assert_eq!(p.output.as_deref(), Some("review-findings"));
        }
        _ => panic!("Expected Parallel node"),
    }

    match &def.body[4] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.agent, AgentRef::Name("review-aggregator".to_string()));
            assert_eq!(c.output.as_deref(), Some("review-aggregator"));
        }
        _ => panic!("Expected Call node for review-aggregator"),
    }

    match &def.body[5] {
        WorkflowNode::While(w) => {
            assert_eq!(w.step, "review-aggregator");
            assert_eq!(w.marker, "has_review_issues");
            assert_eq!(w.max_iterations, 3);
            assert_eq!(w.stuck_after, Some(2));
            // address-reviews, parallel, review-aggregator
            assert_eq!(w.body.len(), 3);
        }
        _ => panic!("Expected While node"),
    }
}

#[test]
fn test_parse_test_coverage_wf() {
    let input = r#"
workflow test-coverage {
  meta {
    description = "Validate PR has sufficient tests; write and commit missing ones"
    trigger     = "manual"
    targets     = ["worktree"]
  }

  call analyze-coverage

  if analyze-coverage.has_missing_tests {
    call write-tests
  }
}
"#;
    let def = parse_workflow_str(input, "test-coverage.wf").unwrap();
    assert_eq!(def.name, "test-coverage");
    assert_eq!(def.body.len(), 2);

    match &def.body[1] {
        WorkflowNode::If(i) => {
            assert!(
                matches!(&i.condition, Condition::StepMarker { step, marker } if step == "analyze-coverage" && marker == "has_missing_tests")
            );
            assert_eq!(i.body.len(), 1);
        }
        _ => panic!("Expected If node"),
    }
}

#[test]
fn test_parse_lint_fix_wf() {
    let input = r#"
workflow lint-fix {
  meta {
    description = "Analyze lint errors and apply fixes"
    trigger     = "manual"
    targets     = ["worktree"]
  }

  script analyze-lint {
    run = ".conductor/scripts/analyze-lint.sh"
  }

  if analyze-lint.has_lint_errors {
    call lint-fix-impl
  }
}
"#;
    let def = parse_workflow_str(input, "lint-fix.wf").unwrap();
    assert_eq!(def.name, "lint-fix");
    assert_eq!(def.body.len(), 2);

    match &def.body[0] {
        WorkflowNode::Script(s) => {
            assert_eq!(s.name, "analyze-lint");
            assert_eq!(s.run, ".conductor/scripts/analyze-lint.sh");
        }
        _ => panic!("Expected Script node"),
    }
}

#[test]
fn test_parse_workflow_without_targets_defaults_to_empty() {
    let input = r#"
workflow no-targets {
  meta {
    description = "Workflow without targets block"
    trigger     = "manual"
  }

  call do-something
}
"#;
    let def = parse_workflow_str(input, "no-targets.wf").unwrap();
    assert_eq!(def.name, "no-targets");
    assert!(
        def.targets.is_empty(),
        "expected targets to be empty when omitted from meta, got {:?}",
        def.targets
    );
}

// ---------------------------------------------------------------------------
// title field — parser and display_name()
// ---------------------------------------------------------------------------

#[test]
fn test_parse_meta_title() {
    let input = r#"
workflow my-wf {
  meta {
    description = "A workflow"
    title = "My Workflow"
    trigger = "manual"
    targets = ["worktree"]
  }
  call step
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.title.as_deref(), Some("My Workflow"));
}

#[test]
fn test_parse_meta_no_title_defaults_to_none() {
    let input = r#"
workflow my-wf {
  meta {
    description = "A workflow"
    trigger = "manual"
    targets = ["worktree"]
  }
  call step
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert!(def.title.is_none());
}

#[test]
fn test_display_name_with_title() {
    let input = r#"
workflow my-wf {
  meta {
    description = "A workflow"
    title = "Pretty Name"
    targets = ["worktree"]
  }
  call step
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.display_name(), "Pretty Name");
}

#[test]
fn test_display_name_falls_back_to_name() {
    let input = r#"
workflow my-wf {
  meta {
    description = "A workflow"
    targets = ["worktree"]
  }
  call step
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.display_name(), "my-wf");
}
