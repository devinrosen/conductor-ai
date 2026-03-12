use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::Frame;

use crate::state::AppState;

/// Render the persistent workflow column split into Defs (top 40%) and Runs (bottom 60%).
pub fn render_workflow_column(frame: &mut Frame, area: Rect, state: &AppState) {
    if !state.workflow_column_visible || area.width < 20 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    super::workflows::render_defs(frame, chunks[0], state);
    super::workflows::render_runs(frame, chunks[1], state);
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
