use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::state::{AppState, SessionFocus};

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let has_session = state.data.current_session.is_some();
    let has_history = !state.data.session_history.is_empty();

    if !has_session && !has_history {
        let msg = Paragraph::new("No active session. Press S to start one.")
            .block(Block::default().borders(Borders::ALL).title(" Session "));
        frame.render_widget(msg, area);
        return;
    }

    // Layout: session info (if active) + worktrees + history
    let constraints = if has_session && has_history {
        vec![
            Constraint::Length(6),
            Constraint::Percentage(40),
            Constraint::Min(4),
        ]
    } else if has_session {
        vec![Constraint::Length(6), Constraint::Min(0)]
    } else {
        // Only history, no active session
        vec![Constraint::Length(3), Constraint::Min(0)]
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    if has_session {
        let session = state.data.current_session.as_ref().unwrap();

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
        let wt_focused = state.session_focus == SessionFocus::Worktrees;
        let wt_border = if wt_focused {
            Color::Cyan
        } else {
            Color::DarkGray
        };

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

        let wt_title = if state.data.session_worktrees.is_empty() {
            " Attached Worktrees (a to attach) "
        } else {
            " Attached Worktrees "
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(wt_border))
                    .title(wt_title),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");

        let mut list_state = ListState::default();
        if !state.data.session_worktrees.is_empty() && wt_focused {
            list_state.select(Some(state.session_wt_index));
        }

        frame.render_stateful_widget(list, layout[1], &mut list_state);

        // History panel (if there are past sessions)
        if has_history {
            render_history(frame, layout[2], state);
        }
    } else {
        // No active session — show prompt + history
        let msg = Paragraph::new("No active session. Press S to start one.").block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Session "),
        );
        frame.render_widget(msg, layout[0]);
        render_history(frame, layout[1], state);
    }
}

fn render_history(frame: &mut Frame, area: Rect, state: &AppState) {
    let hist_focused =
        state.session_focus == SessionFocus::History || state.data.current_session.is_none();
    let hist_border = if hist_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let items: Vec<ListItem> = state
        .data
        .session_history
        .iter()
        .map(|s| {
            let duration = format_session_duration(&s.started_at, s.ended_at.as_deref());
            let wt_count = state
                .data
                .session_wt_counts
                .get(&s.id)
                .copied()
                .unwrap_or(0);
            let notes_str = s
                .notes
                .as_deref()
                .map(|n| format!("  — {n}"))
                .unwrap_or_default();

            ListItem::new(Line::from(vec![
                Span::styled(
                    &s.id[..13.min(s.id.len())],
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(format!("  {duration}")),
                Span::styled(
                    format!("  {wt_count} wt"),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(notes_str),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(hist_border))
                .title(" Session History "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    if !state.data.session_history.is_empty() && hist_focused {
        list_state.select(Some(state.session_history_index));
    }

    frame.render_stateful_widget(list, area, &mut list_state);
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

fn format_session_duration(started_at: &str, ended_at: Option<&str>) -> String {
    let Ok(start) = chrono::DateTime::parse_from_rfc3339(started_at) else {
        return "?".to_string();
    };
    match ended_at {
        Some(end) => {
            let Ok(end_dt) = chrono::DateTime::parse_from_rfc3339(end) else {
                return "?".to_string();
            };
            let dur = end_dt - start;
            let total_secs = dur.num_seconds();
            let hours = total_secs / 3600;
            let mins = (total_secs % 3600) / 60;
            if hours > 0 {
                format!("{hours}h {mins}m")
            } else {
                format!("{mins}m")
            }
        }
        None => "ongoing".to_string(),
    }
}
