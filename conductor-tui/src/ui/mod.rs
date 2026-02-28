mod common;
mod dashboard;
mod help;
mod modal;
mod repo_detail;
mod tickets;
mod worktree_detail;

use ratatui::Frame;

use crate::state::{AppState, Modal, View};

/// Root render function: dispatch by current view, then overlay modals.
pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    // Layout: header (1 line) + body (fill) + status bar (1 line)
    let layout = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(1),
            ratatui::layout::Constraint::Min(0),
            ratatui::layout::Constraint::Length(1),
        ])
        .split(area);

    let header_area = layout[0];
    let body_area = layout[1];
    let status_area = layout[2];

    common::render_header(frame, header_area, state);

    match state.view {
        View::Dashboard => dashboard::render(frame, body_area, state),
        View::RepoDetail => repo_detail::render(frame, body_area, state),
        View::WorktreeDetail => worktree_detail::render(frame, body_area, state),
        View::Tickets => tickets::render(frame, body_area, state),
    }

    common::render_status_bar(frame, status_area, state);

    // Modal overlay on top
    match &state.modal {
        Modal::None => {}
        Modal::Help => help::render(frame, area),
        Modal::Confirm { title, message, .. } => modal::render_confirm(frame, area, title, message),
        Modal::Input {
            title,
            prompt,
            value,
            ..
        } => modal::render_input(frame, area, title, prompt, value),
        Modal::AgentPrompt {
            title,
            prompt,
            textarea,
            ..
        } => modal::render_agent_prompt(frame, area, title, prompt, textarea),
        Modal::Form {
            title,
            fields,
            active_field,
            ..
        } => modal::render_form(frame, area, title, fields, *active_field),
        Modal::Error { message } => modal::render_error(frame, area, message),
        Modal::TicketInfo { ticket } => {
            let agent_totals = state.data.ticket_agent_totals.get(&ticket.id);
            let worktrees = state.data.ticket_worktrees.get(&ticket.id);
            modal::render_ticket_info(frame, area, ticket, agent_totals, worktrees);
        }
        Modal::WorkTargetPicker { targets, selected } => {
            modal::render_work_target_picker(frame, area, targets, *selected)
        }
        Modal::WorkTargetManager { targets, selected } => {
            modal::render_work_target_manager(frame, area, targets, *selected)
        }
    }
}
