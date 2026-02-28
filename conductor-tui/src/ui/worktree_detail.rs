use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
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
    ];

    if let Some(ref completed) = wt.completed_at {
        lines.push(Line::from(vec![
            Span::styled("Completed: ", Style::default().fg(Color::DarkGray)),
            Span::raw(completed),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(ticket_line));

    // Agent status line from DB poll
    if let Some(run) = state.data.latest_agent_runs.get(&wt.id) {
        lines.push(Line::from(""));
        lines.push(render_agent_status_line(run, &state.data.agent_totals));
    }

    lines.push(Line::from(""));

    let actions_text = if wt.is_active() {
        let has_log = state
            .data
            .latest_agent_runs
            .get(&wt.id)
            .is_some_and(|run| run.log_file.is_some());
        if has_log {
            "Actions: r=agent  x=stop  L=log  J/K=scroll  w=work  o=ticket  p=push  P=PR  l=link  d=del  Esc=back"
        } else {
            "Actions: r=agent  x=stop  w=work  o=ticket  p=push  P=PR  l=link  d=delete  Esc=back"
        }
    } else {
        "Actions: o=open ticket  Esc=back  (archived)"
    };
    lines.push(Line::from(Span::styled(
        actions_text,
        Style::default().fg(Color::DarkGray),
    )));

    // Calculate info pane height: lines + 2 for border
    let info_height = (lines.len() as u16) + 2;

    // Split vertically: top = info (fixed), bottom = agent activity (fill)
    let chunks =
        Layout::vertical([Constraint::Length(info_height), Constraint::Min(3)]).split(area);

    // Top pane: worktree info
    let info_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Worktree Detail ");
    let info = Paragraph::new(lines).block(info_block);
    frame.render_widget(info, chunks[0]);

    // Bottom pane: agent activity
    render_agent_activity(frame, chunks[1], state);
}

fn render_agent_activity(frame: &mut Frame, area: Rect, state: &AppState) {
    let events = &state.data.agent_events;

    let activity_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Agent Activity ");

    if events.is_empty() {
        let empty = Paragraph::new(Span::styled(
            "No agent activity",
            Style::default().fg(Color::DarkGray),
        ))
        .block(activity_block);
        frame.render_widget(empty, area);
        return;
    }

    let lines: Vec<Line> = events
        .iter()
        .map(|ev| {
            let style = event_style(&ev.kind);
            Line::from(Span::styled(&ev.summary, style))
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(activity_block)
        .wrap(Wrap { trim: false })
        .scroll((state.agent_event_index as u16, 0));

    frame.render_widget(paragraph, area);
}

fn event_style(kind: &str) -> Style {
    match kind {
        "text" => Style::default().fg(Color::White),
        "tool" => Style::default().fg(Color::Yellow),
        "result" => Style::default().fg(Color::Green),
        "system" => Style::default().fg(Color::DarkGray),
        "error" => Style::default().fg(Color::Red),
        _ => Style::default(),
    }
}

/// Render a single agent status line from the latest AgentRun for this worktree.
/// Shows aggregate totals across all runs when there are multiple.
fn render_agent_status_line(
    run: &conductor_core::agent::AgentRun,
    totals: &crate::state::AgentTotals,
) -> Line<'static> {
    let runs_label = if totals.run_count > 1 {
        format!(" ({} runs)", totals.run_count)
    } else {
        String::new()
    };

    match run.status.as_str() {
        "running" => {
            let turns = totals.live_turns;
            let elapsed = chrono::DateTime::parse_from_rfc3339(&run.started_at)
                .ok()
                .map(|start| {
                    let now = chrono::Utc::now();
                    (now - start.with_timezone(&chrono::Utc))
                        .num_milliseconds()
                        .max(0) as f64
                        / 1000.0
                });
            let stats = match elapsed {
                Some(secs) => format!(" {turns} turns, {secs:.1}s"),
                None => format!(" {turns} turns"),
            };
            Line::from(vec![
                Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
                Span::styled("[running]", Style::default().fg(Color::Yellow)),
                Span::styled(stats, Style::default().fg(Color::DarkGray)),
                Span::styled(" — press x to stop", Style::default().fg(Color::DarkGray)),
            ])
        }
        "completed" => {
            let mut spans = vec![
                Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
                Span::styled("[completed]", Style::default().fg(Color::Green)),
            ];
            let cost = totals.total_cost;
            let turns = totals.total_turns;
            let dur_secs = totals.total_duration_ms as f64 / 1000.0;
            spans.push(Span::styled(
                format!(" ${cost:.4}, {turns} turns, {dur_secs:.1}s{runs_label}"),
                Style::default().fg(Color::DarkGray),
            ));
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
