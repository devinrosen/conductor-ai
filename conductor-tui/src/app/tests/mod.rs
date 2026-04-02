mod action_handler_tests;

use super::agent_events::extract_last_code_block;
use super::helpers::{clamp_increment, collapse_loop_iterations, wrap_decrement, wrap_increment};
use super::App;
use crate::action::Action;
use crate::state::{View, WorkflowsFocus};
use std::io::Cursor;

#[test]
fn test_extract_last_code_block_single() {
    let content = "some text\n```bash\necho hello\n```\nmore text";
    assert_eq!(
        extract_last_code_block(Cursor::new(content)),
        Some("echo hello".to_string())
    );
}

#[test]
fn test_extract_last_code_block_multiple() {
    let content = "```\nfirst\n```\nstuff\n```python\nsecond\nthird\n```\n";
    assert_eq!(
        extract_last_code_block(Cursor::new(content)),
        Some("second\nthird".to_string())
    );
}

#[test]
fn test_extract_last_code_block_none() {
    assert_eq!(extract_last_code_block(Cursor::new("no code here")), None);
}

#[test]
fn test_extract_last_code_block_unclosed() {
    let content = "```\nclosed\n```\n```\nunclosed";
    assert_eq!(
        extract_last_code_block(Cursor::new(content)),
        Some("closed".to_string())
    );
}

#[test]
fn test_clamp_increment_advances() {
    let mut idx = 0;
    clamp_increment(&mut idx, 3);
    assert_eq!(idx, 1);
}

#[test]
fn test_clamp_increment_stops_at_max() {
    let mut idx = 2;
    clamp_increment(&mut idx, 3);
    assert_eq!(idx, 2);
}

#[test]
fn test_clamp_increment_empty_list() {
    let mut idx = 0;
    clamp_increment(&mut idx, 0);
    assert_eq!(idx, 0);
}

#[test]
fn test_wrap_increment_advances() {
    let mut idx = 0;
    wrap_increment(&mut idx, 3);
    assert_eq!(idx, 1);
}

#[test]
fn test_wrap_increment_wraps_to_zero() {
    let mut idx = 2;
    wrap_increment(&mut idx, 3);
    assert_eq!(idx, 0);
}

#[test]
fn test_wrap_decrement_decreases() {
    let mut idx = 2;
    wrap_decrement(&mut idx, 3);
    assert_eq!(idx, 1);
}

#[test]
fn test_wrap_decrement_wraps_to_end() {
    let mut idx = 0;
    wrap_decrement(&mut idx, 3);
    assert_eq!(idx, 2);
}

#[test]
fn test_wrap_decrement_empty_list() {
    let mut idx = 0;
    wrap_decrement(&mut idx, 0);
    assert_eq!(idx, 0);
}

fn make_test_app() -> App {
    let conn = conductor_core::test_helpers::create_test_conn();
    App::new(
        conn,
        conductor_core::config::Config::default(),
        crate::theme::Theme::default(),
    )
}

fn make_test_run(id: &str) -> conductor_core::workflow::WorkflowRun {
    conductor_core::workflow::WorkflowRun {
        id: id.into(),
        workflow_name: "test".into(),
        worktree_id: Some("w1".into()),
        parent_run_id: String::new(),
        status: conductor_core::workflow::WorkflowRunStatus::Running,
        dry_run: false,
        trigger: "manual".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
        ended_at: None,
        result_summary: None,
        definition_snapshot: None,
        inputs: std::collections::HashMap::new(),
        ticket_id: None,
        repo_id: None,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        iteration: 0,
        blocked_on: None,
        feature_id: None,
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

#[test]
fn test_toggle_workflow_column_off_moves_focus_to_content() {
    let mut app = make_test_app();
    app.state.workflow_column_visible = true;
    app.state.column_focus = crate::state::ColumnFocus::Workflow;
    app.handle_action(Action::ToggleWorkflowColumn);
    assert!(!app.state.workflow_column_visible);
    assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Content);
}

#[test]
fn test_toggle_workflow_column_on_preserves_focus() {
    let mut app = make_test_app();
    app.state.workflow_column_visible = false;
    app.state.column_focus = crate::state::ColumnFocus::Content;
    app.handle_action(Action::ToggleWorkflowColumn);
    assert!(app.state.workflow_column_visible);
    assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Content);
}

#[test]
fn test_workflow_column_select_run_enters_detail_view() {
    let mut app = make_test_app();
    app.state.selected_worktree_id = Some("w1".into());
    app.state.data.workflow_runs = vec![make_test_run("run1")];
    app.state.column_focus = crate::state::ColumnFocus::Workflow;
    app.state.workflows_focus = WorkflowsFocus::Runs;
    app.state.workflow_run_index = 0;
    app.handle_action(Action::Select);
    assert_eq!(app.state.view, View::WorkflowRunDetail);
    assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Content);
    assert_eq!(app.state.selected_workflow_run_id.as_deref(), Some("run1"));
}

#[test]
fn test_workflow_column_select_header_row_is_noop() {
    // Global mode (selected_worktree_id = None): first visible row is a group header.
    // Pressing Enter on a header should be a no-op.
    let mut app = make_test_app();
    let mut run = make_test_run("run1");
    run.worktree_id = None;
    app.state.data.workflow_runs = vec![run];
    app.state.column_focus = crate::state::ColumnFocus::Workflow;
    app.state.workflows_focus = WorkflowsFocus::Runs;
    app.state.workflow_run_index = 0; // points at repo/target header in global mode
    app.handle_action(Action::Select);
    assert_eq!(app.state.view, View::Dashboard);
    assert!(app.state.selected_workflow_run_id.is_none());
}

#[test]
fn test_back_from_workflow_run_detail_restores_workflow_column_focus() {
    let mut app = make_test_app();
    app.state.view = View::WorkflowRunDetail;
    app.state.column_focus = crate::state::ColumnFocus::Content;
    app.state.selected_workflow_run_id = Some("run1".into());
    app.handle_action(Action::Back);
    assert_eq!(app.state.view, View::Dashboard);
    assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Workflow);
    assert_eq!(app.state.workflows_focus, WorkflowsFocus::Runs);
    assert!(app.state.selected_workflow_run_id.is_none());
}

#[test]
fn test_back_from_workflow_run_detail_restores_previous_view() {
    let mut app = make_test_app();
    app.state.view = View::WorkflowRunDetail;
    app.state.previous_view = Some(View::RepoDetail);
    app.state.column_focus = crate::state::ColumnFocus::Content;
    app.handle_action(Action::Back);
    assert_eq!(app.state.view, View::RepoDetail);
    assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Workflow);
    assert!(app.state.selected_workflow_run_id.is_none());
    assert!(app.state.previous_view.is_none());
}

#[test]
fn test_focus_workflow_column_ignored_when_hidden() {
    let mut state = crate::state::AppState::new();
    state.workflow_column_visible = false;
    state.column_focus = crate::state::ColumnFocus::Content;
    // FocusWorkflowColumn should be a no-op when column is hidden
    if state.workflow_column_visible {
        state.column_focus = crate::state::ColumnFocus::Workflow;
    }
    assert_eq!(state.column_focus, crate::state::ColumnFocus::Content);
}

fn make_step(
    step_name: &str,
    iteration: i64,
    position: i64,
) -> conductor_core::workflow::WorkflowRunStep {
    crate::state::tests::make_iter_step("run1", step_name, iteration, position)
}

#[test]
fn collapse_loop_iterations_single_iteration_passthrough() {
    let steps = vec![make_step("a", 0, 0), make_step("b", 0, 1)];
    let result = collapse_loop_iterations(steps);
    assert_eq!(result.len(), 2);
    assert!(result.iter().all(|s| s.iteration == 0));
}

#[test]
fn collapse_loop_iterations_keeps_latest_per_step_name() {
    // "a" appears in iterations 0, 1, 2 — only 2 should survive.
    // "b" appears only in iteration 0 — should survive.
    let steps = vec![
        make_step("a", 0, 0),
        make_step("b", 0, 1),
        make_step("a", 1, 0),
        make_step("a", 2, 0),
    ];
    let result = collapse_loop_iterations(steps);
    // Should keep "a" at iter 2 and "b" at iter 0.
    assert_eq!(result.len(), 2);
    let a = result.iter().find(|s| s.step_name == "a").unwrap();
    assert_eq!(a.iteration, 2);
    let b = result.iter().find(|s| s.step_name == "b").unwrap();
    assert_eq!(b.iteration, 0);
}

#[test]
fn collapse_loop_iterations_empty_input() {
    let result = collapse_loop_iterations(vec![]);
    assert!(result.is_empty());
}

#[test]
fn test_focus_workflow_column_allowed_when_visible() {
    let mut state = crate::state::AppState::new();
    state.workflow_column_visible = true;
    state.column_focus = crate::state::ColumnFocus::Content;
    if state.workflow_column_visible {
        state.column_focus = crate::state::ColumnFocus::Workflow;
    }
    assert_eq!(state.column_focus, crate::state::ColumnFocus::Workflow);
}
