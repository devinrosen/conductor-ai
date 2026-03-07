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
        View::Workflows => "Workflows",
        View::WorkflowRunDetail => "Workflow Run",
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
    ]);

    frame.render_widget(Paragraph::new(header), area);
}

pub fn render_status_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    let msg = if let Some(f) = state.active_filter() {
        format!("/{} ", f.text)
    } else if let Some(ref msg) = state.status_message {
        msg.clone()
    } else {
        match state.view {
            View::Dashboard => {
                "Tab:panel  j/k:nav  Enter:select  a:add repo  c:create  s:sync  ?:help  q:quit"
                    .to_string()
            }
            View::RepoDetail => {
                "j/k:nav  Enter:select  c:create  d:remove  S:sources  Esc:back  ?:help".to_string()
            }
            View::WorktreeDetail => {
                let has_running = state
                    .selected_worktree_id
                    .as_ref()
                    .and_then(|wt_id| state.data.latest_agent_runs.get(wt_id))
                    .is_some_and(|run| run.is_active());
                if has_running {
                    "r:agent  x:stop  o:ticket  Esc:back  ?:help".to_string()
                } else {
                    "r:agent  o:ticket  p:push  P:PR  l:link  d:delete  Esc:back  ?:help"
                        .to_string()
                }
            }
            View::Tickets => "j/k:nav  /:filter  Esc:back  ?:help".to_string(),
            View::Workflows => {
                "Tab:panel  j/k:nav  Enter:select  r:run  Esc:back  ?:help".to_string()
            }
            View::WorkflowRunDetail => {
                let has_gate = state
                    .data
                    .workflow_steps
                    .iter()
                    .any(|s| s.status.to_string() == "waiting" && s.gate_type.is_some());
                if has_gate {
                    "j/k:nav  Enter:detail  g:gate  x:cancel  Esc:back  ?:help".to_string()
                } else {
                    "j/k:nav  Enter:detail  x:cancel  Esc:back  ?:help".to_string()
                }
            }
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
        use conductor_core::agent::AgentRunStatus;
        let (symbol, color) = match run.status {
            AgentRunStatus::Running => ("● running", Color::Yellow),
            AgentRunStatus::WaitingForFeedback => ("⏸ waiting for feedback", Color::Magenta),
            AgentRunStatus::Completed => ("✓ completed", Color::Green),
            AgentRunStatus::Failed => ("✗ failed", Color::Red),
            AgentRunStatus::Cancelled => ("○ cancelled", Color::DarkGray),
        };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(symbol, Style::default().fg(color)));
    }

    ListItem::new(Line::from(spans))
}

/// Build optional worktree-indicator spans for a ticket row.
///
/// Returns spans like `  ● feat-auth` (green, active) or `  ○ fix-bug`
/// (gray, merged/abandoned).  When multiple worktrees are linked the
/// label shows the count instead, e.g. `  ● 3 worktrees`.
///
/// Pass `leading_space` to control the whitespace before the indicator
/// (single space for the padded Tickets view, double for compact views).
pub fn ticket_worktree_spans(
    state: &AppState,
    ticket_id: &str,
    leading: &str,
) -> Vec<Span<'static>> {
    let Some(wts) = state.data.ticket_worktrees.get(ticket_id) else {
        return Vec::new();
    };
    let Some(best) = wts.iter().find(|w| w.is_active()).or(wts.first()) else {
        return Vec::new();
    };

    let (indicator, color) = if best.is_active() {
        ("●", Color::Green)
    } else {
        ("○", Color::DarkGray)
    };
    let label = if wts.len() > 1 {
        format!("{indicator} {} worktrees", wts.len())
    } else {
        format!("{indicator} {}", best.slug)
    };
    vec![Span::styled(
        format!("{leading}{label}"),
        Style::default().fg(color),
    )]
}

/// Build optional agent-totals spans for a ticket row.
///
/// Compact views (dashboard, repo-detail) pass `show_duration: false`
/// to get `$X.XX Xt`.  The full Tickets view passes `true` to also
/// show `Xm XXs`.
pub fn ticket_agent_total_spans(
    state: &AppState,
    ticket_id: &str,
    leading: &str,
    show_duration: bool,
) -> Vec<Span<'static>> {
    let Some(totals) = state.data.ticket_agent_totals.get(ticket_id) else {
        return Vec::new();
    };
    let text = if show_duration {
        let dur_secs = totals.total_duration_ms as f64 / 1000.0;
        let mins = (dur_secs / 60.0) as i64;
        let secs = (dur_secs % 60.0) as i64;
        format!(
            "{leading}${:.2}  {}t  {}m{:02}s",
            totals.total_cost, totals.total_turns, mins, secs
        )
    } else {
        format!("{leading}${:.2} {}t", totals.total_cost, totals.total_turns)
    };
    vec![Span::styled(text, Style::default().fg(Color::Magenta))]
}

/// Truncate a string to at most `max` characters, appending "…" if truncated.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}
