use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::state::AppState;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let Some(ref session) = state.data.current_session else {
        let msg = Paragraph::new("No active session. Press S to start one.")
            .block(Block::default().borders(Borders::ALL).title(" Session "));
        frame.render_widget(msg, area);
        return;
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(0)])
        .split(area);

    // Session info
    let elapsed = session_elapsed(&session.started_at);
    let notes = session.notes.as_deref().unwrap_or("(none)");

    let info = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Session: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &session.id[..13.min(session.id.len())],
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Started: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&session.started_at),
        ]),
        Line::from(vec![
            Span::styled("Elapsed: ", Style::default().fg(Color::DarkGray)),
            Span::styled(elapsed, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("Notes: ", Style::default().fg(Color::DarkGray)),
            Span::raw(notes),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Session Info "),
    );

    frame.render_widget(info, layout[0]);

    // Attached worktrees
    let items: Vec<ListItem> = state
        .data
        .session_worktrees
        .iter()
        .map(|wt| {
            let repo_slug = state
                .data
                .repo_slug_map
                .get(&wt.repo_id)
                .map(|s| s.as_str())
                .unwrap_or("?");
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{repo_slug}/"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(&wt.slug, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!("  {}", wt.branch)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Attached Worktrees "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    if !state.data.session_worktrees.is_empty() {
        list_state.select(Some(0));
    }

    frame.render_stateful_widget(list, layout[1], &mut list_state);
}

fn session_elapsed(started_at: &str) -> String {
    let Ok(start) = chrono::DateTime::parse_from_rfc3339(started_at) else {
        return "??:??".to_string();
    };
    let elapsed = chrono::Utc::now().signed_duration_since(start);
    let hours = elapsed.num_hours();
    let minutes = elapsed.num_minutes() % 60;
    let seconds = elapsed.num_seconds() % 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m{seconds:02}s")
    } else {
        format!("{minutes}m{seconds:02}s")
    }
}
