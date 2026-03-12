use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::state::{AppState, ColumnFocus, Modal, View, WorktreeDetailFocus};

/// Map a key event to an action based on the current app state.
/// Priority: Modal > Filter > Normal keybindings.
pub fn map_key(key: KeyEvent, state: &AppState) -> Action {
    // Ctrl+C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }

    // Modal state takes priority
    match &state.modal {
        Modal::Help => {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter => {
                    Action::DismissModal
                }
                _ => Action::None,
            };
        }
        Modal::Confirm { .. } => {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Action::ConfirmYes,
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Action::ConfirmNo,
                _ => Action::None,
            };
        }
        Modal::Input { .. } | Modal::ConfirmByName { .. } => {
            return match key.code {
                KeyCode::Enter => Action::InputSubmit,
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Backspace => Action::InputBackspace,
                KeyCode::Char(c) => Action::InputChar(c),
                _ => Action::None,
            };
        }
        Modal::AgentPrompt { .. } => {
            // Ctrl+S submits; Ctrl+D clears; Enter inserts a newline; Esc cancels
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match key.code {
                    KeyCode::Char('s') => return Action::InputSubmit,
                    KeyCode::Char('d') => return Action::TextAreaClear,
                    _ => {}
                }
            }
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                _ => Action::TextAreaInput(key),
            };
        }
        Modal::Form { .. } => {
            return match key.code {
                KeyCode::Enter => Action::FormSubmit,
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Tab => Action::FormNextField,
                KeyCode::BackTab => Action::FormPrevField,
                KeyCode::Backspace => Action::FormBackspace,
                KeyCode::Char(c) => Action::FormChar(c),
                _ => Action::None,
            };
        }
        Modal::Error { .. } => {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => Action::DismissModal,
                KeyCode::Char('y') => Action::CopyErrorMessage,
                _ => Action::None,
            };
        }
        Modal::TicketInfo { .. } => {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('q') => Action::DismissModal,
                KeyCode::Char('o') => Action::OpenTicketUrl,
                KeyCode::Char('y') => Action::CopyTicketUrl,
                _ => Action::None,
            };
        }
        Modal::EventDetail { .. } => {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('q') => Action::DismissModal,
                KeyCode::Char('j') | KeyCode::Down => Action::MoveDown,
                KeyCode::Char('k') | KeyCode::Up => Action::MoveUp,
                KeyCode::Char('h') | KeyCode::Left => Action::ScrollLeft,
                KeyCode::Char('l') | KeyCode::Right => Action::ScrollRight,
                KeyCode::Char('G') | KeyCode::End => Action::GoToBottom,
                KeyCode::Char('g') if state.pending_g => Action::GoToTop,
                KeyCode::Char('g') => Action::PendingG,
                KeyCode::Home => Action::GoToTop,
                _ => Action::None,
            };
        }
        Modal::ModelPicker { custom_active, .. } => {
            if *custom_active {
                // In custom input mode: type characters, backspace, enter to confirm, esc to leave custom mode
                return match key.code {
                    KeyCode::Enter => Action::InputSubmit,
                    KeyCode::Esc => Action::DismissModal,
                    KeyCode::Backspace => Action::InputBackspace,
                    KeyCode::Char(c) => Action::InputChar(c),
                    _ => Action::None,
                };
            }
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Enter => Action::InputSubmit,
                KeyCode::Backspace => Action::InputBackspace,
                _ => Action::None,
            };
        }
        Modal::ThemePicker {
            themes, selected, ..
        } => {
            let len = themes.len().max(1);
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    let new_idx = if *selected == 0 {
                        len - 1
                    } else {
                        selected - 1
                    };
                    return Action::ThemePreview(new_idx);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let new_idx = (selected + 1) % len;
                    return Action::ThemePreview(new_idx);
                }
                KeyCode::Enter => return Action::InputSubmit,
                KeyCode::Esc => return Action::DismissModal,
                _ => {}
            }
        }
        Modal::IssueSourceManager { .. } => {
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Char('a') => Action::IssueSourceAdd,
                KeyCode::Char('d') => Action::IssueSourceDelete,
                _ => Action::None,
            };
        }
        Modal::GithubDiscoverOrgs {
            loading,
            orgs,
            cursor,
            ..
        } => {
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                _ if *loading => Action::None,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Action::HalfPageDown
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Action::HalfPageUp
                }
                KeyCode::Char('g') if state.pending_g => Action::GoToTop,
                KeyCode::Char('g') => Action::PendingG,
                KeyCode::Char('G') | KeyCode::End => Action::GoToBottom,
                KeyCode::Home => Action::GoToTop,
                KeyCode::Enter => {
                    // orgs[0] == "" means Personal; rest are org logins
                    let owner = orgs.get(*cursor).cloned().unwrap_or_default();
                    Action::GithubDrillIntoOwner { owner }
                }
                _ => Action::None,
            };
        }
        Modal::GithubDiscover { loading, repos, .. } => {
            return match key.code {
                KeyCode::Esc => Action::GithubBackToOrgs,
                // While loading, only allow Esc
                _ if *loading => Action::None,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Action::HalfPageDown
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Action::HalfPageUp
                }
                KeyCode::Char('g') if state.pending_g => Action::GoToTop,
                KeyCode::Char('g') => Action::PendingG,
                KeyCode::Char('G') | KeyCode::End => Action::GoToBottom,
                KeyCode::Home => Action::GoToTop,
                KeyCode::Char(' ') => Action::GithubDiscoverToggle,
                KeyCode::Char('a') => Action::GithubDiscoverSelectAll,
                KeyCode::Char('i') | KeyCode::Enter if !repos.is_empty() => {
                    Action::GithubDiscoverImport
                }
                _ => Action::None,
            };
        }
        Modal::PostCreatePicker { ref items, .. } => {
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Enter => Action::SelectPostCreateChoice(usize::MAX),
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    let n = c.to_digit(10).unwrap() as usize;
                    if n >= 1 && n <= items.len() {
                        Action::SelectPostCreateChoice(n - 1)
                    } else {
                        Action::None
                    }
                }
                _ => Action::None,
            };
        }
        Modal::GateAction { .. } => {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Action::ApproveGate,
                KeyCode::Char('n') | KeyCode::Char('N') => Action::RejectGate,
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Backspace => Action::GateInputBackspace,
                KeyCode::Char(c) => Action::GateInputChar(c),
                _ => Action::None,
            };
        }
        Modal::PrWorkflowPicker { .. } | Modal::WorkflowPicker { .. } => {
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Enter => Action::InputSubmit,
                _ => Action::None,
            };
        }
        Modal::Progress { .. } => {
            // Non-dismissable: swallow all keys while operation is in progress.
            return Action::None;
        }
        Modal::None => {}
    }

    // Filter mode
    if state.any_filter_active() {
        return match key.code {
            KeyCode::Esc => Action::ExitFilter,
            KeyCode::Enter => Action::ExitFilter,
            KeyCode::Backspace => Action::FilterBackspace,
            KeyCode::Char(c) => Action::FilterChar(c),
            _ => Action::None,
        };
    }

    // Vim-style scroll bindings (all views)
    // Handle `gg` chord: pending g + g → jump to top
    if state.pending_g && key.code == KeyCode::Char('g') {
        return Action::GoToTop;
    }

    // Ctrl+d / Ctrl+u for half-page scroll (must precede normal match to avoid
    // Ctrl+d matching 'd' → Delete)
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('d') => return Action::HalfPageDown,
            KeyCode::Char('u') => return Action::HalfPageUp,
            KeyCode::Char('h') => return Action::FocusContentColumn,
            KeyCode::Char('l') => return Action::FocusWorkflowColumn,
            _ => {}
        }
    }

    // Workflow column keybindings (when workflow column has focus) — must precede view-specific
    // bindings so that workflow column keys win when the column is active.
    if state.column_focus == ColumnFocus::Workflow {
        match key.code {
            KeyCode::Char('r') | KeyCode::Char('w') => return Action::RunWorkflow,
            KeyCode::Char('v') if state.workflows_focus == crate::state::WorkflowsFocus::Defs => {
                return Action::ViewWorkflowDef;
            }
            KeyCode::Char('e') if state.workflows_focus == crate::state::WorkflowsFocus::Defs => {
                return Action::EditWorkflowDef;
            }
            KeyCode::Char(' ') if state.workflows_focus == crate::state::WorkflowsFocus::Runs => {
                return Action::ToggleWorkflowRunCollapse;
            }
            _ => {}
        }
    }

    // View-specific keybindings (ticket list — RepoDetail Tickets pane)
    let in_ticket_list = state.view == View::RepoDetail
        && state.repo_detail_focus == crate::state::RepoDetailFocus::Tickets;
    if in_ticket_list {
        match key.code {
            KeyCode::Char('o') => return Action::OpenTicketUrl,
            KeyCode::Char('y') => return Action::CopyTicketUrl,
            KeyCode::Char('w') => return Action::PickWorkflow,
            KeyCode::Char('L') => return Action::EnterLabelFilter,
            _ => {}
        }
    }

    // View-specific keybindings (Dashboard Repos pane)
    if state.view == View::Dashboard && state.dashboard_focus == crate::state::DashboardFocus::Repos
    {
        match key.code {
            KeyCode::Char('o') => return Action::OpenRepoUrl,
            KeyCode::Char('y') => return Action::CopyRepoUrl,
            KeyCode::Char('w') => return Action::PickWorkflow,
            _ => {}
        }
    }

    // View-specific keybindings (Dashboard Worktrees pane)
    if state.view == View::Dashboard
        && state.dashboard_focus == crate::state::DashboardFocus::Worktrees
        && key.code == KeyCode::Char('w')
    {
        return Action::PickWorkflow;
    }

    // View-specific keybindings (WorktreeDetail agent controls)
    if state.view == View::WorktreeDetail {
        let agent_run = state
            .selected_worktree_id
            .as_ref()
            .and_then(|wt_id| state.data.latest_agent_runs.get(wt_id));

        let is_active = agent_run.is_some_and(|run| run.is_active());
        let is_waiting_for_feedback = agent_run.is_some_and(|run| run.is_waiting_for_feedback());

        let focus = state.worktree_detail_focus;

        match key.code {
            KeyCode::Char('p') => return Action::LaunchAgent,
            KeyCode::Char('O') if !is_active => return Action::OrchestrateAgent,
            KeyCode::Char('x') if is_active => return Action::StopAgent,
            KeyCode::Char('f') if is_waiting_for_feedback => return Action::SubmitFeedback,
            KeyCode::Char('F') if is_waiting_for_feedback => return Action::DismissFeedback,
            KeyCode::Char('r') => return Action::ResumeWorktreeWorkflow,
            KeyCode::Char('w') => return Action::PickWorkflow,
            KeyCode::Char('y') => return Action::WorktreeDetailCopy,
            KeyCode::Char('o') => return Action::WorktreeDetailOpen,
            KeyCode::Char('j') if focus == WorktreeDetailFocus::InfoPanel => {
                return Action::MoveDown
            }
            KeyCode::Char('k') if focus == WorktreeDetailFocus::InfoPanel => return Action::MoveUp,
            KeyCode::Char('j') if focus == WorktreeDetailFocus::LogPanel => {
                return Action::AgentActivityDown
            }
            KeyCode::Char('k') if focus == WorktreeDetailFocus::LogPanel => {
                return Action::AgentActivityUp
            }
            KeyCode::Enter if focus == WorktreeDetailFocus::LogPanel => {
                return Action::ExpandAgentEvent
            }
            KeyCode::Enter
                if focus == WorktreeDetailFocus::InfoPanel
                    && state.worktree_detail_selected_row == crate::state::info_row::MODEL =>
            {
                return Action::SetModel
            }
            KeyCode::Enter
                if focus == WorktreeDetailFocus::InfoPanel
                    && state.worktree_detail_selected_row == crate::state::info_row::TICKET =>
            {
                return Action::LinkTicket
            }
            _ => {}
        }
    }

    // View-specific keybindings (WorkflowRunDetail)
    if state.view == View::WorkflowRunDetail {
        match key.code {
            KeyCode::Char('x') => return Action::CancelWorkflow,
            KeyCode::Char('r') => return Action::ResumeWorkflow,
            KeyCode::Char('w') => return Action::PickWorkflow,
            KeyCode::Enter => {
                // Approve a waiting gate step if one exists
                let has_gate = state
                    .data
                    .workflow_steps
                    .iter()
                    .any(|s| s.status.to_string() == "waiting" && s.gate_type.is_some());
                if has_gate {
                    return Action::ApproveGate;
                }
            }
            _ => {}
        }
    }

    // View-specific keybindings (RepoDetail)
    if state.view == View::RepoDetail {
        if state.repo_detail_focus == crate::state::RepoDetailFocus::Info {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => return Action::MoveDown,
                KeyCode::Char('k') | KeyCode::Up => return Action::MoveUp,
                KeyCode::Char('o') => return Action::RepoDetailInfoOpen,
                KeyCode::Char('y') => return Action::RepoDetailInfoCopy,
                KeyCode::Char('w') => return Action::PickWorkflow,
                KeyCode::Enter
                    if state.repo_detail_info_row == crate::state::repo_info_row::MODEL =>
                {
                    return Action::SetModel
                }
                KeyCode::Enter
                    if state.repo_detail_info_row == crate::state::repo_info_row::AGENT_ISSUES =>
                {
                    return Action::ToggleAgentIssues
                }
                _ => {}
            }
        }
        if state.repo_detail_focus == crate::state::RepoDetailFocus::Worktrees {
            if let KeyCode::Char('w') = key.code {
                return Action::PickWorkflow;
            }
        }
        if state.repo_detail_focus == crate::state::RepoDetailFocus::Prs {
            match key.code {
                KeyCode::Char('o') => return Action::OpenPrUrl,
                KeyCode::Char('y') => return Action::CopyPrUrl,
                KeyCode::Char('r') | KeyCode::Char('w') => return Action::RunPrWorkflow,
                _ => {}
            }
        }
        if let KeyCode::Char('I') = key.code {
            return Action::ToggleAgentIssues;
        }
    }

    // Normal keybindings
    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('?') => Action::ShowHelp,
        KeyCode::Char('/') => Action::EnterFilter,
        KeyCode::Esc => Action::Back,
        KeyCode::Tab => Action::NextPanel,
        KeyCode::BackTab => Action::PrevPanel,
        KeyCode::Char('j') | KeyCode::Down => Action::MoveDown,
        KeyCode::Char('k') | KeyCode::Up => Action::MoveUp,
        KeyCode::Enter => Action::Select,

        // Scroll navigation
        KeyCode::Char('G') | KeyCode::End => Action::GoToBottom,
        KeyCode::Char('g') => Action::PendingG,
        KeyCode::Home => Action::GoToTop,

        // Toggle closed tickets visibility (all ticket views)
        KeyCode::Char('A') => Action::ToggleClosedTickets,

        // Toggle workflow column visibility
        KeyCode::Char('\\') => Action::ToggleWorkflowColumn,

        // Open the in-TUI theme picker
        KeyCode::Char('T') => Action::ShowThemePicker,

        // CRUD actions
        KeyCode::Char('a') => Action::RegisterRepo,
        KeyCode::Char('c') => Action::Create,
        KeyCode::Char('d') => Action::Delete,
        KeyCode::Char('s') => Action::SyncTickets,
        KeyCode::Char('S') => Action::ManageIssueSources,
        KeyCode::Char('o') => Action::OpenTicketUrl,

        // Direct view navigation
        KeyCode::Char('1') => Action::GoToDashboard,

        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::agent::{AgentRun, AgentRunStatus};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn make_agent_run(worktree_id: &str, status: AgentRunStatus) -> AgentRun {
        AgentRun {
            id: "run-1".into(),
            worktree_id: Some(worktree_id.to_string()),
            claude_session_id: None,
            prompt: "do stuff".into(),
            status,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            tmux_window: Some("feat-test".into()),
            log_file: None,
            model: None,
            plan: None,
            parent_run_id: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            bot_name: None,
        }
    }

    fn worktree_detail_state_with_run(status: AgentRunStatus) -> AppState {
        let mut state = AppState::new();
        state.view = View::WorktreeDetail;
        state.selected_worktree_id = Some("wt1".into());
        state
            .data
            .latest_agent_runs
            .insert("wt1".into(), make_agent_run("wt1", status));
        state
    }

    // --- PostCreatePicker tests ---

    use crate::state::PostCreateChoice;

    fn post_create_picker_state(item_count: usize) -> AppState {
        let mut items = vec![PostCreateChoice::StartAgent];
        for i in 0..item_count.saturating_sub(2) {
            items.push(PostCreateChoice::RunWorkflow {
                name: format!("workflow-{i}"),
                def: conductor_core::workflow::WorkflowDef {
                    name: format!("workflow-{i}"),
                    description: String::new(),
                    trigger: conductor_core::workflow::WorkflowTrigger::Manual,
                    targets: vec![],
                    inputs: vec![],
                    body: vec![],
                    always: vec![],
                    source_path: String::new(),
                },
            });
        }
        if item_count >= 2 {
            items.push(PostCreateChoice::Skip);
        }
        let mut state = AppState::new();
        state.modal = Modal::PostCreatePicker {
            items,
            selected: 0,
            worktree_id: "wt1".into(),
            worktree_path: "/tmp/wt".into(),
            worktree_slug: "wt-slug".into(),
            repo_path: "/tmp/repo".into(),
            ticket_id: String::new(),
        };
        state
    }

    #[test]
    fn post_create_picker_esc_dismisses_modal() {
        let state = post_create_picker_state(3);
        assert!(matches!(
            map_key(key(KeyCode::Esc), &state),
            Action::DismissModal
        ));
    }

    #[test]
    fn post_create_picker_up_down_navigation() {
        let state = post_create_picker_state(3);
        assert!(matches!(map_key(key(KeyCode::Up), &state), Action::MoveUp));
        assert!(matches!(
            map_key(key(KeyCode::Down), &state),
            Action::MoveDown
        ));
        assert!(matches!(
            map_key(key(KeyCode::Char('k')), &state),
            Action::MoveUp
        ));
        assert!(matches!(
            map_key(key(KeyCode::Char('j')), &state),
            Action::MoveDown
        ));
    }

    #[test]
    fn post_create_picker_enter_selects_with_sentinel() {
        let state = post_create_picker_state(3);
        assert!(matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::SelectPostCreateChoice(usize::MAX)
        ));
    }

    #[test]
    fn post_create_picker_valid_digit_selects_item() {
        let state = post_create_picker_state(3); // items: [StartAgent, workflow-0, Skip]
        assert!(matches!(
            map_key(key(KeyCode::Char('1')), &state),
            Action::SelectPostCreateChoice(0)
        ));
        assert!(matches!(
            map_key(key(KeyCode::Char('3')), &state),
            Action::SelectPostCreateChoice(2)
        ));
    }

    #[test]
    fn post_create_picker_out_of_range_digit_is_none() {
        let state = post_create_picker_state(3);
        // '0' is out of range (valid is 1..=3)
        assert!(matches!(
            map_key(key(KeyCode::Char('0')), &state),
            Action::None
        ));
        // '4' exceeds item count
        assert!(matches!(
            map_key(key(KeyCode::Char('4')), &state),
            Action::None
        ));
        // '9' exceeds item count
        assert!(matches!(
            map_key(key(KeyCode::Char('9')), &state),
            Action::None
        ));
    }

    #[test]
    fn post_create_picker_unhandled_key_is_none() {
        let state = post_create_picker_state(3);
        assert!(matches!(
            map_key(key(KeyCode::Char('x')), &state),
            Action::None
        ));
    }

    // --- WorktreeDetail focus-conditional j/k routing ---

    fn worktree_detail_state_with_focus(focus: WorktreeDetailFocus) -> AppState {
        let mut state = AppState::new();
        state.view = View::WorktreeDetail;
        state.worktree_detail_focus = focus;
        state.selected_worktree_id = Some("wt1".into());
        state
    }

    #[test]
    fn worktree_detail_jk_routes_to_move_when_info_panel_focused() {
        let state = worktree_detail_state_with_focus(WorktreeDetailFocus::InfoPanel);
        assert!(matches!(
            map_key(key(KeyCode::Char('j')), &state),
            Action::MoveDown
        ));
        assert!(matches!(
            map_key(key(KeyCode::Char('k')), &state),
            Action::MoveUp
        ));
    }

    #[test]
    fn worktree_detail_jk_routes_to_scroll_when_log_panel_focused() {
        let state = worktree_detail_state_with_focus(WorktreeDetailFocus::LogPanel);
        assert!(matches!(
            map_key(key(KeyCode::Char('j')), &state),
            Action::AgentActivityDown
        ));
        assert!(matches!(
            map_key(key(KeyCode::Char('k')), &state),
            Action::AgentActivityUp
        ));
    }

    #[test]
    fn worktree_detail_enter_expands_agent_event_when_log_panel_focused() {
        let state = worktree_detail_state_with_focus(WorktreeDetailFocus::LogPanel);
        assert!(matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::ExpandAgentEvent
        ));
    }

    #[test]
    fn worktree_detail_enter_does_not_expand_when_info_panel_focused() {
        let state = worktree_detail_state_with_focus(WorktreeDetailFocus::InfoPanel);
        assert!(!matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::ExpandAgentEvent
        ));
    }

    #[test]
    fn worktree_detail_orchestrate_agent_bound_to_shift_o_when_inactive() {
        // OrchestrateAgent is only available when no agent is active
        let state = worktree_detail_state_with_run(AgentRunStatus::Completed);
        assert!(matches!(
            map_key(key(KeyCode::Char('O')), &state),
            Action::OrchestrateAgent
        ));
    }

    #[test]
    fn worktree_detail_orchestrate_agent_not_available_when_active() {
        let state = worktree_detail_state_with_run(AgentRunStatus::Running);
        assert!(!matches!(
            map_key(key(KeyCode::Char('O')), &state),
            Action::OrchestrateAgent
        ));
    }

    // --- WorktreeDetail renamed bindings: y, o, l ---

    #[test]
    fn worktree_detail_y_maps_to_copy() {
        let state = worktree_detail_state_with_focus(WorktreeDetailFocus::InfoPanel);
        assert!(matches!(
            map_key(key(KeyCode::Char('y')), &state),
            Action::WorktreeDetailCopy
        ));
    }

    #[test]
    fn worktree_detail_o_maps_to_open() {
        let state = worktree_detail_state_with_focus(WorktreeDetailFocus::InfoPanel);
        assert!(matches!(
            map_key(key(KeyCode::Char('o')), &state),
            Action::WorktreeDetailOpen
        ));
    }

    // --- Removed global bindings (p, P, t, w, D) must not fire in Dashboard ---

    fn dashboard_state() -> AppState {
        let mut state = AppState::new();
        state.view = View::Dashboard;
        state
    }

    #[test]
    fn removed_global_bindings_produce_no_action_in_dashboard() {
        let state = dashboard_state();
        // All of these were removed in the keybinding cleanup (#515)
        // Note: 'w' was re-added as PickWorkflow
        for ch in ['p', 'P', 't', 'D'] {
            assert!(
                matches!(map_key(key(KeyCode::Char(ch)), &state), Action::None),
                "key '{ch}' should map to Action::None after removal but did not"
            );
        }
    }

    // --- WorkflowRunDetail: y/Y fires ApproveGate when a gate step is waiting ---

    fn workflow_run_detail_state_with_waiting_gate() -> AppState {
        use conductor_core::workflow::{WorkflowRunStep, WorkflowStepStatus};
        let mut state = AppState::new();
        state.view = View::WorkflowRunDetail;
        state.data.workflow_steps = vec![WorkflowRunStep {
            id: "step-1".into(),
            workflow_run_id: "run-1".into(),
            step_name: "review".into(),
            role: "reviewer".into(),
            can_commit: false,
            condition_expr: None,
            status: WorkflowStepStatus::Waiting,
            child_run_id: None,
            position: 0,
            started_at: None,
            ended_at: None,
            result_text: None,
            condition_met: None,
            iteration: 0,
            parallel_group_id: None,
            context_out: None,
            markers_out: None,
            retry_count: 0,
            gate_type: Some("approval".into()),
            gate_prompt: None,
            gate_timeout: None,
            gate_approved_by: None,
            gate_approved_at: None,
            gate_feedback: None,
            structured_output: None,
        }];
        state
    }

    #[test]
    fn workflow_run_detail_enter_approves_waiting_gate() {
        let state = workflow_run_detail_state_with_waiting_gate();
        assert!(matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::ApproveGate
        ));
    }

    #[test]
    fn workflow_run_detail_enter_does_not_approve_when_no_gate() {
        let mut state = AppState::new();
        state.view = View::WorkflowRunDetail;
        // No workflow steps → no waiting gate
        assert!(!matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::ApproveGate
        ));
    }

    #[test]
    fn workflow_run_detail_y_no_longer_approves_gate() {
        let state = workflow_run_detail_state_with_waiting_gate();
        assert!(!matches!(
            map_key(key(KeyCode::Char('y')), &state),
            Action::ApproveGate
        ));
        assert!(!matches!(
            map_key(key(KeyCode::Char('Y')), &state),
            Action::ApproveGate
        ));
    }

    // --- `w` key: PickWorkflow / RunWorkflow bindings ---

    // --- `r` key: ResumeWorktreeWorkflow binding ---

    #[test]
    fn r_maps_to_resume_worktree_workflow_in_worktree_detail() {
        let state = worktree_detail_state_with_focus(WorktreeDetailFocus::InfoPanel);
        assert!(matches!(
            map_key(key(KeyCode::Char('r')), &state),
            Action::ResumeWorktreeWorkflow
        ));
    }

    #[test]
    fn r_maps_to_run_workflow_in_workflows_view() {
        let mut state = AppState::new();
        state.view = View::Workflows;
        assert!(matches!(
            map_key(key(KeyCode::Char('r')), &state),
            Action::RunWorkflow
        ));
    }

    #[test]
    fn w_maps_to_pick_workflow_in_worktree_detail() {
        let state = worktree_detail_state_with_focus(WorktreeDetailFocus::InfoPanel);
        assert!(matches!(
            map_key(key(KeyCode::Char('w')), &state),
            Action::PickWorkflow
        ));
    }

    #[test]
    fn w_maps_to_run_workflow_in_workflow_column_focus() {
        let mut state = AppState::new();
        state.column_focus = crate::state::ColumnFocus::Workflow;
        assert!(matches!(
            map_key(key(KeyCode::Char('w')), &state),
            Action::RunWorkflow
        ));
    }

    #[test]
    fn w_maps_to_pick_workflow_in_dashboard_repos() {
        let mut state = AppState::new();
        state.view = View::Dashboard;
        state.dashboard_focus = crate::state::DashboardFocus::Repos;
        assert!(matches!(
            map_key(key(KeyCode::Char('w')), &state),
            Action::PickWorkflow
        ));
    }

    #[test]
    fn w_maps_to_pick_workflow_in_dashboard_worktrees() {
        let mut state = AppState::new();
        state.view = View::Dashboard;
        state.dashboard_focus = crate::state::DashboardFocus::Worktrees;
        assert!(matches!(
            map_key(key(KeyCode::Char('w')), &state),
            Action::PickWorkflow
        ));
    }

    #[test]
    fn w_maps_to_pick_workflow_in_repo_detail_info() {
        let mut state = AppState::new();
        state.view = View::RepoDetail;
        state.repo_detail_focus = crate::state::RepoDetailFocus::Info;
        assert!(matches!(
            map_key(key(KeyCode::Char('w')), &state),
            Action::PickWorkflow
        ));
    }

    #[test]
    fn w_maps_to_pick_workflow_in_repo_detail_worktrees() {
        let mut state = AppState::new();
        state.view = View::RepoDetail;
        state.repo_detail_focus = crate::state::RepoDetailFocus::Worktrees;
        assert!(matches!(
            map_key(key(KeyCode::Char('w')), &state),
            Action::PickWorkflow
        ));
    }

    // --- Progress modal key-swallowing ---

    fn progress_modal_state() -> AppState {
        let mut state = AppState::new();
        state.modal = Modal::Progress {
            message: "Creating worktree…".to_string(),
        };
        state
    }

    // --- ThemePicker key-handler tests ---

    fn theme_picker_state(selected: usize) -> AppState {
        let mut state = AppState::new();
        let theme_list: Vec<(String, String)> = crate::theme::KNOWN_THEMES
            .iter()
            .map(|(n, l)| (n.to_string(), l.to_string()))
            .collect();
        let loaded_themes: Vec<crate::theme::Theme> = theme_list
            .iter()
            .map(|(name, _)| crate::theme::Theme::from_name(name).unwrap_or_default())
            .collect();
        state.modal = Modal::ThemePicker {
            themes: theme_list,
            loaded_themes,
            selected,
            original_theme: crate::theme::Theme::default(),
            original_name: "conductor".to_string(),
        };
        state
    }

    #[test]
    fn theme_picker_esc_dismisses_modal() {
        let state = theme_picker_state(0);
        assert!(matches!(
            map_key(key(KeyCode::Esc), &state),
            Action::DismissModal
        ));
    }

    #[test]
    fn theme_picker_enter_submits() {
        let state = theme_picker_state(0);
        assert!(matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::InputSubmit
        ));
    }

    #[test]
    fn theme_picker_down_and_j_preview_next() {
        let len = crate::theme::KNOWN_THEMES.len();
        let state = theme_picker_state(0);
        assert!(matches!(
            map_key(key(KeyCode::Down), &state),
            Action::ThemePreview(1)
        ));
        assert!(matches!(
            map_key(key(KeyCode::Char('j')), &state),
            Action::ThemePreview(1)
        ));
        // wraps around at end
        let state_at_end = theme_picker_state(len - 1);
        assert!(matches!(
            map_key(key(KeyCode::Down), &state_at_end),
            Action::ThemePreview(0)
        ));
    }

    #[test]
    fn theme_picker_up_and_k_preview_prev() {
        let len = crate::theme::KNOWN_THEMES.len();
        let state = theme_picker_state(1);
        assert!(matches!(
            map_key(key(KeyCode::Up), &state),
            Action::ThemePreview(0)
        ));
        assert!(matches!(
            map_key(key(KeyCode::Char('k')), &state),
            Action::ThemePreview(0)
        ));
        // wraps around at start
        let state_at_start = theme_picker_state(0);
        assert!(matches!(
            map_key(key(KeyCode::Up), &state_at_start),
            Action::ThemePreview(idx) if idx == len - 1
        ));
    }

    #[test]
    fn theme_picker_unhandled_key_falls_through_to_none() {
        let state = theme_picker_state(0);
        assert!(matches!(
            map_key(key(KeyCode::Char('x')), &state),
            Action::None
        ));
    }

    #[test]
    fn progress_modal_swallows_esc() {
        let state = progress_modal_state();
        assert!(matches!(map_key(key(KeyCode::Esc), &state), Action::None));
    }

    #[test]
    fn progress_modal_swallows_enter() {
        let state = progress_modal_state();
        assert!(matches!(map_key(key(KeyCode::Enter), &state), Action::None));
    }

    #[test]
    fn progress_modal_swallows_char_keys() {
        let state = progress_modal_state();
        for c in ['q', 'j', 'k', 'w', 'n', ' '] {
            assert!(
                matches!(map_key(key(KeyCode::Char(c)), &state), Action::None),
                "expected None for key '{c}' while Progress modal is active"
            );
        }
    }

    #[test]
    fn progress_modal_swallows_navigation_keys() {
        let state = progress_modal_state();
        assert!(matches!(map_key(key(KeyCode::Up), &state), Action::None));
        assert!(matches!(map_key(key(KeyCode::Down), &state), Action::None));
        assert!(matches!(map_key(key(KeyCode::Left), &state), Action::None));
        assert!(matches!(map_key(key(KeyCode::Right), &state), Action::None));
    }

    // --- Workflow column focus keybindings ---

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn ctrl_l_maps_to_focus_workflow_column() {
        let state = AppState::new();
        assert!(matches!(
            map_key(ctrl(KeyCode::Char('l')), &state),
            Action::FocusWorkflowColumn
        ));
    }

    #[test]
    fn ctrl_h_maps_to_focus_content_column() {
        let state = AppState::new();
        assert!(matches!(
            map_key(ctrl(KeyCode::Char('h')), &state),
            Action::FocusContentColumn
        ));
    }

    #[test]
    fn backslash_maps_to_toggle_workflow_column() {
        let state = AppState::new();
        assert!(matches!(
            map_key(key(KeyCode::Char('\\')), &state),
            Action::ToggleWorkflowColumn
        ));
    }

    #[test]
    fn backslash_toggle_works_in_workflow_column_focus() {
        let mut state = AppState::new();
        state.column_focus = crate::state::ColumnFocus::Workflow;
        assert!(matches!(
            map_key(key(KeyCode::Char('\\')), &state),
            Action::ToggleWorkflowColumn
        ));
    }
}
