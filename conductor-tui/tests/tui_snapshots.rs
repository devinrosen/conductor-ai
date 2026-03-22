use conductor_core::{
    issue_source::IssueSource,
    repo::Repo,
    tickets::Ticket,
    worktree::{Worktree, WorktreeStatus},
};
use conductor_tui::state::{
    AppState, BranchPickerItem, FormAction, FormField, FormFieldType, InputAction, Modal,
    RepoDetailFocus, TreePosition, View, WorktreeDetailFocus,
};
use conductor_tui::ui;
use ratatui::{backend::TestBackend, Terminal};

// ─── Fixture helpers ────────────────────────────────────────────────────────

fn make_state() -> AppState {
    AppState::new()
}

fn make_repos() -> Vec<Repo> {
    vec![
        Repo {
            id: "01REPO0000000000000000000A".into(),
            slug: "my-app".into(),
            local_path: "/home/user/my-app".into(),
            remote_url: "https://github.com/user/my-app".into(),
            default_branch: "main".into(),
            workspace_dir: "/home/user/.conductor/workspaces/my-app".into(),
            created_at: "2024-01-01T00:00:00Z".into(),
            model: None,
            allow_agent_issue_creation: false,
        },
        Repo {
            id: "01REPO0000000000000000000B".into(),
            slug: "backend-api".into(),
            local_path: "/home/user/backend-api".into(),
            remote_url: "https://github.com/user/backend-api".into(),
            default_branch: "main".into(),
            workspace_dir: "/home/user/.conductor/workspaces/backend-api".into(),
            created_at: "2024-01-02T00:00:00Z".into(),
            model: None,
            allow_agent_issue_creation: false,
        },
    ]
}

fn make_worktrees(repos: &[Repo]) -> Vec<Worktree> {
    vec![
        Worktree {
            id: "01WT00000000000000000000A1".into(),
            repo_id: repos[0].id.clone(),
            slug: "feat-123-add-login".into(),
            branch: "feat/123-add-login".into(),
            path: "/home/user/my-app/.worktrees/feat-123-add-login".into(),
            ticket_id: None,
            status: WorktreeStatus::Active,
            created_at: "2024-01-10T00:00:00Z".into(),
            completed_at: None,
            model: None,
            base_branch: None,
        },
        Worktree {
            id: "01WT00000000000000000000A2".into(),
            repo_id: repos[0].id.clone(),
            slug: "fix-456-null-ptr".into(),
            branch: "fix/456-null-ptr".into(),
            path: "/home/user/my-app/.worktrees/fix-456-null-ptr".into(),
            ticket_id: None,
            status: WorktreeStatus::Active,
            created_at: "2024-01-11T00:00:00Z".into(),
            completed_at: None,
            model: None,
            base_branch: None,
        },
        Worktree {
            id: "01WT00000000000000000000B1".into(),
            repo_id: repos[1].id.clone(),
            slug: "feat-789-auth".into(),
            branch: "feat/789-auth".into(),
            path: "/home/user/backend-api/.worktrees/feat-789-auth".into(),
            ticket_id: None,
            status: WorktreeStatus::Merged,
            created_at: "2024-01-05T00:00:00Z".into(),
            completed_at: Some("2024-01-12T00:00:00Z".into()),
            model: None,
            base_branch: None,
        },
    ]
}

fn make_tickets(repos: &[Repo]) -> Vec<Ticket> {
    vec![
        Ticket {
            id: "01TKT0000000000000000000A1".into(),
            repo_id: repos[0].id.clone(),
            source_type: "github".into(),
            source_id: "123".into(),
            title: "Add login flow".into(),
            body: "Implement OAuth login using GitHub provider.".into(),
            state: "open".into(),
            labels: "".into(),
            assignee: None,
            priority: None,
            url: "https://github.com/user/my-app/issues/123".into(),
            synced_at: "2024-01-10T00:00:00Z".into(),
            raw_json: "{}".into(),
        },
        Ticket {
            id: "01TKT0000000000000000000B1".into(),
            repo_id: repos[1].id.clone(),
            source_type: "github".into(),
            source_id: "456".into(),
            title: "Fix null pointer in auth middleware".into(),
            body: "Crash occurs when token is missing from request headers.".into(),
            state: "open".into(),
            labels: "bug".into(),
            assignee: None,
            priority: None,
            url: "https://github.com/user/backend-api/issues/456".into(),
            synced_at: "2024-01-10T00:00:00Z".into(),
            raw_json: "{}".into(),
        },
    ]
}

fn render_to_string(state: &AppState) -> String {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("failed to create terminal");
    terminal
        .draw(|frame| ui::render(frame, state))
        .expect("failed to draw");
    let backend = terminal.backend();
    format!("{}", backend)
}

// ─── Modal snapshot tests ────────────────────────────────────────────────────

#[test]
fn snap_modal_error() {
    let mut state = make_state();
    state.modal = Modal::Error {
        message: "Something went wrong: connection refused".into(),
    };
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_modal_progress() {
    let mut state = make_state();
    state.modal = Modal::Progress {
        message: "Pushing branch…".into(),
    };
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_modal_confirm() {
    use conductor_tui::state::ConfirmAction;
    let mut state = make_state();
    state.modal = Modal::Confirm {
        title: "Delete worktree".into(),
        message: "Are you sure you want to delete feat-123-add-login?".into(),
        on_confirm: ConfirmAction::DeleteWorktree {
            repo_slug: "my-app".into(),
            wt_slug: "feat-123-add-login".into(),
        },
    };
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_modal_help() {
    let mut state = make_state();
    state.modal = Modal::Help;
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_modal_input() {
    use conductor_tui::state::InputAction;
    let mut state = make_state();
    state.modal = Modal::Input {
        title: "New worktree".into(),
        prompt: "Branch name:".into(),
        value: "feat/my-feature".into(),
        on_submit: InputAction::CreateWorktree {
            repo_slug: "my-app".into(),
            ticket_id: None,
        },
    };
    insta::assert_snapshot!(render_to_string(&state));
}

// ─── View snapshot tests ─────────────────────────────────────────────────────

#[test]
fn snap_dashboard_empty() {
    let state = make_state();
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_dashboard_populated() {
    let mut state = make_state();
    let repos = make_repos();
    let worktrees = make_worktrees(&repos);
    let tickets = make_tickets(&repos);
    state.data.repos = repos;
    state.data.worktrees = worktrees;
    state.data.tickets = tickets;
    state.data.rebuild_maps();
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_repo_detail() {
    let mut state = make_state();
    let repos = make_repos();
    let worktrees = make_worktrees(&repos);
    let tickets = make_tickets(&repos);
    let repo_id = repos[0].id.clone();
    state.selected_repo_id = Some(repo_id.clone());
    state.detail_tickets = tickets.clone();
    state.data.repos = repos;
    state.data.worktrees = worktrees;
    state.data.tickets = tickets;
    state.data.rebuild_maps();
    state.rebuild_detail_worktree_tree(&repo_id);
    state.view = View::RepoDetail;
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_repo_detail_tickets_focus() {
    let mut state = make_state();
    let repos = make_repos();
    let worktrees = make_worktrees(&repos);
    let tickets = make_tickets(&repos);
    let repo_id = repos[0].id.clone();
    state.selected_repo_id = Some(repo_id.clone());
    state.detail_tickets = tickets.clone();
    state.data.repos = repos;
    state.data.worktrees = worktrees;
    state.data.tickets = tickets;
    state.data.rebuild_maps();
    state.rebuild_detail_worktree_tree(&repo_id);
    state.rebuild_filtered_tickets();
    state.repo_detail_focus = RepoDetailFocus::Tickets;
    state.view = View::RepoDetail;
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_worktree_detail_info_focus() {
    let mut state = make_state();
    let repos = make_repos();
    let worktrees = make_worktrees(&repos);
    let wt = worktrees[0].clone();
    state.selected_repo_id = Some(repos[0].id.clone());
    state.selected_worktree_id = Some(wt.id.clone());
    state.detail_worktrees = vec![wt];
    state.data.repos = repos;
    state.data.worktrees = worktrees;
    state.data.rebuild_maps();
    state.worktree_detail_focus = WorktreeDetailFocus::InfoPanel;
    state.view = View::WorktreeDetail;
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_worktree_detail_log_focus() {
    let mut state = make_state();
    let repos = make_repos();
    let worktrees = make_worktrees(&repos);
    let wt = worktrees[0].clone();
    state.selected_repo_id = Some(repos[0].id.clone());
    state.selected_worktree_id = Some(wt.id.clone());
    state.detail_worktrees = vec![wt];
    state.data.repos = repos;
    state.data.worktrees = worktrees;
    state.data.rebuild_maps();
    state.worktree_detail_focus = WorktreeDetailFocus::LogPanel;
    state.view = View::WorktreeDetail;
    insta::assert_snapshot!(render_to_string(&state));
}

// ─── Task 8: Workflow view snapshot tests ─────────────────────────────────

#[test]
fn snap_workflow_run_detail_with_steps() {
    use conductor_core::workflow::{WorkflowRun, WorkflowRunStatus, WorkflowRunStep};
    let mut state = make_state();
    let repos = make_repos();
    state.data.repos = repos;
    state.data.rebuild_maps();

    let run = WorkflowRun {
        id: "01RUN0000000000000000000001".into(),
        workflow_name: "deploy".into(),
        worktree_id: Some("01WT00000000000000000000A1".into()),
        parent_run_id: String::new(),
        status: WorkflowRunStatus::Running,
        dry_run: false,
        trigger: "manual".into(),
        started_at: "2024-01-15T10:00:00Z".into(),
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
    };

    let steps = vec![
        WorkflowRunStep {
            id: "step1".into(),
            workflow_run_id: "01RUN0000000000000000000001".into(),
            step_name: "build".into(),
            role: "agent".into(),
            can_commit: false,
            condition_expr: None,
            status: conductor_core::workflow::WorkflowStepStatus::Completed,
            started_at: Some("2024-01-15T10:00:00Z".into()),
            ended_at: Some("2024-01-15T10:05:00Z".into()),
            result_text: Some("Build succeeded".into()),
            child_run_id: None,
            position: 0,
            iteration: 0,
            condition_met: None,
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
        },
        WorkflowRunStep {
            id: "step2".into(),
            workflow_run_id: "01RUN0000000000000000000001".into(),
            step_name: "test".into(),
            role: "agent".into(),
            can_commit: false,
            condition_expr: None,
            status: conductor_core::workflow::WorkflowStepStatus::Running,
            started_at: Some("2024-01-15T10:05:00Z".into()),
            ended_at: None,
            result_text: None,
            child_run_id: None,
            position: 1,
            iteration: 0,
            condition_met: None,
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
        },
    ];

    state.selected_workflow_run_id = Some(run.id.clone());
    state.data.workflow_runs = vec![run];
    state.data.workflow_steps = steps;
    state.view = View::WorkflowRunDetail;
    state.workflow_step_index = 0;
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_workflow_def_detail() {
    use conductor_core::workflow::WorkflowDef;
    let mut state = make_state();

    state.selected_workflow_def = Some(WorkflowDef {
        name: "deploy-pipeline".into(),
        description: "Deploy pipeline workflow".into(),
        trigger: conductor_core::workflow::WorkflowTrigger::Manual,
        targets: vec!["worktree".into()],
        body: vec![],
        inputs: vec![],
        always: vec![],
        source_path: "/home/user/.conductor/workflows/deploy-pipeline.wf".into(),
    });
    state.view = View::WorkflowDefDetail;
    insta::assert_snapshot!(render_to_string(&state));
}

// ─── Task 9: Remaining modal snapshot tests ───────────────────────────────

#[test]
fn snap_modal_form() {
    let mut state = make_state();
    state.modal = Modal::Form {
        title: "Register Repo".into(),
        fields: vec![
            FormField {
                label: "Remote URL".into(),
                value: "https://github.com/user/my-app.git".into(),
                placeholder: "https://github.com/...".into(),
                manually_edited: true,
                required: true,
                readonly: false,
                field_type: FormFieldType::Text,
            },
            FormField {
                label: "Slug".into(),
                value: "my-app".into(),
                placeholder: String::new(),
                manually_edited: false,
                required: true,
                readonly: false,
                field_type: FormFieldType::Text,
            },
            FormField {
                label: "Local Path".into(),
                value: "/home/user/my-app".into(),
                placeholder: String::new(),
                manually_edited: false,
                required: true,
                readonly: false,
                field_type: FormFieldType::Text,
            },
        ],
        active_field: 0,
        on_submit: FormAction::RegisterRepo,
    };
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_modal_ticket_info() {
    let mut state = make_state();
    state.modal = Modal::TicketInfo {
        ticket: Box::new(Ticket {
            id: "t1".into(),
            repo_id: "r1".into(),
            source_type: "github".into(),
            source_id: "42".into(),
            title: "Fix authentication bug".into(),
            body: "Users are unable to log in when session expires.\n\nSteps:\n1. Log in\n2. Wait 30 min\n3. Try to navigate".into(),
            state: "open".into(),
            labels: "bug,auth".into(),
            assignee: Some("alice".into()),
            priority: Some("high".into()),
            url: "https://github.com/user/my-app/issues/42".into(),
            synced_at: "2024-01-15T00:00:00Z".into(),
            raw_json: "{}".into(),
        }),
    };
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_modal_branch_picker() {
    let mut state = make_state();
    state.modal = Modal::BranchPicker {
        repo_slug: "my-app".into(),
        wt_name: "feat-login".into(),
        ticket_id: None,
        items: vec![
            BranchPickerItem {
                branch: None,
                worktree_count: 0,
                ticket_count: 0,
                base_branch: None,
            },
            BranchPickerItem {
                branch: Some("feat/auth-flow".into()),
                worktree_count: 1,
                ticket_count: 1,
                base_branch: Some("main".into()),
            },
            BranchPickerItem {
                branch: Some("feat/dashboard".into()),
                worktree_count: 2,
                ticket_count: 0,
                base_branch: Some("main".into()),
            },
        ],
        tree_positions: vec![
            TreePosition {
                depth: 0,
                is_last_sibling: false,
                ancestors_are_last: vec![],
            },
            TreePosition {
                depth: 1,
                is_last_sibling: false,
                ancestors_are_last: vec![false],
            },
            TreePosition {
                depth: 1,
                is_last_sibling: true,
                ancestors_are_last: vec![true],
            },
        ],
        selected: 0,
    };
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_modal_model_picker() {
    let mut state = make_state();
    state.modal = Modal::ModelPicker {
        context_label: "worktree: feat-login".into(),
        effective_default: Some("claude-sonnet-4-6".into()),
        effective_source: "global config".into(),
        selected: 1,
        custom_input: String::new(),
        custom_active: false,
        suggested: None,
        on_submit: InputAction::SetWorktreeModel {
            worktree_id: "w1".into(),
            repo_slug: "my-app".into(),
            slug: "feat-login".into(),
        },
        allow_default: false,
    };
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_modal_gate_action() {
    let mut state = make_state();
    state.modal = Modal::GateAction {
        run_id: "run1".into(),
        step_id: "step1".into(),
        gate_prompt: "Review the changes and approve if they look correct.".into(),
        feedback: String::new(),
    };
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_modal_issue_source_manager() {
    let mut state = make_state();
    state.modal = Modal::IssueSourceManager {
        repo_id: "r1".into(),
        repo_slug: "my-app".into(),
        remote_url: "https://github.com/user/my-app".into(),
        sources: vec![IssueSource {
            id: "src1".into(),
            repo_id: "r1".into(),
            source_type: "github".into(),
            config_json: r#"{"owner":"user","repo":"my-app"}"#.into(),
        }],
        selected: 0,
    };
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_modal_theme_picker() {
    let mut state = make_state();
    let default_theme = conductor_tui::theme::Theme::default();
    state.modal = Modal::ThemePicker {
        themes: vec![
            ("conductor".into(), "Conductor (default)".into()),
            ("dark".into(), "Dark".into()),
            ("light".into(), "Light".into()),
        ],
        loaded_themes: vec![default_theme, default_theme, default_theme],
        selected: 0,
        original_theme: default_theme,
        original_name: "conductor".into(),
    };
    insta::assert_snapshot!(render_to_string(&state));
}
