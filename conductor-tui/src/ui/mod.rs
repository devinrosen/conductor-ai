mod common;
mod dashboard;
mod help;
pub(crate) mod helpers;
mod modal;
mod repo_detail;
mod tickets;
mod workflows;
mod worktree_detail;

use ratatui::Frame;

use crate::state::{AppState, Modal, View};

/// Root render function: dispatch by current view, then overlay modals.
pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    // Compute global status once per frame — used for both header height and rendering.
    let gs = state.global_status();

    // Layout: header (1–2 lines) + body (fill) + status bar (1 line).
    // Header height is dynamic: 1 line when nothing is active or 4+ items are
    // collapsed, 2 lines when 1–3 active items or the user has expanded the bar.
    let header_h = state.header_height(&gs);

    let layout = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(header_h),
            ratatui::layout::Constraint::Min(0),
            ratatui::layout::Constraint::Length(1),
        ])
        .split(area);

    let header_area = layout[0];
    let body_area = layout[1];
    let status_area = layout[2];

    common::render_header(frame, header_area, state, &gs);

    match state.view {
        View::Dashboard => dashboard::render(frame, body_area, state),
        View::RepoDetail => repo_detail::render(frame, body_area, state),
        View::WorktreeDetail => worktree_detail::render(frame, body_area, state),
        View::Tickets => tickets::render(frame, body_area, state),
        View::Workflows => workflows::render(frame, body_area, state),
        View::WorkflowRunDetail => workflows::render_run_detail(frame, body_area, state),
    }

    common::render_status_bar(frame, status_area, state);

    // Modal overlay on top
    match &state.modal {
        Modal::None => {}
        Modal::Help => help::render(frame, area),
        Modal::Confirm { title, message, .. } => {
            modal::render_confirm(frame, area, title, message, &state.theme)
        }
        Modal::ConfirmByName {
            title,
            message,
            expected,
            value,
            ..
        } => modal::render_confirm_by_name(
            frame,
            area,
            title,
            message,
            expected,
            value,
            &state.theme,
        ),
        Modal::Input {
            title,
            prompt,
            value,
            ..
        } => modal::render_input(frame, area, title, prompt, value, &state.theme),
        Modal::AgentPrompt {
            title,
            prompt,
            textarea,
            ..
        } => modal::render_agent_prompt(frame, area, title, prompt, textarea, &state.theme),
        Modal::Form {
            title,
            fields,
            active_field,
            ..
        } => modal::render_form(frame, area, title, fields, *active_field, &state.theme),
        Modal::Error { message } => modal::render_error(frame, area, message, &state.theme),
        Modal::TicketInfo { ticket } => {
            let agent_totals = state.data.ticket_agent_totals.get(&ticket.id);
            let worktrees = state.data.ticket_worktrees.get(&ticket.id);
            let labels = state
                .data
                .ticket_labels
                .get(&ticket.id)
                .map(|v| v.as_slice());
            modal::render_ticket_info(
                frame,
                area,
                ticket,
                agent_totals,
                worktrees,
                labels,
                &state.theme,
            );
        }
        Modal::PostCreatePicker {
            items,
            selected,
            ticket_id,
            ..
        } => {
            let source_id = state
                .data
                .ticket_map
                .get(ticket_id)
                .map(|t| t.source_id.as_str())
                .unwrap_or("?");
            modal::render_post_create_picker(frame, area, items, *selected, source_id, &state.theme)
        }
        Modal::IssueSourceManager {
            repo_slug,
            sources,
            selected,
            ..
        } => modal::render_issue_source_manager(
            frame,
            area,
            repo_slug,
            sources,
            *selected,
            &state.theme,
        ),
        Modal::ModelPicker {
            context_label,
            effective_default,
            effective_source,
            selected,
            custom_input,
            custom_active,
            suggested,
            ..
        } => modal::render_model_picker(
            frame,
            area,
            context_label,
            effective_default.as_deref(),
            effective_source,
            *selected,
            custom_input,
            *custom_active,
            suggested.as_deref(),
            &state.theme,
        ),
        Modal::GateAction {
            gate_prompt,
            feedback,
            ..
        } => modal::render_gate_action(frame, area, gate_prompt, feedback, &state.theme),
        Modal::EventDetail {
            title,
            body,
            line_count,
            scroll_offset,
            horizontal_offset,
        } => modal::render_event_detail(
            frame,
            area,
            title,
            body,
            *line_count,
            *scroll_offset,
            *horizontal_offset,
            &state.theme,
        ),
        Modal::GithubDiscoverOrgs {
            orgs,
            cursor,
            loading,
            error,
        } => modal::render_github_discover_orgs(
            frame,
            area,
            orgs,
            *cursor,
            *loading,
            error.as_deref(),
            &state.theme,
        ),
        Modal::GithubDiscover {
            repos,
            registered_urls,
            selected,
            cursor,
            loading,
            error,
            ..
        } => modal::render_github_discover(
            frame,
            area,
            repos,
            registered_urls,
            selected,
            *cursor,
            *loading,
            error.as_deref(),
            &state.theme,
        ),
        Modal::PrWorkflowPicker {
            pr_number,
            pr_title,
            workflow_defs,
            selected,
        } => modal::render_pr_workflow_picker(
            frame,
            area,
            *pr_number,
            pr_title,
            workflow_defs,
            *selected,
            &state.theme,
        ),
        Modal::WorkflowPicker {
            target,
            workflow_defs,
            selected,
        } => modal::render_workflow_picker(
            frame,
            area,
            target,
            workflow_defs,
            *selected,
            &state.theme,
        ),
        Modal::Progress { message } => modal::render_progress(frame, area, message, &state.theme),
    }
}
