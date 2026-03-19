use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::Frame;

use crate::state::{AppState, WorkflowsFocus};

/// Render the persistent workflow column split into Gates (top), Runs (middle), and Defs (bottom).
pub fn render_workflow_column(frame: &mut Frame, area: Rect, state: &AppState) {
    if !state.workflow_column_visible || area.width < 20 {
        return;
    }

    let render_middle = |frame: &mut Frame, area: Rect, state: &AppState| {
        if state.workflows_focus == WorkflowsFocus::Defs
            && state.workflow_def_focus == crate::state::WorkflowDefFocus::Steps
        {
            super::workflows::render_def_steps(frame, area, state);
        } else {
            super::workflows::render_runs(frame, area, state);
        }
    };

    // Compute defs pane height: hug content.
    let defs_height = if state.workflow_defs_collapsed {
        // Collapsed: just a single-line header (no border).
        1
    } else {
        // Expanded: items + 2 for top/bottom border.
        let item_count = super::workflows::render_defs_row_count(state);
        let raw = (item_count as u16).saturating_add(2).max(3);
        // Cap at 1/3 of the area to avoid overwhelming runs.
        raw.min(area.height / 3)
    };

    // Build constraints: [optional gates, flex runs, hugging defs]
    let has_gates = !state.detail_gates.is_empty();
    let gate_height = if has_gates {
        (state.detail_gates.len() as u16 + 2)
            .max(3)
            .min(area.height / 4)
    } else {
        0
    };

    let constraints: Vec<Constraint> = if has_gates {
        vec![
            Constraint::Length(gate_height),
            Constraint::Min(0),
            Constraint::Length(defs_height),
        ]
    } else {
        vec![Constraint::Min(0), Constraint::Length(defs_height)]
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    if has_gates {
        let gates_focused = state.workflows_focus == WorkflowsFocus::Gates;
        super::pending_gates::render_pending_gates(frame, chunks[0], state, gates_focused);
        render_middle(frame, chunks[1], state);
        super::workflows::render_defs(frame, chunks[2], state);
    } else {
        render_middle(frame, chunks[0], state);
        super::workflows::render_defs(frame, chunks[1], state);
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
