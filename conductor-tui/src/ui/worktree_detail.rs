use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use conductor_core::worktree::WorktreeStatus;

use super::helpers::shorten_paths;
use crate::state::{AppState, VisualRow, WorktreeDetailFocus};

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    super::workflow_column::render_with_workflow_column(frame, area, state, render_content);
}

fn render_content(frame: &mut Frame, area: Rect, state: &AppState) {
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
            "open" => state.theme.status_completed,
            "closed" => state.theme.label_secondary,
            "in_progress" => state.theme.status_running,
            _ => state.theme.label_primary,
        };
        vec![
            Span::styled("Ticket: ", Style::default().fg(state.theme.label_secondary)),
            Span::raw(format!("#{} — {}", ticket.source_id, ticket.title)),
            Span::raw("  "),
            Span::styled(
                format!("[{}]", ticket.state),
                Style::default().fg(ticket_state_color),
            ),
        ]
    } else {
        vec![
            Span::styled("Ticket: ", Style::default().fg(state.theme.label_secondary)),
            Span::styled(
                "None (press Enter to link)",
                Style::default().fg(state.theme.label_secondary),
            ),
        ]
    };

    let status_color = match wt.status {
        WorktreeStatus::Active => state.theme.status_completed,
        WorktreeStatus::Merged => Color::Blue,
        WorktreeStatus::Abandoned => state.theme.status_failed,
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "Worktree: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(&wt.slug, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("Repo: ", Style::default().fg(state.theme.label_secondary)),
            Span::raw(repo_slug),
        ]),
        Line::from(vec![
            Span::styled("Branch: ", Style::default().fg(state.theme.label_secondary)),
            Span::raw(&wt.branch),
        ]),
        Line::from(vec![
            Span::styled("Base: ", Style::default().fg(state.theme.label_secondary)),
            match wt.base_branch.as_deref() {
                Some(b) => Span::raw(b),
                None => Span::styled(
                    "(repo default)",
                    Style::default().fg(state.theme.label_secondary),
                ),
            },
        ]),
        Line::from(vec![
            Span::styled("Path: ", Style::default().fg(state.theme.label_secondary)),
            Span::raw(shorten_paths(
                &wt.path,
                "",
                dirs::home_dir().as_deref().and_then(|p| p.to_str()),
            )),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(state.theme.label_secondary)),
            Span::styled(wt.status.to_string(), Style::default().fg(status_color)),
        ]),
        Line::from(vec![
            Span::styled("Model: ", Style::default().fg(state.theme.label_secondary)),
            match wt.model.as_deref() {
                Some(m) => Span::raw(m.to_string()),
                None => Span::styled(
                    "(not set)",
                    Style::default().fg(state.theme.label_secondary),
                ),
            },
            Span::styled(
                " (press Enter to change)",
                Style::default().fg(state.theme.label_secondary),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Created: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::raw(&wt.created_at),
        ]),
        // TICKET row — index 8, always present so navigation index stays stable
        Line::from(ticket_line),
    ];

    if let Some(ref completed) = wt.completed_at {
        lines.push(Line::from(vec![
            Span::styled(
                "Completed: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::raw(completed),
        ]));
    }

    lines.push(Line::from(""));

    // Agent status line and plan checklist from DB poll
    if let Some(run) = state.data.latest_agent_runs.get(&wt.id) {
        lines.push(Line::from(""));
        lines.push(render_agent_status_line(
            run,
            &state.data.agent_totals,
            &state.theme,
        ));

        // Show pending feedback request prompt
        if let Some(ref fb) = state.data.pending_feedback {
            lines.push(Line::from(vec![
                Span::styled(
                    "  Feedback: ",
                    Style::default().fg(state.theme.status_waiting),
                ),
                Span::styled(
                    fb.prompt.clone(),
                    Style::default().fg(state.theme.label_primary),
                ),
            ]));
        }

        // Show child runs if this is a parent (supervisor) run
        for child in &state.data.child_runs {
            lines.push(render_child_run_line(child, &state.theme));
        }

        // Plan checklist (if a plan was generated for this run)
        if let Some(ref steps) = run.plan {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Plan:",
                Style::default().fg(state.theme.label_secondary),
            )));
            for step in steps {
                use conductor_core::agent::StepStatus;
                let (checkbox, checkbox_color, style) = match step.status {
                    StepStatus::Completed => (
                        "[x]",
                        state.theme.status_completed,
                        Style::default()
                            .fg(state.theme.status_completed)
                            .add_modifier(Modifier::DIM),
                    ),
                    StepStatus::InProgress => {
                        ("[>]", Color::Blue, Style::default().fg(Color::Blue))
                    }
                    StepStatus::Failed => (
                        "[!]",
                        state.theme.status_failed,
                        Style::default().fg(state.theme.status_failed),
                    ),
                    StepStatus::Pending => (
                        "[ ]",
                        state.theme.label_secondary,
                        Style::default().fg(state.theme.label_primary),
                    ),
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
            Style::default().fg(state.theme.label_secondary),
        )]));
        for issue in &state.data.agent_created_issues {
            lines.push(Line::from(vec![
                Span::styled("  #", Style::default().fg(state.theme.label_secondary)),
                Span::styled(
                    &issue.source_id,
                    Style::default().fg(state.theme.label_accent),
                ),
                Span::raw(" — "),
                Span::raw(&issue.title),
            ]));
        }
    }

    lines.push(Line::from(""));

    let actions_text = if wt.is_active() {
        let has_waiting = state.data.latest_agent_runs.get(&wt.id).is_some_and(|run| {
            run.status == conductor_core::agent::AgentRunStatus::WaitingForFeedback
        });
        let has_resumable_wf = state
            .data
            .latest_workflow_runs_by_worktree
            .get(&wt.id)
            .is_some_and(|r| {
                use conductor_core::workflow::WorkflowRunStatus;
                !matches!(
                    r.status,
                    WorkflowRunStatus::Running
                        | WorkflowRunStatus::Completed
                        | WorkflowRunStatus::Cancelled
                )
            });
        if has_waiting {
            if has_resumable_wf {
                "Tab=switch panel  y=copy  o=act  p=prompt  f=respond  F=dismiss  x=stop  w=workflow  r=resume wf  d=del  Esc=back"
            } else {
                "Tab=switch panel  y=copy  o=act  p=prompt  f=respond  F=dismiss  x=stop  w=workflow  d=del  Esc=back"
            }
        } else if has_resumable_wf {
            "Tab=switch panel  y=copy  o=act  p=prompt  O=orchestrate  x=stop  w=workflow  r=resume wf  d=del  Esc=back"
        } else {
            "Tab=switch panel  y=copy  o=act  p=prompt  O=orchestrate  x=stop  w=workflow  d=del  Esc=back"
        }
    } else {
        "Tab=switch panel  y=copy  o=act  Esc=back  (archived)"
    };
    lines.push(Line::from(Span::styled(
        actions_text,
        Style::default().fg(state.theme.label_secondary),
    )));

    // Calculate info pane height: lines + 2 for border
    let info_height = (lines.len() as u16) + 2;

    // Split vertically: top = info (fixed), bottom = agent activity (fill)
    let chunks =
        Layout::vertical([Constraint::Length(info_height), Constraint::Min(3)]).split(area);

    // Top pane: worktree info
    let info_focus = state.worktree_detail_focus == WorktreeDetailFocus::InfoPanel;
    let info_border_color = if info_focus {
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };
    // Highlight the selected row when the info panel has focus
    if info_focus {
        let sel = state.worktree_detail_selected_row;
        if sel < lines.len() {
            let line = std::mem::take(&mut lines[sel]);
            lines[sel] = line.patch_style(Style::default().add_modifier(Modifier::REVERSED));
        }
    }

    let info_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(info_border_color))
        .title(" Worktree Detail ");
    let info = Paragraph::new(lines).block(info_block);
    frame.render_widget(info, chunks[0]);

    // Bottom pane: agent activity
    render_agent_activity(frame, chunks[1], state);
}

fn render_agent_activity(frame: &mut Frame, area: Rect, state: &AppState) {
    let events = &state.data.agent_events;

    let log_focus = state.worktree_detail_focus == WorktreeDetailFocus::LogPanel;
    let log_border_color = if log_focus {
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };
    let activity_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(log_border_color))
        .title(" Agent Activity ");

    if events.is_empty() {
        let empty = Paragraph::new(Span::styled(
            "No agent activity",
            Style::default().fg(state.theme.label_secondary),
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
                        .fg(state.theme.label_secondary)
                        .add_modifier(Modifier::DIM),
                ))));
            }
            VisualRow::Event(ev) => {
                let style = event_style(&ev.kind);
                let (display_text, effective_style) = if ev.kind == "prompt" {
                    let step_label = extract_step_label(&ev.summary);
                    let is_step = step_label.is_some();
                    let label = step_label.unwrap_or_else(|| {
                        shorten_paths(&ev.summary, worktree_path, state.home_dir.as_deref())
                    });
                    let s = if is_step {
                        Style::default().fg(state.theme.status_waiting)
                    } else {
                        style
                    };
                    (label, s)
                } else if conductor_core::agent::parse_feedback_marker(&ev.summary).is_some() {
                    (
                        shorten_paths(&ev.summary, worktree_path, state.home_dir.as_deref()),
                        Style::default().fg(state.theme.status_waiting),
                    )
                } else {
                    (
                        shorten_paths(&ev.summary, worktree_path, state.home_dir.as_deref()),
                        style,
                    )
                };
                let mut spans = vec![Span::styled(display_text, effective_style)];
                if let Some(dur) = ev.duration_ms() {
                    if dur >= 100 {
                        let dur_s = dur as f64 / 1000.0;
                        spans.push(Span::styled(
                            format!("  ({dur_s:.1}s)"),
                            Style::default().fg(state.theme.label_secondary),
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
    theme: &crate::theme::Theme,
) -> Line<'static> {
    let runs_label = if totals.run_count > 1 {
        format!(" ({} runs)", totals.run_count)
    } else {
        String::new()
    };

    use conductor_core::agent::AgentRunStatus;
    match run.status {
        AgentRunStatus::Running => {
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
            let in_k = super::common::fmt_tokens_k(totals.total_input_tokens);
            let out_k = super::common::fmt_tokens_k(totals.total_output_tokens);
            let stats = if totals.total_input_tokens > 0 || totals.total_output_tokens > 0 {
                format!(" {in_k}↓ {out_k}↑ · {turns} turns · {dur_secs:.1}s{runs_label}")
            } else {
                format!(" {turns} turns · {dur_secs:.1}s{runs_label}")
            };
            Line::from(vec![
                Span::styled("Agent: ", Style::default().fg(theme.label_secondary)),
                Span::styled("[running]", Style::default().fg(theme.status_running)),
                Span::styled(stats, Style::default().fg(theme.label_secondary)),
                Span::styled(
                    " — press x to stop",
                    Style::default().fg(theme.label_secondary),
                ),
            ])
        }
        AgentRunStatus::WaitingForFeedback => Line::from(vec![
            Span::styled("Agent: ", Style::default().fg(theme.label_secondary)),
            Span::styled(
                "[waiting for feedback]",
                Style::default().fg(theme.status_waiting),
            ),
            Span::styled(
                " — press f to respond, F to dismiss",
                Style::default().fg(theme.label_secondary),
            ),
        ]),
        AgentRunStatus::Completed => {
            let mut spans = vec![
                Span::styled("Agent: ", Style::default().fg(theme.label_secondary)),
                Span::styled("[completed]", Style::default().fg(theme.status_completed)),
            ];
            let turns = totals.total_turns;
            let dur_secs = totals.total_duration_ms as f64 / 1000.0;
            let in_k = super::common::fmt_tokens_k(totals.total_input_tokens);
            let out_k = super::common::fmt_tokens_k(totals.total_output_tokens);
            let stats = if totals.total_input_tokens > 0 || totals.total_output_tokens > 0 {
                format!(" {in_k}↓ {out_k}↑ · {turns} turns · {dur_secs:.1}s{runs_label}")
            } else {
                format!(" {turns} turns · {dur_secs:.1}s{runs_label}")
            };
            spans.push(Span::styled(
                stats,
                Style::default().fg(theme.label_secondary),
            ));
            if let Some(ref sid) = run.claude_session_id {
                spans.push(Span::styled(
                    format!("  session: {}", &sid[..13.min(sid.len())]),
                    Style::default().fg(theme.label_secondary),
                ));
            }
            Line::from(spans)
        }
        AgentRunStatus::Failed => {
            let mut spans = vec![
                Span::styled("Agent: ", Style::default().fg(theme.label_secondary)),
                Span::styled("[failed]", Style::default().fg(theme.status_failed)),
            ];
            if run.needs_resume() {
                let remaining = run.incomplete_plan_steps().len();
                spans.push(Span::styled(
                    format!(" [{remaining} steps remaining — press r to resume]"),
                    Style::default().fg(theme.label_warning),
                ));
            } else if let Some(ref err) = run.result_text {
                let truncated: String = err.chars().take(60).collect();
                spans.push(Span::styled(
                    format!(" {truncated}"),
                    Style::default().fg(theme.label_secondary),
                ));
            }
            Line::from(spans)
        }
        AgentRunStatus::Cancelled => {
            let mut spans = vec![
                Span::styled("Agent: ", Style::default().fg(theme.label_secondary)),
                Span::styled("[cancelled]", Style::default().fg(theme.status_cancelled)),
            ];
            if run.needs_resume() {
                let remaining = run.incomplete_plan_steps().len();
                spans.push(Span::styled(
                    format!(" [{remaining} steps remaining — press r to resume]"),
                    Style::default().fg(theme.label_warning),
                ));
            }
            Line::from(spans)
        }
    }
}

/// Render a single child run as an indented line under the parent agent status.
fn render_child_run_line(
    run: &conductor_core::agent::AgentRun,
    theme: &crate::theme::Theme,
) -> Line<'static> {
    use conductor_core::agent::AgentRunStatus;
    let (status_text, status_color) = match run.status {
        AgentRunStatus::Running => ("running", theme.status_running),
        AgentRunStatus::Completed => ("completed", theme.status_completed),
        AgentRunStatus::Failed => ("failed", theme.status_failed),
        AgentRunStatus::Cancelled => ("cancelled", theme.status_cancelled),
        AgentRunStatus::WaitingForFeedback => ("waiting", theme.status_waiting),
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
        Span::styled("  └─ ", Style::default().fg(theme.label_secondary)),
        Span::styled(status_str, Style::default().fg(status_color)),
        Span::styled(
            format!(" {prompt}"),
            Style::default().fg(theme.label_secondary),
        ),
    ];

    let turns = run.num_turns.unwrap_or(0);
    let in_tok = run.input_tokens.unwrap_or(0);
    let out_tok = run.output_tokens.unwrap_or(0);
    if in_tok > 0 || out_tok > 0 || turns > 0 {
        let in_k = super::common::fmt_tokens_k(in_tok);
        let out_k = super::common::fmt_tokens_k(out_tok);
        let tok_str = if in_tok > 0 || out_tok > 0 {
            format!("  {in_k}↓ {out_k}↑ {turns}t")
        } else {
            format!("  {turns}t")
        };
        spans.push(Span::styled(
            tok_str,
            Style::default().fg(theme.status_waiting),
        ));
    }

    Line::from(spans)
}
