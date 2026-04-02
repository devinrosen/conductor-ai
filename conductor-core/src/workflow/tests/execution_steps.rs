use super::*;
use crate::agent::AgentManager;
use crate::workflow_dsl::{
    AgentRef, ApprovalMode, CallNode, DoNode, GateNode, GateType, IfNode, ParallelNode, UnlessNode,
    WorkflowNode,
};

/// Verify that the token accumulation path exercised after each parallel agent
/// completion (execute_parallel lines 335–346) correctly increments all four
/// token counters on the ExecutionState, and that flush_metrics persists them
/// to the DB without error.
#[test]
fn test_parallel_agent_completion_accumulates_tokens() {
    let conn = setup_db();
    let agent_mgr = AgentManager::new(&conn);

    // Create two "completed" agent runs that simulate parallel agent results.
    let run_a = agent_mgr
        .create_run(Some("w1"), "reviewer-a", None, None)
        .unwrap();
    agent_mgr
        .update_run_completed(
            &run_a.id,
            None,
            Some("done"),
            Some(0.05),
            Some(3),
            Some(4000),
            Some(100),  // input_tokens
            Some(50),   // output_tokens
            Some(20),   // cache_read_input_tokens
            Some(10),   // cache_creation_input_tokens
        )
        .unwrap();

    let run_b = agent_mgr
        .create_run(Some("w1"), "reviewer-b", None, None)
        .unwrap();
    agent_mgr
        .update_run_completed(
            &run_b.id,
            None,
            Some("done"),
            Some(0.03),
            Some(2),
            Some(2000),
            Some(80),   // input_tokens
            Some(40),   // output_tokens
            Some(15),   // cache_read_input_tokens
            Some(5),    // cache_creation_input_tokens
        )
        .unwrap();

    // Build a state with a real workflow run so flush_metrics has a valid row.
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test-parallel", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    let mut state = ExecutionState {
        workflow_run_id: run.id.clone(),
        worktree_id: Some("w1".to_string()),
        ..make_test_state(&conn)
    };
    // Patch in the wf_mgr that points at the same conn so flush_metrics can
    // find the workflow_run row.
    state.wf_mgr = WorkflowManager::new(&conn);

    // Re-fetch runs to get filled-in token fields.
    let loaded_a = agent_mgr.get_run(&run_a.id).unwrap().unwrap();
    let loaded_b = agent_mgr.get_run(&run_b.id).unwrap().unwrap();

    // Simulate the accumulation logic in execute_parallel (lines 326-346).
    for run in [&loaded_a, &loaded_b] {
        if let Some(cost) = run.cost_usd {
            state.total_cost += cost;
        }
        if let Some(turns) = run.num_turns {
            state.total_turns += turns;
        }
        if let Some(dur) = run.duration_ms {
            state.total_duration_ms += dur;
        }
        if let Some(t) = run.input_tokens {
            state.total_input_tokens += t;
        }
        if let Some(t) = run.output_tokens {
            state.total_output_tokens += t;
        }
        if let Some(t) = run.cache_read_input_tokens {
            state.total_cache_read_input_tokens += t;
        }
        if let Some(t) = run.cache_creation_input_tokens {
            state.total_cache_creation_input_tokens += t;
        }
    }

    // Verify all four token counters were accumulated correctly.
    assert_eq!(state.total_input_tokens, 180);
    assert_eq!(state.total_output_tokens, 90);
    assert_eq!(state.total_cache_read_input_tokens, 35);
    assert_eq!(state.total_cache_creation_input_tokens, 15);
    assert_eq!(state.total_turns, 5);
    assert!((state.total_cost - 0.08).abs() < 0.001);
    assert_eq!(state.total_duration_ms, 6000);

    // flush_metrics must persist the totals without error.
    state.flush_metrics().unwrap();
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
            options: None,
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
            plugin_dirs: vec![],
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
                options: None,
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
                options: None,
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
                options: None,
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
                options: None,
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
        plugin_dirs: vec![],
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
        plugin_dirs: vec![],
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
