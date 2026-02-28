use conductor_core::worktree::Worktree;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, Paragraph};
use ratatui::Frame;

use crate::state::{AppState, View};

pub fn render_header(frame: &mut Frame, area: Rect, state: &AppState) {
    let view_name = match state.view {
        View::Dashboard => "Dashboard",
        View::RepoDetail => "Repo Detail",
        View::WorktreeDetail => "Worktree Detail",
        View::Tickets => "Tickets",
        View::Session => "Session",
    };

    let session_info = if let Some(ref session) = state.data.current_session {
        if session.ended_at.is_none() {
            let elapsed = session_elapsed(&session.started_at);
            format!(" | Session: {elapsed}")
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let header = Line::from(vec![
        Span::styled(
            " Conductor ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {view_name}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(session_info, Style::default().fg(Color::Yellow)),
    ]);

    frame.render_widget(Paragraph::new(header), area);
}

pub fn render_status_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    let msg = if state.filter_active {
        format!("/{} ", state.filter_text)
    } else if let Some(ref msg) = state.status_message {
        msg.clone()
    } else {
        match state.view {
            View::Dashboard => {
                "Tab:panel  j/k:nav  Enter:select  a:add repo  c:create  s:sync  ?:help  q:quit"
                    .to_string()
            }
            View::RepoDetail => {
                "j/k:nav  Enter:select  c:create  d:remove repo  Esc:back  ?:help".to_string()
            }
            View::WorktreeDetail => {
                let has_running = state
                    .selected_worktree_id
                    .as_ref()
                    .and_then(|wt_id| state.data.latest_agent_runs.get(wt_id))
                    .is_some_and(|run| run.status == "running");
                if has_running {
                    "r:agent  x:stop  o:ticket  Esc:back  ?:help".to_string()
                } else {
                    "r:agent  o:ticket  p:push  P:PR  l:link  d:delete  Esc:back  ?:help"
                        .to_string()
                }
            }
            View::Tickets => "j/k:nav  /:filter  Esc:back  ?:help".to_string(),
            View::Session => "s:end session  Esc:back  ?:help".to_string(),
        }
    };

    let bar = Paragraph::new(Line::from(Span::styled(
        msg,
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(bar, area);
}

/// Build a `ListItem` for a worktree row.
///
/// Both the dashboard and repo-detail worktree panes use this so the
/// format stays consistent.  Pass `repo_prefix` to prepend the repo
/// slug (dashboard style) and `show_branch` to append the branch name
/// (repo-detail style).
pub fn worktree_list_item(
    wt: &Worktree,
    state: &AppState,
    repo_prefix: Option<&str>,
    show_branch: bool,
) -> ListItem<'static> {
    let is_active = wt.is_active();
    let status_color = match wt.status.as_str() {
        "active" => Color::Green,
        "merged" => Color::Blue,
        "abandoned" => Color::Red,
        _ => Color::White,
    };
    let text_style = if is_active {
        Style::default()
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut spans: Vec<Span<'static>> = Vec::new();

    if let Some(prefix) = repo_prefix {
        spans.push(Span::styled(
            format!("{prefix}/"),
            Style::default().fg(Color::DarkGray),
        ));
    }

    spans.push(Span::styled(
        wt.slug.clone(),
        text_style.add_modifier(if is_active {
            Modifier::BOLD
        } else {
            Modifier::DIM
        }),
    ));

    if show_branch {
        spans.push(Span::styled(format!("  {}", wt.branch), text_style));
    }

    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("[{}]", wt.status),
        Style::default().fg(status_color),
    ));

    if let Some(ticket) = wt
        .ticket_id
        .as_ref()
        .and_then(|tid| state.data.ticket_map.get(tid))
    {
        let ticket_state_color = match ticket.state.as_str() {
            "open" => Color::Green,
            "closed" => Color::DarkGray,
            "in_progress" => Color::Yellow,
            _ => Color::White,
        };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("#{} {}", ticket.source_id, ticket.state),
            Style::default().fg(ticket_state_color),
        ));
    }

    if let Some(run) = state.data.latest_agent_runs.get(&wt.id) {
        let (symbol, color) = match run.status.as_str() {
            "running" => ("● running", Color::Yellow),
            "completed" => ("✓ completed", Color::Green),
            "failed" => ("✗ failed", Color::Red),
            "cancelled" => ("○ cancelled", Color::DarkGray),
            _ => ("? unknown", Color::White),
        };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(symbol, Style::default().fg(color)));
    }

    ListItem::new(Line::from(spans))
}

/// Calculate elapsed time from an ISO 8601 timestamp to now.
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
