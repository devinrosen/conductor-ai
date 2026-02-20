use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::state::AppState;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let wt = state
        .selected_worktree_id
        .as_ref()
        .and_then(|id| state.data.worktrees.iter().find(|w| &w.id == id));

    let Some(wt) = wt else {
        let msg = Paragraph::new("No worktree selected").block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Worktree Detail "),
        );
        frame.render_widget(msg, area);
        return;
    };

    let repo_slug = state
        .data
        .repo_slug_map
        .get(&wt.repo_id)
        .map(|s| s.as_str())
        .unwrap_or("?");

    let ticket_info = wt
        .ticket_id
        .as_ref()
        .and_then(|tid| state.data.ticket_map.get(tid))
        .map(|t| format!("#{} — {}", t.source_id, t.title))
        .unwrap_or_else(|| "None (press l to link)".to_string());

    let status_color = match wt.status.as_str() {
        "active" => Color::Green,
        "merged" => Color::Blue,
        _ => Color::Red,
    };

    let content = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Worktree: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&wt.slug, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("Repo: ", Style::default().fg(Color::DarkGray)),
            Span::raw(repo_slug),
        ]),
        Line::from(vec![
            Span::styled("Branch: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&wt.branch),
        ]),
        Line::from(vec![
            Span::styled("Path: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&wt.path),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&wt.status, Style::default().fg(status_color)),
        ]),
        Line::from(vec![
            Span::styled("Created: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&wt.created_at),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Ticket: ", Style::default().fg(Color::DarkGray)),
            Span::raw(ticket_info),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Actions: w=work  p=push  P=PR  l=link ticket  d=delete  Esc=back",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Worktree Detail "),
    );

    frame.render_widget(content, area);
}
