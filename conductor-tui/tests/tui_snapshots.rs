use conductor_core::{
    repo::Repo,
    tickets::Ticket,
    worktree::{Worktree, WorktreeStatus},
};
use conductor_tui::state::{AppState, Modal, RepoDetailFocus, View, WorktreeDetailFocus};
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
            workspace_dir: "/home/user/.conductor/workspaces/my-app".into(),
            created_at: "2024-01-01T00:00:00Z".into(),
            allow_agent_issue_creation: false,
        },
        Repo {
            id: "01REPO0000000000000000000B".into(),
            slug: "backend-api".into(),
            local_path: "/home/user/backend-api".into(),
            remote_url: "https://github.com/user/backend-api".into(),
            workspace_dir: "/home/user/.conductor/workspaces/backend-api".into(),
            created_at: "2024-01-02T00:00:00Z".into(),
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
    state.selected_repo_id = Some(repos[0].id.clone());
    state.detail_worktrees = worktrees
        .iter()
        .filter(|w| w.repo_id == repos[0].id)
        .cloned()
        .collect();
    state.detail_tickets = tickets.clone();
    state.data.repos = repos;
    state.data.worktrees = worktrees;
    state.data.tickets = tickets;
    state.data.rebuild_maps();
    state.view = View::RepoDetail;
    insta::assert_snapshot!(render_to_string(&state));
}

#[test]
fn snap_repo_detail_tickets_focus() {
    let mut state = make_state();
    let repos = make_repos();
    let worktrees = make_worktrees(&repos);
    let tickets = make_tickets(&repos);
    state.selected_repo_id = Some(repos[0].id.clone());
    state.detail_worktrees = worktrees
        .iter()
        .filter(|w| w.repo_id == repos[0].id)
        .cloned()
        .collect();
    state.detail_tickets = tickets.clone();
    state.data.repos = repos;
    state.data.worktrees = worktrees;
    state.data.tickets = tickets;
    state.data.rebuild_maps();
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
