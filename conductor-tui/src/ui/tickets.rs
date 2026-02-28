use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::state::AppState;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let filter = if state.filter_active || !state.filter_text.is_empty() {
        Some(state.filter_text.to_lowercase())
    } else {
        None
    };

    let items: Vec<ListItem> = state
        .data
        .tickets
        .iter()
        .filter(|t| {
            if let Some(ref f) = filter {
                if f.is_empty() {
                    return true;
                }
                t.title.to_lowercase().contains(f)
                    || t.source_id.contains(f.as_str())
                    || t.labels.to_lowercase().contains(f)
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

            let assignee = t.assignee.as_deref().unwrap_or("-");

            let mut spans = vec![
                Span::styled(
                    format!("{repo_slug:<12} "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("#{:<6} ", t.source_id),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(format!("{:<40} ", truncate(&t.title, 40)), Style::default()),
                Span::styled(
                    format!("{:<12} ", t.state),
                    Style::default().fg(state_color),
                ),
                Span::styled(
                    format!("{:<12}", assignee),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            spans.extend(super::common::ticket_worktree_spans(state, &t.id, " "));
            spans.extend(super::common::ticket_agent_total_spans(
                state, &t.id, " ", true,
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let title = if let Some(ref f) = filter {
        if f.is_empty() {
            " Tickets ".to_string()
        } else {
            format!(" Tickets (filter: {f}) ")
        }
    } else {
        " Tickets ".to_string()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    if !state.data.tickets.is_empty() {
        list_state.select(Some(state.ticket_index));
    }

    frame.render_stateful_widget(list, area, &mut list_state);
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}
