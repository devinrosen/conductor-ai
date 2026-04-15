use conductor_core::feature::FeatureStatus;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::common::{format_elapsed, truncate};
use crate::state::AppState;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    // Find the selected feature from detail_features using selected_feature_id.
    let feature = state
        .selected_feature_id
        .as_ref()
        .and_then(|id| state.detail_features.iter().find(|f| &f.id == id));

    let Some(feature) = feature else {
        let msg = Paragraph::new("No feature selected. Press Esc to go back.").block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Feature Detail "),
        );
        frame.render_widget(msg, area);
        return;
    };

    // Layout: metadata header | tickets | worktrees
    let wt_count = state
        .detail_worktrees
        .iter()
        .filter(|wt| wt.base_branch.as_deref() == Some(feature.branch.as_str()))
        .count();
    let wt_height = (wt_count as u16 + 2).max(3).min(area.height / 4);
    let ticket_height = (state.detail_feature_tickets.len() as u16 + 2)
        .max(3)
        .min(area.height / 3);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8), // metadata
            Constraint::Length(ticket_height),
            Constraint::Length(wt_height),
            Constraint::Min(0), // padding
        ])
        .split(area);

    render_metadata(frame, chunks[0], feature, state);
    render_tickets(frame, chunks[1], state);
    render_worktrees(frame, chunks[2], &feature.branch, state);
}

fn render_metadata(
    frame: &mut Frame,
    area: Rect,
    feature: &conductor_core::feature::FeatureRow,
    state: &AppState,
) {
    let status_color = match feature.status {
        FeatureStatus::InProgress => state.theme.label_warning,
        FeatureStatus::ReadyForReview => state.theme.label_info,
        FeatureStatus::Approved => state.theme.status_completed,
        FeatureStatus::Merged | FeatureStatus::Closed => state.theme.label_secondary,
    };

    let progress = if feature.tickets_total > 0 {
        format!("{}/{}", feature.tickets_merged, feature.tickets_total)
    } else {
        "—".to_string()
    };

    let stale = format_elapsed(&feature.created_at);
    let stale_str = if stale.is_empty() {
        "?".to_string()
    } else {
        stale
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(
                "  Name:    ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(
                feature.name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Branch:  ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(
                feature.branch.clone(),
                Style::default().fg(state.theme.label_info),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Base:    ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(
                feature.base_branch.clone(),
                Style::default().fg(state.theme.label_secondary),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Status:  ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(
                feature.status.to_string(),
                Style::default().fg(status_color),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Progress:",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(
                format!(" {} tickets merged", progress),
                Style::default().fg(state.theme.label_accent),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Created: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(
                format!("{} ago ({})", stale_str, &feature.created_at),
                Style::default().fg(state.theme.label_secondary),
            ),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Feature: {} ", feature.name))
        .border_style(Style::default().fg(state.theme.border_focused));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_tickets(frame: &mut Frame, area: Rect, state: &AppState) {
    let tickets = &state.detail_feature_tickets;

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Linked Tickets ({}) ", tickets.len()))
        .border_style(Style::default().fg(state.theme.border_inactive));

    if tickets.is_empty() {
        let empty = Paragraph::new("  No tickets linked to this feature.")
            .style(Style::default().fg(state.theme.label_secondary))
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = tickets
        .iter()
        .enumerate()
        .map(|(idx, t)| {
            let selected = idx == state.detail_feature_ticket_index;
            let title = truncate(&t.title, 60);
            let state_color = match t.state.as_str() {
                "open" => state.theme.status_completed,
                "closed" => state.theme.label_secondary,
                _ => state.theme.label_warning,
            };
            let style = if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let spans = vec![
                Span::styled(
                    format!("  #{:<6}", t.source_id),
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::styled(format!("{:<8}", t.state), Style::default().fg(state_color)),
                Span::styled(title, style),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut list_state = ListState::default();
    if !tickets.is_empty() {
        list_state.select(Some(state.detail_feature_ticket_index));
    }

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(ratatui::style::Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_worktrees(frame: &mut Frame, area: Rect, feature_branch: &str, state: &AppState) {
    let worktrees: Vec<_> = state
        .detail_worktrees
        .iter()
        .filter(|wt| wt.base_branch.as_deref() == Some(feature_branch))
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Active Worktrees ({}) ", worktrees.len()))
        .border_style(Style::default().fg(state.theme.border_inactive));

    if worktrees.is_empty() {
        let empty = Paragraph::new("  No active worktrees for this feature branch.")
            .style(Style::default().fg(state.theme.label_secondary))
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = worktrees
        .iter()
        .map(|wt| {
            let slug = &wt.slug;
            let stale = format_elapsed(&wt.created_at);
            let spans = vec![
                Span::styled(
                    format!("  {:<32}", slug),
                    Style::default().fg(state.theme.label_primary),
                ),
                Span::styled(
                    format!("  {:>6}", stale),
                    Style::default().fg(state.theme.label_secondary),
                ),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}
