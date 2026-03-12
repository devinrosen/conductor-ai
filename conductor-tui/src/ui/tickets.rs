use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use super::common::truncate;
use crate::state::AppState;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let filter = state.filter.as_query();
    let label_filter = state.label_filter.as_query();

    let items: Vec<ListItem> = state
        .filtered_tickets
        .iter()
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
                Span::styled(format!("{:<30} ", truncate(&t.title, 30)), Style::default()),
                Span::styled(
                    format!("{:<12} ", t.state),
                    Style::default().fg(state_color),
                ),
                Span::styled(
                    format!("{:<12}", assignee),
                    Style::default().fg(Color::DarkGray),
                ),
            ];

            // Render label chips (up to 3, then +N)
            let labels = state.data.ticket_labels.get(&t.id);
            if let Some(labels) = labels {
                let mut shown = 0usize;
                for lbl in labels.iter().take(3) {
                    let name = truncate(&lbl.label, 12);
                    let bg = lbl
                        .color
                        .as_deref()
                        .map(super::common::hex_to_color)
                        .unwrap_or(Color::DarkGray);
                    let fg = super::common::label_fg(bg);
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        format!(" {name} "),
                        Style::default().fg(fg).bg(bg),
                    ));
                    shown += 1;
                }
                let remaining = labels.len().saturating_sub(shown);
                if remaining > 0 {
                    spans.push(Span::styled(
                        format!(" +{remaining}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }

            spans.extend(super::common::ticket_worktree_spans(
                state, &t.id, " ", false,
            ));
            spans.extend(super::common::ticket_agent_total_spans(
                state, &t.id, " ", true,
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let hiding = !state.show_closed_tickets;

    // Build block title showing active filters
    let title = {
        let text_filter_active = filter.as_deref().is_some_and(|f| !f.is_empty());
        let label_filter_active = label_filter.as_deref().is_some_and(|f| !f.is_empty());

        let mut parts: Vec<String> = Vec::new();
        if hiding {
            parts.push("hiding closed".to_string());
        }
        if text_filter_active {
            parts.push(format!("filter: {}", filter.as_deref().unwrap_or("")));
        }
        if label_filter_active {
            parts.push(format!("label: {}", label_filter.as_deref().unwrap_or("")));
        }

        if parts.is_empty() {
            " Tickets ".to_string()
        } else if hiding && !text_filter_active && !label_filter_active {
            " Tickets (hiding closed) [A to show all] ".to_string()
        } else {
            format!(" Tickets ({}) ", parts.join(", "))
        }
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
    if !state.filtered_tickets.is_empty() {
        list_state.select(Some(state.ticket_index));
    }

    frame.render_stateful_widget(list, area, &mut list_state);
}
