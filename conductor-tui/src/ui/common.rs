use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
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
