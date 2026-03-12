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
