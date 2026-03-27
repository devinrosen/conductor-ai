use crate::workflow_dsl::*;

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
