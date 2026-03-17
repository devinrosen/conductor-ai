use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::Frame;

use crate::state::{AppState, WorkflowsFocus};

/// Render the persistent workflow column split into Defs, optional Gates, and Runs.
pub fn render_workflow_column(frame: &mut Frame, area: Rect, state: &AppState) {
    if !state.workflow_column_visible || area.width < 20 {
        return;
    }

    let render_lower = |frame: &mut Frame, area: Rect, state: &AppState| {
        if state.workflows_focus == WorkflowsFocus::Defs
            && state.workflow_def_focus == crate::state::WorkflowDefFocus::Steps
        {
            super::workflows::render_def_steps(frame, area, state);
        } else {
            super::workflows::render_runs(frame, area, state);
        }
    };

    if !state.detail_gates.is_empty() {
        let gate_height = (state.detail_gates.len() as u16 + 2)
            .max(3)
            .min(area.height / 4);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(40),
                Constraint::Length(gate_height),
                Constraint::Min(0),
            ])
            .split(area);

        super::workflows::render_defs(frame, chunks[0], state);
        let gates_focused = state.workflows_focus == WorkflowsFocus::Gates;
        super::pending_gates::render_pending_gates(frame, chunks[1], state, gates_focused);
        render_lower(frame, chunks[2], state);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);

        super::workflows::render_defs(frame, chunks[0], state);
        render_lower(frame, chunks[1], state);
    }
}

/// Render a view split into a content pane (65%) and the persistent workflow column (35%).
/// Falls back to full-width content when the workflow column is hidden or the terminal is too narrow.
pub fn render_with_workflow_column(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    render_content: impl Fn(&mut Frame, Rect, &AppState),
) {
    if state.workflow_column_visible && area.width >= 80 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(area);
        render_content(frame, cols[0], state);
        render_workflow_column(frame, cols[1], state);
    } else {
        render_content(frame, area, state);
    }
}
