mod common;
mod dashboard;
mod help;
pub(crate) mod helpers;
mod modal;
mod pending_gates;
mod repo_detail;
mod workflow_column;
mod workflow_def_detail;
pub(crate) mod workflows;
mod worktree_detail;

use ratatui::Frame;

use crate::state::{AppState, Modal, View};

/// Root render function: dispatch by current view, then overlay modals.
pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    // Layout: body (fill) + footer (1 line).
    let layout = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Min(0),
            ratatui::layout::Constraint::Length(1),
        ])
        .split(area);

    let body_area = layout[0];
    let footer_area = layout[1];

    match state.view {
        View::Dashboard => dashboard::render(frame, body_area, state),
        View::RepoDetail => repo_detail::render(frame, body_area, state),
        View::WorktreeDetail => worktree_detail::render(frame, body_area, state),
        View::WorkflowRunDetail => workflows::render_run_detail(frame, body_area, state),
        View::WorkflowDefDetail => workflow_def_detail::render(frame, body_area, state),
    }

    common::render_footer(frame, footer_area, state);

    // Modal overlay on top
    match &state.modal {
        Modal::None => {}
        Modal::Help => help::render(frame, area, &state.theme),
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
        Modal::BranchPicker {
            items,
            tree_positions,
            selected,
            ..
        }
        | Modal::BaseBranchPicker {
            items,
            tree_positions,
            selected,
            ..
        } => {
            modal::render_branch_picker(frame, area, items, tree_positions, *selected, &state.theme)
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
            allow_default,
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
            *allow_default,
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
        Modal::ThemePicker {
            themes,
            selected,
            original_name,
            ..
        } => {
            modal::render_theme_picker(frame, area, themes, *selected, original_name, &state.theme)
        }
        Modal::Notifications {
            notifications,
            selected,
        } => modal::render_notifications(frame, area, notifications, *selected, &state.theme),
    }
}
