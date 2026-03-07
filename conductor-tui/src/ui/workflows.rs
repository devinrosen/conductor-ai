use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use super::common::truncate;
use crate::state::{AppState, WorkflowsFocus};

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

    let items: Vec<ListItem> = state
        .data
        .workflow_defs
        .iter()
        .map(|def| {
            let node_count = def.body.len();
            let input_count = def.inputs.len();
            let mut spans = vec![
                Span::styled(
                    format!("{:<20}", def.name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", truncate(&def.description, 30)),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("  {node_count} steps"),
                    Style::default().fg(Color::Yellow),
                ),
            ];
            if input_count > 0 {
                spans.push(Span::styled(
                    format!("  {input_count} inputs"),
                    Style::default().fg(Color::Magenta),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(" Workflow Definitions "),
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

            let spans = vec![
                Span::styled(status_symbol, Style::default().fg(status_color)),
                Span::raw("  "),
                Span::styled(
                    format!("{:<20}", run.workflow_name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", &run.started_at[..19].replace('T', " ")),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(format!("  {duration}"), Style::default().fg(Color::Yellow)),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(" Workflow Runs "),
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

/// Render the workflow run detail view: step table.
pub fn render_run_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    let run_info = state
        .selected_workflow_run_id
        .as_ref()
        .and_then(|id| state.data.workflow_runs.iter().find(|r| &r.id == id));

    // Header area (3 lines) + steps list
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
        frame.render_widget(
            ratatui::widgets::Paragraph::new(header_lines).block(header_block),
            chunks[0],
        );
    }

    // Steps list
    let items: Vec<ListItem> = state
        .data
        .workflow_steps
        .iter()
        .map(|step| {
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

            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Steps "),
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
    frame.render_stateful_widget(list, chunks[1], &mut list_state);
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
