use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::state::{AppState, Modal, View};

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
            _ => {}
        }
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
            KeyCode::Char('j') => return Action::AgentActivityDown,
            KeyCode::Char('k') => return Action::AgentActivityUp,
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

        // Scroll navigation
        KeyCode::Char('G') | KeyCode::End => Action::GoToBottom,
        KeyCode::Char('g') => Action::PendingG,
        KeyCode::Home => Action::GoToTop,

        // CRUD actions
        KeyCode::Char('a') => Action::AddRepo,
        KeyCode::Char('c') => Action::Create,
        KeyCode::Char('d') => Action::Delete,
        KeyCode::Char('p') => Action::Push,
        KeyCode::Char('P') => Action::CreatePr,
        KeyCode::Char('s') => Action::SyncTickets,
        KeyCode::Char('l') => Action::LinkTicket,
        KeyCode::Char('w') => Action::StartWork,
        KeyCode::Char('W') => Action::ManageWorkTargets,
        KeyCode::Char('S') => Action::ManageIssueSources,
        KeyCode::Char('o') => Action::OpenTicketUrl,

        // Direct view navigation
        KeyCode::Char('t') => Action::GoToTickets,
        KeyCode::Char('1') => Action::GoToDashboard,
        KeyCode::Char('2') => Action::GoToTickets,

        _ => Action::None,
    }
}
