use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use tracing::warn;

use crate::state::{AppState, ColumnFocus, DashboardRow};

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    super::workflow_column::render_with_workflow_column(frame, area, state, render_content);
}

fn render_content(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused = state.column_focus == ColumnFocus::Content;
    let border_style = if focused {
        Style::default().fg(state.theme.border_focused)
    } else {
        Style::default().fg(state.theme.border_inactive)
    };

    let rows = state.dashboard_rows();

    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            DashboardRow::Repo(idx) => {
                let Some(repo) = state.data.repos.get(*idx) else {
                    warn!(
                        "dashboard: repo index {idx} out of bounds (len={})",
                        state.data.repos.len()
                    );
                    return ListItem::new(Line::from(""));
                };
                let active = state
                    .data
                    .worktrees
                    .iter()
                    .filter(|wt| wt.repo_id == repo.id && wt.is_active())
                    .count();
                let dot = if active > 0 {
                    Span::styled("● ", Style::default().fg(state.theme.status_completed))
                } else {
                    Span::styled("○ ", Style::default().fg(state.theme.label_secondary))
                };
                ListItem::new(Line::from(vec![
                    dot,
                    Span::styled(
                        repo.slug.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]))
            }
            DashboardRow::Worktree { idx, prefix } => {
                let Some(wt) = state.data.worktrees.get(*idx) else {
                    warn!(
                        "dashboard: worktree index {idx} out of bounds (len={})",
                        state.data.worktrees.len()
                    );
                    return ListItem::new(Line::from(""));
                };
                let display_prefix = prefix.clone();
                super::common::worktree_list_item_with_prefix(
                    wt,
                    state,
                    None,
                    false,
                    &display_prefix,
                )
            }
        })
        .collect();

    let active_count = state
        .data
        .worktrees
        .iter()
        .filter(|w| w.is_active())
        .count();
    let title = format!(" Repos & Worktrees ({active_count} active) ");

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(state.theme.highlight_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut list_state = ListState::default();
    if focused && !rows.is_empty() {
        list_state.select(Some(
            state.dashboard_index.min(rows.len().saturating_sub(1)),
        ));
    }

    frame.render_stateful_widget(list, area, &mut list_state);
}
