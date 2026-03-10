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
            _ => {}
        }
    }

    // View-specific keybindings (WorktreeDetail agent controls)
    if state.view == View::WorktreeDetail {
        let agent_run = state
            .selected_worktree_id
            .as_ref()
            .and_then(|wt_id| state.data.latest_agent_runs.get(wt_id));

        let is_active = agent_run.is_some_and(|run| run.is_active());
        let is_waiting_for_feedback = agent_run.is_some_and(|run| run.is_waiting_for_feedback());
        let has_log = agent_run.is_some_and(|run| run.log_file.is_some());

        match key.code {
            KeyCode::Char('r') => return Action::LaunchAgent,
            KeyCode::Char('o') if !is_active => return Action::OrchestrateAgent,
            KeyCode::Char('x') if is_active => return Action::StopAgent,
            KeyCode::Char('a') if is_active => return Action::AttachAgent,
            KeyCode::Char('f') if is_waiting_for_feedback => return Action::SubmitFeedback,
            KeyCode::Char('F') if is_waiting_for_feedback => return Action::DismissFeedback,
            KeyCode::Char('L') if has_log => return Action::ViewAgentLog,
            KeyCode::Char('y') if has_log => return Action::CopyLastCodeBlock,
            KeyCode::Char('e') => return Action::ExpandAgentEvent,
            KeyCode::Char('j') => return Action::AgentActivityDown,
            KeyCode::Char('k') => return Action::AgentActivityUp,
            KeyCode::Char('m') => return Action::SetModel,
            _ => {}
        }
    }

    // View-specific keybindings (Workflows)
    if state.view == View::Workflows {
        match key.code {
            KeyCode::Char('r') => return Action::RunWorkflow,
            KeyCode::Char('v') if state.workflows_focus == crate::state::WorkflowsFocus::Defs => {
                return Action::ViewWorkflowDef;
            }
            KeyCode::Char('e') if state.workflows_focus == crate::state::WorkflowsFocus::Defs => {
                return Action::EditWorkflowDef;
            }
            _ => {}
        }
    }

    // View-specific keybindings (WorkflowRunDetail)
    if state.view == View::WorkflowRunDetail {
        match key.code {
            KeyCode::Char('x') => return Action::CancelWorkflow,
            KeyCode::Char('g') if !state.pending_g => {
                // Check for waiting gate
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
        match key.code {
            KeyCode::Char('m') => return Action::SetModel,
            KeyCode::Char('I') => return Action::ToggleAgentIssues,
            _ => {}
        }
    }

    // View-specific keybindings (Dashboard — Repos or Worktrees panel)
    if state.view == View::Dashboard {
        use crate::state::DashboardFocus;
        if let KeyCode::Char('m') = key.code {
            if matches!(
                state.dashboard_focus,
                DashboardFocus::Repos | DashboardFocus::Worktrees
            ) {
                return Action::SetModel;
            }
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

        // Toggle global status bar expansion (useful when 4+ items are active)
        KeyCode::Char('!') => Action::ToggleStatusBar,

        // CRUD actions
        KeyCode::Char('a') => Action::AddRepo,
        KeyCode::Char('c') => Action::Create,
        KeyCode::Char('d') => Action::Delete,
        KeyCode::Char('D') => Action::DiscoverGithubOrgs,
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
        KeyCode::Char('3') => Action::GoToWorkflows,

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
            worktree_id: worktree_id.into(),
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

    #[test]
    fn attach_agent_key_when_active_maps_to_attach_agent() {
        let state = worktree_detail_state_with_run(AgentRunStatus::Running);
        assert!(matches!(
            map_key(key(KeyCode::Char('a')), &state),
            Action::AttachAgent
        ));
    }

    #[test]
    fn attach_agent_key_when_inactive_does_not_map_to_attach_agent() {
        let state = worktree_detail_state_with_run(AgentRunStatus::Completed);
        // 'a' falls through to the global binding (AddRepo), not AttachAgent
        assert!(!matches!(
            map_key(key(KeyCode::Char('a')), &state),
            Action::AttachAgent
        ));
    }

    #[test]
    fn attach_agent_key_when_waiting_for_feedback_maps_to_attach_agent() {
        let state = worktree_detail_state_with_run(AgentRunStatus::WaitingForFeedback);
        assert!(matches!(
            map_key(key(KeyCode::Char('a')), &state),
            Action::AttachAgent
        ));
    }
}
