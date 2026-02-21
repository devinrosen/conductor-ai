use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::state::{AppState, RepoDetailFocus};

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let repo = state
        .selected_repo_id
        .as_ref()
        .and_then(|id| state.data.repos.iter().find(|r| &r.id == id));

    let Some(repo) = repo else {
        let msg = Paragraph::new("No repo selected").block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Repo Detail "),
        );
        frame.render_widget(msg, area);
        return;
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(area);

    // Repo info header
    let info = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Repo: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&repo.slug, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("Remote: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&repo.remote_url),
        ]),
        Line::from(vec![
            Span::styled("Branch: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&repo.default_branch),
            Span::styled("  Path: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&repo.local_path),
        ]),
        Line::from(vec![
            Span::styled("Worktrees Dir: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&repo.workspace_dir),
        ]),
    ])
    .block(Block::default().borders(Borders::ALL).title(" Info "));

    frame.render_widget(info, layout[0]);

    // Scoped worktrees
    let wt_focused = state.repo_detail_focus == RepoDetailFocus::Worktrees;
    let wt_border = if wt_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let wt_items: Vec<ListItem> = state
        .detail_worktrees
        .iter()
        .map(|wt| {
            let status_color = match wt.status.as_str() {
                "active" => Color::Green,
                "merged" => Color::Blue,
                _ => Color::Red,
            };
            ListItem::new(Line::from(vec![
                Span::styled(&wt.slug, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!("  {}", wt.branch)),
                Span::raw("  "),
                Span::styled(
                    format!("[{}]", wt.status),
                    Style::default().fg(status_color),
                ),
            ]))
        })
        .collect();

    let wt_list = List::new(wt_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(wt_border)
                .title(" Worktrees "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut wt_state = ListState::default();
    if wt_focused && !state.detail_worktrees.is_empty() {
        wt_state.select(Some(state.detail_wt_index));
    }
    frame.render_stateful_widget(wt_list, layout[1], &mut wt_state);

    // Scoped tickets
    let ticket_focused = state.repo_detail_focus == RepoDetailFocus::Tickets;
    let ticket_border = if ticket_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let ticket_items: Vec<ListItem> = state
        .detail_tickets
        .iter()
        .map(|t| {
            let state_color = match t.state.as_str() {
                "open" => Color::Green,
                "closed" => Color::Red,
                "in_progress" => Color::Yellow,
                _ => Color::White,
            };
            ListItem::new(Line::from(vec![
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

    let ticket_list = List::new(ticket_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(ticket_border)
                .title(" Tickets "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut ticket_state = ListState::default();
    if ticket_focused && !state.detail_tickets.is_empty() {
        ticket_state.select(Some(state.detail_ticket_index));
    }
    frame.render_stateful_widget(ticket_list, layout[2], &mut ticket_state);
}
