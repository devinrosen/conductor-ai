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
            let wt_count = state.data.repo_worktree_count.get(&r.id).unwrap_or(&0);
            ListItem::new(Line::from(vec![
                Span::styled(&r.slug, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!("  ({wt_count} worktrees)")),
            ]))
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
        .highlight_symbol("> ");

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

    let items: Vec<ListItem> = state
        .data
        .worktrees
        .iter()
        .map(|wt| {
            let repo_slug = state
                .data
                .repo_slug_map
                .get(&wt.repo_id)
                .map(|s| s.as_str())
                .unwrap_or("?");
            let status_color = match wt.status.as_str() {
                "active" => Color::Green,
                "merged" => Color::Blue,
                "abandoned" => Color::Red,
                _ => Color::White,
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{repo_slug}/"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(&wt.slug, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(
                    format!("[{}]", wt.status),
                    Style::default().fg(status_color),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(" Worktrees "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    if focused && !state.data.worktrees.is_empty() {
        list_state.select(Some(state.worktree_index));
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

    let items: Vec<ListItem> = state
        .data
        .tickets
        .iter()
        .filter(|t| {
            if state.filter_active && !state.filter_text.is_empty() {
                let lower = state.filter_text.to_lowercase();
                t.title.to_lowercase().contains(&lower)
                    || t.source_id.contains(&lower)
                    || t.labels.to_lowercase().contains(&lower)
            } else {
                true
            }
        })
        .map(|t| {
            let repo_slug = state
                .data
                .repo_slug_map
                .get(&t.repo_id)
                .map(|s| s.as_str())
                .unwrap_or("?");
            let state_color = match t.state.as_str() {
                "open" => Color::Green,
                "closed" => Color::Red,
                "in_progress" => Color::Yellow,
                _ => Color::White,
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{repo_slug} "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("#{} ", t.source_id),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(&t.title),
                Span::raw("  "),
                Span::styled(format!("[{}]", t.state), Style::default().fg(state_color)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(" Tickets "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    if focused && !state.data.tickets.is_empty() {
        list_state.select(Some(state.ticket_index));
    }

    frame.render_stateful_widget(list, area, &mut list_state);
}
