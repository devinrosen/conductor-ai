use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::state::{AppState, DashboardFocus};

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let workflow_visible = state.workflow_column_visible && area.width >= 80;

    if workflow_visible {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(area);
        render_content(frame, cols[0], state);
        super::workflow_column::render_workflow_column(frame, cols[1], state);
    } else {
        render_content(frame, area, state);
    }
}

fn render_content(frame: &mut Frame, area: Rect, state: &AppState) {
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    render_repos(frame, top[0], state);
    render_worktrees(frame, top[1], state);
}

fn render_repos(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused = state.dashboard_focus == DashboardFocus::Repos;
    let border_style = if focused {
        Style::default().fg(state.theme.border_focused)
    } else {
        Style::default().fg(state.theme.border_inactive)
    };

    let items: Vec<ListItem> = state
        .data
        .repos
        .iter()
        .map(|r| {
            let active = state
                .data
                .worktrees
                .iter()
                .filter(|wt| wt.repo_id == r.id && wt.is_active())
                .count();
            let dot = if active > 0 {
                Span::styled("● ", Style::default().fg(state.theme.status_completed))
            } else {
                Span::styled("○ ", Style::default().fg(state.theme.label_secondary))
            };
            ListItem::new(Line::from(vec![dot, Span::raw(r.slug.clone())]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(" Repos "),
        )
        .highlight_style(
            Style::default()
                .bg(state.theme.highlight_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut list_state = ListState::default();
    if focused && !state.data.repos.is_empty() {
        list_state.select(Some(state.repo_index));
    }

    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_worktrees(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused = state.dashboard_focus == DashboardFocus::Worktrees;
    let border_style = if focused {
        Style::default().fg(state.theme.border_focused)
    } else {
        Style::default().fg(state.theme.border_inactive)
    };

    // data.worktrees is pre-sorted by (repo_slug, wt_slug) in DataCache::rebuild_maps(),
    // so state.worktree_index directly indexes this list (matching selection actions).
    let wts_with_slug: Vec<(String, &conductor_core::worktree::Worktree)> = state
        .data
        .worktrees
        .iter()
        .map(|wt| {
            let slug = state
                .data
                .repo_slug_map
                .get(&wt.repo_id)
                .cloned()
                .unwrap_or_else(|| "?".to_string());
            (slug, wt)
        })
        .collect();

    let mut items: Vec<ListItem> = Vec::new();
    let mut prev_repo = String::new();
    for (repo_slug, wt) in &wts_with_slug {
        if *repo_slug != prev_repo {
            items.push(ListItem::new(Line::from(vec![Span::styled(
                repo_slug.clone(),
                Style::default()
                    .fg(state.theme.label_secondary)
                    .add_modifier(Modifier::BOLD),
            )])));
            prev_repo = repo_slug.clone();
        }
        items.push(super::common::worktree_list_item_with_prefix(
            wt,
            state,
            None,
            false,
            "\u{2514} ",
        ));
    }

    let active_count = state
        .data
        .worktrees
        .iter()
        .filter(|w| w.is_active())
        .count();
    let title = format!(" Worktrees ({active_count} active) ");

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
    if focused && !state.data.worktrees.is_empty() {
        let logical_idx = state
            .worktree_index
            .min(wts_with_slug.len().saturating_sub(1));
        let visual_idx = super::helpers::visual_idx_with_headers(
            &wts_with_slug,
            |(repo_slug, _)| repo_slug.clone(),
            logical_idx,
        );
        list_state.select(Some(visual_idx));
    }

    frame.render_stateful_widget(list, area, &mut list_state);
}
