use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::state::{AppState, VisualRow};

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
            Span::styled("Model: ", Style::default().fg(Color::DarkGray)),
            match wt.model.as_deref() {
                Some(m) => Span::raw(m.to_string()),
                None => Span::styled(
                    "(not set — press m to configure)",
                    Style::default().fg(Color::DarkGray),
                ),
            },
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

    // Agent status line and plan checklist from DB poll
    if let Some(run) = state.data.latest_agent_runs.get(&wt.id) {
        lines.push(Line::from(""));
        lines.push(render_agent_status_line(run, &state.data.agent_totals));

        // Show pending feedback request prompt
        if let Some(ref fb) = state.data.pending_feedback {
            lines.push(Line::from(vec![
                Span::styled("  Feedback: ", Style::default().fg(Color::Magenta)),
                Span::styled(fb.prompt.clone(), Style::default().fg(Color::White)),
            ]));
        }

        // Show child runs if this is a parent (supervisor) run
        for child in &state.data.child_runs {
            lines.push(render_child_run_line(child));
        }

        // Plan checklist (if a plan was generated for this run)
        if let Some(ref steps) = run.plan {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Plan:",
                Style::default().fg(Color::DarkGray),
            )));
            for step in steps {
                let (checkbox, checkbox_color, style) = match step.status.as_str() {
                    "completed" => (
                        "[x]",
                        Color::Green,
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::DIM),
                    ),
                    "in_progress" => ("[>]", Color::Blue, Style::default().fg(Color::Blue)),
                    "failed" => ("[!]", Color::Red, Style::default().fg(Color::Red)),
                    _ => ("[ ]", Color::DarkGray, Style::default().fg(Color::White)),
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {checkbox} "),
                        Style::default().fg(checkbox_color),
                    ),
                    Span::styled(&step.description, style),
                ]));
            }
        }
    }

    // Issues created by agents
    if !state.data.agent_created_issues.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "Issues created:",
            Style::default().fg(Color::DarkGray),
        )]));
        for issue in &state.data.agent_created_issues {
            lines.push(Line::from(vec![
                Span::styled("  #", Style::default().fg(Color::DarkGray)),
                Span::styled(&issue.source_id, Style::default().fg(Color::Cyan)),
                Span::raw(" — "),
                Span::raw(&issue.title),
            ]));
        }
    }

    lines.push(Line::from(""));

    let actions_text = if wt.is_active() {
        let has_waiting = state
            .data
            .latest_agent_runs
            .get(&wt.id)
            .is_some_and(|run| run.status == "waiting_for_feedback");
        let has_log = state
            .data
            .latest_agent_runs
            .get(&wt.id)
            .is_some_and(|run| run.log_file.is_some());
        if has_waiting {
            "Actions: f=respond  F=dismiss  x=stop  r=agent  e=expand  m=model  j/k=scroll  w=work  p=push  P=PR  l=link  d=del  Esc=back"
        } else if has_log {
            "Actions: r=agent  x=stop  L=log  y=copy  e=expand  m=model  j/k=scroll  w=work  p=push  P=PR  l=link  d=del  Esc=back"
        } else {
            "Actions: r=agent  x=stop  e=expand  m=model  j/k=scroll  w=work  p=push  P=PR  l=link  d=del  Esc=back"
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

    let worktree_path = state
        .selected_worktree_id
        .as_ref()
        .and_then(|id| state.data.worktrees.iter().find(|w| &w.id == id))
        .map(|wt| wt.path.as_str())
        .unwrap_or("");

    let mut items: Vec<ListItem> = Vec::new();

    for row in state.data.visual_rows() {
        match row {
            VisualRow::RunSeparator(run_num, model, started_at) => {
                let ts = started_at
                    .get(..16)
                    .unwrap_or(started_at)
                    .replacen('T', " ", 1);
                let model_str = model.unwrap_or("default");
                let header = format!("── Run {run_num}  {ts}  {model_str} ");
                let pad = "─".repeat(60usize.saturating_sub(header.len()));
                items.push(ListItem::new(Line::from(Span::styled(
                    format!("{header}{pad}"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ))));
            }
            VisualRow::Event(ev) => {
                let style = event_style(&ev.kind);
                let (display_text, effective_style) = if ev.kind == "prompt" {
                    let step_label = extract_step_label(&ev.summary);
                    let is_step = step_label.is_some();
                    let label =
                        step_label.unwrap_or_else(|| shorten_paths(&ev.summary, worktree_path));
                    let s = if is_step {
                        Style::default().fg(Color::Magenta)
                    } else {
                        style
                    };
                    (label, s)
                } else if conductor_core::agent::parse_feedback_marker(&ev.summary).is_some() {
                    (
                        shorten_paths(&ev.summary, worktree_path),
                        Style::default().fg(Color::Magenta),
                    )
                } else {
                    (shorten_paths(&ev.summary, worktree_path), style)
                };
                let mut spans = vec![Span::styled(display_text, effective_style)];
                if let Some(dur) = ev.duration_ms() {
                    if dur >= 100 {
                        let dur_s = dur as f64 / 1000.0;
                        spans.push(Span::styled(
                            format!("  ({dur_s:.1}s)"),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }
                items.push(ListItem::new(Line::from(spans)));
            }
        }
    }

    let list = List::new(items)
        .block(activity_block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(list, area, &mut state.agent_list_state.borrow_mut());
}

fn shorten_paths(summary: &str, worktree_path: &str) -> String {
    // Replace worktree path first (more specific), then home dir (less specific)
    let s = if !worktree_path.is_empty() {
        summary.replacen(worktree_path, "{worktree}", 1)
    } else {
        summary.to_string()
    };
    match dirs::home_dir() {
        Some(home) => s.replacen(home.to_string_lossy().as_ref(), "~", 1),
        None => s,
    }
}

/// Extract a clean display label from an orchestrator child prompt.
/// Returns "Step N/M: description" if the prompt matches the child format.
fn extract_step_label(prompt: &str) -> Option<String> {
    let rest = prompt.strip_prefix("You are executing step ")?;
    let space = rest.find(' ')?;
    let step_num = &rest[..space];
    let rest = rest[space..].strip_prefix(" of ")?;
    let space = rest.find(' ')?;
    let total = &rest[..space];

    if let Some(idx) = prompt.find("## Your Assignment") {
        let after = &prompt[idx..];
        if let Some(nl) = after.find('\n') {
            let desc: String = after[nl + 1..]
                .trim_start()
                .lines()
                .take_while(|l| !l.starts_with("Focus only on this step"))
                .collect::<Vec<_>>()
                .join(" ");
            let desc = desc.trim();
            if !desc.is_empty() {
                let truncated = if desc.chars().count() > 80 {
                    let s: String = desc.chars().take(80).collect();
                    format!("{s}...")
                } else {
                    desc.to_string()
                };
                return Some(format!("STEP {step_num}/{total}: {truncated}"));
            }
        }
    }

    Some(format!("STEP {step_num}/{total}"))
}

fn event_style(kind: &str) -> Style {
    match kind {
        "text" => Style::default().fg(Color::White),
        "tool" => Style::default().fg(Color::Yellow),
        "result" => Style::default().fg(Color::Green),
        "system" => Style::default().fg(Color::DarkGray),
        "error" => Style::default().fg(Color::Red),
        "prompt" => Style::default().fg(Color::Cyan),
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
            let turns = totals.total_turns + totals.live_turns;
            let live_elapsed_ms = chrono::DateTime::parse_from_rfc3339(&run.started_at)
                .ok()
                .map(|start| {
                    let now = chrono::Utc::now();
                    (now - start.with_timezone(&chrono::Utc))
                        .num_milliseconds()
                        .max(0)
                });
            let total_ms = totals.total_duration_ms + live_elapsed_ms.unwrap_or(0);
            let dur_secs = total_ms as f64 / 1000.0;
            let cost = totals.total_cost;
            let stats = if cost > 0.0 {
                format!(" ${cost:.4}, {turns} turns, {dur_secs:.1}s{runs_label}")
            } else {
                format!(" {turns} turns, {dur_secs:.1}s{runs_label}")
            };
            Line::from(vec![
                Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
                Span::styled("[running]", Style::default().fg(Color::Yellow)),
                Span::styled(stats, Style::default().fg(Color::DarkGray)),
                Span::styled(" — press x to stop", Style::default().fg(Color::DarkGray)),
            ])
        }
        "waiting_for_feedback" => Line::from(vec![
            Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "[waiting for feedback]",
                Style::default().fg(Color::Magenta),
            ),
            Span::styled(
                " — press f to respond, F to dismiss",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
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
            if run.needs_resume() {
                let remaining = run.incomplete_plan_steps().len();
                spans.push(Span::styled(
                    format!(" [{remaining} steps remaining — press r to resume]"),
                    Style::default().fg(Color::Yellow),
                ));
            } else if let Some(ref err) = run.result_text {
                let truncated: String = err.chars().take(60).collect();
                spans.push(Span::styled(
                    format!(" {truncated}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        }
        "cancelled" => {
            let mut spans = vec![
                Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
                Span::styled("[cancelled]", Style::default().fg(Color::DarkGray)),
            ];
            if run.needs_resume() {
                let remaining = run.incomplete_plan_steps().len();
                spans.push(Span::styled(
                    format!(" [{remaining} steps remaining — press r to resume]"),
                    Style::default().fg(Color::Yellow),
                ));
            }
            Line::from(spans)
        }
        other => Line::from(vec![
            Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("[{other}]")),
        ]),
    }
}

/// Render a single child run as an indented line under the parent agent status.
fn render_child_run_line(run: &conductor_core::agent::AgentRun) -> Line<'static> {
    let (status_text, status_color) = match run.status.as_str() {
        "running" => ("running", Color::Yellow),
        "completed" => ("completed", Color::Green),
        "failed" => ("failed", Color::Red),
        "cancelled" => ("cancelled", Color::DarkGray),
        "waiting_for_feedback" => ("waiting", Color::Magenta),
        other => (other, Color::White),
    };
    let status_str = format!("[{status_text}]");

    let prompt = extract_step_label(&run.prompt).unwrap_or_else(|| {
        if run.prompt.chars().count() > 50 {
            let s: String = run.prompt.chars().take(50).collect();
            format!("{s}...")
        } else {
            run.prompt.clone()
        }
    });

    let mut spans = vec![
        Span::styled("  └─ ", Style::default().fg(Color::DarkGray)),
        Span::styled(status_str, Style::default().fg(status_color)),
        Span::styled(format!(" {prompt}"), Style::default().fg(Color::DarkGray)),
    ];

    let cost = run.cost_usd.unwrap_or(0.0);
    let turns = run.num_turns.unwrap_or(0);
    if cost > 0.0 || turns > 0 {
        spans.push(Span::styled(
            format!("  ${cost:.4} {turns}t"),
            Style::default().fg(Color::Magenta),
        ));
    }

    Line::from(spans)
}
