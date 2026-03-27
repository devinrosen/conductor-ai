use super::*;
use crate::workflow_dsl::{
    AgentRef, ApprovalMode, CallNode, DoWhileNode, GateNode, GateType, IfNode, OnMaxIter,
    ParallelNode, WhileNode, WorkflowNode,
};
use std::collections::HashSet;

#[test]
fn test_do_while_body_runs_once_when_condition_absent() {
    // The defining semantic: body executes before condition check,
    // so even with no marker set the body runs once.
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);

    let node = DoWhileNode {
        step: "check".into(),
        marker: "needs_work".into(),
        max_iterations: 3,
        stuck_after: None,
        on_max_iter: OnMaxIter::Fail,
        body: vec![], // empty body — still runs the loop once
    };

    // No step_results set → marker absent → loop exits after 1 iteration
    let result = execute_do_while(&mut state, &node);
    assert!(result.is_ok());
    assert!(state.all_succeeded);
}

#[test]
fn test_do_while_max_iterations_fail() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);

    // Pre-set a marker that stays true forever (body is empty so nothing clears it)
    state.step_results.insert(
        "check".into(),
        make_step_result("check", vec!["needs_work"]),
    );

    let node = DoWhileNode {
        step: "check".into(),
        marker: "needs_work".into(),
        max_iterations: 2,
        stuck_after: None,
        on_max_iter: OnMaxIter::Fail,
        body: vec![],
    };

    let result = execute_do_while(&mut state, &node);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("max_iterations"));
    assert!(!state.all_succeeded);
}

#[test]
fn test_do_while_max_iterations_continue() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);

    state.step_results.insert(
        "check".into(),
        make_step_result("check", vec!["needs_work"]),
    );

    let node = DoWhileNode {
        step: "check".into(),
        marker: "needs_work".into(),
        max_iterations: 2,
        stuck_after: None,
        on_max_iter: OnMaxIter::Continue,
        body: vec![],
    };

    let result = execute_do_while(&mut state, &node);
    assert!(result.is_ok());
    assert!(state.all_succeeded);
}

#[test]
fn test_do_while_stuck_detection() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);

    // Marker stays the same every iteration → stuck after 2
    state.step_results.insert(
        "check".into(),
        make_step_result("check", vec!["needs_work"]),
    );

    let node = DoWhileNode {
        step: "check".into(),
        marker: "needs_work".into(),
        max_iterations: 10,
        stuck_after: Some(2),
        on_max_iter: OnMaxIter::Fail,
        body: vec![],
    };

    let result = execute_do_while(&mut state, &node);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("stuck"));
    assert!(!state.all_succeeded);
}

#[test]
fn test_do_while_iterates_body_multiple_times() {
    // Verify the body actually executes on each iteration by tracking
    // state.position, which Gate nodes increment in dry_run mode.
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);
    state.exec_config.dry_run = true;

    // Marker present → loop keeps iterating until max_iterations
    state.step_results.insert(
        "check".into(),
        make_step_result("check", vec!["needs_work"]),
    );

    let initial_position = state.position;

    let node = DoWhileNode {
        step: "check".into(),
        marker: "needs_work".into(),
        max_iterations: 3,
        stuck_after: None,
        on_max_iter: OnMaxIter::Continue,
        body: vec![WorkflowNode::Gate(GateNode {
            name: "counter".into(),
            gate_type: GateType::HumanApproval,
            prompt: None,
            min_approvals: 1,
            approval_mode: ApprovalMode::default(),
            timeout_secs: 1,
            on_timeout: OnTimeout::Fail,
            bot_name: None,
            quality_gate: None,
        })],
    };

    let result = execute_do_while(&mut state, &node);
    assert!(result.is_ok());
    // Gate node increments position once per iteration; 3 iterations expected
    assert_eq!(state.position - initial_position, 3);
}

// NOTE: Testing the natural-exit path (marker transitions from true→false
// mid-loop) is not feasible in a unit test because no WorkflowNode type
// modifies step_results without running a real agent. The `!has_marker → break`
// branch after body execution IS covered when the marker is absent from the
// start (see test_do_while_body_runs_once_when_condition_absent). The
// transition case (marker present → body clears marker → loop exits) requires
// integration testing with actual agent execution.

#[test]
fn test_do_while_fail_fast_exits_early() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);
    state.exec_config.fail_fast = true;

    // Marker is set so the loop would keep iterating if not for fail_fast
    state.step_results.insert(
        "check".into(),
        make_step_result("check", vec!["needs_work"]),
    );

    // Simulate a prior failure — all_succeeded is already false
    state.all_succeeded = false;

    // Body has a no-op If node (condition never true → body skipped, returns Ok)
    let node = DoWhileNode {
        step: "check".into(),
        marker: "needs_work".into(),
        max_iterations: 10,
        stuck_after: None,
        on_max_iter: OnMaxIter::Fail,
        body: vec![WorkflowNode::If(IfNode {
            condition: crate::workflow_dsl::Condition::StepMarker {
                step: "nonexistent".into(),
                marker: "nope".into(),
            },
            body: vec![],
        })],
    };

    // fail_fast should cause early exit with Ok(()) instead of looping to max_iterations
    let result = execute_do_while(&mut state, &node);
    assert!(result.is_ok());
    assert!(!state.all_succeeded);
}

#[test]
fn test_while_fail_fast_exits_early() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);
    state.exec_config.fail_fast = true;

    // Marker is set so the loop would keep iterating if not for fail_fast
    state.step_results.insert(
        "check".into(),
        make_step_result("check", vec!["needs_work"]),
    );

    // Simulate a prior failure — all_succeeded is already false
    state.all_succeeded = false;

    // Body has a no-op If node (condition never true → body skipped, returns Ok)
    let node = WhileNode {
        step: "check".into(),
        marker: "needs_work".into(),
        max_iterations: 10,
        stuck_after: None,
        on_max_iter: OnMaxIter::Fail,
        body: vec![WorkflowNode::If(IfNode {
            condition: crate::workflow_dsl::Condition::StepMarker {
                step: "nonexistent".into(),
                marker: "nope".into(),
            },
            body: vec![],
        })],
    };

    // fail_fast should cause early exit with Ok(()) instead of looping to max_iterations
    let result = execute_while(&mut state, &node);
    assert!(result.is_ok());
    assert!(!state.all_succeeded);
}

#[test]
fn test_do_while_child_results_refreshed_between_iterations() {
    let conn = setup_db();
    let config = make_resume_config();
    let (mut state, _run_id) = make_state_with_run(&conn, config);

    // Simulate iteration 1: child produced has_blocking_findings
    state.step_results.insert(
        "review-aggregator".to_string(),
        StepResult {
            step_name: "review-aggregator".to_string(),
            status: WorkflowStepStatus::Completed,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            markers: vec!["has_blocking_findings".to_string()],
            context: "iteration 1 had blocking findings".to_string(),
            child_run_id: None,
            structured_output: None,
            output_file: None,
        },
    );

    // Simulate iteration 2: child completed cleanly — no markers
    let (child_wf_mgr, child_run_id) = create_child_run(&conn);
    let step1 = child_wf_mgr
        .insert_step(&child_run_id, "review-aggregator", "reviewer", false, 0, 0)
        .unwrap();
    child_wf_mgr
        .update_step_status(
            &step1,
            WorkflowStepStatus::Completed,
            None,
            Some("all clear"),
            Some("no blocking findings"),
            Some(r#"[]"#),
            None,
        )
        .unwrap();

    let child_steps = bubble_up_child_step_results(&child_wf_mgr, &child_run_id);
    for (key, value) in child_steps {
        state.step_results.insert(key, value);
    }

    // Stale marker must be gone — do-while condition would evaluate false and exit
    let result = state.step_results.get("review-aggregator").unwrap();
    assert!(
        !result
            .markers
            .contains(&"has_blocking_findings".to_string()),
        "stale has_blocking_findings marker must be cleared after iteration 2"
    );
}

#[test]
fn test_find_max_completed_while_iteration_none_completed() {
    let conn = setup_db();
    let state = make_test_state(&conn);

    let node = WhileNode {
        step: "check".to_string(),
        marker: "needs_work".to_string(),
        max_iterations: 5,
        stuck_after: None,
        on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
        body: vec![WorkflowNode::Call(CallNode {
            agent: crate::workflow_dsl::AgentRef::Name("step-a".to_string()),
            retries: 0,
            on_fail: None,
            output: None,
            with: vec![],
            bot_name: None,
            plugin_dirs: vec![],
        })],
    };

    // No resume context → returns 0
    assert_eq!(find_max_completed_while_iteration(&state, &node), 0);
}

#[test]
fn test_find_max_completed_while_iteration_two_completed() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    let skip: HashSet<StepKey> = [("step-a".to_string(), 0), ("step-a".to_string(), 1)]
        .into_iter()
        .collect();
    state.resume_ctx = Some(ResumeContext {
        skip_completed: skip,
        step_map: HashMap::new(),
        child_runs: HashMap::new(),
    });

    let node = WhileNode {
        step: "check".to_string(),
        marker: "needs_work".to_string(),
        max_iterations: 5,
        stuck_after: None,
        on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
        body: vec![WorkflowNode::Call(CallNode {
            agent: crate::workflow_dsl::AgentRef::Name("step-a".to_string()),
            retries: 0,
            on_fail: None,
            output: None,
            with: vec![],
            bot_name: None,
            plugin_dirs: vec![],
        })],
    };

    // Iterations 0 and 1 completed → start from 2
    assert_eq!(find_max_completed_while_iteration(&state, &node), 2);
}

#[test]
fn test_find_max_completed_while_iteration_empty_body() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    state.resume_ctx = Some(ResumeContext {
        skip_completed: HashSet::new(),
        step_map: HashMap::new(),
        child_runs: HashMap::new(),
    });

    let node = WhileNode {
        step: "check".to_string(),
        marker: "needs_work".to_string(),
        max_iterations: 5,
        stuck_after: None,
        on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
        body: vec![], // no call nodes
    };

    // Empty body → returns 0
    assert_eq!(find_max_completed_while_iteration(&state, &node), 0);
}

#[test]
fn test_find_max_completed_while_iteration_partial_body() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    // Two body nodes, but only one completed for iteration 0
    let skip: HashSet<StepKey> = [("step-a".to_string(), 0)].into_iter().collect();
    state.resume_ctx = Some(ResumeContext {
        skip_completed: skip,
        step_map: HashMap::new(),
        child_runs: HashMap::new(),
    });
    // step-b:0 is NOT in skip_completed

    let node = WhileNode {
        step: "check".to_string(),
        marker: "needs_work".to_string(),
        max_iterations: 5,
        stuck_after: None,
        on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
        body: vec![
            WorkflowNode::Call(CallNode {
                agent: crate::workflow_dsl::AgentRef::Name("step-a".to_string()),
                retries: 0,
                on_fail: None,
                output: None,
                with: vec![],
                bot_name: None,
                plugin_dirs: vec![],
            }),
            WorkflowNode::Call(CallNode {
                agent: crate::workflow_dsl::AgentRef::Name("step-b".to_string()),
                retries: 0,
                on_fail: None,
                output: None,
                with: vec![],
                bot_name: None,
                plugin_dirs: vec![],
            }),
        ],
    };

    // Only partial completion → start from 0
    assert_eq!(find_max_completed_while_iteration(&state, &node), 0);
}

#[test]
fn test_find_max_completed_while_iteration_with_parallel_and_gate() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    let skip: HashSet<StepKey> = [
        ("agent-a".to_string(), 0),
        ("agent-b".to_string(), 0),
        ("approval".to_string(), 0),
    ]
    .into_iter()
    .collect();
    state.resume_ctx = Some(ResumeContext {
        skip_completed: skip,
        step_map: HashMap::new(),
        child_runs: HashMap::new(),
    });

    let node = WhileNode {
        step: "check".to_string(),
        marker: "needs_work".to_string(),
        max_iterations: 5,
        stuck_after: None,
        on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
        body: vec![
            WorkflowNode::Parallel(ParallelNode {
                fail_fast: true,
                min_success: None,
                calls: vec![
                    crate::workflow_dsl::AgentRef::Name("agent-a".to_string()),
                    crate::workflow_dsl::AgentRef::Name("agent-b".to_string()),
                ],
                output: None,
                call_outputs: HashMap::new(),
                with: vec![],
                call_with: HashMap::new(),
                call_if: HashMap::new(),
            }),
            WorkflowNode::Gate(GateNode {
                name: "approval".to_string(),
                gate_type: crate::workflow_dsl::GateType::HumanApproval,
                prompt: None,
                min_approvals: 1,
                approval_mode: ApprovalMode::default(),
                timeout_secs: 300,
                on_timeout: crate::workflow_dsl::OnTimeout::Fail,
                bot_name: None,
                quality_gate: None,
            }),
        ],
    };

    // Iteration 0 fully completed → start from 1
    assert_eq!(find_max_completed_while_iteration(&state, &node), 1);
}
