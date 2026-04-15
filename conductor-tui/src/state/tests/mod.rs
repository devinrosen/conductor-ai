use super::*;
use conductor_core::agent::AgentRunEvent;
use conductor_core::workflow::{WorkflowRun, WorkflowRunStatus};

mod dashboard_tests;
mod enum_tests;
mod filter_tests;
mod tree_tests;
mod visual_row_tests;
mod workflow_hidden_tests;
mod workflow_run_tests;

pub(crate) fn make_event(id: &str, run_id: &str) -> AgentRunEvent {
    AgentRunEvent {
        id: id.to_string(),
        run_id: run_id.to_string(),
        kind: "tool_use".to_string(),
        summary: "test".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        ended_at: None,
        metadata: None,
    }
}

pub(crate) fn make_ticket(id: &str, state: &str) -> conductor_core::tickets::Ticket {
    conductor_core::tickets::Ticket {
        id: id.to_string(),
        repo_id: "repo-1".to_string(),
        source_type: "github".to_string(),
        source_id: id.to_string(),
        title: format!("Ticket {id}"),
        body: String::new(),
        state: state.to_string(),
        labels: String::new(),
        assignee: None,
        priority: None,
        url: String::new(),
        synced_at: "2026-01-01T00:00:00Z".to_string(),
        raw_json: String::new(),
        workflow: None,
        agent_map: None,
        workflow_completed: false,
    }
}

pub(crate) fn make_repo(id: &str, slug: &str) -> conductor_core::repo::Repo {
    conductor_core::repo::Repo {
        id: id.into(),
        slug: slug.into(),
        local_path: String::new(),
        remote_url: String::new(),
        default_branch: "main".into(),
        workspace_dir: String::new(),
        created_at: String::new(),
        model: None,
        allow_agent_issue_creation: false,
    }
}

pub(crate) fn make_worktree(
    id: &str,
    repo_id: &str,
    base_branch: Option<&str>,
    status: conductor_core::worktree::WorktreeStatus,
) -> conductor_core::worktree::Worktree {
    conductor_core::worktree::Worktree {
        id: id.into(),
        repo_id: repo_id.into(),
        slug: id.into(),
        branch: format!("feat/{id}"),
        path: String::new(),
        ticket_id: None,
        status,
        created_at: String::new(),
        completed_at: None,
        model: None,
        base_branch: base_branch.map(|s| s.to_string()),
    }
}

pub(crate) fn make_wf_run_full(
    id: &str,
    status: WorkflowRunStatus,
    parent_workflow_run_id: Option<&str>,
) -> WorkflowRun {
    WorkflowRun {
        id: id.into(),
        workflow_name: "test-workflow".into(),
        worktree_id: None,
        parent_run_id: "run-1".into(),
        status,
        dry_run: false,
        trigger: "manual".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
        ended_at: None,
        result_summary: None,
        error: None,
        definition_snapshot: None,
        inputs: std::collections::HashMap::new(),
        ticket_id: None,
        repo_id: None,
        parent_workflow_run_id: parent_workflow_run_id.map(|s| s.into()),
        target_label: None,
        default_bot_name: None,
        iteration: 0,
        blocked_on: None,
        feature_id: None,
        workflow_title: None,
        total_input_tokens: None,
        total_output_tokens: None,
        total_cache_read_input_tokens: None,
        total_cache_creation_input_tokens: None,
        total_turns: None,
        total_cost_usd: None,
        total_duration_ms: None,
        model: None,
    }
}

/// Helper: create a WorkflowRun with a specific iteration and workflow_name.
pub(crate) fn make_wf_run_with_iter(
    id: &str,
    status: WorkflowRunStatus,
    parent_workflow_run_id: Option<&str>,
    workflow_name: &str,
    iteration: i64,
) -> WorkflowRun {
    let mut run = make_wf_run_full(id, status, parent_workflow_run_id);
    run.workflow_name = workflow_name.into();
    run.iteration = iteration;
    run
}

pub(crate) fn make_wf_step(
    id: &str,
    run_id: &str,
    step_name: &str,
    position: i64,
) -> conductor_core::workflow::WorkflowRunStep {
    conductor_core::workflow::WorkflowRunStep {
        id: id.into(),
        workflow_run_id: run_id.into(),
        step_name: step_name.into(),
        role: "actor".into(),
        status: conductor_core::workflow::WorkflowStepStatus::Completed,
        position,
        ..Default::default()
    }
}

pub(crate) fn make_picker_item(
    branch: Option<&str>,
    base_branch: Option<&str>,
) -> BranchPickerItem {
    BranchPickerItem {
        branch: branch.map(|s| s.to_string()),
        worktree_count: 0,
        ticket_count: 0,
        base_branch: base_branch.map(|s| s.to_string()),
        stale_days: None,
    }
}

pub(crate) fn make_wt(
    branch: &str,
    base_branch: Option<&str>,
) -> conductor_core::worktree::Worktree {
    conductor_core::worktree::Worktree {
        id: branch.to_string(),
        repo_id: "r1".to_string(),
        slug: branch.replace('/', "-"),
        branch: branch.to_string(),
        path: format!("/tmp/{branch}"),
        ticket_id: None,
        status: conductor_core::worktree::WorktreeStatus::Active,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        completed_at: None,
        model: None,
        base_branch: base_branch.map(|s| s.to_string()),
    }
}

pub(crate) fn make_workflow_run(
    id: &str,
    status: WorkflowRunStatus,
    summary: Option<&str>,
) -> WorkflowRun {
    let mut run = make_wf_run_full(id, status, None);
    run.workflow_name = "test".to_string();
    run.parent_run_id = String::new();
    run.result_summary = summary.map(|s| s.to_string());
    run.error = summary.map(|s| s.to_string());
    run
}

pub(crate) fn make_iter_step(
    run_id: &str,
    step_name: &str,
    iteration: i64,
    position: i64,
) -> conductor_core::workflow::WorkflowRunStep {
    let id = format!("{run_id}-{step_name}-{iteration}");
    let mut step = make_wf_step(&id, run_id, step_name, position);
    step.role = "agent".to_string();
    step.iteration = iteration;
    step
}

/// Helper: put state into single-worktree (non-global) mode.
pub(crate) fn set_worktree_mode(state: &mut AppState) {
    state.selected_worktree_id = Some("wt-id".into());
}
