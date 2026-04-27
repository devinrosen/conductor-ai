#![allow(dead_code)]

use super::*;
use crate::agent::AgentManager;
use crate::config::Config;
use crate::schema_config;
use crate::schema_config::OutputSchema;
use rusqlite::{named_params, Connection};
use std::collections::HashMap;

pub(super) fn setup_db() -> Connection {
    crate::test_helpers::setup_db()
}

/// Create a temp-file SQLite database pre-populated with repo `r1` and worktree `w1`.
/// Used by tests that call `execute_workflow_standalone` (which opens its own connection).
pub(super) fn make_standalone_db() -> (tempfile::NamedTempFile, std::path::PathBuf) {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let path = tmp.path().to_path_buf();
    {
        let conn = crate::db::open_database(&path).expect("open temp db");
        crate::test_helpers::insert_test_repo(&conn, "r1", "test-repo", "/tmp/repo");
        crate::test_helpers::insert_test_worktree(
            &conn,
            "w1",
            "r1",
            "feat-test",
            "/tmp/ws/feat-test",
        );
    }
    (tmp, path)
}

/// Set a step's status without touching any optional fields.
pub(super) fn set_step_status(mgr: &WorkflowManager, step_id: &str, status: WorkflowStepStatus) {
    mgr.update_step_status(step_id, status, None, None, None, None, None)
        .unwrap();
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

/// Minimal workflow with no agents or steps — used to exercise the
/// execute_workflow guard without touching real agent infrastructure.
pub(super) fn make_empty_workflow() -> WorkflowDef {
    WorkflowDef {
        name: "test-wf".into(),
        title: None,
        description: "test".into(),
        trigger: WorkflowTrigger::Manual,
        targets: vec![],
        group: None,
        inputs: vec![],
        body: vec![],
        always: vec![],
        source_path: "test.wf".into(),
    }
}

pub(super) fn create_child_run(conn: &Connection) -> (WorkflowManager<'_>, String) {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let wf_mgr = WorkflowManager::new(conn);
    let run = wf_mgr
        .create_workflow_run("child-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    (wf_mgr, run.id)
}

/// Helper: create a workflow run with steps in various statuses.
pub(super) fn setup_run_with_steps(conn: &Connection) -> (String, WorkflowManager<'_>) {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
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
        gate_options: None,
        gate_selections: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        fan_out_total: None,
        fan_out_completed: 0,
        fan_out_failed: 0,
        fan_out_skipped: 0,
        step_error: None,
    }
}

/// Helper: create a Config suitable for resume tests.
pub(super) fn make_resume_config() -> &'static Config {
    Box::leak(Box::new(Config::default()))
}

pub(super) fn make_workflow_def_with_inputs(
    inputs: Vec<runkon_flow::dsl::InputDecl>,
) -> runkon_flow::dsl::WorkflowDef {
    runkon_flow::dsl::WorkflowDef {
        name: "test-wf".to_string(),
        title: None,
        description: String::new(),
        trigger: runkon_flow::dsl::WorkflowTrigger::Manual,
        targets: vec![],
        group: None,
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
         VALUES (:id, :repo_id, 'github', :id, 'Test ticket title', '', 'open', '[]', \
         'https://github.com/test/repo/issues/1', '2024-01-01T00:00:00Z', '{}')",
        named_params! { ":id": id, ":repo_id": repo_id },
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
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at, \
          parent_workflow_run_id) \
         VALUES (:id, :name, NULL, :parent_run_id, :status, 0, 'manual', '2025-01-01T00:00:00Z', :parent_workflow_run_id)",
        named_params! { ":id": id, ":name": name, ":parent_run_id": parent.id, ":status": status, ":parent_workflow_run_id": parent_workflow_run_id },
    )
    .unwrap();
}

/// Insert a workflow_run_step in 'running' status for the given run.
pub(super) fn insert_running_step(conn: &Connection, step_id: &str, run_id: &str, step_name: &str) {
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration) \
         VALUES (:step_id, :run_id, :step_name, 'actor', 0, 'running', 1)",
        named_params! { ":step_id": step_id, ":run_id": run_id, ":step_name": step_name },
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
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();
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
    let parent = agent_mgr.create_run(None, "workflow", None).unwrap();

    // Set the parent agent run to the requested status directly.
    conn.execute(
        "UPDATE agent_runs SET status = :status WHERE id = :id",
        named_params! { ":status": parent_status, ":id": parent.id },
    )
    .unwrap();

    // Create the workflow run in 'waiting' status.
    conn.execute(
        "INSERT INTO workflow_runs \
         (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
          started_at, parent_workflow_run_id) \
         VALUES (:run_id, 'test-wf', NULL, :parent_run_id, 'waiting', 0, 'manual', \
                 '2025-01-01T00:00:00Z', NULL)",
        named_params! { ":run_id": run_id, ":parent_run_id": parent.id },
    )
    .unwrap();

    // Insert a waiting gate step.
    let step_id = crate::new_id();
    let started = step_started_at.unwrap_or("2025-01-01T00:00:00Z");
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, \
          gate_type, gate_timeout, started_at) \
         VALUES (:step_id, :run_id, 'approval-gate', 'gate', 0, 'waiting', 1, \
                 'human_approval', :gate_timeout, :started)",
        named_params! { ":step_id": step_id, ":run_id": run_id, ":gate_timeout": gate_timeout, ":started": started },
    )
    .unwrap();

    step_id
}

pub(super) fn make_workflow_run(
    conn: &Connection,
) -> (WorkflowManager<'_>, crate::agent::AgentRun, WorkflowRun) {
    let agent_mgr = AgentManager::new(conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let mgr = WorkflowManager::new(conn);
    let run = mgr
        .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    (mgr, parent, run)
}

/// Helper: set up a temp dir with `.conductor/config.toml` and optional workflow files.
pub(super) fn setup_hooks_dir(config_toml: &str, workflows: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let conductor_dir = dir.path().join(".conductor");
    std::fs::create_dir_all(conductor_dir.join("workflows")).unwrap();
    std::fs::write(conductor_dir.join("config.toml"), config_toml).unwrap();
    for (name, content) in workflows {
        std::fs::write(conductor_dir.join("workflows").join(name), content).unwrap();
    }
    dir
}

/// Helper: create a running workflow run with a parent agent run.
pub(super) fn make_running_wf(conn: &Connection, name: &str) -> (String, String) {
    let agent_mgr = AgentManager::new(conn);
    let wf_mgr = WorkflowManager::new(conn);
    let parent = agent_mgr.create_run(Some("w1"), name, None).unwrap();
    let run = wf_mgr
        .create_workflow_run(name, Some("w1"), &parent.id, false, "manual", None)
        .unwrap();
    wf_mgr
        .update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
        .unwrap();
    (run.id, parent.id)
}

/// Helper: insert a terminal step into a workflow run (auto-generates step ID via WorkflowManager).
pub(super) fn insert_terminal_step(
    conn: &Connection,
    wf_run_id: &str,
    status: WorkflowStepStatus,
    position: i64,
) {
    let wf_mgr = WorkflowManager::new(conn);
    let step_id = wf_mgr
        .insert_step(wf_run_id, "step", "actor", false, position, 0)
        .unwrap();
    wf_mgr
        .update_step_status(&step_id, status, None, None, None, None, None)
        .unwrap();
}

/// Helper: insert a terminal step with an explicit step ID and `ended_at` timestamp.
/// Used by time-gated query tests that need control over the `ended_at` value.
pub(super) fn insert_terminal_step_with_id(
    conn: &Connection,
    step_id: &str,
    run_id: &str,
    status: &str,
    ended_at: &str,
) {
    conn.execute(
        "INSERT INTO workflow_run_steps \
         (id, workflow_run_id, step_name, role, position, status, iteration, ended_at) \
         VALUES (:step_id, :run_id, 'step-a', 'actor', 0, :status, 0, :ended_at)",
        named_params! { ":step_id": step_id, ":run_id": run_id, ":status": status, ":ended_at": ended_at },
    )
    .unwrap();
}
