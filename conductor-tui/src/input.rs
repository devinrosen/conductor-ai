use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::state::{
    AppState, ColumnFocus, Modal, View, WorkflowRunDetailFocus, WorktreeDetailFocus,
};

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
                KeyCode::Char(' ') => Action::FormToggle,
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
                KeyCode::Char('g') => Action::GoToTop,
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
                KeyCode::Char('g') | KeyCode::Home => Action::GoToTop,
                KeyCode::Char('G') | KeyCode::End => Action::GoToBottom,
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
                KeyCode::Char('g') | KeyCode::Home => {
                    return Action::ThemePreview(0);
                }
                KeyCode::Char('G') | KeyCode::End => {
                    let new_idx = len.saturating_sub(1);
                    return Action::ThemePreview(new_idx);
                }
                KeyCode::Enter => return Action::InputSubmit,
                KeyCode::Esc => return Action::DismissModal,
                _ => return Action::None,
            }
        }
        Modal::IssueSourceManager { .. } => {
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Char('a') => Action::IssueSourceAdd,
                KeyCode::Char('d') => Action::IssueSourceDelete,
                KeyCode::Char('g') | KeyCode::Home => Action::GoToTop,
                KeyCode::Char('G') | KeyCode::End => Action::GoToBottom,
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
                KeyCode::Char('g') => Action::GoToTop,
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
                KeyCode::Char('g') => Action::GoToTop,
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
        Modal::BranchPicker { ref items, .. } => {
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Enter => Action::SelectBranch(None),
                KeyCode::Char('g') | KeyCode::Home => Action::GoToTop,
                KeyCode::Char('G') | KeyCode::End => Action::GoToBottom,
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    let n = c.to_digit(10).unwrap() as usize;
                    if n >= 1 && n <= items.len() {
                        Action::SelectBranch(Some(n - 1))
                    } else {
                        Action::None
                    }
                }
                _ => Action::None,
            };
        }
        Modal::BaseBranchPicker { ref items, .. } => {
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Enter => Action::SelectBaseBranch(None),
                KeyCode::Char('g') | KeyCode::Home => Action::GoToTop,
                KeyCode::Char('G') | KeyCode::End => Action::GoToBottom,
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    let n = c.to_digit(10).unwrap() as usize;
                    if n >= 1 && n <= items.len() {
                        Action::SelectBaseBranch(Some(n - 1))
                    } else {
                        Action::None
                    }
                }
                _ => Action::None,
            };
        }
        Modal::GateAction { options, .. } => {
            if options.is_empty() {
                // Binary approve/reject mode — original behaviour.
                return match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Action::ApproveGate,
                    KeyCode::Char('n') | KeyCode::Char('N') => Action::RejectGate,
                    KeyCode::Esc => Action::DismissModal,
                    KeyCode::Backspace => Action::GateInputBackspace,
                    KeyCode::Char(c) => Action::GateInputChar(c),
                    _ => Action::None,
                };
            } else {
                // Checklist mode.
                return match key.code {
                    KeyCode::Char('j') | KeyCode::Down => Action::MoveDown,
                    KeyCode::Char('k') | KeyCode::Up => Action::MoveUp,
                    KeyCode::Char(' ') => Action::GateToggleOption,
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => Action::ApproveGate,
                    KeyCode::Char('n') | KeyCode::Char('N') => Action::RejectGate,
                    KeyCode::Esc => Action::DismissModal,
                    _ => Action::None,
                };
            }
        }
        Modal::TemplatePicker { ref items, .. } => {
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Enter => Action::InputSubmit,
                KeyCode::Char('g') | KeyCode::Home => Action::GoToTop,
                KeyCode::Char('G') | KeyCode::End => Action::GoToBottom,
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    let n = c.to_digit(10).unwrap() as usize;
                    if n >= 1 && n <= items.len() {
                        // Jump to the selected item
                        Action::SelectListItem(n - 1)
                    } else {
                        Action::None
                    }
                }
                _ => Action::None,
            };
        }
        Modal::WorkflowPicker { .. } => {
            return match key.code {
                KeyCode::Esc => Action::DismissModal,
                KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
                KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
                KeyCode::Enter => Action::InputSubmit,
                KeyCode::Char('g') | KeyCode::Home => Action::GoToTop,
                KeyCode::Char('G') | KeyCode::End => Action::GoToBottom,
                _ => Action::None,
            };
        }
        Modal::Progress { .. } => {
            // Non-dismissable: swallow all keys while operation is in progress.
            return Action::None;
        }
        Modal::GraphView { .. } => {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('q') => Action::DismissModal,
                KeyCode::Char('h') | KeyCode::Left => Action::GraphNavLeft,
                KeyCode::Char('l') | KeyCode::Right => Action::GraphNavRight,
                KeyCode::Char('k') | KeyCode::Up => Action::GraphNavUp,
                KeyCode::Char('j') | KeyCode::Down => Action::GraphNavDown,
                KeyCode::Char('H') => Action::GraphPanLeft,
                KeyCode::Char('L') => Action::GraphPanRight,
                KeyCode::Char('K') => Action::GraphPanUp,
                KeyCode::Char('J') => Action::GraphPanDown,
                KeyCode::Enter => Action::Select,
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

    // Ctrl+d / Ctrl+u for half-page scroll (must precede normal match to avoid
    // Ctrl+d matching 'd' → Delete)
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('d') => return Action::HalfPageDown,
            KeyCode::Char('u') => return Action::HalfPageUp,
            _ => {}
        }
    }

    // Workflow column keybindings (when workflow column has focus) — must precede view-specific
    // bindings so that workflow column keys win when the column is active.
    if state.column_focus == ColumnFocus::Workflow {
        match key.code {
            KeyCode::Char('H') => return Action::ToggleCompletedRuns,
            KeyCode::Char('V') => return Action::ToggleDismissedRuns,
            KeyCode::Char('r') => return Action::RunWorkflow,
            KeyCode::Char('v')
                if state.workflows_focus == crate::state::WorkflowsFocus::Defs
                    && state.workflow_def_focus == crate::state::WorkflowDefFocus::List =>
            {
                return Action::ViewWorkflowDef;
            }
            KeyCode::Char('e')
                if state.workflows_focus == crate::state::WorkflowsFocus::Defs
                    && state.workflow_def_focus == crate::state::WorkflowDefFocus::List =>
            {
                return Action::EditWorkflowDef;
            }
            KeyCode::Char(' ') if state.workflows_focus == crate::state::WorkflowsFocus::Runs => {
                return Action::ToggleWorkflowRunCollapse;
            }
            KeyCode::Char(' ')
                if state.workflows_focus == crate::state::WorkflowsFocus::Defs
                    && state.workflow_def_focus == crate::state::WorkflowDefFocus::List =>
            {
                return Action::ToggleWorkflowDefsCollapse;
            }
            KeyCode::Char('w') if state.workflows_focus == crate::state::WorkflowsFocus::Runs => {
                return Action::PickWorkflow;
            }
            KeyCode::Char('t') if state.workflows_focus == crate::state::WorkflowsFocus::Runs => {
                return Action::PickTemplate;
            }
            KeyCode::Char('D') if state.workflows_focus == crate::state::WorkflowsFocus::Runs => {
                return Action::DeleteWorkflowRun;
            }
            // / key: open inline workflow name filter when Runs pane is focused.
            KeyCode::Char('/')
                if state.workflows_focus == crate::state::WorkflowsFocus::Runs
                    && state.column_focus == crate::state::ColumnFocus::Workflow =>
            {
                return Action::OpenWorkflowFilter;
            }
            // Filter-bar input handling when filter is active.
            KeyCode::Esc if state.workflows_focus == crate::state::WorkflowsFocus::Filter => {
                return Action::ClearWorkflowFilter;
            }
            KeyCode::Enter if state.workflows_focus == crate::state::WorkflowsFocus::Filter => {
                return Action::ConfirmWorkflowFilter;
            }
            KeyCode::Backspace if state.workflows_focus == crate::state::WorkflowsFocus::Filter => {
                return Action::WorkflowFilterBackspace;
            }
            KeyCode::Char(c) if state.workflows_focus == crate::state::WorkflowsFocus::Filter => {
                return Action::WorkflowFilterInput(c);
            }
            // Right / l: enter or exit the step tree pane when viewing defs.
            KeyCode::Right | KeyCode::Char('l')
                if state.workflows_focus == crate::state::WorkflowsFocus::Defs =>
            {
                return Action::ToggleDefStepTree;
            }
            KeyCode::Enter => return Action::Select,
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
            KeyCode::Char('g') => return Action::OpenTicketGraphView,
            KeyCode::Char(' ') if state.column_focus == crate::state::ColumnFocus::Content => {
                return Action::ToggleTicketCollapse;
            }
            _ => {}
        }
    }

    // View-specific keybindings (Settings view)
    if state.view == View::Settings {
        return match key.code {
            KeyCode::Esc => Action::Back,
            KeyCode::Tab => Action::NextPanel,
            KeyCode::BackTab => Action::PrevPanel,
            KeyCode::Char('j') | KeyCode::Down => Action::MoveDown,
            KeyCode::Char('k') | KeyCode::Up => Action::MoveUp,
            KeyCode::Enter => {
                use crate::state::SettingsFocus;
                if state.settings_focus == SettingsFocus::SettingsList {
                    Action::SettingsEditSetting
                } else {
                    // Enter on category list focuses the right pane
                    Action::NextPanel
                }
            }
            KeyCode::Char('c') => Action::SettingsCycleValue,
            KeyCode::Char('t') => {
                use crate::state::SettingsCategory;
                if state.settings_category == SettingsCategory::Notifications {
                    if let Some(idx) = state.settings_selected_hook_index() {
                        Action::SettingsTestHook { hook_index: idx }
                    } else {
                        Action::None
                    }
                } else {
                    Action::None
                }
            }
            KeyCode::Char('o') => {
                use crate::state::SettingsCategory;
                if state.settings_category == SettingsCategory::Notifications {
                    if let Some(idx) = state.settings_selected_hook_index() {
                        Action::SettingsOpenHookScript { hook_index: idx }
                    } else {
                        Action::None
                    }
                } else {
                    Action::None
                }
            }
            _ => Action::None,
        };
    }

    // View-specific keybindings (Dashboard)
    if state.view == View::Dashboard {
        match key.code {
            KeyCode::Char('o') => return Action::OpenRepoUrl,
            KeyCode::Char('y') => return Action::CopyRepoUrl,
            KeyCode::Char('w') => return Action::PickWorkflow,
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
        let is_failed = agent_run.is_some_and(|run| {
            matches!(
                run.status,
                conductor_core::agent::AgentRunStatus::Failed
                    | conductor_core::agent::AgentRunStatus::Cancelled
            )
        });

        let focus = state.worktree_detail_focus;

        match key.code {
            KeyCode::Char('p') => return Action::LaunchAgent,
            KeyCode::Char('X') if !is_active => return Action::ClearConversation,
            KeyCode::Char('x') if is_active => return Action::StopAgent,
            KeyCode::Char('R') if is_failed => return Action::RestartAgent,
            KeyCode::Char('f') if is_waiting_for_feedback => return Action::SubmitFeedback,
            KeyCode::Char('F') if is_waiting_for_feedback => return Action::DismissFeedback,
            KeyCode::Char('r') => return Action::ResumeWorktreeWorkflow,
            KeyCode::Char('w') => return Action::PickWorkflow,
            KeyCode::Char('t') => return Action::PickTemplate,
            KeyCode::Char('y') => return Action::WorktreeDetailCopy,
            KeyCode::Char('o') => return Action::WorktreeDetailOpen,
            KeyCode::Char('j')
                if focus == WorktreeDetailFocus::InfoPanel
                    && state.column_focus == ColumnFocus::Content =>
            {
                return Action::MoveDown
            }
            KeyCode::Char('k')
                if focus == WorktreeDetailFocus::InfoPanel
                    && state.column_focus == ColumnFocus::Content =>
            {
                return Action::MoveUp
            }
            KeyCode::Char('j')
                if focus == WorktreeDetailFocus::LogPanel
                    && state.column_focus == ColumnFocus::Content =>
            {
                return Action::AgentActivityDown
            }
            KeyCode::Char('k')
                if focus == WorktreeDetailFocus::LogPanel
                    && state.column_focus == ColumnFocus::Content =>
            {
                return Action::AgentActivityUp
            }
            KeyCode::Enter
                if focus == WorktreeDetailFocus::LogPanel
                    && state.column_focus == ColumnFocus::Content =>
            {
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
                    && state.worktree_detail_selected_row == crate::state::info_row::BASE =>
            {
                return Action::SetBaseBranch
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
        // Info-pane-specific keys (j/k/y only when Info is focused)
        if state.workflow_run_detail_focus == WorkflowRunDetailFocus::Info {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => return Action::MoveDown,
                KeyCode::Char('k') | KeyCode::Up => return Action::MoveUp,
                KeyCode::Char('y') => return Action::WorkflowRunDetailCopy,
                KeyCode::Enter
                    if state.workflow_run_info_row
                        == crate::state::workflow_run_info_row::DISMISSED =>
                {
                    return Action::ToggleWorkflowRunDismissed
                }
                _ => {}
            }
        }
        // Error-pane-specific keys (j/k scroll, y copies full error text)
        if state.workflow_run_detail_focus == WorkflowRunDetailFocus::Error {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => return Action::MoveDown,
                KeyCode::Char('k') | KeyCode::Up => return Action::MoveUp,
                KeyCode::Char('y') => return Action::WorkflowRunDetailCopy,
                _ => {}
            }
        }
        // Space toggles expand/collapse for foreach steps (Steps focus only)
        if state.workflow_run_detail_focus == WorkflowRunDetailFocus::Steps {
            if let KeyCode::Char(' ') = key.code {
                if let Some(step) = state.data.workflow_steps.get(state.workflow_step_index) {
                    if step.role == conductor_core::workflow::STEP_ROLE_FOREACH {
                        return Action::ToggleForeachStepExpand;
                    }
                }
            }
        }
        match key.code {
            KeyCode::Char('x') => return Action::CancelWorkflow,
            KeyCode::Char('r') => return Action::ResumeWorkflow,
            KeyCode::Char('D') => return Action::DeleteWorkflowRun,
            KeyCode::Char('w') => return Action::PickWorkflow,
            KeyCode::Char('g') => return Action::OpenWorkflowStepGraphView,
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
        if state.repo_detail_focus == crate::state::RepoDetailFocus::RepoAgent {
            let latest_run = state
                .selected_repo_id
                .as_ref()
                .and_then(|id| state.data.latest_repo_agent_runs.get(id));
            let is_active = latest_run.map(|r| r.is_active()).unwrap_or(false);
            let is_waiting = latest_run
                .map(|r| r.is_waiting_for_feedback())
                .unwrap_or(false);
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => return Action::AgentActivityDown,
                KeyCode::Char('k') | KeyCode::Up => return Action::AgentActivityUp,
                KeyCode::Char('x') if is_active => return Action::StopAgent,
                KeyCode::Char('f') if is_waiting => return Action::SubmitFeedback,
                KeyCode::Char('F') if is_waiting => return Action::DismissFeedback,
                KeyCode::Enter => return Action::ExpandAgentEvent,
                _ => {}
            }
        }
        if let KeyCode::Char('p') = key.code {
            return Action::PromptRepoAgent;
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
        KeyCode::Char('g') => Action::GoToTop,
        KeyCode::Home => Action::GoToTop,

        // Toggle closed tickets visibility (all ticket views)
        KeyCode::Char('A') => Action::ToggleClosedTickets,

        // Toggle workflow column visibility
        KeyCode::Char('\\') => Action::ToggleWorkflowColumn,

        // Navigate between columns
        KeyCode::Char('[') => Action::FocusContentColumn,
        KeyCode::Char(']') => Action::FocusWorkflowColumn,

        // Open the in-TUI theme picker
        KeyCode::Char('T') => Action::ShowThemePicker,

        // CRUD actions
        KeyCode::Char('a') => Action::RegisterRepo,
        KeyCode::Char('c') => Action::Create,
        KeyCode::Char('d') => Action::Delete,
        KeyCode::Char('s') => Action::SyncTickets,
        KeyCode::Char('S') => Action::OpenSettings,
        KeyCode::Char('o') => Action::OpenTicketUrl,

        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    // --- WorkflowPicker tests (post-create variant) ---

    use crate::state::WorkflowPickerItem;

    fn workflow_picker_state(item_count: usize) -> AppState {
        let mut items = vec![WorkflowPickerItem::StartAgent];
        for i in 0..item_count.saturating_sub(2) {
            items.push(WorkflowPickerItem::Workflow(
                conductor_core::workflow::WorkflowDef {
                    name: format!("workflow-{i}"),
                    title: None,
                    description: String::new(),
                    trigger: conductor_core::workflow::WorkflowTrigger::Manual,
                    targets: vec![],
                    group: None,
                    inputs: vec![],
                    body: vec![],
                    always: vec![],
                    source_path: String::new(),
                },
            ));
        }
        if item_count >= 2 {
            items.push(WorkflowPickerItem::Skip);
        }
        let mut state = AppState::new();
        state.modal = Modal::WorkflowPicker {
            target: crate::state::WorkflowPickerTarget::PostCreate {
                worktree_id: "wt1".into(),
                worktree_path: "/tmp/wt".into(),
                worktree_slug: "wt-slug".into(),
                repo_path: "/tmp/repo".into(),
                ticket_id: String::new(),
            },
            items,
            selected: 0,
            scroll_offset: 0,
        };
        state
    }

    #[test]
    fn workflow_picker_esc_dismisses_modal() {
        let state = workflow_picker_state(3);
        assert!(matches!(
            map_key(key(KeyCode::Esc), &state),
            Action::DismissModal
        ));
    }

    #[test]
    fn workflow_picker_up_down_navigation() {
        let state = workflow_picker_state(3);
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
    fn workflow_picker_enter_submits() {
        let state = workflow_picker_state(3);
        assert!(matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::InputSubmit
        ));
    }

    #[test]
    fn workflow_picker_unhandled_key_is_none() {
        let state = workflow_picker_state(3);
        assert!(matches!(
            map_key(key(KeyCode::Char('x')), &state),
            Action::None
        ));
    }

    // --- BranchPicker tests ---

    use crate::state::BranchPickerItem;

    fn branch_picker_state(item_count: usize) -> AppState {
        let mut items = Vec::with_capacity(item_count);
        // First item: default branch (branch = None)
        if item_count > 0 {
            items.push(BranchPickerItem {
                branch: None,
                worktree_count: 0,
                ticket_count: 0,
                base_branch: None,
                stale_days: None,
                inferred_from: None,
            });
        }
        for i in 1..item_count {
            items.push(BranchPickerItem {
                branch: Some(format!("feat/branch-{i}")),
                worktree_count: 0,
                ticket_count: 0,
                base_branch: Some("main".into()),
                stale_days: None,
                inferred_from: None,
            });
        }
        let (ordered, tree_positions) = crate::state::build_branch_picker_tree(&items);
        let mut state = AppState::new();
        state.modal = Modal::BranchPicker {
            repo_slug: "test-repo".into(),
            wt_name: "wt-name".into(),
            ticket_id: None,
            items: ordered,
            tree_positions,
            selected: 0,
        };
        state
    }

    #[test]
    fn branch_picker_esc_dismisses_modal() {
        let state = branch_picker_state(3);
        assert!(matches!(
            map_key(key(KeyCode::Esc), &state),
            Action::DismissModal
        ));
    }

    #[test]
    fn branch_picker_up_down_navigation() {
        let state = branch_picker_state(3);
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
    fn branch_picker_enter_selects_with_none() {
        let state = branch_picker_state(3);
        assert!(matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::SelectBranch(None)
        ));
    }

    #[test]
    fn branch_picker_valid_digit_selects_item() {
        let state = branch_picker_state(3);
        assert!(matches!(
            map_key(key(KeyCode::Char('1')), &state),
            Action::SelectBranch(Some(0))
        ));
        assert!(matches!(
            map_key(key(KeyCode::Char('3')), &state),
            Action::SelectBranch(Some(2))
        ));
    }

    #[test]
    fn branch_picker_out_of_range_digit_is_none() {
        let state = branch_picker_state(3);
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
    fn branch_picker_unhandled_key_is_none() {
        let state = branch_picker_state(3);
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
    fn worktree_detail_jk_routes_to_move_when_workflow_column_focused() {
        // When workflow column has focus, j/k should not be captured by WorktreeDetail
        // and should fall through to MoveDown/MoveUp for workflow column navigation.
        let mut state = worktree_detail_state_with_focus(WorktreeDetailFocus::LogPanel);
        state.column_focus = crate::state::ColumnFocus::Workflow;
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
    fn worktree_detail_enter_does_not_expand_when_workflow_column_focused() {
        let mut state = worktree_detail_state_with_focus(WorktreeDetailFocus::LogPanel);
        state.column_focus = crate::state::ColumnFocus::Workflow;
        assert!(!matches!(
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
        for ch in ['p', 'P', 'D'] {
            assert!(
                matches!(map_key(key(KeyCode::Char(ch)), &state), Action::None),
                "key '{ch}' should map to Action::None after removal but did not"
            );
        }
    }

    // --- WorkflowRunDetail: y/Y fires ApproveGate when a gate step is waiting ---

    fn workflow_run_detail_state_with_waiting_gate() -> AppState {
        use conductor_core::workflow::{GateType, WorkflowRunStep, WorkflowStepStatus};
        let mut state = AppState::new();
        state.view = View::WorkflowRunDetail;
        let base = crate::state::tests::make_wf_step("step-1", "run-1", "review", 0);
        state.data.workflow_steps = vec![WorkflowRunStep {
            role: "reviewer".into(),
            status: WorkflowStepStatus::Waiting,
            gate_type: Some(GateType::HumanApproval),
            ..base
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

    // --- Space key: ToggleForeachStepExpand binding ---

    fn workflow_run_detail_steps_state_with_foreach() -> AppState {
        use conductor_core::workflow::{WorkflowRunStep, STEP_ROLE_FOREACH};
        let mut state = AppState::new();
        state.view = View::WorkflowRunDetail;
        state.workflow_run_detail_focus = crate::state::WorkflowRunDetailFocus::Steps;
        let base = crate::state::tests::make_wf_step("step-fe", "run-1", "items", 0);
        state.data.workflow_steps = vec![WorkflowRunStep {
            role: STEP_ROLE_FOREACH.into(),
            ..base
        }];
        state.workflow_step_index = 0;
        state
    }

    #[test]
    fn space_on_foreach_step_fires_toggle_foreach_expand() {
        let state = workflow_run_detail_steps_state_with_foreach();
        assert!(matches!(
            map_key(key(KeyCode::Char(' ')), &state),
            Action::ToggleForeachStepExpand
        ));
    }

    #[test]
    fn space_on_non_foreach_step_does_not_fire_toggle() {
        let mut state = AppState::new();
        state.view = View::WorkflowRunDetail;
        state.workflow_run_detail_focus = crate::state::WorkflowRunDetailFocus::Steps;
        // Default role from make_wf_step is "actor", not "foreach".
        let step = crate::state::tests::make_wf_step("step-actor", "run-1", "build", 0);
        state.data.workflow_steps = vec![step];
        state.workflow_step_index = 0;
        assert!(!matches!(
            map_key(key(KeyCode::Char(' ')), &state),
            Action::ToggleForeachStepExpand
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
    fn r_maps_to_run_workflow_in_workflow_column_focus() {
        let mut state = AppState::new();
        state.column_focus = crate::state::ColumnFocus::Workflow;
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
    fn w_maps_to_pick_workflow_in_workflow_column_focus() {
        let mut state = AppState::new();
        state.column_focus = crate::state::ColumnFocus::Workflow;
        assert!(matches!(
            map_key(key(KeyCode::Char('w')), &state),
            Action::PickWorkflow
        ));
    }

    #[test]
    fn w_maps_to_pick_workflow_in_dashboard() {
        let mut state = AppState::new();
        state.view = View::Dashboard;
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
    fn theme_picker_q_does_not_quit() {
        // Regression test for #847: pressing 'q' while ThemePicker is open
        // must return Action::None, not Action::Quit.
        let state = theme_picker_state(0);
        assert!(matches!(
            map_key(key(KeyCode::Char('q')), &state),
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

    #[test]
    fn left_bracket_maps_to_focus_content_column() {
        let state = AppState::new();
        assert!(matches!(
            map_key(key(KeyCode::Char('[')), &state),
            Action::FocusContentColumn
        ));
    }

    #[test]
    fn right_bracket_maps_to_focus_workflow_column() {
        let state = AppState::new();
        assert!(matches!(
            map_key(key(KeyCode::Char(']')), &state),
            Action::FocusWorkflowColumn
        ));
    }

    #[test]
    fn w_maps_to_pick_workflow_in_workflow_column_runs() {
        let mut state = AppState::new();
        state.column_focus = crate::state::ColumnFocus::Workflow;
        state.workflows_focus = crate::state::WorkflowsFocus::Runs;
        assert!(matches!(
            map_key(key(KeyCode::Char('w')), &state),
            Action::PickWorkflow
        ));
    }

    // --- `t` key: PickTemplate binding ---

    #[test]
    fn t_maps_to_pick_template_in_workflow_column_runs() {
        let mut state = AppState::new();
        state.column_focus = crate::state::ColumnFocus::Workflow;
        state.workflows_focus = crate::state::WorkflowsFocus::Runs;
        assert!(matches!(
            map_key(key(KeyCode::Char('t')), &state),
            Action::PickTemplate
        ));
    }

    #[test]
    fn t_maps_to_pick_template_in_worktree_detail() {
        let state = worktree_detail_state_with_focus(WorktreeDetailFocus::InfoPanel);
        assert!(matches!(
            map_key(key(KeyCode::Char('t')), &state),
            Action::PickTemplate
        ));
    }

    #[test]
    fn t_does_not_map_to_pick_template_in_workflow_column_defs() {
        let mut state = AppState::new();
        state.column_focus = crate::state::ColumnFocus::Workflow;
        state.workflows_focus = crate::state::WorkflowsFocus::Defs;
        assert!(!matches!(
            map_key(key(KeyCode::Char('t')), &state),
            Action::PickTemplate
        ));
    }

    // Regression test for #2092: Enter in workflow column (Runs focus) must
    // produce Action::Select (drill into run), not Action::LinkTicket.
    #[test]
    fn enter_in_workflow_column_runs_maps_to_select() {
        let mut state = AppState::new();
        state.column_focus = crate::state::ColumnFocus::Workflow;
        state.workflows_focus = crate::state::WorkflowsFocus::Runs;
        assert!(matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::Select
        ));
    }

    #[test]
    fn enter_in_workflow_column_defs_maps_to_select() {
        let mut state = AppState::new();
        state.column_focus = crate::state::ColumnFocus::Workflow;
        state.workflows_focus = crate::state::WorkflowsFocus::Defs;
        assert!(matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::Select
        ));
    }

    // --- WorkflowPicker tests (Repo target variant) ---
    // Key-mapping is target-agnostic (the WorkflowPicker arm in map_key does not
    // inspect the target), so these tests confirm the same bindings hold.

    fn workflow_picker_repo_state() -> AppState {
        let mut state = AppState::new();
        state.modal = Modal::WorkflowPicker {
            target: crate::state::WorkflowPickerTarget::Repo {
                repo_id: "r1".into(),
                repo_path: "/tmp/repo".into(),
                repo_name: "my-repo".into(),
            },
            items: vec![crate::state::WorkflowPickerItem::Workflow(
                conductor_core::workflow::WorkflowDef {
                    name: "deploy".into(),
                    title: None,
                    description: String::new(),
                    trigger: conductor_core::workflow::WorkflowTrigger::Manual,
                    targets: vec!["repo".into()],
                    group: None,
                    inputs: vec![],
                    body: vec![],
                    always: vec![],
                    source_path: ".conductor/workflows/deploy.wf".into(),
                },
            )],
            selected: 0,
            scroll_offset: 0,
        };
        state
    }

    #[test]
    fn workflow_picker_repo_esc_dismisses_modal() {
        let state = workflow_picker_repo_state();
        assert!(matches!(
            map_key(key(KeyCode::Esc), &state),
            Action::DismissModal
        ));
    }

    #[test]
    fn workflow_picker_repo_up_down_navigation() {
        let state = workflow_picker_repo_state();
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
    fn workflow_picker_repo_enter_submits() {
        let state = workflow_picker_repo_state();
        assert!(matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::InputSubmit
        ));
    }

    #[test]
    fn workflow_picker_repo_unhandled_key_is_none() {
        let state = workflow_picker_repo_state();
        assert!(matches!(
            map_key(key(KeyCode::Char('x')), &state),
            Action::None
        ));
    }

    // --- WorkflowPicker tests (standalone Worktree target variant) ---

    fn workflow_picker_worktree_state() -> AppState {
        let mut state = AppState::new();
        state.modal = Modal::WorkflowPicker {
            target: crate::state::WorkflowPickerTarget::Worktree {
                worktree_id: "w1".into(),
                worktree_path: "/tmp/ws/w1".into(),
                repo_path: "/tmp/repo".into(),
            },
            items: vec![crate::state::WorkflowPickerItem::Workflow(
                conductor_core::workflow::WorkflowDef {
                    name: "build".into(),
                    title: None,
                    description: String::new(),
                    trigger: conductor_core::workflow::WorkflowTrigger::Manual,
                    targets: vec!["worktree".into()],
                    group: None,
                    inputs: vec![],
                    body: vec![],
                    always: vec![],
                    source_path: ".conductor/workflows/build.wf".into(),
                },
            )],
            selected: 0,
            scroll_offset: 0,
        };
        state
    }

    #[test]
    fn workflow_picker_worktree_esc_dismisses_modal() {
        let state = workflow_picker_worktree_state();
        assert!(matches!(
            map_key(key(KeyCode::Esc), &state),
            Action::DismissModal
        ));
    }

    #[test]
    fn workflow_picker_worktree_up_down_navigation() {
        let state = workflow_picker_worktree_state();
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
    fn workflow_picker_worktree_enter_submits() {
        let state = workflow_picker_worktree_state();
        assert!(matches!(
            map_key(key(KeyCode::Enter), &state),
            Action::InputSubmit
        ));
    }

    #[test]
    fn workflow_picker_worktree_unhandled_key_is_none() {
        let state = workflow_picker_worktree_state();
        assert!(matches!(
            map_key(key(KeyCode::Char('x')), &state),
            Action::None
        ));
    }
}
