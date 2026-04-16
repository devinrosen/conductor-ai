#![allow(unused_imports)]

use super::*;
use crate::error::ConductorError;
use crate::workflow::helpers::{
    collect_leaf_step_keys, find_max_completed_while_iteration, sanitize_tmux_name,
};
use crate::workflow_dsl::{
    AgentRef, ApprovalMode, CallNode, CallWorkflowNode, Condition, DoNode, DoWhileNode, GateNode,
    GateType, IfNode, OnMaxIter, OnTimeout, ParallelNode, ScriptNode, UnlessNode, WhileNode,
    WorkflowNode,
};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// sanitize_tmux_name
// ---------------------------------------------------------------------------

#[test]
fn test_sanitize_tmux_name_special_chars() {
    assert_eq!(sanitize_tmux_name("a.b:c\\d'e\"f"), "a-b-c-d-e-f");
}

#[test]
fn test_sanitize_tmux_name_control_chars() {
    assert_eq!(sanitize_tmux_name("hello\x00world\x1f!"), "hello-world-!");
}

#[test]
fn test_sanitize_tmux_name_clean_input() {
    assert_eq!(sanitize_tmux_name("my-workflow_v2"), "my-workflow_v2");
}

#[test]
fn test_sanitize_tmux_name_empty_string() {
    assert_eq!(sanitize_tmux_name(""), "");
}

#[test]
fn test_sanitize_tmux_name_all_special() {
    assert_eq!(sanitize_tmux_name(".:.\\."), "-----");
}

// ---------------------------------------------------------------------------
// collect_leaf_step_keys
// ---------------------------------------------------------------------------

fn make_call_node(name: &str) -> WorkflowNode {
    WorkflowNode::Call(CallNode {
        agent: AgentRef::Name(name.into()),
        retries: 0,
        on_fail: None,
        output: None,
        with: vec![],
        bot_name: None,
        plugin_dirs: vec![],
    })
}

fn make_while_node(body: Vec<WorkflowNode>) -> WhileNode {
    WhileNode {
        step: "s".into(),
        marker: "m".into(),
        max_iterations: 5,
        stuck_after: None,
        on_max_iter: OnMaxIter::Fail,
        body,
    }
}

fn make_gate_node_wf(name: &str) -> WorkflowNode {
    WorkflowNode::Gate(GateNode {
        name: name.into(),
        gate_type: GateType::HumanApproval,
        prompt: None,
        min_approvals: 1,
        approval_mode: Default::default(),
        timeout_secs: 60,
        on_timeout: OnTimeout::Fail,
        bot_name: None,
        quality_gate: None,
        options: None,
    })
}

#[test]
fn test_collect_leaf_call() {
    let node = make_call_node("build");
    assert_eq!(collect_leaf_step_keys(&node), vec!["build"]);
}

#[test]
fn test_collect_leaf_parallel() {
    let node = WorkflowNode::Parallel(ParallelNode {
        fail_fast: true,
        min_success: None,
        calls: vec![AgentRef::Name("a".into()), AgentRef::Name("b".into())],
        output: None,
        call_outputs: HashMap::new(),
        with: vec![],
        call_with: HashMap::new(),
        call_if: HashMap::new(),
    });
    assert_eq!(collect_leaf_step_keys(&node), vec!["a", "b"]);
}

#[test]
fn test_collect_leaf_gate() {
    let node = make_gate_node_wf("approval");
    assert_eq!(collect_leaf_step_keys(&node), vec!["approval"]);
}

#[test]
fn test_collect_leaf_call_workflow() {
    let node = WorkflowNode::CallWorkflow(CallWorkflowNode {
        workflow: "sub-wf".into(),
        inputs: HashMap::new(),
        retries: 0,
        on_fail: None,
        bot_name: None,
    });
    assert_eq!(collect_leaf_step_keys(&node), vec!["workflow:sub-wf"]);
}

#[test]
fn test_collect_leaf_script() {
    let node = WorkflowNode::Script(ScriptNode {
        name: "run-tests".into(),
        run: "test.sh".into(),
        env: HashMap::new(),
        timeout: None,
        retries: 0,
        on_fail: None,
        bot_name: None,
    });
    assert_eq!(collect_leaf_step_keys(&node), vec!["run-tests"]);
}

#[test]
fn test_collect_leaf_if_node() {
    let node = WorkflowNode::If(IfNode {
        condition: Condition::BoolInput {
            input: "flag".into(),
        },
        body: vec![make_call_node("inner")],
    });
    assert_eq!(collect_leaf_step_keys(&node), vec!["inner"]);
}

#[test]
fn test_collect_leaf_unless_node() {
    let node = WorkflowNode::Unless(UnlessNode {
        condition: Condition::BoolInput {
            input: "skip".into(),
        },
        body: vec![make_call_node("guarded")],
    });
    assert_eq!(collect_leaf_step_keys(&node), vec!["guarded"]);
}

#[test]
fn test_collect_leaf_while_node() {
    let node = WorkflowNode::While(make_while_node(vec![make_call_node("loop-body")]));
    assert_eq!(collect_leaf_step_keys(&node), vec!["loop-body"]);
}

#[test]
fn test_collect_leaf_do_while_node() {
    let node = WorkflowNode::DoWhile(DoWhileNode {
        step: "check".into(),
        marker: "done".into(),
        max_iterations: 3,
        stuck_after: None,
        on_max_iter: OnMaxIter::Fail,
        body: vec![make_call_node("dw-body")],
    });
    assert_eq!(collect_leaf_step_keys(&node), vec!["dw-body"]);
}

#[test]
fn test_collect_leaf_do_node() {
    let node = WorkflowNode::Do(DoNode {
        output: None,
        with: vec![],
        body: vec![make_call_node("do-body"), make_gate_node_wf("gate-1")],
    });
    assert_eq!(collect_leaf_step_keys(&node), vec!["do-body", "gate-1"]);
}

#[test]
fn test_collect_leaf_always_node() {
    let node = WorkflowNode::Always(crate::workflow_dsl::AlwaysNode {
        body: vec![make_call_node("cleanup")],
    });
    assert_eq!(collect_leaf_step_keys(&node), vec!["cleanup"]);
}

#[test]
fn test_collect_leaf_nested() {
    // if { while { call + gate } }
    let node = WorkflowNode::If(IfNode {
        condition: Condition::BoolInput { input: "go".into() },
        body: vec![WorkflowNode::While(make_while_node(vec![
            make_call_node("deep"),
            make_gate_node_wf("deep-gate"),
        ]))],
    });
    assert_eq!(collect_leaf_step_keys(&node), vec!["deep", "deep-gate"]);
}

// ---------------------------------------------------------------------------
// build_workflow_summary — never-executed annotation
// ---------------------------------------------------------------------------

#[test]
fn test_summary_labels_never_executed_failed_step() {
    let conn = setup_db();
    let config = make_resume_config();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    // Insert step in 'pending' state (started_at = NULL), then mark Failed
    // without ever transitioning through Running — simulates a step that
    // never executed.
    let step_id = wf_mgr
        .insert_step(&run.id, "push-and-pr", "actor", false, 0, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step_id,
            WorkflowStepStatus::Failed,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let state = ExecutionState {
        workflow_run_id: run.id.clone(),
        workflow_name: "test-wf".to_string(),
        all_succeeded: false,
        ..make_loop_test_state(&conn, config)
    };

    let summary = build_workflow_summary(&state);
    assert!(
        summary.contains("(never executed)"),
        "expected '(never executed)' in summary:\n{summary}"
    );
}

#[test]
fn test_summary_does_not_label_started_failed_step() {
    let conn = setup_db();
    let config = make_resume_config();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    // Step goes through Running (sets started_at), then fails.
    let step_id = wf_mgr
        .insert_step(&run.id, "build", "actor", false, 0, 0)
        .unwrap();
    wf_mgr
        .update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    wf_mgr
        .update_step_status(
            &step_id,
            WorkflowStepStatus::Failed,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let state = ExecutionState {
        workflow_run_id: run.id.clone(),
        workflow_name: "test-wf".to_string(),
        all_succeeded: false,
        ..make_loop_test_state(&conn, config)
    };

    let summary = build_workflow_summary(&state);
    assert!(
        !summary.contains("(never executed)"),
        "should not contain '(never executed)' for a step that ran:\n{summary}"
    );
}

#[test]
fn test_record_step_failure_never_started_message() {
    let conn = setup_db();
    let config = make_resume_config();
    let mut state = make_loop_test_state(&conn, config);
    // fail_fast defaults to true in WorkflowExecConfig::default()

    let err = record_step_failure(
        &mut state,
        "push-and-pr".to_string(),
        "push-and-pr",
        "setup failed".to_string(),
        1,
        false,
    )
    .unwrap_err();

    match err {
        ConductorError::Workflow(msg) => {
            assert!(
                msg.contains("failed to start (never executed)"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected ConductorError::Workflow, got: {other:?}"),
    }
}

#[test]
fn test_record_step_failure_started_message() {
    let conn = setup_db();
    let config = make_resume_config();
    let mut state = make_loop_test_state(&conn, config);

    let err = record_step_failure(
        &mut state,
        "build".to_string(),
        "build",
        "exit code 1".to_string(),
        3,
        true,
    )
    .unwrap_err();

    match err {
        ConductorError::Workflow(msg) => {
            assert!(
                msg.contains("failed after 3 attempts"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected ConductorError::Workflow, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// find_max_completed_while_iteration
// ---------------------------------------------------------------------------

#[test]
fn test_find_max_completed_while_iteration_no_resume_ctx() {
    let conn = setup_db();
    let config = make_resume_config();
    let state = make_loop_test_state(&conn, config);
    // state.resume_ctx is None → should return 0
    let node = make_while_node(vec![make_call_node("body")]);
    assert_eq!(find_max_completed_while_iteration(&state, &node), 0);
}

#[test]
fn test_find_max_completed_while_iteration_empty_body() {
    let conn = setup_db();
    let config = make_resume_config();
    let mut state = make_loop_test_state(&conn, config);
    state.resume_ctx = Some(ResumeContext {
        skip_completed: std::collections::HashSet::new(),
        step_map: HashMap::new(),
        child_runs: HashMap::new(),
    });
    let node = make_while_node(vec![]); // no body nodes
    assert_eq!(find_max_completed_while_iteration(&state, &node), 0);
}

#[test]
fn test_find_max_completed_while_iteration_partial() {
    let conn = setup_db();
    let config = make_resume_config();
    let mut state = make_loop_test_state(&conn, config);

    let mut skip = std::collections::HashSet::new();
    // iteration 0: body completed
    skip.insert(("body".to_string(), 0u32));
    // iteration 1: body completed
    skip.insert(("body".to_string(), 1u32));
    // iteration 2: NOT completed

    state.resume_ctx = Some(ResumeContext {
        skip_completed: skip,
        step_map: HashMap::new(),
        child_runs: HashMap::new(),
    });

    let node = make_while_node(vec![make_call_node("body")]);
    // Iterations 0 and 1 are complete, so resume from 2
    assert_eq!(find_max_completed_while_iteration(&state, &node), 2);
}

#[test]
fn test_find_max_completed_while_iteration_multi_body_keys() {
    let conn = setup_db();
    let config = make_resume_config();
    let mut state = make_loop_test_state(&conn, config);

    let mut skip = std::collections::HashSet::new();
    // iteration 0: both keys completed
    skip.insert(("a".to_string(), 0u32));
    skip.insert(("b".to_string(), 0u32));
    // iteration 1: only "a" completed — incomplete iteration
    skip.insert(("a".to_string(), 1u32));

    state.resume_ctx = Some(ResumeContext {
        skip_completed: skip,
        step_map: HashMap::new(),
        child_runs: HashMap::new(),
    });

    let node = make_while_node(vec![make_call_node("a"), make_call_node("b")]);
    // Only iteration 0 is fully complete
    assert_eq!(find_max_completed_while_iteration(&state, &node), 1);
}
