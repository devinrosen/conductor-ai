use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::state::{AppState, DashboardFocus};

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    // Split: top row (repos left 40%, worktrees right 60%), bottom (tickets 100%)
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(vert[0]);

    render_repos(frame, top[0], state);
    render_worktrees(frame, top[1], state);
    render_tickets(frame, vert[1], state);
}

/// Given a flat list sorted by repo (get_repo returns an owned String key),
/// return the visual row index (including interleaved header rows) for
/// the item at `logical_idx`.
fn visual_idx_with_headers<T>(
    items: &[T],
    get_repo: impl Fn(&T) -> String,
    logical_idx: usize,
) -> usize {
    let mut headers = 0usize;
    let mut prev = String::new();
    for item in items.iter().take(logical_idx + 1) {
        let repo = get_repo(item);
        if repo != prev {
            headers += 1;
            prev = repo;
        }
    }
    logical_idx + headers
}

fn render_repos(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused = state.dashboard_focus == DashboardFocus::Repos;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
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
                Span::styled("● ", Style::default().fg(Color::Green))
            } else {
                Span::styled("○ ", Style::default().fg(Color::DarkGray))
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
                .bg(Color::DarkGray)
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
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
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
                    .fg(Color::DarkGray)
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
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut list_state = ListState::default();
    if focused && !state.data.worktrees.is_empty() {
        let logical_idx = state
            .worktree_index
            .min(wts_with_slug.len().saturating_sub(1));
        let visual_idx = visual_idx_with_headers(
            &wts_with_slug,
            |(repo_slug, _)| repo_slug.clone(),
            logical_idx,
        );
        list_state.select(Some(visual_idx));
    }

    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_tickets(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused = state.dashboard_focus == DashboardFocus::Tickets;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut items: Vec<ListItem> = Vec::new();
    let mut prev_repo_id = String::new();
    for t in &state.filtered_tickets {
        let repo_slug = state
            .data
            .repo_slug_map
            .get(&t.repo_id)
            .map(|s| s.as_str())
            .unwrap_or("?");
        if t.repo_id != prev_repo_id {
            items.push(ListItem::new(Line::from(vec![Span::styled(
                repo_slug.to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )])));
            prev_repo_id = t.repo_id.clone();
        }
        let mut spans = vec![
            Span::raw("\u{2514} "),
            super::common::ticket_worktree_dot_span(state, &t.id),
            Span::styled(
                format!("#{} ", t.source_id),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(t.title.clone()),
            Span::raw("  "),
        ];
        let labels = state
            .data
            .ticket_labels
            .get(&t.id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        spans.extend(super::common::ticket_label_spans_compact(labels));
        spans.extend(super::common::ticket_agent_total_spans(
            state, &t.id, "  ", false,
        ));
        items.push(ListItem::new(Line::from(spans)));
    }

    let ticket_title = if state.show_closed_tickets {
        " Tickets "
    } else {
        " Tickets (hiding closed) "
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(ticket_title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut list_state = ListState::default();
    if focused && !state.filtered_tickets.is_empty() {
        let logical_idx = state
            .ticket_index
            .min(state.filtered_tickets.len().saturating_sub(1));
        let visual_idx = visual_idx_with_headers(
            &state.filtered_tickets,
            |t| {
                state
                    .data
                    .repo_slug_map
                    .get(&t.repo_id)
                    .cloned()
                    .unwrap_or_else(|| "?".to_string())
            },
            logical_idx,
        );
        list_state.select(Some(visual_idx));
    }

    frame.render_stateful_widget(list, area, &mut list_state);
}
