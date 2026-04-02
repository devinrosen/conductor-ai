use super::super::helpers::advance_form_field;
use super::super::App;
use crate::action::Action;
use crate::state::{FormField, Modal, View};
use crate::theme::Theme;
use conductor_core::config::Config;

fn make_app() -> App {
    let conn = conductor_core::db::open_database(std::path::Path::new(":memory:")).unwrap();
    App::new(conn, Config::default(), Theme::default())
}

// Action::Quit with an open modal immediately sets should_quit = true
// (bypasses the confirm dialog which only shows when modal is None).
#[test]
fn quit_sets_should_quit() {
    let mut app = make_app();
    app.state.modal = Modal::Help;
    app.update(Action::Quit);
    assert!(app.state.should_quit);
}

#[test]
fn help_modal_opens_and_dismisses() {
    let mut app = make_app();
    assert!(matches!(app.state.modal, Modal::None));

    app.update(Action::ShowHelp);
    assert!(matches!(app.state.modal, Modal::Help));

    app.update(Action::DismissModal);
    assert!(matches!(app.state.modal, Modal::None));
}

#[test]
fn filter_state_lifecycle() {
    let mut app = make_app();

    // Enter filter mode
    app.update(Action::EnterFilter);
    assert!(app.state.filter.active);
    assert!(app.state.filter.text.is_empty());

    // Type two chars
    app.update(Action::FilterChar('f'));
    app.update(Action::FilterChar('o'));
    assert_eq!(app.state.filter.text, "fo");

    // Backspace removes one char
    app.update(Action::FilterBackspace);
    assert_eq!(app.state.filter.text, "f");

    // Exit clears active flag (text is preserved until next Enter)
    app.update(Action::ExitFilter);
    assert!(!app.state.filter.active);
}

#[test]
fn worktree_created_action_updates_status() {
    let mut app = make_app();
    app.update(Action::WorktreeCreated {
        wt_id: "01TEST".to_string(),
        wt_path: "/tmp/my-wt".to_string(),
        wt_slug: "my-wt".to_string(),
        wt_repo_id: "01REPO".to_string(),
        warnings: vec![],
        ticket_id: None,
    });
    assert!(matches!(app.state.modal, Modal::None));
    assert!(app.state.status_message.is_some());
    let msg = app.state.status_message.as_deref().unwrap();
    assert!(msg.contains("my-wt"), "expected wt slug in message: {msg}");
}

#[test]
fn data_refreshed_updates_repos() {
    let mut app = make_app();
    assert!(app.state.data.repos.is_empty());

    let repos = vec![
        conductor_core::repo::Repo {
            id: "01AAA".to_string(),
            slug: "repo-a".to_string(),
            local_path: "/tmp/repo-a".to_string(),
            remote_url: "https://github.com/x/a".to_string(),
            default_branch: "main".to_string(),
            workspace_dir: "/tmp".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            model: None,
            allow_agent_issue_creation: false,
        },
        conductor_core::repo::Repo {
            id: "01BBB".to_string(),
            slug: "repo-b".to_string(),
            local_path: "/tmp/repo-b".to_string(),
            remote_url: "https://github.com/x/b".to_string(),
            default_branch: "main".to_string(),
            workspace_dir: "/tmp".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            model: None,
            allow_agent_issue_creation: false,
        },
    ];

    app.update(Action::DataRefreshed(Box::new(
        crate::action::DataRefreshedPayload {
            repos,
            worktrees: vec![],
            tickets: vec![],
            ticket_labels: std::collections::HashMap::new(),
            latest_agent_runs: std::collections::HashMap::new(),
            ticket_agent_totals: std::collections::HashMap::new(),
            latest_workflow_runs_by_worktree: std::collections::HashMap::new(),
            workflow_step_summaries: std::collections::HashMap::new(),
            active_non_worktree_workflow_runs: vec![],
            pending_feedback_requests: vec![],
            waiting_gate_steps: vec![],
            live_turns_by_worktree: std::collections::HashMap::new(),
            features_by_repo: std::collections::HashMap::new(),
            unread_notification_count: 0,
            latest_repo_agent_runs: std::collections::HashMap::new(),
            worktree_agent_events: std::collections::HashMap::new(),
            repo_agent_events: std::collections::HashMap::new(),
        },
    )));

    assert_eq!(app.state.data.repos.len(), 2);
}

#[test]
fn confirm_no_clears_modal_without_side_effect() {
    let mut app = make_app();
    app.state.modal = Modal::Confirm {
        title: "Delete?".to_string(),
        message: "Are you sure?".to_string(),
        on_confirm: crate::state::ConfirmAction::Quit,
    };
    app.update(Action::ConfirmNo);
    assert!(matches!(app.state.modal, Modal::None));
    assert!(
        !app.state.should_quit,
        "ConfirmNo must not trigger the action"
    );
}

#[test]
fn workflow_data_refreshed_populates_declared_inputs() {
    let mut app = make_app();
    assert!(app.state.data.workflow_run_declared_inputs.is_empty());

    // A minimal workflow DSL snapshot that declares one required input.
    let snapshot = r#"
workflow my-wf {
    meta { trigger = "manual" targets = ["worktree"] }
    inputs {
        pr_url required
    }
    call agent
}
"#;

    let mut run = conductor_core::workflow::WorkflowRun {
        id: "run-abc".to_string(),
        workflow_name: "my-wf".to_string(),
        worktree_id: None,
        parent_run_id: String::new(),
        status: conductor_core::workflow::WorkflowRunStatus::Running,
        dry_run: false,
        trigger: "manual".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        ended_at: None,
        result_summary: None,
        definition_snapshot: Some(snapshot.to_string()),
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
    };
    run.inputs
        .insert("pr_url".to_string(), "https://example.com".to_string());

    app.update(Action::WorkflowDataRefreshed(Box::new(
        crate::action::WorkflowDataPayload {
            workflow_defs: None,
            workflow_def_slugs: None,
            workflow_runs: vec![run],
            workflow_steps: vec![],
            step_agent_events: vec![],
            step_agent_run: None,
            workflow_parse_warnings: vec![],
            all_run_steps: std::collections::HashMap::new(),
        },
    )));

    let decls = app
        .state
        .data
        .workflow_run_declared_inputs
        .get("run-abc")
        .expect("declared inputs should be populated for run-abc");
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0].name, "pr_url");
    assert!(decls[0].required);
}

fn make_field(readonly: bool) -> FormField {
    FormField {
        label: String::new(),
        value: String::new(),
        placeholder: String::new(),
        manually_edited: false,
        required: false,
        readonly,
        field_type: crate::state::FormFieldType::Text,
    }
}

#[test]
fn test_advance_form_field_forward_skips_readonly() {
    // [editable, readonly, editable] — from 0 forward should land on 2
    let fields = vec![make_field(false), make_field(true), make_field(false)];
    assert_eq!(advance_form_field(&fields, 0, true), Some(2));
}

#[test]
fn test_advance_form_field_backward_skips_readonly() {
    // [editable, readonly, editable] — from 2 backward should land on 0
    let fields = vec![make_field(false), make_field(true), make_field(false)];
    assert_eq!(advance_form_field(&fields, 2, false), Some(0));
}

#[test]
fn test_advance_form_field_wraps_forward() {
    // [editable, editable, editable] — from last position wraps to 0
    let fields = vec![make_field(false), make_field(false), make_field(false)];
    assert_eq!(advance_form_field(&fields, 2, true), Some(0));
}

#[test]
fn test_advance_form_field_wraps_backward() {
    // [editable, editable] — from 0 backward wraps to last
    let fields = vec![make_field(false), make_field(false)];
    assert_eq!(advance_form_field(&fields, 0, false), Some(1));
}

#[test]
fn test_advance_form_field_all_readonly_returns_none() {
    let fields = vec![make_field(true), make_field(true), make_field(true)];
    assert_eq!(advance_form_field(&fields, 0, true), None);
    assert_eq!(advance_form_field(&fields, 0, false), None);
}

#[test]
fn test_advance_form_field_empty_returns_none() {
    let fields: Vec<FormField> = vec![];
    assert_eq!(advance_form_field(&fields, 0, true), None);
    assert_eq!(advance_form_field(&fields, 0, false), None);
}

#[test]
fn test_advance_form_field_only_start_editable() {
    // All others are readonly — should stay at start
    let fields = vec![make_field(false), make_field(true), make_field(true)];
    assert_eq!(advance_form_field(&fields, 0, true), Some(0));
    assert_eq!(advance_form_field(&fields, 0, false), Some(0));
}

// ═══════════════════════════════════════════════════════════════════════
// Task 2: Navigation tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn back_from_repo_detail_goes_to_dashboard() {
    let mut app = make_app();
    app.state.view = View::RepoDetail;
    app.state.selected_repo_id = Some("r1".into());
    app.update(Action::Back);
    assert_eq!(app.state.view, View::Dashboard);
    assert!(app.state.selected_repo_id.is_none());
}

#[test]
fn back_from_worktree_detail_with_repo_goes_to_repo_detail() {
    let mut app = make_app();
    app.state.view = View::WorktreeDetail;
    app.state.previous_view = Some(View::RepoDetail);
    app.state.selected_repo_id = Some("r1".into());
    app.state.selected_worktree_id = Some("w1".into());
    app.update(Action::Back);
    assert_eq!(app.state.view, View::RepoDetail);
    assert!(app.state.selected_worktree_id.is_none());
}

#[test]
fn back_from_worktree_detail_without_repo_goes_to_dashboard() {
    let mut app = make_app();
    app.state.view = View::WorktreeDetail;
    app.state.previous_view = Some(View::Dashboard);
    app.state.selected_worktree_id = Some("w1".into());
    app.update(Action::Back);
    assert_eq!(app.state.view, View::Dashboard);
    assert!(app.state.selected_worktree_id.is_none());
}

#[test]
fn back_from_workflow_def_detail_restores_previous_view() {
    let mut app = make_app();
    app.state.view = View::WorkflowDefDetail;
    app.state.previous_view = Some(View::RepoDetail);
    app.update(Action::Back);
    assert_eq!(app.state.view, View::RepoDetail);
    assert!(app.state.selected_workflow_def.is_none());
    assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Workflow);
    assert_eq!(
        app.state.workflows_focus,
        crate::state::WorkflowsFocus::Defs
    );
}

#[test]
fn back_from_workflow_step_tree_exits_pane_not_view() {
    let mut app = make_app();
    app.state.column_focus = crate::state::ColumnFocus::Workflow;
    app.state.workflows_focus = crate::state::WorkflowsFocus::Defs;
    app.state.workflow_def_focus = crate::state::WorkflowDefFocus::Steps;
    app.state.view = View::Dashboard;
    app.update(Action::Back);
    // Should exit the step tree pane, not the view
    assert_eq!(
        app.state.workflow_def_focus,
        crate::state::WorkflowDefFocus::List
    );
    assert_eq!(app.state.view, View::Dashboard);
}

#[test]
fn next_panel_cycles_repo_detail_focus() {
    let mut app = make_app();
    app.state.view = View::RepoDetail;
    app.state.column_focus = crate::state::ColumnFocus::Content;
    app.state.repo_detail_focus = crate::state::RepoDetailFocus::Info;
    // Cycle: Info → Worktrees → Prs → Tickets → Info
    app.update(Action::NextPanel);
    assert_eq!(
        app.state.repo_detail_focus,
        crate::state::RepoDetailFocus::Worktrees
    );
    app.update(Action::NextPanel);
    assert_eq!(
        app.state.repo_detail_focus,
        crate::state::RepoDetailFocus::Prs
    );
    app.update(Action::NextPanel);
    assert_eq!(
        app.state.repo_detail_focus,
        crate::state::RepoDetailFocus::Tickets
    );
}

#[test]
fn prev_panel_cycles_repo_detail_focus_backward() {
    let mut app = make_app();
    app.state.view = View::RepoDetail;
    app.state.column_focus = crate::state::ColumnFocus::Content;
    app.state.repo_detail_focus = crate::state::RepoDetailFocus::Worktrees;
    app.update(Action::PrevPanel);
    assert_eq!(
        app.state.repo_detail_focus,
        crate::state::RepoDetailFocus::Info
    );
}

#[test]
fn next_panel_toggles_worktree_detail_focus() {
    let mut app = make_app();
    app.state.view = View::WorktreeDetail;
    app.state.column_focus = crate::state::ColumnFocus::Content;
    app.state.worktree_detail_focus = crate::state::WorktreeDetailFocus::InfoPanel;
    app.update(Action::NextPanel);
    assert_eq!(
        app.state.worktree_detail_focus,
        crate::state::WorktreeDetailFocus::LogPanel
    );
    app.update(Action::NextPanel);
    assert_eq!(
        app.state.worktree_detail_focus,
        crate::state::WorktreeDetailFocus::InfoPanel
    );
}

#[test]
fn clamp_indices_handles_empty_lists() {
    let mut app = make_app();
    app.state.dashboard_index = 5;
    // With no data, dashboard_rows is empty → index stays as-is (clamp only when len > 0)
    app.clamp_indices();
    // dashboard_rows is empty so the clamp block doesn't fire
    assert_eq!(app.state.dashboard_index, 5);
}

#[test]
fn move_down_dashboard_clamps_at_end() {
    let mut app = make_app();
    app.state.view = View::Dashboard;
    app.state.column_focus = crate::state::ColumnFocus::Content;
    // No repos/worktrees → dashboard_rows is empty
    app.update(Action::MoveDown);
    assert_eq!(app.state.dashboard_index, 0);
}

#[test]
fn move_up_dashboard_clamps_at_zero() {
    let mut app = make_app();
    app.state.view = View::Dashboard;
    app.state.column_focus = crate::state::ColumnFocus::Content;
    app.state.dashboard_index = 0;
    app.update(Action::MoveUp);
    assert_eq!(app.state.dashboard_index, 0);
}

// ═══════════════════════════════════════════════════════════════════════
// Task 3: Modal dialog tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn confirm_quit_sets_should_quit() {
    let mut app = make_app();
    app.state.modal = Modal::Confirm {
        title: "Confirm Quit".into(),
        message: "Quit?".into(),
        on_confirm: crate::state::ConfirmAction::Quit,
    };
    app.update(Action::ConfirmYes);
    assert!(app.state.should_quit);
}

#[test]
fn show_confirm_quit_no_agents_generic_message() {
    let mut app = make_app();
    app.show_confirm_quit();
    if let Modal::Confirm { message, .. } = &app.state.modal {
        assert_eq!(message, "Quit conductor?");
    } else {
        panic!("expected Confirm modal");
    }
}

#[test]
fn show_confirm_quit_with_running_agents_includes_count() {
    let mut app = make_app();
    // Insert a running agent run
    app.state.data.latest_agent_runs.insert(
        "wt1".into(),
        conductor_core::agent::AgentRun {
            id: "run1".into(),
            worktree_id: Some("wt1".into()),
            repo_id: None,
            claude_session_id: None,
            prompt: String::new(),
            status: conductor_core::agent::AgentRunStatus::Running,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: "2024-01-01T00:00:00Z".into(),
            ended_at: None,
            tmux_window: None,
            log_file: None,
            model: None,
            plan: None,
            parent_run_id: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            bot_name: None,
        },
    );
    app.show_confirm_quit();
    if let Modal::Confirm { message, .. } = &app.state.modal {
        assert!(
            message.contains("1 agent is running"),
            "expected agent count in message: {message}"
        );
    } else {
        panic!("expected Confirm modal");
    }
}

#[test]
fn delete_worktree_no_bg_tx_no_crash() {
    let mut app = make_app();
    assert!(app.bg_tx.is_none());
    app.execute_confirm_action(crate::state::ConfirmAction::DeleteWorktree {
        repo_slug: "test".into(),
        wt_slug: "test-wt".into(),
    });
    // No crash, modal should not change to Progress (because bg_tx is None → early return)
    assert!(matches!(app.state.modal, Modal::None));
}

#[test]
fn unregister_repo_no_bg_tx_no_crash() {
    let mut app = make_app();
    assert!(app.bg_tx.is_none());
    app.execute_confirm_action(crate::state::ConfirmAction::UnregisterRepo {
        repo_slug: "test".into(),
    });
    assert!(matches!(app.state.modal, Modal::None));
}

// ═══════════════════════════════════════════════════════════════════════
// Task 4: Git operations result handling tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn push_complete_ok_clears_modal_sets_status() {
    let mut app = make_app();
    app.state.modal = Modal::Progress {
        message: "Pushing…".into(),
    };
    app.update(Action::PushComplete {
        result: Ok("Pushed to origin/feat-x".into()),
    });
    assert!(matches!(app.state.modal, Modal::None));
    assert_eq!(
        app.state.status_message.as_deref(),
        Some("Pushed to origin/feat-x")
    );
}

#[test]
fn push_complete_err_shows_error_modal() {
    let mut app = make_app();
    app.update(Action::PushComplete {
        result: Err("auth failed".into()),
    });
    if let Modal::Error { message } = &app.state.modal {
        assert!(message.contains("auth failed"));
    } else {
        panic!("expected Error modal");
    }
}

#[test]
fn pr_create_complete_ok_sets_status() {
    let mut app = make_app();
    app.update(Action::PrCreateComplete {
        result: Ok("https://github.com/x/y/pull/1".into()),
    });
    assert!(matches!(app.state.modal, Modal::None));
    let msg = app.state.status_message.as_deref().unwrap();
    assert!(msg.contains("PR created"));
}

#[test]
fn pr_create_complete_err_shows_error() {
    let mut app = make_app();
    app.update(Action::PrCreateComplete {
        result: Err("no commits".into()),
    });
    assert!(matches!(app.state.modal, Modal::Error { .. }));
}

#[test]
fn worktree_delete_complete_ok_navigates_to_dashboard() {
    let mut app = make_app();
    app.state.view = View::WorktreeDetail;
    app.state.selected_worktree_id = Some("w1".into());
    app.update(Action::WorktreeDeleteComplete {
        wt_slug: "feat-x".into(),
        result: Ok("Merged".into()),
    });
    assert!(matches!(app.state.modal, Modal::None));
    assert_eq!(app.state.view, View::Dashboard);
    assert!(app.state.selected_worktree_id.is_none());
    let msg = app.state.status_message.as_deref().unwrap();
    assert!(msg.contains("feat-x") && msg.contains("Merged"));
}

#[test]
fn worktree_delete_complete_err_shows_error() {
    let mut app = make_app();
    app.update(Action::WorktreeDeleteComplete {
        wt_slug: "feat-x".into(),
        result: Err("worktree busy".into()),
    });
    assert!(matches!(app.state.modal, Modal::Error { .. }));
}

#[test]
fn repo_unregister_complete_ok_navigates_to_dashboard() {
    let mut app = make_app();
    app.state.view = View::RepoDetail;
    app.state.selected_repo_id = Some("r1".into());
    app.update(Action::RepoUnregisterComplete {
        repo_slug: "my-repo".into(),
        result: Ok(()),
    });
    assert_eq!(app.state.view, View::Dashboard);
    assert!(app.state.selected_repo_id.is_none());
    let msg = app.state.status_message.as_deref().unwrap();
    assert!(msg.contains("my-repo"));
}

#[test]
fn repo_unregister_complete_err_shows_error() {
    let mut app = make_app();
    app.update(Action::RepoUnregisterComplete {
        repo_slug: "my-repo".into(),
        result: Err("has worktrees".into()),
    });
    assert!(matches!(app.state.modal, Modal::Error { .. }));
}

#[test]
fn background_error_shows_error_modal() {
    let mut app = make_app();
    app.update(Action::BackgroundError {
        message: "something broke".into(),
    });
    if let Modal::Error { message } = &app.state.modal {
        assert_eq!(message, "something broke");
    } else {
        panic!("expected Error modal");
    }
}

#[test]
fn background_success_sets_status_message() {
    let mut app = make_app();
    app.update(Action::BackgroundSuccess {
        message: "done".into(),
    });
    assert_eq!(app.state.status_message.as_deref(), Some("done"));
}

#[test]
fn handle_push_no_worktree_selected() {
    let mut app = make_app();
    app.state.selected_worktree_id = None;
    app.handle_push();
    assert_eq!(
        app.state.status_message.as_deref(),
        Some("Select a worktree first")
    );
}

#[test]
fn handle_create_pr_no_worktree_selected() {
    let mut app = make_app();
    app.state.selected_worktree_id = None;
    app.handle_create_pr();
    assert_eq!(
        app.state.status_message.as_deref(),
        Some("Select a worktree first")
    );
}

#[test]
fn handle_sync_tickets_already_in_progress() {
    let mut app = make_app();
    app.state.ticket_sync_in_progress = true;
    app.handle_sync_tickets();
    assert_eq!(
        app.state.status_message.as_deref(),
        Some("Sync already in progress...")
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Task 5: Input handling tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn form_char_appends_to_active_field() {
    let mut app = make_app();
    app.state.modal = Modal::Form {
        title: "Test".into(),
        fields: vec![FormField {
            label: "Name".into(),
            value: String::new(),
            placeholder: String::new(),
            manually_edited: false,
            required: false,
            readonly: false,
            field_type: crate::state::FormFieldType::Text,
        }],
        active_field: 0,
        on_submit: crate::state::FormAction::RegisterRepo,
    };
    app.update(Action::FormChar('x'));
    if let Modal::Form { ref fields, .. } = app.state.modal {
        assert_eq!(fields[0].value, "x");
        assert!(fields[0].manually_edited);
    } else {
        panic!("expected Form modal");
    }
}

#[test]
fn form_backspace_removes_last_char() {
    let mut app = make_app();
    app.state.modal = Modal::Form {
        title: "Test".into(),
        fields: vec![FormField {
            label: "Name".into(),
            value: "abc".into(),
            placeholder: String::new(),
            manually_edited: true,
            required: false,
            readonly: false,
            field_type: crate::state::FormFieldType::Text,
        }],
        active_field: 0,
        on_submit: crate::state::FormAction::RegisterRepo,
    };
    app.update(Action::FormBackspace);
    if let Modal::Form { ref fields, .. } = app.state.modal {
        assert_eq!(fields[0].value, "ab");
    } else {
        panic!("expected Form modal");
    }
}

#[test]
fn form_next_prev_field_skips_readonly() {
    let mut app = make_app();
    app.state.modal = Modal::Form {
        title: "Test".into(),
        fields: vec![
            FormField {
                label: "A".into(),
                value: String::new(),
                placeholder: String::new(),
                manually_edited: false,
                required: false,
                readonly: false,
                field_type: crate::state::FormFieldType::Text,
            },
            FormField {
                label: "B".into(),
                value: String::new(),
                placeholder: String::new(),
                manually_edited: false,
                required: false,
                readonly: true,
                field_type: crate::state::FormFieldType::Text,
            },
            FormField {
                label: "C".into(),
                value: String::new(),
                placeholder: String::new(),
                manually_edited: false,
                required: false,
                readonly: false,
                field_type: crate::state::FormFieldType::Text,
            },
        ],
        active_field: 0,
        on_submit: crate::state::FormAction::RegisterRepo,
    };
    // Next from 0 should skip readonly field 1 and land on 2
    app.update(Action::FormNextField);
    if let Modal::Form { active_field, .. } = app.state.modal {
        assert_eq!(active_field, 2);
    } else {
        panic!("expected Form modal");
    }
    // Prev from 2 should skip readonly field 1 and land on 0
    app.update(Action::FormPrevField);
    if let Modal::Form { active_field, .. } = app.state.modal {
        assert_eq!(active_field, 0);
    } else {
        panic!("expected Form modal");
    }
}

#[test]
fn input_char_appends_to_modal_value() {
    let mut app = make_app();
    app.state.modal = Modal::Input {
        title: "Test".into(),
        prompt: "Enter:".into(),
        value: "hel".into(),
        on_submit: crate::state::InputAction::CreateWorktree {
            repo_slug: "r".into(),
            ticket_id: None,
        },
    };
    app.update(Action::InputChar('l'));
    app.update(Action::InputChar('o'));
    if let Modal::Input { ref value, .. } = app.state.modal {
        assert_eq!(value, "hello");
    } else {
        panic!("expected Input modal");
    }
}

#[test]
fn input_backspace_removes_from_modal_value() {
    let mut app = make_app();
    app.state.modal = Modal::Input {
        title: "Test".into(),
        prompt: "Enter:".into(),
        value: "abc".into(),
        on_submit: crate::state::InputAction::CreateWorktree {
            repo_slug: "r".into(),
            ticket_id: None,
        },
    };
    app.update(Action::InputBackspace);
    if let Modal::Input { ref value, .. } = app.state.modal {
        assert_eq!(value, "ab");
    } else {
        panic!("expected Input modal");
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Task 6: Theme management tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn themes_loaded_opens_theme_picker_modal() {
    let mut app = make_app();
    let themes = vec![
        ("conductor".to_string(), "Conductor".to_string()),
        ("dark".to_string(), "Dark".to_string()),
    ];
    let loaded_themes = vec![Theme::default(), Theme::default()];
    app.handle_themes_loaded(themes.clone(), loaded_themes, vec![]);
    if let Modal::ThemePicker {
        themes: ref t,
        selected,
        ..
    } = app.state.modal
    {
        assert_eq!(t.len(), 2);
        // Default config theme is None → fallback "conductor" → should select idx 0
        assert_eq!(selected, 0);
    } else {
        panic!("expected ThemePicker modal");
    }
}

#[test]
fn theme_preview_updates_theme() {
    let mut app = make_app();
    let default_theme = Theme::default();
    let other_theme = Theme::default(); // same type, different instance
    app.state.modal = Modal::ThemePicker {
        themes: vec![("a".into(), "A".into()), ("b".into(), "B".into())],
        loaded_themes: vec![default_theme, other_theme],
        selected: 0,
        original_theme: default_theme,
        original_name: "a".into(),
    };
    app.handle_theme_preview(1);
    if let Modal::ThemePicker { selected, .. } = app.state.modal {
        assert_eq!(selected, 1);
    } else {
        panic!("expected ThemePicker modal");
    }
}

#[test]
fn theme_save_complete_ok_sets_status() {
    let mut app = make_app();
    app.update(Action::ThemeSaveComplete {
        result: Ok("Theme set to \"dark\"".into()),
    });
    assert!(matches!(app.state.modal, Modal::None));
    assert_eq!(
        app.state.status_message.as_deref(),
        Some("Theme set to \"dark\"")
    );
}

#[test]
fn theme_save_complete_err_shows_error() {
    let mut app = make_app();
    app.update(Action::ThemeSaveComplete {
        result: Err("permission denied".into()),
    });
    if let Modal::Error { message } = &app.state.modal {
        assert!(message.contains("permission denied"));
    } else {
        panic!("expected Error modal");
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Task 7: URL operations tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn selected_ticket_url_from_ticket_info_modal() {
    let mut app = make_app();
    app.state.modal = Modal::TicketInfo {
        ticket: Box::new(conductor_core::tickets::Ticket {
            id: "t1".into(),
            repo_id: "r1".into(),
            source_type: "github".into(),
            source_id: "123".into(),
            title: "Test".into(),
            body: "body".into(),
            state: "open".into(),
            labels: "".into(),
            assignee: None,
            priority: None,
            url: "https://github.com/x/y/issues/123".into(),
            synced_at: "2024-01-01T00:00:00Z".into(),
            raw_json: "{}".into(),
            workflow: None,
            agent_map: None,
        }),
    };
    assert_eq!(
        app.selected_ticket_url(),
        Some("https://github.com/x/y/issues/123".into())
    );
}

#[test]
fn selected_ticket_url_no_ticket_available() {
    let app = make_app();
    assert!(app.selected_ticket_url().is_none());
}

#[test]
fn repo_web_url_with_valid_github_remote() {
    let mut app = make_app();
    let repo = conductor_core::repo::Repo {
        id: "r1".into(),
        slug: "my-repo".into(),
        local_path: "/tmp/my-repo".into(),
        remote_url: "https://github.com/user/my-repo.git".into(),
        default_branch: "main".into(),
        workspace_dir: "/tmp".into(),
        created_at: "2024-01-01T00:00:00Z".into(),
        model: None,
        allow_agent_issue_creation: false,
    };
    app.state.selected_repo_id = Some("r1".into());
    app.state.data.repos = vec![repo];
    let url = app.repo_web_url();
    assert_eq!(url, Some("https://github.com/user/my-repo".into()));
}

#[test]
fn repo_web_url_no_selected_repo() {
    let app = make_app();
    assert!(app.repo_web_url().is_none());
}

#[test]
fn selected_pr_url_with_pr() {
    let mut app = make_app();
    app.state.detail_prs = vec![conductor_core::github::GithubPr {
        number: 1,
        title: "PR".into(),
        url: "https://github.com/x/y/pull/1".into(),
        author: "user".into(),
        head_ref_name: "feat-x".into(),
        state: "open".into(),
        is_draft: false,
        review_decision: None,
        ci_status: "success".into(),
    }];
    app.state.detail_pr_index = 0;
    assert_eq!(
        app.selected_pr_url(),
        Some("https://github.com/x/y/pull/1".into())
    );
}

#[test]
fn selected_pr_url_empty_list() {
    let app = make_app();
    assert!(app.selected_pr_url().is_none());
}

#[test]
fn selected_ticket_url_from_repo_detail_tickets() {
    let mut app = make_app();
    app.state.view = View::RepoDetail;
    app.state.repo_detail_focus = crate::state::RepoDetailFocus::Tickets;
    app.state.filtered_detail_tickets = vec![conductor_core::tickets::Ticket {
        id: "t1".into(),
        repo_id: "r1".into(),
        source_type: "github".into(),
        source_id: "42".into(),
        title: "A ticket".into(),
        body: "".into(),
        state: "open".into(),
        labels: "".into(),
        assignee: None,
        priority: None,
        url: "https://github.com/x/y/issues/42".into(),
        synced_at: "2024-01-01T00:00:00Z".into(),
        raw_json: "{}".into(),
        workflow: None,
        agent_map: None,
    }];
    app.state.detail_ticket_index = 0;
    assert_eq!(
        app.selected_ticket_url(),
        Some("https://github.com/x/y/issues/42".into())
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Task 6: Tick behavior, scroll, input modal, dismiss modal tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn tick_auto_clears_status_after_timeout() {
    let mut app = make_app();
    app.state.status_message = Some("hello".into());
    // Backdate so it appears to have been set 5 seconds ago
    app.state.status_message_at =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(5));
    app.handle_action(Action::Tick);
    assert!(app.state.status_message.is_none());
}

#[test]
fn tick_preserves_recent_status_message() {
    let mut app = make_app();
    app.state.status_message = Some("fresh".into());
    app.state.status_message_at = Some(std::time::Instant::now());
    app.handle_action(Action::Tick);
    assert_eq!(app.state.status_message.as_deref(), Some("fresh"));
}

#[test]
fn tick_prunes_finished_workflow_threads() {
    let mut app = make_app();
    // Spawn a thread that finishes immediately
    let handle = std::thread::spawn(|| {});
    // Wait for it to finish
    std::thread::sleep(std::time::Duration::from_millis(10));
    app.workflow_threads.push(handle);
    assert_eq!(app.workflow_threads.len(), 1);
    app.handle_action(Action::Tick);
    assert_eq!(app.workflow_threads.len(), 0);
}

#[test]
fn scroll_left_decrements_horizontal_offset() {
    let mut app = make_app();
    app.state.modal = Modal::EventDetail {
        title: "Test".into(),
        body: "long line".into(),
        line_count: 1,
        scroll_offset: 0,
        horizontal_offset: 8,
    };
    app.handle_action(Action::ScrollLeft);
    if let Modal::EventDetail {
        horizontal_offset, ..
    } = app.state.modal
    {
        assert_eq!(horizontal_offset, 4);
    } else {
        panic!("expected EventDetail");
    }
}

#[test]
fn scroll_right_increments_horizontal_offset() {
    let mut app = make_app();
    app.state.modal = Modal::EventDetail {
        title: "Test".into(),
        body: "long line".into(),
        line_count: 1,
        scroll_offset: 0,
        horizontal_offset: 0,
    };
    app.handle_action(Action::ScrollRight);
    if let Modal::EventDetail {
        horizontal_offset, ..
    } = app.state.modal
    {
        assert_eq!(horizontal_offset, 4);
    } else {
        panic!("expected EventDetail");
    }
}

#[test]
fn scroll_left_noop_outside_event_detail() {
    let mut app = make_app();
    app.state.modal = Modal::Help;
    app.handle_action(Action::ScrollLeft);
    assert!(matches!(app.state.modal, Modal::Help));
}

#[test]
fn input_char_appends_to_input_modal_value() {
    let mut app = make_app();
    app.state.modal = Modal::Input {
        title: "Test".into(),
        prompt: "Enter:".into(),
        value: "ab".into(),
        on_submit: crate::state::InputAction::LinkTicket {
            worktree_id: "w1".into(),
        },
    };
    app.handle_action(Action::InputChar('c'));
    if let Modal::Input { ref value, .. } = app.state.modal {
        assert_eq!(value, "abc");
    } else {
        panic!("expected Input modal");
    }
}

#[test]
fn input_backspace_removes_from_input_modal_value() {
    let mut app = make_app();
    app.state.modal = Modal::Input {
        title: "Test".into(),
        prompt: "Enter:".into(),
        value: "abc".into(),
        on_submit: crate::state::InputAction::LinkTicket {
            worktree_id: "w1".into(),
        },
    };
    app.handle_action(Action::InputBackspace);
    if let Modal::Input { ref value, .. } = app.state.modal {
        assert_eq!(value, "ab");
    } else {
        panic!("expected Input modal");
    }
}

#[test]
fn dismiss_modal_noop_on_progress() {
    let mut app = make_app();
    app.state.modal = Modal::Progress {
        message: "Working…".into(),
    };
    app.handle_action(Action::DismissModal);
    // Progress modal should NOT be dismissed
    assert!(matches!(app.state.modal, Modal::Progress { .. }));
}

#[test]
fn dismiss_modal_theme_picker_restores_original_theme() {
    let mut app = make_app();
    let original = app.state.theme;
    // Create a modified theme to simulate preview
    let preview_theme = crate::theme::Theme {
        border_focused: ratatui::style::Color::Red,
        ..Default::default()
    };
    app.state.theme = preview_theme;
    // Set up ThemePicker modal with original saved
    app.state.modal = Modal::ThemePicker {
        themes: vec![("dark".into(), "Built-in".into())],
        loaded_themes: vec![preview_theme],
        selected: 0,
        original_theme: original,
        original_name: "default".into(),
    };
    app.handle_action(Action::DismissModal);
    assert!(matches!(app.state.modal, Modal::None));
    // Theme should be restored to original (Cyan border, not Red)
    assert_eq!(app.state.theme.border_focused, original.border_focused);
}

// Navigation dispatch smoke tests (detailed logic tested in navigation.rs)

#[test]
fn action_select_dispatches_to_select() {
    let mut app = make_app();
    app.state.view = View::Dashboard;
    app.state.column_focus = crate::state::ColumnFocus::Content;
    // Empty dashboard → select is a no-op, but doesn't crash
    app.handle_action(Action::Select);
    assert_eq!(app.state.view, View::Dashboard);
}

#[test]
fn action_move_up_dispatches() {
    let mut app = make_app();
    app.state.view = View::Dashboard;
    app.state.column_focus = crate::state::ColumnFocus::Content;
    app.state.dashboard_index = 1;
    app.state.data.repos = vec![conductor_core::repo::Repo {
        id: "r1".into(),
        slug: "repo".into(),
        local_path: "/tmp/repo".into(),
        remote_url: "https://github.com/x/r".into(),
        default_branch: "main".into(),
        workspace_dir: "/tmp".into(),
        created_at: "2024-01-01T00:00:00Z".into(),
        model: None,
        allow_agent_issue_creation: false,
    }];
    app.state.data.worktrees = vec![conductor_core::worktree::Worktree {
        id: "w1".into(),
        repo_id: "r1".into(),
        slug: "feat-a".into(),
        branch: "feat/a".into(),
        path: "/tmp/ws/feat-a".into(),
        ticket_id: None,
        status: conductor_core::worktree::WorktreeStatus::Active,
        created_at: "2024-01-01T00:00:00Z".into(),
        completed_at: None,
        model: None,
        base_branch: None,
    }];
    app.handle_action(Action::MoveUp);
    assert_eq!(app.state.dashboard_index, 0);
}

#[test]
fn input_backspace_on_model_picker_non_custom_clears_model() {
    use crate::state::InputAction;

    // Set up a repo so SetRepoModel has something to work with
    let mut app = make_app();
    let repo_mgr = conductor_core::repo::RepoManager::new(&app.conn, &app.config);
    repo_mgr
        .register(
            "test-repo",
            "/tmp/test-repo",
            "https://github.com/test/test-repo",
            None,
        )
        .expect("register repo");

    app.state.modal = Modal::ModelPicker {
        context_label: "test".into(),
        effective_default: Some("claude-sonnet-4-5-20250514".into()),
        effective_source: "global config".into(),
        selected: 0,
        custom_input: String::new(),
        custom_active: false,
        suggested: None,
        on_submit: InputAction::SetRepoModel {
            slug: "test-repo".into(),
        },
        allow_default: false,
    };

    app.handle_action(Action::InputBackspace);

    // Modal should be dismissed (not ModelPicker anymore)
    assert!(
        !matches!(app.state.modal, Modal::ModelPicker { .. }),
        "ModelPicker should be dismissed after Backspace in non-custom mode"
    );
}

// ─── workflow picker: Repo & standalone Worktree target tests ────────────────

fn make_workflow_def(name: &str, target: &str) -> conductor_core::workflow::WorkflowDef {
    conductor_core::workflow::WorkflowDef {
        name: name.to_string(),
        description: String::new(),
        trigger: conductor_core::workflow::WorkflowTrigger::Manual,
        targets: vec![target.to_string()],
        group: None,
        inputs: vec![],
        body: vec![],
        always: vec![],
        source_path: format!(".conductor/workflows/{name}.wf"),
    }
}

// WorkflowPickerDefsLoaded with a Repo target should open the WorkflowPicker modal.
// The guard `state.loading_workflow_picker_defs = true` must be set first to avoid
// the race-condition early-return in handle_workflow_picker_defs_loaded.
#[test]
fn workflow_picker_defs_loaded_repo_target() {
    let mut app = make_app();
    app.state.loading_workflow_picker_defs = true;
    app.update(Action::WorkflowPickerDefsLoaded {
        target: crate::state::WorkflowPickerTarget::Repo {
            repo_id: "r1".into(),
            repo_path: "/tmp/repo".into(),
            repo_name: "my-repo".into(),
        },
        defs: vec![make_workflow_def("deploy", "repo")],
        error: None,
    });
    assert!(
        matches!(app.state.modal, Modal::WorkflowPicker { .. }),
        "expected WorkflowPicker modal after loading repo-target defs"
    );
}

// PickWorkflow with view=WorktreeDetail and a seeded worktree-scoped def should
// open the WorkflowPicker modal via the synchronous in-memory path.
#[test]
fn workflow_picker_defs_loaded_worktree_target() {
    let mut app = make_app();
    app.state.data.repos = vec![conductor_core::repo::Repo {
        id: "r1".into(),
        slug: "my-repo".into(),
        local_path: "/tmp/my-repo".into(),
        remote_url: "https://github.com/x/my-repo".into(),
        default_branch: "main".into(),
        workspace_dir: "/tmp".into(),
        created_at: "2024-01-01T00:00:00Z".into(),
        model: None,
        allow_agent_issue_creation: false,
    }];
    app.state.data.worktrees = vec![conductor_core::worktree::Worktree {
        id: "w1".into(),
        repo_id: "r1".into(),
        slug: "feat-a".into(),
        branch: "feat/a".into(),
        path: "/tmp/ws/feat-a".into(),
        ticket_id: None,
        status: conductor_core::worktree::WorktreeStatus::Active,
        created_at: "2024-01-01T00:00:00Z".into(),
        completed_at: None,
        model: None,
        base_branch: None,
    }];
    app.state.selected_worktree_id = Some("w1".into());
    app.state.view = View::WorktreeDetail;
    app.state.data.workflow_defs = vec![make_workflow_def("build", "worktree")];
    app.update(Action::PickWorkflow);
    assert!(
        matches!(app.state.modal, Modal::WorkflowPicker { .. }),
        "expected WorkflowPicker modal after PickWorkflow for Worktree target"
    );
}

// Confirming a Repo-targeted workflow with no inputs should open the ModelPicker.
#[test]
fn workflow_picker_confirm_repo_target() {
    let mut app = make_app();
    let def = make_workflow_def("deploy", "repo");
    app.state.modal = Modal::WorkflowPicker {
        target: crate::state::WorkflowPickerTarget::Repo {
            repo_id: "r1".into(),
            repo_path: "/tmp/repo".into(),
            repo_name: "my-repo".into(),
        },
        items: vec![crate::state::WorkflowPickerItem::Workflow(def)],
        selected: 0,
        scroll_offset: 0,
    };
    app.handle_workflow_picker_confirm();
    assert!(
        matches!(app.state.modal, Modal::ModelPicker { .. }),
        "expected ModelPicker after confirming repo workflow with no inputs"
    );
}

// Confirming a standalone Worktree-targeted workflow with no inputs and no
// active agent run should open the ModelPicker.
#[test]
fn workflow_picker_confirm_worktree_target() {
    let mut app = make_app();
    let def = make_workflow_def("build", "worktree");
    app.state.modal = Modal::WorkflowPicker {
        target: crate::state::WorkflowPickerTarget::Worktree {
            worktree_id: "w1".into(),
            worktree_path: "/tmp/ws/w1".into(),
            repo_path: "/tmp/repo".into(),
        },
        items: vec![crate::state::WorkflowPickerItem::Workflow(def)],
        selected: 0,
        scroll_offset: 0,
    };
    // Empty in-memory DB → active_run_blocks_dispatch returns false → proceeds to ModelPicker.
    app.handle_workflow_picker_confirm();
    assert!(
        matches!(app.state.modal, Modal::ModelPicker { .. }),
        "expected ModelPicker after confirming worktree workflow with no inputs"
    );
}
