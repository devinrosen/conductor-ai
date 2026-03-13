use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use conductor_core::workflow::{WorkflowDef, WorkflowRun, WorkflowRunStatus};

use super::common::truncate;
use super::helpers::{shorten_paths, visual_idx_with_headers};
use crate::state::AppState;
use crate::state::TargetType;
use crate::state::WorkflowRunDetailFocus;
use crate::state::WorkflowRunRow;
use crate::state::WorkflowsFocus;

/// Render the Workflows split-pane view: defs (left) + runs (right).
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    // Always show a 1-line context bar so the user knows which worktree's
    // workflows they are viewing (or that they are in global mode).
    let selected_wt = state
        .selected_worktree_id
        .as_ref()
        .and_then(|id| state.data.worktrees.iter().find(|w| &w.id == id));

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let (header_area, area) = (v[0], v[1]);

    let header_line = if let Some(wt) = selected_wt {
        Line::from(vec![
            Span::styled(
                "Worktree: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(
                wt.slug.clone(),
                Style::default()
                    .fg(state.theme.label_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  Branch: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::raw(wt.branch.clone()),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                "Worktree: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled("global", Style::default().fg(state.theme.label_secondary)),
        ])
    };
    frame.render_widget(Paragraph::new(header_line), header_area);

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
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };

    let global_mode = state.selected_worktree_id.is_none();

    if global_mode {
        // Use pre-computed (repo_slug, def) pairs from state (populated by background thread).
        let fallback = String::from("?");
        let defs_with_slug: Vec<(&str, &WorkflowDef)> = state
            .data
            .workflow_defs
            .iter()
            .enumerate()
            .map(|(i, def)| {
                let slug = state
                    .data
                    .workflow_def_slugs
                    .get(i)
                    .unwrap_or(&fallback)
                    .as_str();
                (slug, def)
            })
            .collect();

        let mut items: Vec<ListItem> = Vec::new();
        let mut prev_repo = "";
        for (repo_slug, def) in &defs_with_slug {
            if *repo_slug != prev_repo {
                let fill = format!("{:─<30}", "");
                items.push(ListItem::new(Line::from(vec![Span::styled(
                    format!("─ {repo_slug} {fill}"),
                    Style::default()
                        .fg(state.theme.label_secondary)
                        .add_modifier(Modifier::BOLD),
                )])));
                prev_repo = repo_slug;
            }
            let node_count = def.body.len();
            let input_count = def.inputs.len();
            let mut spans = vec![
                Span::raw("  \u{2514} "),
                Span::styled(
                    format!("{:<20}", def.name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {node_count} steps"),
                    Style::default().fg(state.theme.label_warning),
                ),
            ];
            if !def.targets.is_empty() {
                let badge = format!("  [{}]", def.targets.join(", "));
                spans.push(Span::styled(
                    badge,
                    Style::default().fg(state.theme.label_accent),
                ));
            }
            if input_count > 0 {
                spans.push(Span::styled(
                    format!("  {input_count} inputs"),
                    Style::default().fg(state.theme.status_waiting),
                ));
            }
            items.push(ListItem::new(Line::from(spans)));
        }

        let visual_idx = if !state.data.workflow_defs.is_empty() {
            let logical_idx = state
                .workflow_def_index
                .min(defs_with_slug.len().saturating_sub(1));
            visual_idx_with_headers(&defs_with_slug, |(slug, _)| slug.to_string(), logical_idx)
        } else {
            0
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color))
                    .title(" All Workflow Definitions "),
            )
            .highlight_style(
                Style::default()
                    .bg(state.theme.highlight_bg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");

        let mut list_state = ListState::default();
        if !state.data.workflow_defs.is_empty() {
            list_state.select(Some(visual_idx));
        }
        frame.render_stateful_widget(list, area, &mut list_state);
    } else {
        // Single-worktree mode: flat list with description and target badges.
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
                        Style::default().fg(state.theme.label_secondary),
                    ),
                    Span::styled(
                        format!("  {node_count} steps"),
                        Style::default().fg(state.theme.label_warning),
                    ),
                ];
                if !def.targets.is_empty() {
                    let badge = format!("  [{}]", def.targets.join(", "));
                    spans.push(Span::styled(
                        badge,
                        Style::default().fg(state.theme.label_accent),
                    ));
                }
                if input_count > 0 {
                    spans.push(Span::styled(
                        format!("  {input_count} inputs"),
                        Style::default().fg(state.theme.status_waiting),
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
                    .bg(state.theme.highlight_bg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");

        let mut list_state = ListState::default();
        if !state.data.workflow_defs.is_empty() {
            list_state.select(Some(state.workflow_def_index));
        }
        frame.render_stateful_widget(list, area, &mut list_state);
    }
}

fn render_runs(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused = state.workflows_focus == WorkflowsFocus::Runs;
    let border_color = if focused {
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };

    // In global mode (no worktree selected), show target context on each run row.
    let global_mode = state.selected_worktree_id.is_none();

    let visible = state.visible_workflow_run_rows();

    // Build run_id → WorkflowRun map for O(1) lookup.
    let run_map: HashMap<&str, &WorkflowRun> = state
        .data
        .workflow_runs
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();

    let items: Vec<ListItem> = visible
        .iter()
        .map(|row| {
            // Handle header rows first — they have no associated WorkflowRun.
            match row {
                WorkflowRunRow::RepoHeader {
                    repo_slug,
                    collapsed,
                    run_count,
                } => {
                    let arrow = if *collapsed { "▶" } else { "▼" };
                    let label = if *collapsed {
                        format!("{arrow} {repo_slug}  (+{run_count})")
                    } else {
                        format!("{arrow} {repo_slug}")
                    };
                    return ListItem::new(Line::from(vec![Span::styled(
                        label,
                        Style::default()
                            .fg(state.theme.group_header)
                            .add_modifier(Modifier::BOLD),
                    )]));
                }
                WorkflowRunRow::TargetHeader {
                    label,
                    target_type,
                    collapsed,
                    run_count,
                    ..
                } => {
                    let arrow = if *collapsed { "▶" } else { "▼" };
                    let type_badge = match target_type {
                        TargetType::Pr => "[pr]",
                        TargetType::Worktree => "[wt]",
                    };
                    let display = if *collapsed {
                        format!("  {arrow} {:<30}  {type_badge}  (+{run_count})", label)
                    } else {
                        format!("  {arrow} {:<30}  {type_badge}", label)
                    };
                    return ListItem::new(Line::from(vec![Span::styled(
                        display,
                        Style::default().fg(state.theme.label_secondary),
                    )]));
                }
                _ => {}
            }

            // Parent / Child rows: look up the run.
            let Some(run_id) = row.run_id() else {
                return ListItem::new(Line::from(vec![Span::raw("?")]));
            };
            let Some(run) = run_map.get(run_id) else {
                return ListItem::new(Line::from(vec![Span::raw("?")]));
            };

            let (status_symbol, status_color) = status_display(&run.status.to_string());
            let duration = if let Some(ref ended) = run.ended_at {
                format_duration(&run.started_at, ended)
            } else {
                "…".to_string()
            };

            match row {
                WorkflowRunRow::Parent {
                    collapsed,
                    child_count,
                    ..
                } => {
                    // Prefix: collapse toggle indicator (only when there are children).
                    let prefix = if *child_count > 0 {
                        if *collapsed {
                            "▶ "
                        } else {
                            "▼ "
                        }
                    } else {
                        "  "
                    };

                    // In global mode, indent run rows under their target header.
                    let indent = if global_mode { "    " } else { "" };

                    let mut spans = vec![
                        Span::raw(format!("{indent}{prefix}")),
                        Span::styled(status_symbol, Style::default().fg(status_color)),
                        Span::raw("  "),
                        Span::styled(
                            format!("{:<20}", truncate(&run.workflow_name, 20)),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                    ];

                    // Show timestamp in both modes; the target context is now on the header row.
                    spans.push(Span::styled(
                        format!(
                            "  {}",
                            run.started_at
                                .get(..19)
                                .unwrap_or(&run.started_at)
                                .replace('T', " ")
                        ),
                        Style::default().fg(state.theme.label_secondary),
                    ));

                    spans.push(Span::styled(
                        format!("  {duration}"),
                        Style::default().fg(state.theme.label_accent),
                    ));

                    // Child count badge when collapsed.
                    if *collapsed && *child_count > 0 {
                        spans.push(Span::styled(
                            format!("  (+{child_count})"),
                            Style::default().fg(state.theme.label_secondary),
                        ));
                    }

                    if run.status == WorkflowRunStatus::Failed {
                        if let Some(ref summary) = run.result_summary {
                            let snippet = truncate(summary.lines().next().unwrap_or(""), 50);
                            spans.push(Span::styled(
                                format!("  {snippet}"),
                                Style::default().fg(state.theme.label_error),
                            ));
                        }
                    }

                    ListItem::new(Line::from(spans))
                }
                WorkflowRunRow::Child {
                    depth,
                    collapsed,
                    child_count,
                    ..
                } => {
                    let base_indent = if global_mode { "    " } else { "" };
                    let level_indent = "  ".repeat(*depth as usize);
                    let toggle = if *child_count > 0 {
                        if *collapsed {
                            "\u{25b6} " // ▶
                        } else {
                            "\u{25bc} " // ▼
                        }
                    } else {
                        "\u{2570} " // └
                    };
                    let mut spans = vec![
                        Span::raw(format!("{base_indent}{level_indent}")),
                        Span::styled(toggle, Style::default().fg(state.theme.label_secondary)),
                        Span::styled(status_symbol, Style::default().fg(status_color)),
                        Span::raw("  "),
                        Span::styled(
                            format!("{:<20}", truncate(&run.workflow_name, 20)),
                            Style::default()
                                .fg(state.theme.label_secondary)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("  {duration}"),
                            Style::default().fg(state.theme.label_accent),
                        ),
                    ];

                    if *collapsed && *child_count > 0 {
                        spans.push(Span::styled(
                            format!("  (+{child_count})"),
                            Style::default().fg(state.theme.label_secondary),
                        ));
                    }

                    if run.status == WorkflowRunStatus::Failed {
                        if let Some(ref summary) = run.result_summary {
                            let snippet = truncate(summary.lines().next().unwrap_or(""), 40);
                            spans.push(Span::styled(
                                format!("  {snippet}"),
                                Style::default().fg(state.theme.label_error),
                            ));
                        }
                    }

                    ListItem::new(Line::from(spans))
                }
                WorkflowRunRow::Step {
                    step_name,
                    status,
                    position,
                    depth,
                    ..
                } => {
                    let base_indent = if global_mode { "    " } else { "" };
                    let level_indent = "  ".repeat(*depth as usize);
                    let (status_symbol, status_color) = status_display(status);
                    ListItem::new(Line::from(vec![
                        Span::raw(format!("{base_indent}{level_indent}")),
                        Span::styled(
                            "\u{2570} ",
                            Style::default().fg(state.theme.label_secondary),
                        ),
                        Span::styled(
                            format!("{position}. "),
                            Style::default().fg(state.theme.label_secondary),
                        ),
                        Span::styled(status_symbol, Style::default().fg(status_color)),
                        Span::raw("  "),
                        Span::raw(step_name.clone()),
                    ]))
                }
                // Header arms already handled above; this branch is unreachable.
                WorkflowRunRow::RepoHeader { .. } | WorkflowRunRow::TargetHeader { .. } => {
                    ListItem::new(Line::from(vec![Span::raw("")]))
                }
            }
        })
        .collect();

    let runs_title = if global_mode {
        " All Workflow Runs (Space=expand/collapse) "
    } else {
        " Workflow Runs (Space=expand/collapse) "
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
                .bg(state.theme.highlight_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut list_state = ListState::default();
    if !visible.is_empty() {
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

    // Resolve worktree and ticket for the selected run (if any).
    let run_worktree = run_info.and_then(|run| {
        state
            .data
            .worktrees
            .iter()
            .find(|wt| Some(wt.id.as_str()) == run.worktree_id.as_deref())
    });
    let run_ticket = run_worktree.and_then(|wt| {
        wt.ticket_id
            .as_ref()
            .and_then(|tid| state.data.ticket_map.get(tid))
    });

    // Header height: 3 base lines + optional worktree lines (branch + path) + optional ticket line + 1 border
    let worktree_extra = if run_worktree.is_some() { 2 } else { 0 };
    let ticket_extra = if run_ticket.is_some() { 1 } else { 0 };
    let header_height = 3 + worktree_extra + ticket_extra + 1;

    // Header area + body
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(header_height as u16), Constraint::Min(0)])
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

        let mut header_lines = vec![Line::from(vec![
            Span::styled(
                " Workflow: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(
                run.workflow_name.clone(),
                Style::default()
                    .fg(state.theme.label_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(status_symbol, Style::default().fg(status_color)),
        ])];

        if let Some(wt) = run_worktree {
            header_lines.push(Line::from(vec![
                Span::styled(
                    " Branch:   ",
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::raw(wt.branch.clone()),
            ]));
            let display_path = match state.home_dir.as_deref() {
                Some(home) => wt.path.replacen(home, "~", 1),
                None => wt.path.clone(),
            };
            header_lines.push(Line::from(vec![
                Span::styled(
                    " Path:     ",
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::raw(display_path),
            ]));
        }

        if let Some(ticket) = run_ticket {
            header_lines.push(Line::from(vec![
                Span::styled(
                    " Ticket:   ",
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::styled(
                    format!("#{} — {}", ticket.source_id, ticket.title),
                    Style::default().fg(state.theme.group_header),
                ),
            ]));
        }

        header_lines.push(Line::from(vec![
            Span::styled(
                " Started:  ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::raw(started_display),
            if run.dry_run {
                Span::styled(
                    "  [dry-run]",
                    Style::default().fg(state.theme.label_warning),
                )
            } else {
                Span::raw("")
            },
        ]));
        if run.status == WorkflowRunStatus::Failed {
            header_lines.push(Line::from(vec![
                Span::styled(" Error:    ", Style::default().fg(state.theme.label_error)),
                Span::styled(
                    summary_display,
                    Style::default().fg(state.theme.label_error),
                ),
            ]));
        } else {
            header_lines.push(Line::from(vec![
                Span::styled(
                    " Summary:  ",
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::raw(summary_display),
            ]));
        }

        let header_block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(state.theme.border_inactive));
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
        state.theme.border_focused
    } else {
        state.theme.border_inactive
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
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::styled(status_symbol, Style::default().fg(status_color)),
                Span::raw("  "),
                Span::styled(
                    format!("{:<20}", step.step_name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  [{:<5}]", step.role),
                    Style::default().fg(state.theme.status_waiting),
                ),
                Span::styled(
                    format!("  {duration}"),
                    Style::default().fg(state.theme.label_accent),
                ),
            ];

            if step.iteration > 0 {
                spans.push(Span::styled(
                    format!("  iter:{}", step.iteration),
                    Style::default().fg(state.theme.label_accent),
                ));
            }
            if step.retry_count > 0 {
                spans.push(Span::styled(
                    format!("  retries:{}", step.retry_count),
                    Style::default().fg(state.theme.label_error),
                ));
            }
            if let Some(ref gate_type) = step.gate_type {
                spans.push(Span::styled(
                    format!("  gate:{gate_type}"),
                    Style::default().fg(state.theme.label_warning),
                ));
            }

            // Inline detail: show snippet of result/context/markers for non-selected steps
            if i != state.workflow_step_index {
                if let Some(ref rt) = step.result_text {
                    let snippet = truncate(rt.lines().next().unwrap_or(""), 40);
                    spans.push(Span::styled(
                        format!("  → {snippet}"),
                        Style::default().fg(state.theme.label_secondary),
                    ));
                } else if let Some(ref ctx) = step.context_out {
                    let snippet = truncate(ctx.lines().next().unwrap_or(""), 40);
                    spans.push(Span::styled(
                        format!("  ctx:{snippet}"),
                        Style::default().fg(state.theme.label_secondary),
                    ));
                }
            }

            if let Some(ref mk) = step.markers_out {
                spans.push(Span::styled(
                    format!("  [{mk}]"),
                    Style::default().fg(state.theme.label_accent),
                ));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let has_waiting_gate = state
        .data
        .workflow_steps
        .iter()
        .any(|s| s.status.to_string() == "waiting" && s.gate_type.is_some());

    let title = match (focused, has_waiting_gate) {
        (true, true) => " Steps (Enter=approve gate, Tab=switch) ",
        (true, false) => " Steps (Enter=detail, Tab=switch) ",
        (false, true) => " Steps (Enter=approve gate) ",
        (false, false) => " Steps ",
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
                .bg(state.theme.highlight_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

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
        state.theme.border_focused
    } else {
        state.theme.border_inactive
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
        let empty = Paragraph::new(Span::styled(
            msg,
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
        .or_else(|| {
            state.data.step_agent_run.as_ref().and_then(|run| {
                state
                    .data
                    .worktrees
                    .iter()
                    .find(|w| run.worktree_id.as_deref() == Some(w.id.as_str()))
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
            let summary = truncate(
                &shorten_paths(&ev.summary, worktree_path, state.home_dir.as_deref()),
                80,
            );
            let spans = vec![
                Span::styled(
                    format!("{ts} "),
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::styled(format!("{:<10}", ev.kind), style),
                Span::styled(dur, Style::default().fg(state.theme.label_secondary)),
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
                    .bg(state.theme.highlight_bg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");
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
