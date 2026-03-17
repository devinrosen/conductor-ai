use crate::workflow_dsl::*;
use std::fs;

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
            assert_eq!(c.on_fail, Some(AgentRef::Name("diagnose".to_string())));
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
fn test_load_from_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wf_dir = tmp.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    fs::write(
        wf_dir.join("simple.wf"),
        "workflow simple { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();

    let (defs, warnings) =
        load_workflow_defs(tmp.path().to_str().unwrap(), "/nonexistent").unwrap();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "simple");
    assert!(warnings.is_empty());
}

#[test]
fn test_load_partial_failure_returns_successes_and_warnings() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wf_dir = tmp.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    // Valid workflow
    fs::write(
        wf_dir.join("good.wf"),
        "workflow good { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();
    // Invalid workflow (syntax error)
    fs::write(
        wf_dir.join("bad.wf"),
        "this is not valid workflow syntax !!!",
    )
    .unwrap();

    let (defs, warnings) =
        load_workflow_defs(tmp.path().to_str().unwrap(), "/nonexistent").unwrap();
    // The good workflow is returned despite the bad one failing
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "good");
    // One warning for the bad file
    assert_eq!(warnings.len(), 1);
    // Warning carries the filename in the structured `file` field
    assert_eq!(warnings[0].file, "bad.wf");
    assert!(!warnings[0].message.is_empty());
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
fn test_validate_workflow_name_valid() {
    assert!(validate_workflow_name("ticket-to-pr").is_ok());
    assert!(validate_workflow_name("test_coverage").is_ok());
    assert!(validate_workflow_name("simple").is_ok());
    assert!(validate_workflow_name("A-Z_0-9").is_ok());
}

#[test]
fn test_validate_workflow_name_empty() {
    assert!(validate_workflow_name("").is_err());
}

#[test]
fn test_validate_workflow_name_path_traversal() {
    assert!(validate_workflow_name("..").is_err());
    assert!(validate_workflow_name("../etc/passwd").is_err());
    assert!(validate_workflow_name("foo/bar").is_err());
    assert!(validate_workflow_name("foo\\bar").is_err());
}

#[test]
fn test_validate_workflow_name_special_chars() {
    assert!(validate_workflow_name("name with spaces").is_err());
    assert!(validate_workflow_name("name.wf").is_err());
    assert!(validate_workflow_name("name;rm -rf").is_err());
    assert!(validate_workflow_name("name\0null").is_err());
}

#[test]
fn test_load_workflow_by_name() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wf_dir = tmp.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    fs::write(
        wf_dir.join("deploy.wf"),
        "workflow deploy { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();

    let def =
        load_workflow_by_name(tmp.path().to_str().unwrap(), "/nonexistent", "deploy").unwrap();
    assert_eq!(def.name, "deploy");
}

#[test]
fn test_load_workflow_by_name_not_found() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wf_dir = tmp.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    fs::write(
        wf_dir.join("deploy.wf"),
        "workflow deploy { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();

    let result = load_workflow_by_name(tmp.path().to_str().unwrap(), "/nonexistent", "nonexistent");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("not found"));
}

#[test]
fn test_load_workflow_by_name_rejects_invalid() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = load_workflow_by_name(
        tmp.path().to_str().unwrap(),
        "/nonexistent",
        "../etc/passwd",
    );
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Invalid workflow name"));
}

#[test]
fn test_load_workflow_by_name_falls_back_to_repo_path() {
    let repo = tempfile::TempDir::new().unwrap();
    let wf_dir = repo.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    fs::write(
        wf_dir.join("deploy.wf"),
        "workflow deploy { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();

    // worktree has no .conductor/workflows/, should fall back to repo_path
    let worktree = tempfile::TempDir::new().unwrap();
    let def = load_workflow_by_name(
        worktree.path().to_str().unwrap(),
        repo.path().to_str().unwrap(),
        "deploy",
    )
    .unwrap();
    assert_eq!(def.name, "deploy");
}

#[test]
fn test_load_workflow_by_name_no_workflows_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = load_workflow_by_name(
        tmp.path().to_str().unwrap(),
        tmp.path().to_str().unwrap(),
        "deploy",
    );
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("not found"));
}

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
fn test_call_with_single_snippet() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call plan { with = "ticket-context" }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.with, vec!["ticket-context".to_string()]);
        }
        _ => panic!("Expected Call node"),
    }
}

#[test]
fn test_call_with_array_snippets() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call plan { with = ["ticket-context", "rust-conventions"] }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(
                c.with,
                vec!["ticket-context".to_string(), "rust-conventions".to_string()]
            );
        }
        _ => panic!("Expected Call node"),
    }
}

#[test]
fn test_parallel_with_block_level_snippets() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        parallel {
            with      = ["review-diff-scope", "rust-conventions"]
            fail_fast = false
            call review-security
            call review-style
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Parallel(p) => {
            assert_eq!(
                p.with,
                vec![
                    "review-diff-scope".to_string(),
                    "rust-conventions".to_string()
                ]
            );
            assert!(p.call_with.is_empty());
            assert_eq!(p.calls.len(), 2);
        }
        _ => panic!("Expected Parallel node"),
    }
}

#[test]
fn test_parallel_with_per_call_snippets() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        parallel {
            with = ["review-diff-scope"]
            call ".conductor/agents/review-architecture.md"
            call ".conductor/agents/review-db-migrations.md" { with = ["migration-rules"] }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Parallel(p) => {
            assert_eq!(p.with, vec!["review-diff-scope".to_string()]);
            assert!(!p.call_with.contains_key("0"));
            assert_eq!(
                p.call_with.get("1").unwrap(),
                &vec!["migration-rules".to_string()]
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

#[test]
fn test_collect_snippet_refs() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call plan { with = ["context-a"] }
        parallel {
            with = ["scope-b"]
            call agent-1
            call agent-2 { with = ["extra-c"] }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let mut refs = collect_snippet_refs(&def.body);
    refs.sort();
    refs.dedup();
    assert_eq!(
        refs,
        vec![
            "context-a".to_string(),
            "extra-c".to_string(),
            "scope-b".to_string(),
        ]
    );
}

#[test]
fn test_call_with_no_snippets() {
    let input = r#"workflow test { meta { targets = ["worktree"] } call plan }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert!(c.with.is_empty());
        }
        _ => panic!("Expected Call node"),
    }
}

#[test]
fn test_collect_snippet_refs_inside_if() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call plan
        if plan.approved {
            call implement { with = ["if-context"] }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs = collect_snippet_refs(&def.body);
    assert_eq!(refs, vec!["if-context".to_string()]);
}

#[test]
fn test_collect_snippet_refs_inside_unless() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call review
        unless review.approved {
            call fix { with = ["unless-context"] }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs = collect_snippet_refs(&def.body);
    assert_eq!(refs, vec!["unless-context".to_string()]);
}

#[test]
fn test_collect_snippet_refs_inside_while() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call review
        while review.has_issues {
            max_iterations = 3
            call fix { with = ["while-context"] }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs = collect_snippet_refs(&def.body);
    assert_eq!(refs, vec!["while-context".to_string()]);
}

#[test]
fn test_collect_snippet_refs_inside_always() {
    // Top-level `always { }` block is parsed into `def.always`, not `def.body`.
    // collect_all_snippet_refs() covers both; here we test the always slice directly.
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call plan
        always {
            call cleanup { with = ["always-context"] }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs = collect_snippet_refs(&def.always);
    assert_eq!(refs, vec!["always-context".to_string()]);
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

#[test]
fn test_collect_all_snippet_refs_deduplicates_across_body_and_always() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call plan { with = ["shared-context", "body-only"] }
        always {
            call cleanup { with = ["shared-context", "always-only"] }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let refs = def.collect_all_snippet_refs();
    // Should be sorted and deduplicated: "shared-context" appears in both blocks
    assert_eq!(
        refs,
        vec![
            "always-only".to_string(),
            "body-only".to_string(),
            "shared-context".to_string(),
        ]
    );
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

// ---------------------------------------------------------------------------
// collect_schema_refs / collect_bot_names
// ---------------------------------------------------------------------------

#[test]
fn test_collect_schema_refs_empty() {
    assert!(collect_schema_refs(&[]).is_empty());
}

/// Minimal workflow header that satisfies the parser's required fields.
const WF_HEADER: &str = r#"meta { targets = ["worktree"] }"#;

fn make_wf(body: &str) -> String {
    format!("workflow w {{\n  {WF_HEADER}\n{body}\n}}")
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

/// Regression test for #1195: DirEntry errors silently dropped during workflow directory scan.
///
/// The original code used `.filter_map(|e| e.ok())`, which silently discarded DirEntry errors.
/// The fix emits a `tracing::warn!` and skips the bad entry so callers receive all successfully
/// parsed definitions.
///
/// This test exercises the `parse_workflow_file` error path: a `.wf` file with mode 000 causes
/// parsing to fail, which is collected as a `WorkflowWarning`. The sibling test
/// `test_filter_wf_dir_entries_skips_io_errors` directly exercises the DirEntry iterator-error
/// path (api.rs lines 31–39).
#[cfg(unix)]
#[test]
fn test_load_workflow_defs_skips_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::TempDir::new().unwrap();
    let wf_dir = tmp.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();

    // A valid workflow that should always be returned.
    fs::write(
        wf_dir.join("good.wf"),
        "workflow good { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();

    // A `.wf` file made unreadable — simulates a permission-denied scenario.
    let bad_path = wf_dir.join("unreadable.wf");
    fs::write(
        &bad_path,
        "workflow unreadable { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();
    fs::set_permissions(&bad_path, fs::Permissions::from_mode(0o000)).unwrap();

    let result = load_workflow_defs(tmp.path().to_str().unwrap(), "/nonexistent");

    // Restore permissions so TempDir cleanup doesn't fail.
    fs::set_permissions(&bad_path, fs::Permissions::from_mode(0o644)).unwrap();

    let (defs, warnings) = result.unwrap();
    // The readable workflow is returned.
    assert_eq!(defs.len(), 1, "expected exactly one parseable workflow");
    assert_eq!(defs[0].name, "good");
    // The unreadable file produces a warning (file-read error path), not a panic.
    assert_eq!(
        warnings.len(),
        1,
        "expected one warning for the unreadable file"
    );
    assert_eq!(warnings[0].file, "unreadable.wf");
}

/// Directly tests the DirEntry iterator-error path in `filter_wf_dir_entries` (api.rs lines 31–39).
///
/// Feeds synthetic `io::Error` values (which cannot be constructed from real filesystem calls in
/// tests) directly into the helper to confirm they are skipped rather than panicking or returning
/// an `Err`. Valid `.wf` DirEntries read from a temporary directory are passed through correctly.
#[test]
fn test_filter_wf_dir_entries_skips_io_errors() {
    use crate::workflow_dsl::api::filter_wf_dir_entries;
    use std::io;

    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path();

    // Write a real .wf file and a non-.wf file to the temp dir.
    fs::write(dir.join("real.wf"), "content").unwrap();
    fs::write(dir.join("ignored.txt"), "content").unwrap();

    // Collect the real DirEntries first so we can chain them with synthetic errors.
    let real_entries: Vec<io::Result<fs::DirEntry>> = fs::read_dir(dir).unwrap().collect();

    // Prepend two synthetic DirEntry errors — these exercise the Err arm of the filter_map.
    let mixed = vec![
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "synthetic bad entry 1",
        )),
        Err(io::Error::other("synthetic bad entry 2")),
    ]
    .into_iter()
    .chain(real_entries);

    let result = filter_wf_dir_entries(mixed, dir);

    // Only the real .wf file survives; errors and non-.wf files are dropped.
    assert_eq!(
        result.len(),
        1,
        "errors and non-.wf entries must be skipped"
    );
    assert_eq!(
        result[0].path().file_name().unwrap().to_str().unwrap(),
        "real.wf"
    );
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
