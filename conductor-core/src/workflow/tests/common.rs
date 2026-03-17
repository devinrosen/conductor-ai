#![allow(unused_imports)]

use super::*;
use crate::agent::{AgentManager, AgentRunStatus};
use crate::agent_runtime;
use crate::config::Config;
use crate::error::ConductorError;
use crate::schema_config;
use crate::schema_config::OutputSchema;
use crate::workflow_dsl::WorkflowNode;
use crate::workflow_dsl::{
    AgentRef, ApprovalMode, CallNode, DoNode, DoWhileNode, GateNode, GateType, IfNode, OnMaxIter,
    OnTimeout, ParallelNode, UnlessNode, WhileNode,
};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

pub(super) fn setup_db() -> Connection {
    crate::test_helpers::setup_db()
}

/// Set a step's status without touching any optional fields.
pub(super) fn set_step_status(mgr: &WorkflowManager, step_id: &str, status: WorkflowStepStatus) {
    mgr.update_step_status(step_id, status, None, None, None, None, None)
        .unwrap();
}

pub(super) fn make_gate_node(gate_type: GateType, on_timeout: OnTimeout) -> GateNode {
    GateNode {
        name: "test_gate".to_string(),
        gate_type,
        prompt: None,
        min_approvals: 1,
        approval_mode: ApprovalMode::default(),
        timeout_secs: 1,
        on_timeout,
        bot_name: None,
    }
}

pub(super) fn make_state_with_run<'a>(
    conn: &'a Connection,
    config: &'static Config,
) -> (ExecutionState<'a>, String) {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Waiting, None)
        .unwrap();
    let run_id = run.id.clone();
    let state = ExecutionState {
        conn,
        config,
        workflow_run_id: run_id.clone(),
        workflow_name: "test".to_string(),
        worktree_id: Some("w1".to_string()),
        working_dir: String::new(),
        worktree_slug: String::new(),
        repo_path: String::new(),
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: WorkflowExecConfig::default(),
        inputs: HashMap::new(),
        agent_mgr: AgentManager::new(conn),
        wf_mgr: WorkflowManager::new(conn),
        parent_run_id: parent.id,
        depth: 0,
        target_label: None,
        step_results: HashMap::new(),
        contexts: Vec::new(),
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        last_gate_feedback: None,
        block_output: None,
        block_with: Vec::new(),
        resume_ctx: None,
        default_bot_name: None,
    };
    (state, run_id)
}

/// Helper to create a minimal ExecutionState for testing build_variable_map.
pub(super) fn make_test_state(conn: &Connection) -> ExecutionState<'_> {
    let config = Config::default();
    // We need a config that lives long enough — use a leaked Box for test simplicity.
    let config: &'static Config = Box::leak(Box::new(config));
    ExecutionState {
        conn,
        config,
        workflow_run_id: String::new(),
        workflow_name: String::new(),
        worktree_id: None,
        working_dir: String::new(),
        worktree_slug: String::new(),
        repo_path: String::new(),
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: WorkflowExecConfig::default(),
        inputs: HashMap::new(),
        agent_mgr: AgentManager::new(conn),
        wf_mgr: WorkflowManager::new(conn),
        parent_run_id: String::new(),
        depth: 0,
        target_label: None,
        step_results: HashMap::new(),
        contexts: Vec::new(),
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        last_gate_feedback: None,
        block_output: None,
        block_with: Vec::new(),
        resume_ctx: None,
        default_bot_name: None,
    }
}

pub(super) fn make_test_schema() -> OutputSchema {
    schema_config::parse_schema_content("fields:\n  approved: boolean\n  summary: string\n", "test")
        .unwrap()
}

pub(super) fn make_step_result(step_name: &str, markers: Vec<&str>) -> StepResult {
    StepResult {
        step_name: step_name.into(),
        status: WorkflowStepStatus::Completed,
        result_text: None,
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        markers: markers.into_iter().map(String::from).collect(),
        context: String::new(),
        child_run_id: None,
        structured_output: None,
        output_file: None,
    }
}

/// Helper to build an `ExecutionState` suitable for testing loop functions
/// (no real agents or worktrees needed).
pub(super) fn make_loop_test_state<'a>(
    conn: &'a Connection,
    config: &'a Config,
) -> ExecutionState<'a> {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    ExecutionState {
        conn,
        config,
        workflow_run_id: run.id,
        workflow_name: "test".into(),
        worktree_id: Some("w1".into()),
        working_dir: "/tmp/test".into(),
        worktree_slug: "test".into(),
        repo_path: "/tmp/repo".into(),
        ticket_id: None,
        repo_id: None,
        model: None,
        exec_config: WorkflowExecConfig::default(),
        inputs: HashMap::new(),
        agent_mgr: AgentManager::new(conn),
        wf_mgr: WorkflowManager::new(conn),
        parent_run_id: parent.id,
        depth: 0,
        target_label: None,
        step_results: HashMap::new(),
        contexts: Vec::new(),
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        last_gate_feedback: None,
        block_output: None,
        block_with: Vec::new(),
        resume_ctx: None,
        default_bot_name: None,
    }
}

/// Minimal workflow with no agents or steps — used to exercise the
/// execute_workflow guard without touching real agent infrastructure.
pub(super) fn make_empty_workflow() -> WorkflowDef {
    WorkflowDef {
        name: "test-wf".into(),
        description: "test".into(),
        trigger: WorkflowTrigger::Manual,
        targets: vec![],
        inputs: vec![],
        body: vec![],
        always: vec![],
        source_path: "test.wf".into(),
    }
}

pub(super) fn create_child_run(conn: &Connection) -> (WorkflowManager<'_>, String) {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(conn);
    let run = wf_mgr
        .create_workflow_run("child-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    (wf_mgr, run.id)
}

/// Helper: create a workflow run with steps in various statuses.
pub(super) fn setup_run_with_steps(conn: &Connection) -> (String, WorkflowManager<'_>) {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let mgr = WorkflowManager::new(conn);
    let run = mgr
        .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    // Step 0: completed
    let s0 = mgr
        .insert_step(&run.id, "step-a", "actor", false, 0, 0)
        .unwrap();
    mgr.update_step_status(
        &s0,
        WorkflowStepStatus::Completed,
        None,
        Some("result-a"),
        Some("ctx-a"),
        Some(r#"["marker_a"]"#),
        Some(0),
    )
    .unwrap();

    // Step 1: failed
    let s1 = mgr
        .insert_step(&run.id, "step-b", "actor", false, 1, 0)
        .unwrap();
    mgr.update_step_status(
        &s1,
        WorkflowStepStatus::Failed,
        None,
        Some("error"),
        None,
        None,
        Some(0),
    )
    .unwrap();

    // Step 2: running (stalled)
    let s2 = mgr
        .insert_step(&run.id, "step-c", "actor", false, 2, 0)
        .unwrap();
    set_step_status(&mgr, &s2, WorkflowStepStatus::Running);

    (run.id, mgr)
}

/// Helper to build a WorkflowRunStep for testing without listing every field.
pub(super) fn make_test_step(
    step_name: &str,
    status: WorkflowStepStatus,
    result_text: Option<&str>,
    context_out: Option<&str>,
    markers_out: Option<&str>,
    child_run_id: Option<&str>,
    structured_output: Option<&str>,
) -> WorkflowRunStep {
    WorkflowRunStep {
        id: "s1".to_string(),
        workflow_run_id: "run1".to_string(),
        step_name: step_name.to_string(),
        role: "actor".to_string(),
        can_commit: false,
        condition_expr: None,
        status,
        child_run_id: child_run_id.map(String::from),
        position: 0,
        started_at: None,
        ended_at: None,
        result_text: result_text.map(String::from),
        condition_met: None,
        iteration: 0,
        parallel_group_id: None,
        context_out: context_out.map(String::from),
        markers_out: markers_out.map(String::from),
        retry_count: 0,
        gate_type: None,
        gate_prompt: None,
        gate_timeout: None,
        gate_approved_by: None,
        gate_approved_at: None,
        gate_feedback: None,
        structured_output: structured_output.map(String::from),
        output_file: None,
    }
}

/// Helper to build a ResumeContext from a step map.
pub(super) fn make_resume_ctx(
    step_map: HashMap<StepKey, WorkflowRunStep>,
    child_runs: HashMap<String, crate::agent::AgentRun>,
) -> ResumeContext {
    let skip_completed = step_map.keys().cloned().collect();
    ResumeContext {
        skip_completed,
        step_map,
        child_runs,
    }
}

/// Helper: create a Config suitable for resume tests.
pub(super) fn make_resume_config() -> &'static Config {
    Box::leak(Box::new(Config::default()))
}

pub(super) fn make_workflow_def_with_inputs(
    inputs: Vec<crate::workflow_dsl::InputDecl>,
) -> crate::workflow_dsl::WorkflowDef {
    crate::workflow_dsl::WorkflowDef {
        name: "test-wf".to_string(),
        description: String::new(),
        trigger: crate::workflow_dsl::WorkflowTrigger::Manual,
        targets: vec![],
        inputs,
        body: vec![],
        always: vec![],
        source_path: String::new(),
    }
}

/// Insert a minimal ticket row into the test DB and return its id.
pub(super) fn insert_test_ticket(conn: &Connection, id: &str, repo_id: &str) {
    conn.execute(
        "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, \
         labels, url, synced_at, raw_json) \
         VALUES (?1, ?2, 'github', ?3, 'Test ticket title', '', 'open', '[]', \
         'https://github.com/test/repo/issues/1', '2024-01-01T00:00:00Z', '{}')",
        rusqlite::params![id, repo_id, id],
    )
    .unwrap();
}

/// Insert a minimal workflow_run directly into the DB for testing chain walks.
/// Creates a throwaway agent_run to satisfy the `parent_run_id` FK constraint.
pub(super) fn insert_workflow_run(
    conn: &Connection,
    id: &str,
    name: &str,
    status: &str,
    parent_workflow_run_id: Option<&str>,
) {
    // Create a dummy agent_run so the FK on parent_run_id is satisfied.
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at, \
          parent_workflow_run_id) \
         VALUES (?1, ?2, NULL, ?3, ?4, 0, 'manual', '2025-01-01T00:00:00Z', ?5)",
        params![id, name, parent.id, status, parent_workflow_run_id],
    )
    .unwrap();
}

/// Insert a workflow_run_step in 'running' status for the given run.
pub(super) fn insert_running_step(conn: &Connection, step_id: &str, run_id: &str, step_name: &str) {
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration) \
         VALUES (?1, ?2, ?3, 'actor', 0, 'running', 1)",
        params![step_id, run_id, step_name],
    )
    .unwrap();
}

/// Helper: create a minimal workflow_run row with explicit worktree_id / repo_id.
pub(super) fn insert_workflow_run_with_targets(
    conn: &Connection,
    worktree_id: Option<&str>,
    repo_id: Option<&str>,
) -> String {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
    let mgr = WorkflowManager::new(conn);
    let run = mgr
        .create_workflow_run_with_targets(
            "test-wf",
            worktree_id,
            None,
            repo_id,
            &parent.id,
            false,
            "manual",
            None,
            None,
            None,
        )
        .unwrap();
    run.id
}

/// Insert a workflow run in 'waiting' status with a waiting gate step.
/// The parent agent run is created with the given `parent_status`.
/// Returns the step_id.
pub(super) fn insert_waiting_run_with_gate(
    conn: &Connection,
    run_id: &str,
    parent_status: &str,
    gate_timeout: Option<&str>,
    step_started_at: Option<&str>,
) -> String {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();

    // Set the parent agent run to the requested status directly.
    conn.execute(
        "UPDATE agent_runs SET status = ?1 WHERE id = ?2",
        params![parent_status, parent.id],
    )
    .unwrap();

    // Create the workflow run in 'waiting' status.
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES (?1, 'test-wf', NULL, ?2, 'waiting', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        params![run_id, parent.id],
    )
    .unwrap();

    // Insert a waiting gate step.
    let step_id = crate::new_id();
    let started = step_started_at.unwrap_or("2025-01-01T00:00:00Z");
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          gate_type, gate_timeout, started_at) \
         VALUES (?1, ?2, 'approval-gate', 'gate', 0, 'waiting', 1, \
                 'human_approval', ?3, ?4)",
        params![step_id, run_id, gate_timeout, started],
    )
    .unwrap();

    step_id
}

pub(super) fn make_workflow_run(
    conn: &Connection,
) -> (WorkflowManager<'_>, crate::agent::AgentRun, WorkflowRun) {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let mgr = WorkflowManager::new(conn);
    let run = mgr
        .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    (mgr, parent, run)
}
