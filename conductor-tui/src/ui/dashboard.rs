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

    let items: Vec<ListItem> =
        rows.iter()
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
                DashboardRow::Feature {
                    repo_idx,
                    feature_idx,
                    total,
                    merged,
                } => {
                    let Some(feature) = state.feature_at(*repo_idx, *feature_idx) else {
                        warn!("dashboard: feature_at({repo_idx}, {feature_idx}) returned None");
                        return ListItem::new(Line::from(""));
                    };
                    let collapsed = state.collapsed_features.contains(&feature.id);

                    let arrow = if collapsed { "▸" } else { "▾" };
                    let progress = format!(" ({merged}/{total} merged)");

                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("  {arrow} "),
                            Style::default().fg(state.theme.label_secondary),
                        ),
                        Span::styled(
                            feature.name.clone(),
                            Style::default()
                                .fg(state.theme.status_running)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(progress, Style::default().fg(state.theme.label_secondary)),
                    ]))
                }
                DashboardRow::Worktree(idx) => {
                    let Some(wt) = state.data.worktrees.get(*idx) else {
                        warn!(
                            "dashboard: worktree index {idx} out of bounds (len={})",
                            state.data.worktrees.len()
                        );
                        return ListItem::new(Line::from(""));
                    };
                    // Derive indentation from data: a worktree whose base_branch
                    // matches a feature branch in its repo is a feature child.
                    let is_feature_child = state
                        .data
                        .features_by_repo
                        .get(&wt.repo_id)
                        .is_some_and(|features| {
                            features
                                .iter()
                                .any(|f| wt.belongs_to_feature(&wt.repo_id, &f.branch))
                        });
                    let prefix = if is_feature_child {
                        "    \u{2514} "
                    } else {
                        "  \u{2514} "
                    };
                    super::common::worktree_list_item_with_prefix(wt, state, None, false, prefix)
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
