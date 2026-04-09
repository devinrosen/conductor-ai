#![allow(unused_imports)]

use super::*;
use crate::agent::{AgentManager, AgentRunStatus};
use crate::agent_runtime;
use crate::config::Config;
use crate::error::ConductorError;
use crate::workflow_dsl::{
    AgentRef, ApprovalMode, CallNode, CallWorkflowNode, DoNode, DoWhileNode, GateNode, GateType,
    IfNode, OnMaxIter, OnTimeout, ParallelNode, UnlessNode, WhileNode, WorkflowNode,
};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

#[test]
fn test_poll_child_completion_already_completed() {
    let conn = setup_db();
    let mgr = AgentManager::new(&conn);

    let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();
    mgr.update_run_completed(
        &run.id,
        None,
        Some("done"),
        Some(0.05),
        Some(3),
        Some(5000),
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let result = agent_runtime::poll_child_completion(
        &conn,
        &run.id,
        Duration::from_millis(10),
        Duration::from_secs(1),
        None,
        None,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().status, AgentRunStatus::Completed);
}

#[test]
fn test_poll_child_completion_timeout() {
    let conn = setup_db();
    let mgr = AgentManager::new(&conn);

    let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();

    let result = agent_runtime::poll_child_completion(
        &conn,
        &run.id,
        Duration::from_millis(10),
        Duration::from_millis(50),
        None,
        None,
    );
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        agent_runtime::PollError::Timeout(_)
    ));
}

#[test]
fn test_poll_child_completion_shutdown() {
    use std::sync::{atomic::AtomicBool, Arc};

    let conn = setup_db();
    let mgr = AgentManager::new(&conn);

    let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();
    // run stays in Running; flag is already set
    let flag = Arc::new(AtomicBool::new(true));

    let result = agent_runtime::poll_child_completion(
        &conn,
        &run.id,
        Duration::from_millis(10),
        Duration::from_secs(5),
        Some(&flag),
        None,
    );
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        agent_runtime::PollError::Shutdown
    ));
}

#[test]
fn test_recover_stuck_steps_syncs_completed() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let wf_mgr = WorkflowManager::new(&conn);

    // Create a parent agent run and a workflow run
    let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let wf_run = wf_mgr
        .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    // Insert a step stuck in 'running' with a child_run_id
    let step_id = wf_mgr
        .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
        .unwrap();
    let child = agent_mgr
        .create_run(Some("w1"), "child-agent", None, None)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&child.id),
            None,
            None,
            None,
            None,
        )
        .unwrap();

    // Mark child run as completed
    agent_mgr
        .update_run_completed(
            &child.id,
            None,
            Some("great output"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let recovered = wf_mgr.recover_stuck_steps().unwrap();
    assert_eq!(recovered, 1);

    let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Completed);
    assert_eq!(steps[0].result_text.as_deref(), Some("great output"));
}

#[test]
fn test_recover_stuck_steps_skips_still_running() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let wf_mgr = WorkflowManager::new(&conn);

    let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let wf_run = wf_mgr
        .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let step_id = wf_mgr
        .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
        .unwrap();
    let child = agent_mgr
        .create_run(Some("w1"), "child-agent", None, None)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&child.id),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    // child run stays in 'running' — should NOT be recovered

    let recovered = wf_mgr.recover_stuck_steps().unwrap();
    assert_eq!(recovered, 0);

    let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Running);
}

#[test]
fn test_recover_stuck_steps_failed_child_marks_step_failed() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let wf_mgr = WorkflowManager::new(&conn);

    let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
    let wf_run = wf_mgr
        .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let step_id = wf_mgr
        .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
        .unwrap();
    let child = agent_mgr
        .create_run(Some("w1"), "child-agent", None, None)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&child.id),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    agent_mgr
        .update_run_failed(&child.id, "agent crashed")
        .unwrap();

    let recovered = wf_mgr.recover_stuck_steps().unwrap();
    assert_eq!(recovered, 1);

    let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
    assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
    assert_eq!(steps[0].result_text.as_deref(), Some("agent crashed"));
}

#[test]
fn test_fetch_child_final_output_returns_last_completed_step() {
    let conn = setup_db();
    let (mgr, run_id) = create_child_run(&conn);

    // Insert two completed steps; the second (position=1) should be returned
    let step1_id = mgr
        .insert_step(&run_id, "step-a", "actor", false, 0, 0)
        .unwrap();
    mgr.update_step_status(
        &step1_id,
        WorkflowStepStatus::Completed,
        None,
        Some("step-a done"),
        Some("context-a"),
        Some(r#"["marker_a"]"#),
        Some(0),
    )
    .unwrap();

    let step2_id = mgr
        .insert_step(&run_id, "step-b", "actor", false, 1, 0)
        .unwrap();
    mgr.update_step_status(
        &step2_id,
        WorkflowStepStatus::Completed,
        None,
        Some("step-b done"),
        Some("context-b"),
        Some(r#"["marker_b1","marker_b2"]"#),
        Some(0),
    )
    .unwrap();

    let (markers, context) = fetch_child_final_output(&mgr, &run_id);
    assert_eq!(markers, vec!["marker_b1", "marker_b2"]);
    assert_eq!(context, "context-b");
}

#[test]
fn test_fetch_child_final_output_no_completed_steps() {
    let conn = setup_db();
    let (mgr, run_id) = create_child_run(&conn);

    // Insert a failed step only
    let step_id = mgr
        .insert_step(&run_id, "step-a", "actor", false, 0, 0)
        .unwrap();
    mgr.update_step_status(
        &step_id,
        WorkflowStepStatus::Failed,
        None,
        Some("failed"),
        None,
        None,
        Some(0),
    )
    .unwrap();

    let (markers, context) = fetch_child_final_output(&mgr, &run_id);
    assert!(markers.is_empty());
    assert!(context.is_empty());
}

#[test]
fn test_fetch_child_final_output_malformed_markers_json() {
    let conn = setup_db();
    let (mgr, run_id) = create_child_run(&conn);

    let step_id = mgr
        .insert_step(&run_id, "step-a", "actor", false, 0, 0)
        .unwrap();
    mgr.update_step_status(
        &step_id,
        WorkflowStepStatus::Completed,
        None,
        Some("done"),
        Some("some context"),
        Some("not valid json {{{"),
        Some(0),
    )
    .unwrap();

    let (markers, context) = fetch_child_final_output(&mgr, &run_id);
    assert!(markers.is_empty()); // malformed JSON falls back to empty
    assert_eq!(context, "some context");
}

#[test]
fn test_fetch_child_final_output_nonexistent_run() {
    let conn = setup_db();
    let mgr = WorkflowManager::new(&conn);
    let (markers, context) = fetch_child_final_output(&mgr, "nonexistent-run-id");
    assert!(markers.is_empty());
    assert!(context.is_empty());
}

#[test]
fn test_build_variable_map_includes_inputs_and_prior_context() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);
    state
        .inputs
        .insert("branch".to_string(), "main".to_string());
    state.contexts.push(ContextEntry {
        step: "step-a".to_string(),
        iteration: 0,
        context: "previous output".to_string(),
        markers: vec![],
        structured_output: None,
        output_file: None,
    });

    let vars = build_variable_map(&state);
    assert_eq!(vars.get("branch").unwrap(), "main");
    assert_eq!(vars.get("prior_context").unwrap(), "previous output");
    assert!(vars.get("prior_contexts").unwrap().contains("step-a"));
}

#[test]
fn test_parallel_contexts_included_in_prior_contexts() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    // Simulate multiple parallel agents completing and pushing contexts
    // (this is the pattern now used in execute_parallel's success branch)
    state.contexts.push(ContextEntry {
        step: "reviewer-a".to_string(),
        iteration: 0,
        context: "LGTM from reviewer A".to_string(),
        markers: vec![],
        structured_output: None,
        output_file: None,
    });
    state.contexts.push(ContextEntry {
        step: "reviewer-b".to_string(),
        iteration: 0,
        context: "Needs changes from reviewer B".to_string(),
        markers: vec!["has_review_issues".to_string()],
        structured_output: None,
        output_file: None,
    });

    let vars = build_variable_map(&state);

    // prior_context should be the last context pushed
    assert_eq!(
        vars.get("prior_context").unwrap(),
        "Needs changes from reviewer B"
    );

    // prior_contexts should contain both parallel agent entries
    let prior_contexts = vars.get("prior_contexts").unwrap();
    assert!(prior_contexts.contains("reviewer-a"));
    assert!(prior_contexts.contains("reviewer-b"));
    assert!(prior_contexts.contains("LGTM from reviewer A"));
    assert!(prior_contexts.contains("Needs changes from reviewer B"));
}

#[test]
fn test_build_variable_map_includes_gate_feedback() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);
    state.last_gate_feedback = Some("looks good".to_string());

    let vars = build_variable_map(&state);
    assert_eq!(vars.get("gate_feedback").unwrap(), "looks good");
}

#[test]
fn test_build_variable_map_no_gate_feedback() {
    let conn = setup_db();
    let state = make_test_state(&conn);
    let vars = build_variable_map(&state);
    assert!(!vars.contains_key("gate_feedback"));
    // prior_context should be empty string when no contexts
    assert_eq!(vars.get("prior_context").unwrap(), "");
    // prior_output should be absent when no structured output
    assert!(!vars.contains_key("prior_output"));
}

#[test]
fn test_build_variable_map_includes_prior_output() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);
    let json = r#"{"approved":true,"summary":"All clear"}"#.to_string();
    state.contexts.push(crate::workflow::types::ContextEntry {
        step: "test_step".to_string(),
        iteration: 0,
        context: String::new(),
        markers: Vec::new(),
        structured_output: Some(json.clone()),
        output_file: None,
    });

    let vars = build_variable_map(&state);
    assert_eq!(vars.get("prior_output").unwrap(), &json);
}

#[test]
fn test_build_variable_map_includes_dry_run() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    // Default exec_config has dry_run = false
    let vars = build_variable_map(&state);
    assert_eq!(vars.get("dry_run").unwrap(), "false");

    // Set dry_run = true
    state.exec_config.dry_run = true;
    let vars = build_variable_map(&state);
    assert_eq!(vars.get("dry_run").unwrap(), "true");
}

#[test]
fn test_resolve_child_inputs_substitutes_variables() {
    use crate::workflow_dsl::InputDecl;

    let mut raw = HashMap::new();
    raw.insert("msg".to_string(), "Hello {{name}}!".to_string());

    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("name", "World".to_string());

    let decls = vec![InputDecl {
        name: "msg".to_string(),
        required: true,
        default: None,
        description: None,
        input_type: Default::default(),
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert_eq!(result.get("msg").unwrap(), "Hello World!");
}

#[test]
fn test_resolve_child_inputs_applies_defaults() {
    use crate::workflow_dsl::InputDecl;

    let raw = HashMap::new(); // no inputs provided

    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "mode".to_string(),
        required: false,
        default: Some("fast".to_string()),
        description: None,
        input_type: Default::default(),
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert_eq!(result.get("mode").unwrap(), "fast");
}

#[test]
fn test_resolve_child_inputs_missing_required() {
    use crate::workflow_dsl::InputDecl;

    let raw = HashMap::new();
    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "pr_url".to_string(),
        required: true,
        default: None,
        description: None,
        input_type: Default::default(),
    }];

    let err = resolve_child_inputs(&raw, &vars, &decls).unwrap_err();
    assert_eq!(err, "pr_url");
}

#[test]
fn test_resolve_child_inputs_provided_overrides_default() {
    use crate::workflow_dsl::InputDecl;

    let mut raw = HashMap::new();
    raw.insert("mode".to_string(), "slow".to_string());

    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "mode".to_string(),
        required: false,
        default: Some("fast".to_string()),
        description: None,
        input_type: Default::default(),
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert_eq!(result.get("mode").unwrap(), "slow");
}

#[test]
fn test_resolve_child_inputs_optional_without_default_omitted() {
    use crate::workflow_dsl::InputDecl;

    let raw = HashMap::new();
    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "optional_field".to_string(),
        required: false,
        default: None,
        description: None,
        input_type: Default::default(),
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert!(!result.contains_key("optional_field"));
}

#[test]
fn test_resolve_child_inputs_boolean_defaults_to_false() {
    use crate::workflow_dsl::{InputDecl, InputType};

    let raw = HashMap::new(); // boolean input not explicitly passed
    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "flag".to_string(),
        required: false,
        default: None,
        description: None,
        input_type: InputType::Boolean,
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert_eq!(result.get("flag").map(|s| s.as_str()), Some("false"));
}

#[test]
fn test_resolve_child_inputs_boolean_provided_value_not_overwritten() {
    use crate::workflow_dsl::{InputDecl, InputType};

    let mut raw = HashMap::new();
    raw.insert("flag".to_string(), "true".to_string());

    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "flag".to_string(),
        required: false,
        default: None,
        description: None,
        input_type: InputType::Boolean,
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert_eq!(result.get("flag").map(|s| s.as_str()), Some("true"));
}

#[test]
fn test_execute_unless_marker_absent_runs_body() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    // Step "build" exists but does NOT have the "has_errors" marker
    state.step_results.insert(
        "build".to_string(),
        make_step_result("build", vec!["build_ok"]),
    );

    let node = UnlessNode {
        condition: crate::workflow_dsl::Condition::StepMarker {
            step: "build".to_string(),
            marker: "has_errors".to_string(),
        },
        body: vec![], // empty body — just verify it enters the branch without error
    };

    // Should succeed (marker absent → body executes, empty body is fine)
    execute_unless(&mut state, &node).unwrap();
}

#[test]
fn test_execute_unless_marker_present_skips_body() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    // Step "build" has the "has_errors" marker
    state.step_results.insert(
        "build".to_string(),
        make_step_result("build", vec!["has_errors"]),
    );

    let node = UnlessNode {
        condition: crate::workflow_dsl::Condition::StepMarker {
            step: "build".to_string(),
            marker: "has_errors".to_string(),
        },
        body: vec![], // empty body
    };

    // Should succeed (marker present → body skipped)
    execute_unless(&mut state, &node).unwrap();
}

#[test]
fn test_execute_unless_step_not_found_runs_body() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    // No step results at all — step "build" not in step_results
    let node = UnlessNode {
        condition: crate::workflow_dsl::Condition::StepMarker {
            step: "build".to_string(),
            marker: "has_errors".to_string(),
        },
        body: vec![], // empty body
    };

    // Should succeed (step not found → unwrap_or(false) → !false → body runs)
    execute_unless(&mut state, &node).unwrap();
}

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
fn test_cannot_start_workflow_run_when_active() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("running-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: Some("w1"),
        working_dir: "/tmp/ws/feat-test",
        repo_path: "/tmp/repo",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    let err = execute_workflow(&input).unwrap_err();
    assert!(
        matches!(err, ConductorError::WorkflowRunAlreadyActive { .. }),
        "expected WorkflowRunAlreadyActive, got: {err}"
    );
}

/// Verify that force=true cancels the active run and allows a new one.
/// Part of: process-escape-hatch@1.0.0
#[test]
fn test_force_bypasses_active_workflow_guard() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("running-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: Some("w1"),
        working_dir: "/tmp/ws/feat-test",
        repo_path: "/tmp/repo",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: true,
    };
    // With force=true, the active run should be cancelled and a new one starts
    let result = execute_workflow(&input);
    assert!(
        !matches!(result, Err(ConductorError::WorkflowRunAlreadyActive { .. })),
        "force=true should bypass WorkflowRunAlreadyActive, got: {result:?}"
    );

    // Verify the old run was cancelled
    let old_run = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
    assert_eq!(old_run.status.to_string(), "cancelled");
}

#[test]
fn test_can_start_workflow_run_after_completion() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("done-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"), None)
        .unwrap();

    let workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: Some("w1"),
        working_dir: "/tmp/ws/feat-test",
        repo_path: "/tmp/repo",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    // Guard should pass; empty workflow completes successfully.
    let result = execute_workflow(&input);
    assert!(
        !matches!(result, Err(ConductorError::WorkflowRunAlreadyActive { .. })),
        "should not be blocked by completed run"
    );
}

#[test]
fn test_child_workflow_not_blocked_by_parent() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("parent-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();

    let workflow = make_empty_workflow();
    // depth = 1 means this is a child workflow — guard must be skipped.
    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: Some("w1"),
        working_dir: "/tmp/ws/feat-test",
        repo_path: "/tmp/repo",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 1,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    let result = execute_workflow(&input);
    assert!(
        !matches!(result, Err(ConductorError::WorkflowRunAlreadyActive { .. })),
        "child workflow should not be blocked by active parent run"
    );
}

#[test]
fn test_run_id_notify_slot_is_populated() {
    // Verify that execute_workflow writes the newly-created run ID into
    // run_id_notify before any steps execute. This is the mechanism used
    // by the MCP tool_run_workflow handler to return a run_id immediately.
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    let workflow = make_empty_workflow();

    let slot: RunIdSlot =
        std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: "/tmp/repo",
        repo_path: "/tmp/repo",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: Some(std::sync::Arc::clone(&slot)),
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };

    execute_workflow(&input).expect("workflow should complete");

    let notified_id = slot
        .0
        .lock()
        .expect("mutex not poisoned")
        .clone()
        .expect("run_id_notify slot should have been written");

    // The written ID must match the run that was actually created.
    let mgr = WorkflowManager::new(&conn);
    let run = mgr
        .get_workflow_run(&notified_id)
        .expect("db query ok")
        .expect("run should exist");
    assert_eq!(run.workflow_name, "test-wf");
}

/// setup_db() creates worktree `w1` with path `/tmp/ws/feat-test` which does not
/// exist on disk. Prior to #816 this would propagate a path-not-found error; after
/// the fix the engine must silently fall back to the repo root and succeed.
#[test]
fn test_execute_workflow_falls_back_to_repo_root_when_worktree_path_missing() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: Some("w1"), // path /tmp/ws/feat-test — does not exist on disk
        working_dir: "/tmp/repo",
        repo_path: "/tmp/repo",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };

    let result = execute_workflow(&input).expect(
        "execute_workflow must succeed when worktree path is missing (fallback to repo root)",
    );
    assert!(
        result.all_succeeded,
        "empty workflow should complete with all_succeeded=true"
    );
}

#[test]
fn test_execute_do_empty_body() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);

    let node = DoNode {
        output: None,
        with: vec![],
        body: vec![],
    };

    let result = execute_do(&mut state, &node);
    assert!(result.is_ok());
    assert!(state.all_succeeded);
}

#[test]
fn test_execute_do_sets_and_restores_block_state() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);
    state.exec_config.dry_run = true;

    // Set some outer block state that should be saved and restored
    state.block_output = Some("outer-schema".into());
    state.block_with = vec!["outer-snippet".into()];

    let node = DoNode {
        output: Some("inner-schema".into()),
        with: vec!["inner-snippet".into()],
        // Use a Gate in dry_run mode as a no-op body node
        body: vec![WorkflowNode::Gate(GateNode {
            name: "noop".into(),
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

    let result = execute_do(&mut state, &node);
    assert!(result.is_ok());

    // After execute_do, outer state must be restored
    assert_eq!(state.block_output.as_deref(), Some("outer-schema"));
    assert_eq!(state.block_with, vec!["outer-snippet".to_string()]);
}

#[test]
fn test_execute_do_restores_state_on_error() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);

    state.block_output = Some("outer-schema".into());
    state.block_with = vec!["outer-snippet".into()];

    // A call node without dry_run and no real agent will error
    let node = DoNode {
        output: Some("inner-schema".into()),
        with: vec!["inner-snippet".into()],
        body: vec![WorkflowNode::Call(CallNode {
            agent: AgentRef::Name("nonexistent-agent".into()),
            retries: 0,
            on_fail: None,
            output: None,
            with: vec![],
            bot_name: None,
        })],
    };

    let result = execute_do(&mut state, &node);
    assert!(result.is_err());

    // Block state must be restored even after error
    assert_eq!(state.block_output.as_deref(), Some("outer-schema"));
    assert_eq!(state.block_with, vec!["outer-snippet".to_string()]);
}

#[test]
fn test_execute_do_fail_fast_exits_early() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);
    state.exec_config.fail_fast = true;
    state.exec_config.dry_run = true;
    state.all_succeeded = false; // simulate prior failure

    let initial_position = state.position;

    let node = DoNode {
        output: None,
        with: vec![],
        body: vec![
            WorkflowNode::Gate(GateNode {
                name: "g1".into(),
                gate_type: GateType::HumanApproval,
                prompt: None,
                min_approvals: 1,
                approval_mode: ApprovalMode::default(),
                timeout_secs: 1,
                on_timeout: OnTimeout::Fail,
                bot_name: None,
                quality_gate: None,
            }),
            WorkflowNode::Gate(GateNode {
                name: "g2".into(),
                gate_type: GateType::HumanApproval,
                prompt: None,
                min_approvals: 1,
                approval_mode: ApprovalMode::default(),
                timeout_secs: 1,
                on_timeout: OnTimeout::Fail,
                bot_name: None,
                quality_gate: None,
            }),
        ],
    };

    let result = execute_do(&mut state, &node);
    assert!(result.is_ok());
    // fail_fast should skip after first node — only 1 position increment
    assert_eq!(state.position - initial_position, 1);
}

#[test]
fn test_execute_do_nested_with_combination() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);
    state.exec_config.dry_run = true;

    // Outer do sets with=["a"], inner do sets with=["b"].
    // After inner do runs, inner block_with should have been ["b", "a"].
    // After both do blocks complete, state should be fully restored.
    let node = DoNode {
        output: Some("outer-schema".into()),
        with: vec!["a".into()],
        body: vec![WorkflowNode::Do(DoNode {
            output: None,
            with: vec!["b".into()],
            body: vec![WorkflowNode::Gate(GateNode {
                name: "noop".into(),
                gate_type: GateType::HumanApproval,
                prompt: None,
                min_approvals: 1,
                approval_mode: ApprovalMode::default(),
                timeout_secs: 1,
                on_timeout: OnTimeout::Fail,
                bot_name: None,
                quality_gate: None,
            })],
        })],
    };

    let result = execute_do(&mut state, &node);
    assert!(result.is_ok());
    // Outer state fully restored
    assert!(state.block_output.is_none());
    assert!(state.block_with.is_empty());
}

#[test]
fn test_execute_do_nested_inner_output_overrides_outer() {
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);
    state.exec_config.dry_run = true;

    // Outer do sets output="outer", inner do sets output="inner".
    // Inner body should see block_output="inner".
    // Verify state restoration after nested execution.
    let node = DoNode {
        output: Some("outer".into()),
        with: vec![],
        body: vec![WorkflowNode::Do(DoNode {
            output: Some("inner".into()),
            with: vec![],
            body: vec![WorkflowNode::Gate(GateNode {
                name: "noop".into(),
                gate_type: GateType::HumanApproval,
                prompt: None,
                min_approvals: 1,
                approval_mode: ApprovalMode::default(),
                timeout_secs: 1,
                on_timeout: OnTimeout::Fail,
                bot_name: None,
                quality_gate: None,
            })],
        })],
    };

    let result = execute_do(&mut state, &node);
    assert!(result.is_ok());
    // Outer state fully restored
    assert!(state.block_output.is_none());
    assert!(state.block_with.is_empty());
}

#[test]
fn test_execute_call_merges_block_state() {
    // Verify execute_call picks up block_output and block_with from state.
    // The call will fail (no agent file on disk) but it should attempt to
    // load with the effective values rather than panicking.
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);

    state.block_output = Some("block-schema".into());
    state.block_with = vec!["block-snippet".into()];

    let node = CallNode {
        agent: AgentRef::Name("nonexistent".into()),
        retries: 0,
        on_fail: None,
        output: None,
        with: vec!["call-snippet".into()],
        bot_name: None,
    };

    // Call will error on load_agent, but the merging logic should execute
    // without panics and the error should be from agent loading, not from
    // the effective_output/effective_with computation.
    let result = execute_call(&mut state, &node, 0);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("agent") || err.contains("nonexistent"),
        "expected agent load error, got: {err}"
    );
}

#[test]
fn test_execute_call_node_output_overrides_block_output() {
    // When a CallNode has its own output, it should take precedence
    // over block_output. Verify the call attempts to use "call-schema".
    let conn = setup_db();
    let config = Config::default();
    let mut state = make_loop_test_state(&conn, &config);

    state.block_output = Some("block-schema".into());

    let node = CallNode {
        agent: AgentRef::Name("nonexistent".into()),
        retries: 0,
        on_fail: None,
        output: Some("call-schema".into()),
        with: vec![],
        bot_name: None,
    };

    let result = execute_call(&mut state, &node, 0);
    assert!(result.is_err());
    // The error is from agent loading, not from the merging logic
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("agent") || err.contains("nonexistent"),
        "expected agent load error, got: {err}"
    );
}

#[test]
fn test_bubble_up_child_step_results_basic() {
    let conn = setup_db();
    let (wf_mgr, run_id) = create_child_run(&conn);

    // Insert two completed steps with markers
    let step1 = wf_mgr
        .insert_step(&run_id, "review-aggregator", "reviewer", false, 0, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step1,
            WorkflowStepStatus::Completed,
            None,
            Some("done"),
            Some("some context"),
            Some(r#"["has_review_issues"]"#),
            None,
        )
        .unwrap();

    let step2 = wf_mgr
        .insert_step(&run_id, "lint-checker", "reviewer", false, 1, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step2,
            WorkflowStepStatus::Completed,
            None,
            Some("done"),
            Some("lint ok"),
            Some(r#"["lint_passed"]"#),
            None,
        )
        .unwrap();

    let result = bubble_up_child_step_results(&wf_mgr, &run_id);

    assert_eq!(result.len(), 2);
    let agg = result.get("review-aggregator").unwrap();
    assert!(agg.markers.contains(&"has_review_issues".to_string()));
    let lint = result.get("lint-checker").unwrap();
    assert!(lint.markers.contains(&"lint_passed".to_string()));
}

#[test]
fn test_bubble_up_child_step_results_child_overwrites_parent() {
    let conn = setup_db();
    let config = make_resume_config();
    let (mut state, _run_id) = make_state_with_run(&conn, config);

    // Parent already has a step result for "review-aggregator" (stale from iteration 1)
    state.step_results.insert(
        "review-aggregator".to_string(),
        StepResult {
            step_name: "review-aggregator".to_string(),
            status: WorkflowStepStatus::Completed,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            markers: vec!["parent_marker".to_string()],
            context: "parent context".to_string(),
            child_run_id: None,
            structured_output: None,
            output_file: None,
        },
    );

    // Child run with same step name but different marker (fresh result from iteration 2)
    let (child_wf_mgr, child_run_id) = create_child_run(&conn);
    let step1 = child_wf_mgr
        .insert_step(&child_run_id, "review-aggregator", "reviewer", false, 0, 0)
        .unwrap();
    child_wf_mgr
        .update_step_status(
            &step1,
            WorkflowStepStatus::Completed,
            None,
            Some("done"),
            Some("child context"),
            Some(r#"["child_marker"]"#),
            None,
        )
        .unwrap();

    let child_steps = bubble_up_child_step_results(&child_wf_mgr, &child_run_id);
    for (key, value) in child_steps {
        state.step_results.insert(key, value);
    }

    // Child's fresh value should overwrite the stale parent value
    let result = state.step_results.get("review-aggregator").unwrap();
    assert!(result.markers.contains(&"child_marker".to_string()));
    assert!(!result.markers.contains(&"parent_marker".to_string()));
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
fn test_bubble_up_child_step_results_no_completed_steps() {
    let conn = setup_db();
    let (wf_mgr, run_id) = create_child_run(&conn);

    // Insert a failed step — should not be bubbled up
    let step1 = wf_mgr
        .insert_step(&run_id, "some-step", "reviewer", false, 0, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step1,
            WorkflowStepStatus::Failed,
            None,
            Some("failed"),
            None,
            None,
            None,
        )
        .unwrap();

    let result = bubble_up_child_step_results(&wf_mgr, &run_id);
    assert!(result.is_empty());
}

#[test]
fn test_metadata_fields_basic() {
    let step = WorkflowRunStep {
        id: "s1".into(),
        workflow_run_id: "r1".into(),
        step_name: "lint".into(),
        role: "reviewer".into(),
        can_commit: false,
        condition_expr: None,
        status: WorkflowStepStatus::Completed,
        child_run_id: None,
        position: 1,
        started_at: Some("2025-01-01T00:00:00Z".into()),
        ended_at: Some("2025-01-01T00:01:00Z".into()),
        result_text: None,
        condition_met: None,
        iteration: 1,
        parallel_group_id: None,
        context_out: None,
        markers_out: None,
        retry_count: 0,
        gate_type: None,
        gate_prompt: None,
        gate_timeout: None,
        gate_approved_by: None,
        gate_approved_at: None,
        gate_feedback: None,
        structured_output: None,
        output_file: None,
        gate_options: None,
        gate_selections: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
    };
    let entries = step.metadata_fields();
    assert_eq!(entries.len(), 6); // 4 always-present + Started + Ended
    assert_eq!(
        entries[0],
        MetadataEntry::Field {
            label: "Status",
            value: "completed".into()
        }
    );
    assert_eq!(
        entries[1],
        MetadataEntry::Field {
            label: "Role",
            value: "reviewer".into()
        }
    );
    assert_eq!(
        entries[2],
        MetadataEntry::Field {
            label: "Can commit",
            value: "false".into()
        }
    );
    assert_eq!(
        entries[3],
        MetadataEntry::Field {
            label: "Iteration",
            value: "1".into()
        }
    );
    assert_eq!(
        entries[4],
        MetadataEntry::Field {
            label: "Started",
            value: "2025-01-01T00:00:00Z".into()
        }
    );
    assert_eq!(
        entries[5],
        MetadataEntry::Field {
            label: "Ended",
            value: "2025-01-01T00:01:00Z".into()
        }
    );
    // No gate or section entries
    assert!(!entries
        .iter()
        .any(|e| matches!(e, MetadataEntry::Section { .. })));
}

#[test]
fn test_metadata_fields_optional_sections() {
    let step = WorkflowRunStep {
        id: "s2".into(),
        workflow_run_id: "r1".into(),
        step_name: "review".into(),
        role: "reviewer".into(),
        can_commit: false,
        condition_expr: None,
        status: WorkflowStepStatus::Running,
        child_run_id: None,
        position: 2,
        started_at: None,
        ended_at: None,
        result_text: Some("All good".into()),
        condition_met: None,
        iteration: 0,
        parallel_group_id: None,
        context_out: Some("ctx data".into()),
        markers_out: Some("marker1".into()),
        retry_count: 0,
        gate_type: Some(GateType::HumanApproval),
        gate_prompt: Some("Please approve".into()),
        gate_timeout: None,
        gate_approved_by: None,
        gate_approved_at: None,
        gate_feedback: Some("Looks good".into()),
        structured_output: None,
        output_file: None,
        gate_options: None,
        gate_selections: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
    };
    let entries = step.metadata_fields();
    assert!(entries.contains(&MetadataEntry::Field {
        label: "Gate type",
        value: "human_approval".into()
    }));
    assert!(entries.contains(&MetadataEntry::Section {
        heading: "Gate Prompt",
        body: "Please approve".into()
    }));
    assert!(entries.contains(&MetadataEntry::Section {
        heading: "Gate Feedback",
        body: "Looks good".into()
    }));
    assert!(entries.contains(&MetadataEntry::Section {
        heading: "Result",
        body: "All good".into()
    }));
    assert!(entries.contains(&MetadataEntry::Section {
        heading: "Context Out",
        body: "ctx data".into()
    }));
    assert!(entries.contains(&MetadataEntry::Section {
        heading: "Markers Out",
        body: "marker1".into()
    }));
}

#[test]
fn test_get_completed_step_keys() {
    let conn = setup_db();
    let (run_id, mgr) = setup_run_with_steps(&conn);

    let keys = mgr.get_completed_step_keys(&run_id).unwrap();
    assert_eq!(keys.len(), 1);
    assert!(keys.contains(&("step-a".to_string(), 0)));
    // Failed/running steps should not be in the set
    assert!(!keys.contains(&("step-b".to_string(), 0)));
    assert!(!keys.contains(&("step-c".to_string(), 0)));
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
            }),
            WorkflowNode::Call(CallNode {
                agent: crate::workflow_dsl::AgentRef::Name("step-b".to_string()),
                retries: 0,
                on_fail: None,
                output: None,
                with: vec![],
                bot_name: None,
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

#[test]
fn test_restore_completed_step_basic() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    let step = make_test_step(
        "review",
        WorkflowStepStatus::Completed,
        Some("looks good"),
        Some("reviewed code"),
        Some(r#"["approved"]"#),
        None,
        Some(r#"{"verdict":"approve"}"#),
    );
    let ctx = make_resume_ctx(
        [(("review".to_string(), 0), step)].into_iter().collect(),
        HashMap::new(),
    );

    restore_completed_step(&mut state, &ctx, "review", 0);

    // Verify step_results populated
    let result = state.step_results.get("review").unwrap();
    assert_eq!(result.status, WorkflowStepStatus::Completed);
    assert_eq!(result.result_text.as_deref(), Some("looks good"));
    assert_eq!(result.markers, vec!["approved"]);
    assert_eq!(result.context, "reviewed code");
    assert_eq!(
        result.structured_output.as_deref(),
        Some(r#"{"verdict":"approve"}"#)
    );

    // Verify contexts populated
    assert_eq!(state.contexts.len(), 1);
    assert_eq!(state.contexts[0].step, "review");
    assert_eq!(state.contexts[0].context, "reviewed code");
    assert_eq!(
        state.contexts[0].structured_output.as_deref(),
        Some(r#"{"verdict":"approve"}"#)
    );

    // Verify structured output is accessible via contexts
    assert_eq!(
        state
            .contexts
            .iter()
            .rev()
            .find_map(|c| c.structured_output.as_deref()),
        Some(r#"{"verdict":"approve"}"#)
    );
}

#[test]
fn test_restore_completed_step_not_found() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    let ctx = make_resume_ctx(HashMap::new(), HashMap::new());
    restore_completed_step(&mut state, &ctx, "nonexistent", 0);

    // Should be a no-op (with warning logged)
    assert!(state.step_results.is_empty());
    assert!(state.contexts.is_empty());
}

#[test]
fn test_restore_completed_step_accumulates_costs() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    // Create a child agent run with cost data
    let child_run = agent_mgr
        .create_run(Some("w1"), "test agent", None, None)
        .unwrap();
    agent_mgr
        .update_run_completed(
            &child_run.id,
            None,
            Some("done"),
            Some(0.05),
            Some(3),
            Some(5000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let mut state = make_test_state(&conn);
    state.total_cost = 0.10;
    state.total_turns = 5;
    state.total_duration_ms = 10000;

    // Re-fetch the child run so we have the full AgentRun with costs
    let loaded_run = agent_mgr.get_run(&child_run.id).unwrap().unwrap();

    let step = make_test_step(
        "build",
        WorkflowStepStatus::Completed,
        Some("built"),
        Some("build output"),
        None,
        Some(&child_run.id),
        None,
    );
    let ctx = make_resume_ctx(
        [(("build".to_string(), 0), step)].into_iter().collect(),
        [(child_run.id.clone(), loaded_run)].into_iter().collect(),
    );

    restore_completed_step(&mut state, &ctx, "build", 0);

    // Costs should be accumulated from the child run
    assert!((state.total_cost - 0.15).abs() < 0.001);
    assert_eq!(state.total_turns, 8);
    assert_eq!(state.total_duration_ms, 15000);
}

#[test]
fn test_restore_completed_step_restores_gate_feedback() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    let mut step = make_test_step(
        "approval-gate",
        WorkflowStepStatus::Completed,
        Some("approved"),
        None,
        None,
        None,
        None,
    );
    step.gate_feedback = Some("LGTM, ship it".to_string());

    let ctx = make_resume_ctx(
        [(("approval-gate".to_string(), 0), step)]
            .into_iter()
            .collect(),
        HashMap::new(),
    );

    restore_completed_step(&mut state, &ctx, "approval-gate", 0);

    // Gate feedback should be restored for downstream steps
    assert_eq!(state.last_gate_feedback.as_deref(), Some("LGTM, ship it"));
}

#[test]
fn test_execute_workflow_injects_repo_variables() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    // repo `r1` with local_path `/tmp/repo` is inserted by setup_db()
    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: "/tmp/repo",
        repo_path: "/tmp/repo",
        ticket_id: None,
        repo_id: Some("r1"),
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .get_workflow_run(&result.workflow_run_id)
        .unwrap()
        .unwrap();

    assert_eq!(run.inputs.get("repo_id").map(String::as_str), Some("r1"));
    assert_eq!(
        run.inputs.get("repo_path").map(String::as_str),
        Some("/tmp/repo")
    );
    assert_eq!(
        run.inputs.get("repo_name").map(String::as_str),
        Some("test-repo")
    );
    // Assert the repo_id column is persisted on the WorkflowRun record itself.
    assert_eq!(run.repo_id.as_deref(), Some("r1"));
    assert_eq!(run.ticket_id, None);
}

#[test]
fn test_execute_workflow_injects_ticket_variables() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    insert_test_ticket(&conn, "tkt-1", "r1");

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: "/tmp/repo",
        repo_path: "/tmp/repo",
        ticket_id: Some("tkt-1"),
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .get_workflow_run(&result.workflow_run_id)
        .unwrap()
        .unwrap();

    assert_eq!(
        run.inputs.get("ticket_id").map(String::as_str),
        Some("tkt-1")
    );
    assert_eq!(
        run.inputs.get("ticket_title").map(String::as_str),
        Some("Test ticket title")
    );
    assert!(
        run.inputs.contains_key("ticket_url"),
        "ticket_url should be injected"
    );
    // Assert the ticket_id column is persisted on the WorkflowRun record itself.
    assert_eq!(run.ticket_id.as_deref(), Some("tkt-1"));
    assert_eq!(run.repo_id, None);
}

#[test]
fn test_execute_workflow_existing_input_not_overwritten_by_injection() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    let mut explicit_inputs = HashMap::new();
    explicit_inputs.insert("repo_name".to_string(), "my-override".to_string());

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: "/tmp/repo",
        repo_path: "/tmp/repo",
        ticket_id: None,
        repo_id: Some("r1"),
        model: None,
        exec_config: &exec_config,
        inputs: explicit_inputs,
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .get_workflow_run(&result.workflow_run_id)
        .unwrap()
        .unwrap();

    // Caller-supplied repo_name must not be overwritten by the injected value.
    assert_eq!(
        run.inputs.get("repo_name").map(String::as_str),
        Some("my-override")
    );
}

#[test]
fn test_execute_workflow_unknown_ticket_id_returns_error() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: "",
        repo_path: "",
        ticket_id: Some("nonexistent-ticket"),
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    assert!(
        execute_workflow(&input).is_err(),
        "referencing a nonexistent ticket_id must return an error"
    );
}

#[test]
fn test_execute_workflow_unknown_repo_id_returns_error() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: "",
        repo_path: "",
        ticket_id: None,
        repo_id: Some("nonexistent-repo"),
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    assert!(
        execute_workflow(&input).is_err(),
        "referencing a nonexistent repo_id must return an error"
    );
}

#[test]
fn test_execute_workflow_ephemeral_skips_concurrent_guard() {
    // Verify that when worktree_id is None (ephemeral run), a second concurrent
    // call at depth==0 does NOT return WorkflowRunAlreadyActive — the guard is
    // intentionally skipped for ephemeral runs which have no registered worktree.
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    let workflow = make_empty_workflow();

    // First ephemeral call — must succeed (empty workflow, no agents to spawn).
    let input1 = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: "",
        repo_path: "",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    let result1 = execute_workflow(&input1);
    assert!(
        !matches!(
            result1,
            Err(ConductorError::WorkflowRunAlreadyActive { .. })
        ),
        "first ephemeral call should not be blocked by the concurrent guard"
    );

    // Second ephemeral call — must also not be blocked by the guard, even though
    // the first run's record now exists in the DB (it has no worktree_id, so the
    // guard is skipped entirely for ephemeral runs).
    let input2 = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: "",
        repo_path: "",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    let result2 = execute_workflow(&input2);
    assert!(
        !matches!(
            result2,
            Err(ConductorError::WorkflowRunAlreadyActive { .. })
        ),
        "second ephemeral call should not be blocked by the concurrent guard"
    );
}

#[test]
fn test_apply_workflow_input_defaults_fills_missing_default() {
    use crate::workflow_dsl::InputDecl;

    let workflow = make_workflow_def_with_inputs(vec![InputDecl {
        name: "skip_tests".to_string(),
        required: false,
        default: Some("false".to_string()),
        description: None,
        input_type: Default::default(),
    }]);

    let mut inputs = HashMap::new();
    apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
    assert_eq!(inputs.get("skip_tests").map(String::as_str), Some("false"));
}

#[test]
fn test_apply_workflow_input_defaults_does_not_overwrite_provided_value() {
    use crate::workflow_dsl::InputDecl;

    let workflow = make_workflow_def_with_inputs(vec![InputDecl {
        name: "skip_tests".to_string(),
        required: false,
        default: Some("false".to_string()),
        description: None,
        input_type: Default::default(),
    }]);

    let mut inputs = HashMap::new();
    inputs.insert("skip_tests".to_string(), "true".to_string());
    apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
    // Provided value must not be replaced by the default.
    assert_eq!(inputs.get("skip_tests").map(String::as_str), Some("true"));
}

#[test]
fn test_apply_workflow_input_defaults_errors_on_missing_required() {
    use crate::workflow_dsl::InputDecl;

    let workflow = make_workflow_def_with_inputs(vec![InputDecl {
        name: "ticket_id".to_string(),
        required: true,
        default: None,
        description: None,
        input_type: Default::default(),
    }]);

    let mut inputs = HashMap::new();
    let result = apply_workflow_input_defaults(&workflow, &mut inputs);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("ticket_id"),
        "error message should name the missing input, got: {msg}"
    );
}

#[test]
fn test_apply_workflow_input_defaults_required_input_provided_succeeds() {
    use crate::workflow_dsl::InputDecl;

    let workflow = make_workflow_def_with_inputs(vec![InputDecl {
        name: "ticket_id".to_string(),
        required: true,
        default: None,
        description: None,
        input_type: Default::default(),
    }]);

    let mut inputs = HashMap::new();
    inputs.insert("ticket_id".to_string(), "TKT-1".to_string());
    apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
    assert_eq!(inputs.get("ticket_id").map(String::as_str), Some("TKT-1"));
}

#[test]
fn test_apply_workflow_input_defaults_no_inputs_is_noop() {
    let workflow = make_workflow_def_with_inputs(vec![]);
    let mut inputs = HashMap::new();
    apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
    assert!(inputs.is_empty());
}

/// When `if` condition IS met (marker present), the agent is not skipped.
/// This tests the pure marker-lookup logic used by execute_parallel.
#[test]
fn test_if_condition_met_does_not_skip() {
    // Simulate: detect-db-migrations emitted has_db_migrations → review-db-migrations runs
    let cond_step = "detect-db-migrations";
    let cond_marker = "has_db_migrations";

    let mut step_results: HashMap<String, StepResult> = HashMap::new();
    step_results.insert(
        cond_step.to_string(),
        StepResult {
            step_name: cond_step.to_string(),
            status: WorkflowStepStatus::Completed,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            markers: vec![cond_marker.to_string()],
            context: "Found 2 migration files".to_string(),
            child_run_id: None,
            structured_output: None,
            output_file: None,
        },
    );

    let has_marker = step_results
        .get(cond_step)
        .map(|r| r.markers.iter().any(|m| m == cond_marker))
        .unwrap_or(false);

    assert!(has_marker, "marker present → agent should NOT be skipped");
}

/// When `if` condition is NOT met (marker absent), the agent is skipped.
#[test]
fn test_if_condition_not_met_skips() {
    // Simulate: detect-db-migrations ran but did NOT emit has_db_migrations
    let cond_step = "detect-db-migrations";
    let cond_marker = "has_db_migrations";

    let mut step_results: HashMap<String, StepResult> = HashMap::new();
    step_results.insert(
        cond_step.to_string(),
        StepResult {
            step_name: cond_step.to_string(),
            status: WorkflowStepStatus::Completed,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            markers: vec![], // no markers emitted
            context: "No migration files in diff".to_string(),
            child_run_id: None,
            structured_output: None,
            output_file: None,
        },
    );

    let has_marker = step_results
        .get(cond_step)
        .map(|r| r.markers.iter().any(|m| m == cond_marker))
        .unwrap_or(false);

    assert!(!has_marker, "marker absent → agent SHOULD be skipped");
}

/// When the cond_step is not in step_results at all, `if` skips the agent.
#[test]
fn test_if_step_not_found_skips() {
    let cond_step = "detect-db-migrations";
    let cond_marker = "has_db_migrations";
    let step_results: HashMap<String, StepResult> = HashMap::new();

    let has_marker = step_results
        .get(cond_step)
        .map(|r| r.markers.iter().any(|m| m == cond_marker))
        .unwrap_or(false);

    assert!(
        !has_marker,
        "step not found → should skip (unwrap_or(false))"
    );
}

/// Condition-skipped steps (status=Skipped) must NOT appear in completed_keys_from_steps,
/// so they re-evaluate on resume rather than being treated as done.
#[test]
fn test_condition_skipped_steps_not_in_completed_keys() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    // Insert a Completed step and a Skipped step
    let step_completed = wf_mgr
        .insert_step(&run.id, "detect-db-migrations", "reviewer", false, 0, 0)
        .unwrap();
    set_step_status(&wf_mgr, &step_completed, WorkflowStepStatus::Completed);

    let step_skipped = wf_mgr
        .insert_step(&run.id, "review-db-migrations", "reviewer", false, 1, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step_skipped,
            WorkflowStepStatus::Skipped,
            None,
            Some("skipped: detect-db-migrations.has_db_migrations not emitted"),
            None,
            None,
            None,
        )
        .unwrap();

    let steps = wf_mgr.get_workflow_steps(&run.id).unwrap();
    let keys = completed_keys_from_steps(&steps);

    assert!(
        keys.contains(&("detect-db-migrations".to_string(), 0)),
        "Completed step must be in completed_keys"
    );
    assert!(
        !keys.contains(&("review-db-migrations".to_string(), 0)),
        "Skipped step must NOT be in completed_keys (re-evaluates on resume)"
    );
}

/// `if`-skipped agents count toward skipped_count (and thus effective_successes),
/// so the parallel block succeeds even if some calls were condition-skipped.
#[test]
fn test_parallel_if_counts_toward_skipped_count() {
    // Scenario: 2 agents. 1 ran and succeeded, 1 was condition-skipped.
    let successes: u32 = 1;
    let skipped_count: u32 = 1; // condition-skipped
    let children_len: u32 = 1; // only the non-skipped agent was spawned

    let effective_successes = successes + skipped_count; // 2
    let total_agents = children_len + skipped_count; // 2
    let min_required: u32 = total_agents; // default: all

    let status = if effective_successes >= min_required {
        WorkflowStepStatus::Completed
    } else {
        WorkflowStepStatus::Failed
    };
    assert_eq!(
        status,
        WorkflowStepStatus::Completed,
        "condition-skipped agents must count toward min_success so parallel block succeeds"
    );
}

#[test]
fn test_parallel_min_success_with_skipped_resume_agents() {
    // Scenario: 3 agents in a parallel block, min_success = 3.
    // On resume, 2 agents were already completed (skipped), 1 new agent succeeds.
    let successes: u32 = 1; // newly succeeded
    let skipped_count: u32 = 2; // completed on previous run
    let children_len: u32 = 1; // only the non-skipped agent was spawned

    let effective_successes = successes + skipped_count; // 3
    let total_agents = children_len + skipped_count; // 3
    let min_required: u32 = 3; // all must succeed

    // The synthetic step should be Completed, not Failed
    let status = if effective_successes >= min_required {
        WorkflowStepStatus::Completed
    } else {
        WorkflowStepStatus::Failed
    };
    assert_eq!(
        status,
        WorkflowStepStatus::Completed,
        "skipped agents must count toward min_success"
    );

    // Verify the all_succeeded flag would NOT be set to false
    let all_succeeded = effective_successes >= min_required;
    assert!(
        all_succeeded,
        "effective_successes ({effective_successes}) should meet min_required ({min_required})"
    );

    // Verify default min_success (None → total_agents) also works
    let default_min = total_agents;
    assert!(
        effective_successes >= default_min,
        "default min_success should be met when all agents (including skipped) succeed"
    );

    // Edge case: one new agent fails, only skipped agents succeeded
    let successes_fail: u32 = 0;
    let effective_fail = successes_fail + skipped_count; // 2
    let status_fail = if effective_fail >= min_required {
        WorkflowStepStatus::Completed
    } else {
        WorkflowStepStatus::Failed
    };
    assert_eq!(
        status_fail,
        WorkflowStepStatus::Failed,
        "should fail when effective successes don't meet min_required"
    );
}

#[test]
fn test_execute_workflow_iteration_persisted() {
    // When iteration > 0, execute_workflow should persist the iteration on the
    // created workflow run record via set_workflow_run_iteration.
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    // Use run_id_notify to capture the workflow run ID.
    let slot: RunIdSlot =
        std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: "",
        repo_path: "",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 1,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 3,
        run_id_notify: Some(slot.clone()),
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };

    let result = execute_workflow(&input);
    // The workflow will complete (empty body, no agents to spawn).
    assert!(
        result.is_ok(),
        "execute_workflow should succeed: {:?}",
        result
    );

    // Retrieve the run ID from the notify slot.
    let run_id = slot
        .0
        .lock()
        .unwrap()
        .clone()
        .expect("run_id should be set");

    // Verify the run record has iteration == 3.
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .get_workflow_run(&run_id)
        .unwrap()
        .expect("run should exist");
    assert_eq!(
        run.iteration, 3,
        "iteration should be persisted on the workflow run"
    );
}

#[test]
fn test_execute_workflow_fails_on_invalid_schema() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    // Create a temp dir with a valid agent definition so the agent check passes
    let tmp = tempfile::tempdir().unwrap();
    let agents_dir = tmp.path().join(".conductor/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("test-agent.md"), "You are a test agent.").unwrap();
    let working_dir = tmp.path().to_str().unwrap();

    // Build a workflow with a step referencing a schema that doesn't exist
    let mut workflow = make_empty_workflow();
    workflow.body.push(WorkflowNode::Call(CallNode {
        agent: AgentRef::Name("test-agent".into()),
        retries: 0,
        on_fail: None,
        output: Some("broken".into()),
        with: vec![],
        bot_name: None,
    }));

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir,
        repo_path: "",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };

    let err = execute_workflow(&input).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Schema validation failed"),
        "expected schema validation error, got: {msg}"
    );
    assert!(
        msg.contains("broken"),
        "error should mention the bad schema name, got: {msg}"
    );

    // Verify no agent runs were created (zero tokens spent)
    let agent_mgr = AgentManager::new(&conn);
    let runs = agent_mgr.list_agent_runs(None, None, None, 100, 0).unwrap();
    assert!(
        runs.is_empty(),
        "no agent runs should be created when schema validation fails"
    );
}

#[test]
fn test_execute_workflow_fails_on_invalid_schema_parse() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    let tmp = tempfile::tempdir().unwrap();
    let agents_dir = tmp.path().join(".conductor/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("test-agent.md"), "You are a test agent.").unwrap();

    // Create a schema file with invalid YAML so it triggers SchemaIssue::Invalid
    let schemas_dir = tmp.path().join(".conductor/schemas");
    std::fs::create_dir_all(&schemas_dir).unwrap();
    std::fs::write(
        schemas_dir.join("bad-schema.yaml"),
        "fields: [this: is: not: valid\n",
    )
    .unwrap();

    let working_dir = tmp.path().to_str().unwrap();

    let mut workflow = make_empty_workflow();
    workflow.body.push(WorkflowNode::Call(CallNode {
        agent: AgentRef::Name("test-agent".into()),
        retries: 0,
        on_fail: None,
        output: Some("bad-schema".into()),
        with: vec![],
        bot_name: None,
    }));

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir,
        repo_path: working_dir,
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };

    let err = execute_workflow(&input).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Schema validation failed"),
        "expected schema validation error, got: {msg}"
    );
    assert!(
        msg.contains("invalid"),
        "error should indicate the schema is invalid, got: {msg}"
    );
    assert!(
        msg.contains("bad-schema"),
        "error should mention the schema name, got: {msg}"
    );

    // Verify no agent runs were created
    let agent_mgr = AgentManager::new(&conn);
    let runs = agent_mgr.list_agent_runs(None, None, None, 100, 0).unwrap();
    assert!(
        runs.is_empty(),
        "no agent runs should be created when schema validation fails"
    );
}

#[test]
fn test_execute_workflow_passes_preflight_with_valid_schema() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    let tmp = tempfile::tempdir().unwrap();
    let agents_dir = tmp.path().join(".conductor/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("test-agent.md"), "You are a test agent.").unwrap();

    // Create a valid schema file
    let schemas_dir = tmp.path().join(".conductor/schemas");
    std::fs::create_dir_all(&schemas_dir).unwrap();
    std::fs::write(
        schemas_dir.join("good-schema.yaml"),
        "fields:\n  summary: string\n",
    )
    .unwrap();

    let working_dir = tmp.path().to_str().unwrap();

    let mut workflow = make_empty_workflow();
    workflow.body.push(WorkflowNode::Call(CallNode {
        agent: AgentRef::Name("test-agent".into()),
        retries: 0,
        on_fail: None,
        output: Some("good-schema".into()),
        with: vec![],
        bot_name: None,
    }));

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir,
        repo_path: working_dir,
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };

    // execute_workflow should pass pre-flight validation (schema exists and is valid).
    // It will fail later when trying to actually run the agent (no tmux, etc.),
    // but the error should NOT be about schema validation.
    let result = execute_workflow(&input);
    match result {
        Ok(_) => {} // fine if it somehow succeeds
        Err(e) => {
            let msg = e.to_string();
            assert!(
                !msg.contains("Schema validation failed"),
                "valid schema should not trigger schema validation error, got: {msg}"
            );
        }
    }
}

#[test]
fn test_execute_workflow_injects_feature_variables() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    // Insert a feature for repo r1 (created by setup_db).
    conn.execute(
        "INSERT INTO features (id, repo_id, name, branch, base_branch, status, created_at) \
         VALUES ('f1', 'r1', 'my-feature', 'feat/my-feature', 'main', 'active', '2025-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: "/tmp/repo",
        repo_path: "/tmp/repo",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: Some("f1"),
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .get_workflow_run(&result.workflow_run_id)
        .unwrap()
        .unwrap();

    // Feature variables should be injected into persisted inputs.
    assert_eq!(run.inputs.get("feature_id").map(String::as_str), Some("f1"));
    assert_eq!(
        run.inputs.get("feature_name").map(String::as_str),
        Some("my-feature")
    );
    assert_eq!(
        run.inputs.get("feature_branch").map(String::as_str),
        Some("feat/my-feature")
    );
    // feature_id should also be persisted on the workflow run record.
    assert_eq!(run.feature_id.as_deref(), Some("f1"));
}

#[test]
fn test_execute_workflow_invalid_feature_id_returns_error() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let workflow = make_empty_workflow();

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: "/tmp/repo",
        repo_path: "/tmp/repo",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: Some("nonexistent-feature-id"),
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    let err = execute_workflow(&input).unwrap_err();
    assert!(
        matches!(err, ConductorError::FeatureNotFound { .. }),
        "expected FeatureNotFound error, got: {err:?}"
    );
}

#[test]
fn test_call_workflow_propagates_feature_id_to_child() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    // Create a temp dir with a child workflow file (empty body, so it completes instantly).
    let tmp = tempfile::tempdir().unwrap();
    let wf_dir = tmp.path().join(".conductor/workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(
        wf_dir.join("child.wf"),
        "workflow child { meta { targets = [\"worktree\"] } }",
    )
    .unwrap();
    let working_dir = tmp.path().to_str().unwrap();

    // Insert a feature for repo r1 (created by setup_db).
    conn.execute(
        "INSERT INTO features (id, repo_id, name, branch, base_branch, status, created_at) \
         VALUES ('f1', 'r1', 'my-feature', 'feat/my-feature', 'main', 'active', '2025-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    // Parent workflow that calls the child.
    let mut parent = make_empty_workflow();
    parent
        .body
        .push(WorkflowNode::CallWorkflow(CallWorkflowNode {
            workflow: "child".into(),
            inputs: HashMap::new(),
            retries: 0,
            on_fail: None,
            bot_name: None,
        }));

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &parent,
        worktree_id: None,
        working_dir,
        repo_path: "",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: Some("f1"),
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };
    let result = execute_workflow(&input).unwrap();

    let wf_mgr = WorkflowManager::new(&conn);

    // Find the child run by querying for runs whose parent is our parent run.
    use rusqlite::params;
    let child_run_id: String = conn
        .query_row(
            "SELECT id FROM workflow_runs WHERE parent_workflow_run_id = ?1",
            params![result.workflow_run_id],
            |row| row.get(0),
        )
        .expect("child run should exist");
    let child_run = wf_mgr
        .get_workflow_run(&child_run_id)
        .unwrap()
        .expect("child run should exist");
    assert_eq!(
        child_run.feature_id.as_deref(),
        Some("f1"),
        "child run should inherit feature_id from parent"
    );
    assert_eq!(
        child_run.inputs.get("feature_id").map(String::as_str),
        Some("f1"),
        "child run should have feature_id in its inputs"
    );
    assert_eq!(
        child_run.inputs.get("feature_name").map(String::as_str),
        Some("my-feature"),
        "child run should have feature_name in its inputs"
    );
    assert_eq!(
        child_run.inputs.get("feature_branch").map(String::as_str),
        Some("feat/my-feature"),
        "child run should have feature_branch in its inputs"
    );
}

#[test]
fn test_call_workflow_propagates_triggered_by_hook_to_child() {
    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();

    // Create a temp dir with a child workflow file.
    let tmp = tempfile::tempdir().unwrap();
    let wf_dir = tmp.path().join(".conductor/workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(
        wf_dir.join("child.wf"),
        "workflow child { meta { targets = [\"worktree\"] } }",
    )
    .unwrap();
    let working_dir = tmp.path().to_str().unwrap();

    // Parent workflow that calls the child, triggered by hook.
    let mut parent = make_empty_workflow();
    parent
        .body
        .push(WorkflowNode::CallWorkflow(CallWorkflowNode {
            workflow: "child".into(),
            inputs: HashMap::new(),
            retries: 0,
            on_fail: None,
            bot_name: None,
        }));

    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &parent,
        worktree_id: None,
        working_dir,
        repo_path: "",
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: true,
        conductor_bin_dir: None,
        force: false,
    };
    let result = execute_workflow(&input).unwrap();
    assert!(result.all_succeeded);

    // Parent run must have trigger='hook'.
    let wf_mgr = WorkflowManager::new(&conn);
    let parent_run = wf_mgr
        .get_workflow_run(&result.workflow_run_id)
        .unwrap()
        .expect("parent run should exist");
    assert!(
        parent_run.is_triggered_by_hook(),
        "parent run should have trigger='hook'"
    );

    // Child run must also have trigger='hook' (propagated via triggered_by_hook).
    use rusqlite::params;
    let child_run_id: String = conn
        .query_row(
            "SELECT id FROM workflow_runs WHERE parent_workflow_run_id = ?1",
            params![result.workflow_run_id],
            |row| row.get(0),
        )
        .expect("child run should exist");
    let child_run = wf_mgr
        .get_workflow_run(&child_run_id)
        .unwrap()
        .expect("child run should exist");
    assert_eq!(
        child_run.trigger, "hook",
        "child run should inherit trigger='hook' from parent"
    );
    assert!(
        child_run.is_triggered_by_hook(),
        "child run should be marked as triggered by hook"
    );
}

// ---------------------------------------------------------------------------
// evaluate_hooks integration tests
// ---------------------------------------------------------------------------

#[test]
fn test_hook_chain_prevention_when_triggered_by_hook() {
    // When triggered_by_hook is true, hooks should NOT fire (prevents infinite chains).
    let dir = setup_hooks_dir(
        r#"
[hooks.test-wf]
on_complete = "should-not-fire"
"#,
        &[(
            "should-not-fire.wf",
            r#"workflow should-not-fire {
  meta {
    description = "should never run"
    trigger = "manual"
    targets = ["worktree"]
  }
}"#,
        )],
    );

    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let dir_path = dir.path().to_str().unwrap();

    let workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: dir_path,
        repo_path: dir_path,
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: true,
        conductor_bin_dir: None,
        force: false,
    };

    let result = execute_workflow(&input).unwrap();
    assert!(result.all_succeeded);

    // Verify no hook workflow run was created — only the main run should exist.
    // Query all runs directly (no worktree_id filter).
    let all_runs: Vec<WorkflowRun> = crate::db::query_collect(
        &conn,
        &format!(
            "SELECT {} FROM workflow_runs ORDER BY started_at",
            crate::workflow::constants::RUN_COLUMNS
        ),
        [],
        crate::workflow::manager::row_to_workflow_run,
    )
    .unwrap();
    assert_eq!(
        all_runs.len(),
        1,
        "only the main run should exist (no hook run)"
    );
    assert!(
        all_runs[0].is_triggered_by_hook(),
        "main run should have trigger='hook'"
    );
}

#[test]
fn test_hook_skips_missing_workflow() {
    // When hooks config references a workflow that doesn't exist, the main
    // workflow should still complete successfully.
    let dir = setup_hooks_dir(
        r#"
[hooks.test-wf]
on_complete = "nonexistent-hook-wf"
"#,
        &[], // no workflow files
    );

    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let dir_path = dir.path().to_str().unwrap();

    let workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: dir_path,
        repo_path: dir_path,
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };

    let result = execute_workflow(&input).unwrap();
    assert!(
        result.all_succeeded,
        "main workflow should succeed even when hook workflow is missing"
    );
}

#[test]
fn test_hook_fires_on_complete() {
    // When a top-level workflow completes and hooks config has an on_complete
    // entry, the hook workflow should be triggered with trigger='hook'.
    let dir = setup_hooks_dir(
        r#"
[hooks.test-wf]
on_complete = "post-complete"
"#,
        &[(
            "post-complete.wf",
            r#"workflow post-complete {
  meta {
    description = "post-complete hook"
    trigger = "manual"
    targets = ["worktree"]
  }
}"#,
        )],
    );

    let conn = setup_db();
    let config = Config::default();
    let exec_config = WorkflowExecConfig::default();
    let dir_path = dir.path().to_str().unwrap();

    let workflow = make_empty_workflow();
    let input = WorkflowExecInput {
        conn: &conn,
        config: &config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: dir_path,
        repo_path: dir_path,
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: &exec_config,
        inputs: HashMap::new(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
    };

    let result = execute_workflow(&input).unwrap();
    assert!(result.all_succeeded);

    // Verify that a hook workflow run was created with trigger='hook'.
    let all_runs: Vec<WorkflowRun> = crate::db::query_collect(
        &conn,
        &format!(
            "SELECT {} FROM workflow_runs ORDER BY started_at",
            crate::workflow::constants::RUN_COLUMNS
        ),
        [],
        crate::workflow::manager::row_to_workflow_run,
    )
    .unwrap();
    assert_eq!(all_runs.len(), 2, "main + hook run should exist");

    let hook_run = all_runs
        .iter()
        .find(|r| r.workflow_name == "post-complete")
        .expect("hook workflow run should exist");
    assert_eq!(hook_run.trigger, "hook");
    assert!(hook_run.is_triggered_by_hook());
    assert_eq!(
        hook_run.parent_workflow_run_id.as_deref(),
        Some(result.workflow_run_id.as_str()),
        "hook run should link to parent"
    );
}
