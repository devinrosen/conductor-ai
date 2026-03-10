use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use conductor_core::worktree::Worktree;

use super::common::truncate;
use super::helpers::shorten_paths;
use crate::state::AppState;
use crate::state::WorkflowRunDetailFocus;
use crate::state::WorkflowsFocus;

/// Return the slug of the worktree matching `predicate`, or `"?"` if not found.
fn worktree_slug(worktrees: &[Worktree], predicate: impl Fn(&Worktree) -> bool) -> &str {
    worktrees
        .iter()
        .find(|wt| predicate(wt))
        .map(|wt| wt.slug.as_str())
        .unwrap_or("?")
}

/// Render the Workflows split-pane view: defs (left) + runs (right).
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    render_defs(frame, chunks[0], state);
    render_runs(frame, chunks[1], state);
}

fn render_defs(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused = state.workflows_focus == WorkflowsFocus::Defs;
    let border_color = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let global_mode = state.selected_worktree_id.is_none();

    let items: Vec<ListItem> = state
        .data
        .workflow_defs
        .iter()
        .map(|def| {
            let node_count = def.body.len();
            let input_count = def.inputs.len();
            let mut spans = vec![Span::styled(
                format!("{:<20}", def.name),
                Style::default().add_modifier(Modifier::BOLD),
            )];
            if global_mode {
                // Derive worktree slug from source_path by matching against known worktrees
                let wt_slug = worktree_slug(&state.data.worktrees, |wt| {
                    def.source_path.starts_with(&wt.path)
                });
                spans.push(Span::styled(
                    format!("  {wt_slug}"),
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                spans.push(Span::styled(
                    format!("  {}", truncate(&def.description, 30)),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            spans.push(Span::styled(
                format!("  {node_count} steps"),
                Style::default().fg(Color::Yellow),
            ));
            if input_count > 0 {
                spans.push(Span::styled(
                    format!("  {input_count} inputs"),
                    Style::default().fg(Color::Magenta),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let defs_title = if global_mode {
        " All Workflow Definitions "
    } else {
        " Workflow Definitions "
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(defs_title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    if !state.data.workflow_defs.is_empty() {
        list_state.select(Some(state.workflow_def_index));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_runs(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused = state.workflows_focus == WorkflowsFocus::Runs;
    let border_color = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    // In global mode (no worktree selected), show worktree context on each run row.
    let global_mode = state.selected_worktree_id.is_none();

    let items: Vec<ListItem> = state
        .data
        .workflow_runs
        .iter()
        .map(|run| {
            let (status_symbol, status_color) = status_display(&run.status.to_string());
            let duration = if let Some(ref ended) = run.ended_at {
                format_duration(&run.started_at, ended)
            } else {
                "…".to_string()
            };

            let mut spans = vec![
                Span::styled(status_symbol, Style::default().fg(status_color)),
                Span::raw("  "),
                Span::styled(
                    format!("{:<20}", run.workflow_name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ];

            if global_mode {
                let wt_slug = worktree_slug(&state.data.worktrees, |w| w.id == run.worktree_id);
                spans.push(Span::styled(
                    format!("  {wt_slug}"),
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                spans.push(Span::styled(
                    format!("  {}", &run.started_at[..19].replace('T', " ")),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            spans.push(Span::styled(
                format!("  {duration}"),
                Style::default().fg(Color::Yellow),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let runs_title = if global_mode {
        " All Workflow Runs "
    } else {
        " Workflow Runs "
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(runs_title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    if !state.data.workflow_runs.is_empty() {
        list_state.select(Some(state.workflow_run_index));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Render the workflow run detail view: header + split pane (steps | agent activity).
pub fn render_run_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    let run_info = state
        .selected_workflow_run_id
        .as_ref()
        .and_then(|id| state.data.workflow_runs.iter().find(|r| &r.id == id));

    // Header area (3 lines) + body
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(area);

    // Header
    if let Some(run) = run_info {
        let (status_symbol, status_color) = status_display(&run.status.to_string());
        let started_display = run
            .started_at
            .get(..19)
            .unwrap_or(&run.started_at)
            .replace('T', " ");
        let summary_display = run.result_summary.as_deref().unwrap_or("—").to_string();
        let header_lines = vec![
            Line::from(vec![
                Span::styled(" Workflow: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    run.workflow_name.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(status_symbol, Style::default().fg(status_color)),
            ]),
            Line::from(vec![
                Span::styled(" Started:  ", Style::default().fg(Color::DarkGray)),
                Span::raw(started_display),
                if run.dry_run {
                    Span::styled("  [dry-run]", Style::default().fg(Color::Yellow))
                } else {
                    Span::raw("")
                },
            ]),
            Line::from(vec![
                Span::styled(" Summary:  ", Style::default().fg(Color::DarkGray)),
                Span::raw(summary_display),
            ]),
        ];
        let header_block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray));
        frame.render_widget(Paragraph::new(header_lines).block(header_block), chunks[0]);
    }

    // Determine if the selected step has agent activity to show
    let selected_step = state.data.workflow_steps.get(state.workflow_step_index);
    let has_agent_activity = selected_step
        .map(|s| s.child_run_id.is_some())
        .unwrap_or(false);

    if has_agent_activity {
        // Split pane: steps (left 45%) | agent activity (right 55%)
        let body_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[1]);

        let focus = state.workflow_run_detail_focus;
        render_step_list(frame, body_chunks[0], state, focus);
        render_step_agent_activity(frame, body_chunks[1], state, focus);
    } else {
        // Full-width step list when no agent activity to show —
        // force Steps focus since agent pane is hidden.
        render_step_list(frame, chunks[1], state, WorkflowRunDetailFocus::Steps);
    }
}

fn render_step_list(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    focus: WorkflowRunDetailFocus,
) {
    let focused = focus == WorkflowRunDetailFocus::Steps;
    let border_color = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let items: Vec<ListItem> = state
        .data
        .workflow_steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let (status_symbol, status_color) = status_display(&step.status.to_string());
            let duration = match (&step.started_at, &step.ended_at) {
                (Some(start), Some(end)) => format_duration(start, end),
                (Some(_), None) => "…".to_string(),
                _ => "—".to_string(),
            };

            let mut spans = vec![
                Span::styled(
                    format!(" {:>2}. ", step.position),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(status_symbol, Style::default().fg(status_color)),
                Span::raw("  "),
                Span::styled(
                    format!("{:<20}", step.step_name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  [{:<5}]", step.role),
                    Style::default().fg(Color::Magenta),
                ),
                Span::styled(format!("  {duration}"), Style::default().fg(Color::Yellow)),
            ];

            if step.iteration > 0 {
                spans.push(Span::styled(
                    format!("  iter:{}", step.iteration),
                    Style::default().fg(Color::Cyan),
                ));
            }
            if step.retry_count > 0 {
                spans.push(Span::styled(
                    format!("  retries:{}", step.retry_count),
                    Style::default().fg(Color::Red),
                ));
            }
            if let Some(ref gate_type) = step.gate_type {
                spans.push(Span::styled(
                    format!("  gate:{gate_type}"),
                    Style::default().fg(Color::Yellow),
                ));
            }

            // Inline detail: show snippet of result/context/markers for non-selected steps
            if i != state.workflow_step_index {
                if let Some(ref rt) = step.result_text {
                    let snippet = truncate(rt.lines().next().unwrap_or(""), 40);
                    spans.push(Span::styled(
                        format!("  → {snippet}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                } else if let Some(ref ctx) = step.context_out {
                    let snippet = truncate(ctx.lines().next().unwrap_or(""), 40);
                    spans.push(Span::styled(
                        format!("  ctx:{snippet}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }

            if let Some(ref mk) = step.markers_out {
                spans.push(Span::styled(
                    format!("  [{mk}]"),
                    Style::default().fg(Color::Cyan),
                ));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let title = if focused {
        " Steps (Enter=detail, Tab=switch) "
    } else {
        " Steps "
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    if !state.data.workflow_steps.is_empty() {
        list_state.select(Some(state.workflow_step_index));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Render agent activity for the selected workflow step's child run.
fn render_step_agent_activity(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    focus: WorkflowRunDetailFocus,
) {
    let focused = focus == WorkflowRunDetailFocus::AgentActivity;
    let border_color = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let events = &state.data.step_agent_events;
    let agent_run = &state.data.step_agent_run;

    // Title with run status
    let title = if let Some(ref run) = agent_run {
        let model = run.model.as_deref().unwrap_or("default");
        if focused {
            format!(" Agent: {model} ({}) (Tab=switch) ", run.status)
        } else {
            format!(" Agent: {model} ({}) ", run.status)
        }
    } else if focused {
        " Agent Activity (Tab=switch) ".to_string()
    } else {
        " Agent Activity ".to_string()
    };

    let activity_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(title);

    if events.is_empty() {
        let msg = if agent_run
            .as_ref()
            .map(|r| r.status == conductor_core::agent::AgentRunStatus::Running)
            .unwrap_or(false)
        {
            "Agent running — waiting for events…"
        } else {
            "No agent events"
        };
        let empty = Paragraph::new(Span::styled(msg, Style::default().fg(Color::DarkGray)))
            .block(activity_block);
        frame.render_widget(empty, area);
        return;
    }

    let worktree_path = state
        .selected_worktree_id
        .as_ref()
        .and_then(|id| state.data.worktrees.iter().find(|w| &w.id == id))
        .or_else(|| {
            state.data.step_agent_run.as_ref().and_then(|run| {
                state
                    .data
                    .worktrees
                    .iter()
                    .find(|w| w.id == run.worktree_id)
            })
        })
        .map(|wt| wt.path.as_str())
        .unwrap_or("");

    let items: Vec<ListItem> = events
        .iter()
        .map(|ev| {
            let style = event_kind_style(&ev.kind);
            let dur = ev
                .duration_ms()
                .map(|ms| format!(" ({:.1}s)", ms as f64 / 1000.0))
                .unwrap_or_default();
            let ts = ev.started_at.get(11..19).unwrap_or(&ev.started_at);
            let summary = truncate(&shorten_paths(&ev.summary, worktree_path), 80);
            let spans = vec![
                Span::styled(format!("{ts} "), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:<10}", ev.kind), style),
                Span::styled(dur, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(summary, style),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    if focused {
        let list = List::new(items)
            .block(activity_block)
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");
        let mut list_state = ListState::default();
        if !events.is_empty() {
            list_state.select(Some(state.step_agent_event_index));
        }
        frame.render_stateful_widget(list, area, &mut list_state);
    } else {
        let list = List::new(items).block(activity_block);
        frame.render_widget(list, area);
    }
}

fn event_kind_style(kind: &str) -> Style {
    match kind {
        "tool_use" => Style::default().fg(Color::Blue),
        "tool_result" => Style::default().fg(Color::Green),
        "api_request" => Style::default().fg(Color::Yellow),
        "error" => Style::default().fg(Color::Red),
        "prompt" => Style::default().fg(Color::Magenta),
        "result" => Style::default().fg(Color::Cyan),
        _ => Style::default().fg(Color::White),
    }
}

fn status_display(status: &str) -> (String, Color) {
    match status {
        "pending" => ("○ pending".to_string(), Color::DarkGray),
        "running" => ("● running".to_string(), Color::Yellow),
        "completed" => ("✓ completed".to_string(), Color::Green),
        "failed" => ("✗ failed".to_string(), Color::Red),
        "cancelled" => ("○ cancelled".to_string(), Color::DarkGray),
        "waiting" => ("⏸ waiting".to_string(), Color::Magenta),
        "skipped" => ("⊘ skipped".to_string(), Color::DarkGray),
        _ => (format!("? {status}"), Color::White),
    }
}

fn format_duration(start: &str, end: &str) -> String {
    let Ok(s) = chrono::DateTime::parse_from_rfc3339(start) else {
        return "?".to_string();
    };
    let Ok(e) = chrono::DateTime::parse_from_rfc3339(end) else {
        return "?".to_string();
    };
    let dur = e.signed_duration_since(s);
    let secs = dur.num_seconds();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}
