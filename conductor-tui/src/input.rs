use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::state::{AppState, DashboardFocus, Modal, SessionFocus, View};

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
        Modal::Input { .. } => {
            return match key.code {
                KeyCode::Enter => Action::InputSubmit,
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Backspace => Action::InputBackspace,
                KeyCode::Char(c) => Action::InputChar(c),
                _ => Action::None,
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
                _ => Action::None,
            };
        }
        Modal::TicketInfo { .. } => {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('q') => Action::DismissModal,
                KeyCode::Char('o') => Action::OpenTicketUrl,
                _ => Action::None,
            };
        }
        Modal::WorkTargetPicker { targets, .. } => {
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Enter => Action::SelectWorkTarget(usize::MAX), // sentinel: use selected
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    let n = c.to_digit(10).unwrap() as usize;
                    if n >= 1 && n <= targets.len() {
                        Action::SelectWorkTarget(n - 1)
                    } else {
                        Action::None
                    }
                }
                _ => Action::None,
            };
        }
        Modal::WorkTargetManager { .. } => {
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Char('K') => Action::WorkTargetMoveUp,
                KeyCode::Char('J') => Action::WorkTargetMoveDown,
                KeyCode::Char('a') => Action::WorkTargetAdd,
                KeyCode::Char('d') => Action::WorkTargetDelete,
                _ => Action::None,
            };
        }
        Modal::None => {}
    }

    // Filter mode
    if state.filter_active {
        return match key.code {
            KeyCode::Esc => Action::ExitFilter,
            KeyCode::Enter => Action::ExitFilter,
            KeyCode::Backspace => Action::FilterBackspace,
            KeyCode::Char(c) => Action::FilterChar(c),
            _ => Action::None,
        };
    }

    // View-specific keybindings (WorktreeDetail agent controls)
    if state.view == View::WorktreeDetail {
        let agent_run = state
            .selected_worktree_id
            .as_ref()
            .and_then(|wt_id| state.data.latest_agent_runs.get(wt_id));

        let has_running_agent = agent_run.is_some_and(|run| run.status == "running");
        let has_log = agent_run.is_some_and(|run| run.log_file.is_some());

        match key.code {
            KeyCode::Char('r') => return Action::LaunchAgent,
            KeyCode::Char('x') if has_running_agent => return Action::StopAgent,
            KeyCode::Char('L') if has_log => return Action::ViewAgentLog,
            KeyCode::Char('J') => return Action::AgentActivityDown,
            KeyCode::Char('K') => return Action::AgentActivityUp,
            _ => {}
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

        // CRUD actions (context-dependent)
        KeyCode::Char('a') => match state.view {
            View::Session => Action::AttachWorktree,
            _ => Action::AddRepo,
        },
        KeyCode::Char('c') => Action::Create,
        KeyCode::Char('d') => Action::Delete,
        KeyCode::Char('p') => Action::Push,
        KeyCode::Char('P') => Action::CreatePr,
        KeyCode::Char('s') => match state.view {
            View::Session => Action::EndSession,
            _ => Action::SyncTickets,
        },
        KeyCode::Char('S') => Action::StartSession,
        KeyCode::Char('l') => Action::LinkTicket,
        KeyCode::Char('w') => Action::StartWork,
        KeyCode::Char('W') => Action::ManageWorkTargets,
        KeyCode::Char('o') => Action::OpenTicketUrl,

        // Direct view navigation
        KeyCode::Char('t') => Action::GoToTickets,
        KeyCode::Char('1') => Action::GoToDashboard,
        KeyCode::Char('2') => Action::GoToTickets,
        KeyCode::Char('3') => Action::GoToSession,

        _ => Action::None,
    }
}

/// Get the current list length for the focused panel (used for bounds-checking navigation).
#[allow(dead_code)]
pub fn focused_list_len(state: &AppState) -> usize {
    match state.view {
        View::Dashboard => match state.dashboard_focus {
            DashboardFocus::Repos => state.data.repos.len(),
            DashboardFocus::Worktrees => state.data.worktrees.len(),
            DashboardFocus::Tickets => state.data.tickets.len(),
        },
        View::RepoDetail => state.detail_worktrees.len().max(state.detail_tickets.len()),
        View::WorktreeDetail => 0,
        View::Tickets => state.data.tickets.len(),
        View::Session => match state.session_focus {
            SessionFocus::Worktrees => state.data.session_worktrees.len(),
            SessionFocus::History => state.data.session_history.len(),
        },
    }
}
