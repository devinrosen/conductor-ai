use super::*;

#[test]
fn repo_detail_focus_next_cycles_forward() {
    assert_eq!(RepoDetailFocus::Info.next(), RepoDetailFocus::Worktrees);
    assert_eq!(RepoDetailFocus::Worktrees.next(), RepoDetailFocus::Prs);
    assert_eq!(RepoDetailFocus::Prs.next(), RepoDetailFocus::Tickets);
    assert_eq!(RepoDetailFocus::Tickets.next(), RepoDetailFocus::RepoAgent);
    assert_eq!(RepoDetailFocus::RepoAgent.next(), RepoDetailFocus::Info);
}

#[test]
fn repo_detail_focus_prev_cycles_backward() {
    assert_eq!(RepoDetailFocus::Info.prev(), RepoDetailFocus::RepoAgent);
    assert_eq!(RepoDetailFocus::Worktrees.prev(), RepoDetailFocus::Info);
    assert_eq!(RepoDetailFocus::Prs.prev(), RepoDetailFocus::Worktrees);
    assert_eq!(RepoDetailFocus::Tickets.prev(), RepoDetailFocus::Prs);
    assert_eq!(RepoDetailFocus::RepoAgent.prev(), RepoDetailFocus::Tickets);
}

#[test]
fn repo_detail_focus_next_prev_are_inverses() {
    for focus in [
        RepoDetailFocus::Info,
        RepoDetailFocus::Worktrees,
        RepoDetailFocus::Tickets,
        RepoDetailFocus::Prs,
        RepoDetailFocus::RepoAgent,
    ] {
        assert_eq!(focus.next().prev(), focus);
        assert_eq!(focus.prev().next(), focus);
    }
}

#[test]
fn workflows_focus_next_for_gates_with_gates() {
    assert_eq!(
        WorkflowsFocus::Gates.next_for_gates(true),
        WorkflowsFocus::Runs
    );
    assert_eq!(
        WorkflowsFocus::Runs.next_for_gates(true),
        WorkflowsFocus::Defs
    );
    assert_eq!(
        WorkflowsFocus::Defs.next_for_gates(true),
        WorkflowsFocus::Gates
    );
}

#[test]
fn workflows_focus_next_for_gates_without_gates() {
    assert_eq!(
        WorkflowsFocus::Runs.next_for_gates(false),
        WorkflowsFocus::Defs
    );
    assert_eq!(
        WorkflowsFocus::Defs.next_for_gates(false),
        WorkflowsFocus::Runs
    );
}

#[test]
fn workflows_focus_prev_for_gates_with_gates() {
    assert_eq!(
        WorkflowsFocus::Defs.prev_for_gates(true),
        WorkflowsFocus::Runs
    );
    assert_eq!(
        WorkflowsFocus::Runs.prev_for_gates(true),
        WorkflowsFocus::Gates
    );
    assert_eq!(
        WorkflowsFocus::Gates.prev_for_gates(true),
        WorkflowsFocus::Defs
    );
}

#[test]
fn workflows_focus_prev_for_gates_without_gates() {
    assert_eq!(
        WorkflowsFocus::Defs.prev_for_gates(false),
        WorkflowsFocus::Runs
    );
    assert_eq!(
        WorkflowsFocus::Runs.prev_for_gates(false),
        WorkflowsFocus::Defs
    );
}

#[test]
fn workflow_run_detail_focus_next_with_agent_no_error() {
    assert_eq!(
        WorkflowRunDetailFocus::Info.next(true, false),
        WorkflowRunDetailFocus::Steps
    );
    assert_eq!(
        WorkflowRunDetailFocus::Steps.next(true, false),
        WorkflowRunDetailFocus::AgentActivity
    );
    assert_eq!(
        WorkflowRunDetailFocus::AgentActivity.next(true, false),
        WorkflowRunDetailFocus::Info
    );
}

#[test]
fn workflow_run_detail_focus_next_without_agent_no_error() {
    assert_eq!(
        WorkflowRunDetailFocus::Info.next(false, false),
        WorkflowRunDetailFocus::Steps
    );
    assert_eq!(
        WorkflowRunDetailFocus::Steps.next(false, false),
        WorkflowRunDetailFocus::Info
    );
}

#[test]
fn workflow_run_detail_focus_next_with_error() {
    assert_eq!(
        WorkflowRunDetailFocus::Info.next(false, true),
        WorkflowRunDetailFocus::Error
    );
    assert_eq!(
        WorkflowRunDetailFocus::Error.next(false, true),
        WorkflowRunDetailFocus::Steps
    );
    assert_eq!(
        WorkflowRunDetailFocus::Steps.next(false, true),
        WorkflowRunDetailFocus::Info
    );
}

#[test]
fn workflow_run_detail_focus_next_with_agent_and_error() {
    assert_eq!(
        WorkflowRunDetailFocus::Info.next(true, true),
        WorkflowRunDetailFocus::Error
    );
    assert_eq!(
        WorkflowRunDetailFocus::Error.next(true, true),
        WorkflowRunDetailFocus::Steps
    );
    assert_eq!(
        WorkflowRunDetailFocus::Steps.next(true, true),
        WorkflowRunDetailFocus::AgentActivity
    );
    assert_eq!(
        WorkflowRunDetailFocus::AgentActivity.next(true, true),
        WorkflowRunDetailFocus::Info
    );
}

#[test]
fn workflow_run_detail_focus_prev_with_agent_no_error() {
    assert_eq!(
        WorkflowRunDetailFocus::Info.prev(true, false),
        WorkflowRunDetailFocus::AgentActivity
    );
    assert_eq!(
        WorkflowRunDetailFocus::Steps.prev(true, false),
        WorkflowRunDetailFocus::Info
    );
    assert_eq!(
        WorkflowRunDetailFocus::AgentActivity.prev(true, false),
        WorkflowRunDetailFocus::Steps
    );
}

#[test]
fn workflow_run_detail_focus_prev_without_agent_no_error() {
    assert_eq!(
        WorkflowRunDetailFocus::Info.prev(false, false),
        WorkflowRunDetailFocus::Steps
    );
    assert_eq!(
        WorkflowRunDetailFocus::Steps.prev(false, false),
        WorkflowRunDetailFocus::Info
    );
}

#[test]
fn workflow_run_detail_focus_prev_with_error() {
    assert_eq!(
        WorkflowRunDetailFocus::Info.prev(false, true),
        WorkflowRunDetailFocus::Steps
    );
    assert_eq!(
        WorkflowRunDetailFocus::Steps.prev(false, true),
        WorkflowRunDetailFocus::Error
    );
    assert_eq!(
        WorkflowRunDetailFocus::Error.prev(false, true),
        WorkflowRunDetailFocus::Info
    );
}

#[test]
fn workflow_run_detail_focus_next_prev_are_inverses() {
    for has_agent in [true, false] {
        for has_error in [true, false] {
            let variants: Vec<WorkflowRunDetailFocus> = {
                let mut v = vec![WorkflowRunDetailFocus::Info];
                if has_error {
                    v.push(WorkflowRunDetailFocus::Error);
                }
                v.push(WorkflowRunDetailFocus::Steps);
                if has_agent {
                    v.push(WorkflowRunDetailFocus::AgentActivity);
                }
                v
            };
            for focus in variants {
                assert_eq!(
                    focus.next(has_agent, has_error).prev(has_agent, has_error),
                    focus
                );
                assert_eq!(
                    focus.prev(has_agent, has_error).next(has_agent, has_error),
                    focus
                );
            }
        }
    }
}

// --- ColumnFocus navigation tests ---

#[test]
fn focused_index_and_len_workflow_column_defs() {
    let mut state = AppState::new();
    state.column_focus = ColumnFocus::Workflow;
    state.workflows_focus = WorkflowsFocus::Defs;
    state.workflow_def_index = 2;
    let (idx, len) = state.focused_index_and_len();
    assert_eq!(idx, 2);
    assert_eq!(len, state.data.workflow_defs.len());
}

#[test]
fn focused_index_and_len_workflow_column_runs() {
    let mut state = AppState::new();
    state.column_focus = ColumnFocus::Workflow;
    state.workflows_focus = WorkflowsFocus::Runs;
    state.workflow_run_index = 1;
    state.selected_worktree_id = Some("w1".into());
    state.data.workflow_runs = vec![make_wf_run_full("r1", WorkflowRunStatus::Running, None)];
    state.rebuild_workflow_run_rows();
    let (idx, len) = state.focused_index_and_len();
    assert_eq!(idx, 1);
    assert_eq!(len, 1);
}

#[test]
fn focused_index_and_len_content_column_not_affected_by_workflow_index() {
    let mut state = AppState::new();
    state.column_focus = ColumnFocus::Content;
    state.workflows_focus = WorkflowsFocus::Runs;
    state.workflow_run_index = 99;
    let (idx, len) = state.focused_index_and_len();
    assert_eq!(idx, 0);
    assert_eq!(len, 0);
}

#[test]
fn set_focused_index_workflow_column_defs() {
    let mut state = AppState::new();
    state.column_focus = ColumnFocus::Workflow;
    state.workflows_focus = WorkflowsFocus::Defs;
    state.set_focused_index(3);
    assert_eq!(state.workflow_def_index, 3);
}

#[test]
fn set_focused_index_workflow_column_runs() {
    let mut state = AppState::new();
    state.column_focus = ColumnFocus::Workflow;
    state.workflows_focus = WorkflowsFocus::Runs;
    state.set_focused_index(7);
    assert_eq!(state.workflow_run_index, 7);
}

#[test]
fn set_focused_index_content_column_does_not_touch_workflow_indices() {
    let mut state = AppState::new();
    state.column_focus = ColumnFocus::Content;
    state.workflows_focus = WorkflowsFocus::Defs;
    state.workflow_def_index = 5;
    state.set_focused_index(2);
    assert_eq!(state.workflow_def_index, 5);
    assert_eq!(state.dashboard_index, 2);
}

// --- WorkflowPickerTarget::target_filter tests ---

#[test]
fn target_filter_pr() {
    let t = WorkflowPickerTarget::Pr {
        pr_number: 1,
        pr_title: String::new(),
    };
    assert_eq!(t.target_filter(), "pr");
}

#[test]
fn target_filter_worktree() {
    let t = WorkflowPickerTarget::Worktree {
        worktree_id: String::new(),
        worktree_path: String::new(),
        repo_path: String::new(),
    };
    assert_eq!(t.target_filter(), "worktree");
}

#[test]
fn target_filter_ticket() {
    let t = WorkflowPickerTarget::Ticket {
        ticket_id: String::new(),
        ticket_title: String::new(),
        ticket_url: String::new(),
        repo_id: String::new(),
        repo_path: String::new(),
    };
    assert_eq!(t.target_filter(), "ticket");
}

#[test]
fn target_filter_repo() {
    let t = WorkflowPickerTarget::Repo {
        repo_id: String::new(),
        repo_path: String::new(),
        repo_name: String::new(),
    };
    assert_eq!(t.target_filter(), "repo");
}

#[test]
fn target_filter_workflow_run() {
    let t = WorkflowPickerTarget::WorkflowRun {
        workflow_run_id: String::new(),
        workflow_name: String::new(),
        worktree_id: None,
        worktree_path: None,
        repo_path: String::new(),
    };
    assert_eq!(t.target_filter(), "workflow_run");
}

#[test]
fn target_filter_post_create_maps_to_worktree() {
    let t = WorkflowPickerTarget::PostCreate {
        worktree_id: String::new(),
        worktree_path: String::new(),
        worktree_slug: String::new(),
        ticket_id: String::new(),
        repo_path: String::new(),
    };
    assert_eq!(t.target_filter(), "worktree");
}

// --- selected_run_has_error tests ---

#[test]
fn selected_run_has_error_no_selected_run() {
    let state = AppState::new();
    assert!(!state.selected_run_has_error());
}

#[test]
fn selected_run_has_error_run_not_found() {
    let mut state = AppState::new();
    state.selected_workflow_run_id = Some("nonexistent".to_string());
    assert!(!state.selected_run_has_error());
}

#[test]
fn selected_run_has_error_failed_with_summary() {
    let mut state = AppState::new();
    let run = make_workflow_run("run1", WorkflowRunStatus::Failed, Some("step X failed"));
    state.data.workflow_runs.push(run);
    state.selected_workflow_run_id = Some("run1".to_string());
    assert!(state.selected_run_has_error());
}

#[test]
fn selected_run_has_error_failed_empty_summary() {
    let mut state = AppState::new();
    let run = make_workflow_run("run2", WorkflowRunStatus::Failed, Some(""));
    state.data.workflow_runs.push(run);
    state.selected_workflow_run_id = Some("run2".to_string());
    assert!(!state.selected_run_has_error());
}

#[test]
fn selected_run_has_error_completed_with_summary() {
    let mut state = AppState::new();
    let run = make_workflow_run("run3", WorkflowRunStatus::Completed, Some("all good"));
    state.data.workflow_runs.push(run);
    state.selected_workflow_run_id = Some("run3".to_string());
    assert!(!state.selected_run_has_error());
}
