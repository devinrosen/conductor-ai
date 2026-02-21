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

    let ticket_line: Vec<Span> = if let Some(ticket) = wt
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
        vec![
            Span::styled("Ticket: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("#{} — {}", ticket.source_id, ticket.title)),
            Span::raw("  "),
            Span::styled(
                format!("[{}]", ticket.state),
                Style::default().fg(ticket_state_color),
            ),
        ]
    } else {
        vec![
            Span::styled("Ticket: ", Style::default().fg(Color::DarkGray)),
            Span::raw("None (press l to link)"),
        ]
    };

    let status_color = match wt.status.as_str() {
        "active" => Color::Green,
        "merged" => Color::Blue,
        _ => Color::Red,
    };

    let mut lines = vec![
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
        Line::from(ticket_line),
    ];

    // Agent status line from DB poll
    if let Some(run) = state.data.latest_agent_runs.get(&wt.id) {
        lines.push(Line::from(""));
        lines.push(render_agent_status_line(run));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Actions: r=agent  a=attach  x=stop  w=work  o=ticket  p=push  P=PR  l=link  d=delete  Esc=back",
        Style::default().fg(Color::DarkGray),
    )));

    let content = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Worktree Detail "),
    );

    frame.render_widget(content, area);
}

/// Render a single agent status line from the latest AgentRun for this worktree.
fn render_agent_status_line(run: &conductor_core::agent::AgentRun) -> Line<'static> {
    match run.status.as_str() {
        "running" => Line::from(vec![
            Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
            Span::styled("[running]", Style::default().fg(Color::Yellow)),
            Span::styled(
                " — press a to attach, x to stop",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        "completed" => {
            let mut spans = vec![
                Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
                Span::styled("[completed]", Style::default().fg(Color::Green)),
            ];
            if let Some(cost) = run.cost_usd {
                let turns = run.num_turns.unwrap_or(0);
                let dur_secs = run.duration_ms.map(|ms| ms as f64 / 1000.0).unwrap_or(0.0);
                spans.push(Span::styled(
                    format!(" ${cost:.4}, {turns} turns, {dur_secs:.1}s"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            if let Some(ref sid) = run.claude_session_id {
                spans.push(Span::styled(
                    format!("  session: {}", &sid[..13.min(sid.len())]),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        }
        "failed" => {
            let mut spans = vec![
                Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
                Span::styled("[failed]", Style::default().fg(Color::Red)),
            ];
            if let Some(ref err) = run.result_text {
                let truncated = if err.len() > 60 { &err[..60] } else { err };
                spans.push(Span::styled(
                    format!(" {truncated}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        }
        "cancelled" => Line::from(vec![
            Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
            Span::styled("[cancelled]", Style::default().fg(Color::DarkGray)),
        ]),
        other => Line::from(vec![
            Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("[{other}]")),
        ]),
    }
}
