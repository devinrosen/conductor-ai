use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::state::{AppState, DashboardFocus, Modal, View};

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
        View::Session => state.data.session_worktrees.len(),
    }
}
