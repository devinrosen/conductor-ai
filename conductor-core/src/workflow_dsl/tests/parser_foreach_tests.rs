use crate::workflow_dsl::*;

// ---------------------------------------------------------------------------
// Happy-path foreach parsing
// ---------------------------------------------------------------------------

#[test]
fn test_foreach_over_tickets() {
    let input = r#"
        workflow test {
            foreach sprint-work {
                over         = tickets
                scope        = { ticket_id = "42" }
                ordered      = true
                max_parallel = 3
                workflow     = "ticket-to-pr"
                inputs       = { ticket_id = "{{item.id}}" }
                on_child_fail = skip_dependents
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => {
            assert_eq!(n.name, "sprint-work");
            assert_eq!(n.over, ForeachOver::Tickets);
            assert!(n.ordered);
            assert_eq!(n.max_parallel, 3);
            assert_eq!(n.workflow, "ticket-to-pr");
            assert_eq!(
                n.inputs.get("ticket_id").map(|s| s.as_str()),
                Some("{{item.id}}")
            );
            assert_eq!(n.on_child_fail, OnChildFail::SkipDependents);
            match &n.scope {
                Some(ForeachScope::Ticket(TicketScope::TicketId(id))) => assert_eq!(id, "42"),
                other => panic!("Expected TicketId scope, got {other:?}"),
            }
        }
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_over_tickets_label_scope() {
    let input = r#"
        workflow test {
            foreach sprint {
                over         = tickets
                scope        = { label = "sprint-42" }
                max_parallel = 5
                workflow     = "ticket-to-pr"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => {
            assert_eq!(n.over, ForeachOver::Tickets);
            match &n.scope {
                Some(ForeachScope::Ticket(TicketScope::Label(lbl))) => assert_eq!(lbl, "sprint-42"),
                other => panic!("Expected Label scope, got {other:?}"),
            }
        }
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_over_repos() {
    let input = r#"
        workflow test {
            foreach coverage-check {
                over         = repos
                max_parallel = 2
                workflow     = "assess-coverage"
                inputs       = { repo_slug = "{{item.slug}}" }
                on_child_fail = continue
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => {
            assert_eq!(n.over, ForeachOver::Repos);
            assert_eq!(n.max_parallel, 2);
            assert_eq!(n.on_child_fail, OnChildFail::Continue);
        }
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_over_workflow_runs() {
    let input = r#"
        workflow test {
            foreach failed-runs {
                over         = workflow_runs
                filter       = { status = "failed" }
                max_parallel = 4
                workflow     = "diagnose-and-issue"
                inputs       = { run_id = "{{item.id}}" }
                on_child_fail = continue
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => {
            assert_eq!(n.over, ForeachOver::WorkflowRuns);
            assert_eq!(n.filter.get("status").map(|s| s.as_str()), Some("failed"));
        }
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_on_cycle_warn() {
    let input = r#"
        workflow test {
            foreach sprint {
                over         = tickets
                scope        = { label = "sprint" }
                ordered      = true
                on_cycle     = warn
                max_parallel = 3
                workflow     = "ticket-to-pr"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => {
            assert_eq!(n.on_cycle, OnCycle::Warn);
        }
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_on_child_fail_halt() {
    let input = r#"
        workflow test {
            foreach sprint {
                over          = tickets
                scope         = { label = "sprint" }
                max_parallel  = 3
                workflow      = "ticket-to-pr"
                on_child_fail = halt
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => {
            assert_eq!(n.on_child_fail, OnChildFail::Halt);
        }
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Error cases
// ---------------------------------------------------------------------------

#[test]
fn test_foreach_missing_over() {
    let input = r#"
        workflow test {
            foreach sprint {
                max_parallel = 3
                workflow = "ticket-to-pr"
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("missing required key 'over'"), "got: {err}");
}

#[test]
fn test_foreach_missing_max_parallel() {
    let input = r#"
        workflow test {
            foreach sprint {
                over     = tickets
                scope    = { label = "sprint" }
                workflow = "ticket-to-pr"
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("missing required key 'max_parallel'"),
        "got: {err}"
    );
}

#[test]
fn test_foreach_missing_workflow() {
    let input = r#"
        workflow test {
            foreach sprint {
                over         = tickets
                scope        = { label = "sprint" }
                max_parallel = 3
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("missing required key 'workflow'"),
        "got: {err}"
    );
}

#[test]
fn test_foreach_invalid_over_value() {
    let input = r#"
        workflow test {
            foreach sprint {
                over         = files
                max_parallel = 3
                workflow     = "foo"
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("invalid over value"), "got: {err}");
}

#[test]
fn test_foreach_invalid_on_child_fail() {
    let input = r#"
        workflow test {
            foreach sprint {
                over          = repos
                max_parallel  = 3
                workflow      = "foo"
                on_child_fail = retry
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("invalid on_child_fail"), "got: {err}");
}

#[test]
fn test_foreach_scope_unlabeled_true() {
    let input = r#"
        workflow test {
            foreach unlabeled-work {
                over         = tickets
                scope        = { unlabeled = true }
                max_parallel = 5
                workflow     = "label-ticket"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => {
            assert_eq!(n.over, ForeachOver::Tickets);
            match &n.scope {
                Some(ForeachScope::Ticket(TicketScope::Unlabeled)) => {}
                other => panic!("Expected Unlabeled scope, got {other:?}"),
            }
        }
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_scope_unlabeled_false_is_error() {
    let input = r#"
        workflow test {
            foreach unlabeled-work {
                over         = tickets
                scope        = { unlabeled = false }
                max_parallel = 5
                workflow     = "label-ticket"
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("scope.unlabeled must be true"), "got: {err}");
}

#[test]
fn test_foreach_scope_missing_key() {
    let input = r#"
        workflow test {
            foreach sprint {
                over         = tickets
                scope        = { unknown_key = "x" }
                max_parallel = 3
                workflow     = "foo"
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("scope must contain ticket_id, label, or unlabeled"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Worktrees foreach parsing
// ---------------------------------------------------------------------------

#[test]
fn test_foreach_over_worktrees_basic() {
    let input = r#"
        workflow test {
            foreach release-worktrees {
                over          = worktrees
                scope         = { base_branch = "release/0.5.2" }
                max_parallel  = 3
                workflow      = "ticket-to-pr"
                inputs        = { worktree_slug = "{{item.slug}}" }
                on_child_fail = skip_dependents
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => {
            assert_eq!(n.name, "release-worktrees");
            assert_eq!(n.over, ForeachOver::Worktrees);
            assert!(!n.ordered);
            assert_eq!(n.max_parallel, 3);
            assert_eq!(n.workflow, "ticket-to-pr");
            assert_eq!(n.on_child_fail, OnChildFail::SkipDependents);
            match &n.scope {
                Some(ForeachScope::Worktree(ws)) => {
                    assert_eq!(ws.base_branch, Some("release/0.5.2".to_string()));
                    assert_eq!(ws.has_open_pr, None);
                }
                other => panic!("Expected Worktree scope, got {other:?}"),
            }
        }
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_over_worktrees_ordered() {
    let input = r#"
        workflow test {
            foreach release-wts {
                over         = worktrees
                scope        = { base_branch = "release/1.0" }
                ordered      = true
                on_cycle     = warn
                max_parallel = 2
                workflow     = "ticket-to-pr"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => {
            assert_eq!(n.over, ForeachOver::Worktrees);
            assert!(n.ordered);
            assert_eq!(n.on_cycle, OnCycle::Warn);
            match &n.scope {
                Some(ForeachScope::Worktree(ws)) => {
                    assert_eq!(ws.base_branch, Some("release/1.0".to_string()));
                }
                other => panic!("Expected Worktree scope, got {other:?}"),
            }
        }
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_over_worktrees_scope_only_unknown_keys() {
    // Unknown scope keys are silently ignored; base_branch and has_open_pr both default to None.
    let input = r#"
        workflow test {
            foreach release-wts {
                over         = worktrees
                scope        = { unknown_key = "x" }
                max_parallel = 3
                workflow     = "foo"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => match &n.scope {
            Some(ForeachScope::Worktree(ws)) => {
                assert_eq!(ws.base_branch, None);
                assert_eq!(ws.has_open_pr, None);
            }
            other => panic!("Expected Worktree scope, got {other:?}"),
        },
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_over_worktrees_missing_scope() {
    // Missing scope for worktrees is a parser success (validated at semantic layer).
    let input = r#"
        workflow test {
            foreach release-wts {
                over         = worktrees
                max_parallel = 3
                workflow     = "foo"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => {
            assert_eq!(n.over, ForeachOver::Worktrees);
            assert!(n.scope.is_none());
        }
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_over_worktrees_has_open_pr_false() {
    let input = r#"
        workflow test {
            foreach run-children {
                over         = worktrees
                scope        = { base_branch = "release/1.0" has_open_pr = false }
                max_parallel = 2
                workflow     = "ticket-to-pr"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => match &n.scope {
            Some(ForeachScope::Worktree(ws)) => {
                assert_eq!(ws.base_branch, Some("release/1.0".to_string()));
                assert_eq!(ws.has_open_pr, Some(false));
            }
            other => panic!("Expected Worktree scope, got {other:?}"),
        },
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_over_worktrees_has_open_pr_true() {
    let input = r#"
        workflow test {
            foreach run-children {
                over         = worktrees
                scope        = { base_branch = "main" has_open_pr = true }
                max_parallel = 2
                workflow     = "review"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => match &n.scope {
            Some(ForeachScope::Worktree(ws)) => {
                assert_eq!(ws.has_open_pr, Some(true));
            }
            other => panic!("Expected Worktree scope, got {other:?}"),
        },
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_over_worktrees_has_open_pr_omitted() {
    let input = r#"
        workflow test {
            foreach run-children {
                over         = worktrees
                scope        = { base_branch = "main" }
                max_parallel = 2
                workflow     = "foo"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => match &n.scope {
            Some(ForeachScope::Worktree(ws)) => {
                assert_eq!(ws.has_open_pr, None);
            }
            other => panic!("Expected Worktree scope, got {other:?}"),
        },
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_over_worktrees_scope_without_base_branch() {
    let input = r#"
        workflow test {
            foreach run-children {
                over         = worktrees
                scope        = { has_open_pr = false }
                max_parallel = 2
                workflow     = "ticket-to-pr"
            }
        }
    "#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::ForEach(n) => match &n.scope {
            Some(ForeachScope::Worktree(ws)) => {
                assert_eq!(ws.base_branch, None);
                assert_eq!(ws.has_open_pr, Some(false));
            }
            other => panic!("Expected Worktree scope, got {other:?}"),
        },
        other => panic!("Expected ForEach node, got {other:?}"),
    }
}

#[test]
fn test_foreach_over_worktrees_has_open_pr_invalid_value() {
    let input = r#"
        workflow test {
            foreach run-children {
                over         = worktrees
                scope        = { has_open_pr = maybe }
                max_parallel = 2
                workflow     = "foo"
            }
        }
    "#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("has_open_pr"), "got: {err}");
}
