use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use conductor_core::github::GithubPr;

use super::helpers::{shorten_paths, visual_idx_with_headers};
use crate::state::{AppState, ColumnFocus, RepoDetailFocus, TreePosition, VisualRow};

fn pr_group_key(pr: &GithubPr) -> &'static str {
    if pr.is_draft {
        "Draft"
    } else {
        match pr.review_decision.as_deref() {
            Some("CHANGES_REQUESTED") => "Changes Requested",
            Some("APPROVED") => "Approved",
            _ => "Review Required",
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    super::workflow_column::render_with_workflow_column(frame, area, state, render_content);
}

fn render_content(frame: &mut Frame, area: Rect, state: &AppState) {
    let repo = state
        .selected_repo_id
        .as_ref()
        .and_then(|id| state.data.repos.iter().find(|r| &r.id == id));

    let Some(repo) = repo else {
        let msg = Paragraph::new("No repo selected").block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Repo Detail "),
        );
        frame.render_widget(msg, area);
        return;
    };

    // Worktrees pane: sized to content, capped at 1/3 of available height.
    let wt_height = (state.detail_worktrees.len() as u16 + 2)
        .max(3)
        .min(area.height / 3);

    // PRs pane: count visual rows (group headers + items), capped at 1/4 of height.
    let pr_visual_rows = {
        let mut count = 0u16;
        let mut prev_group = "";
        for pr in &state.detail_prs {
            let g = pr_group_key(pr);
            if g != prev_group {
                count += 1;
                prev_group = g;
            }
            count += 1;
        }
        count
    };
    let pr_height = (pr_visual_rows + 2).max(3).min(area.height / 4);

    // Repo Agent pane: show if there are any repo agent events or a latest run
    let has_repo_agent = state
        .selected_repo_id
        .as_ref()
        .and_then(|id| state.data.latest_repo_agent_runs.get(id))
        .is_some();
    let repo_agent_height = if has_repo_agent {
        // Status line + some events, capped at 1/4 of height
        let event_rows = state.data.repo_agent_activity_len() as u16;
        (event_rows + 4).max(5).min(area.height / 4)
    } else {
        3 // minimal empty pane
    };

    // Layout: Info | Worktrees | PRs | Tickets | RepoAgent
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),
            Constraint::Length(wt_height),
            Constraint::Length(pr_height),
            Constraint::Min(3),
            Constraint::Length(repo_agent_height),
        ])
        .split(area);

    // Repo info header
    let info_focused = state.column_focus == ColumnFocus::Content
        && state.repo_detail_focus == RepoDetailFocus::Info;
    let info_border_color = if info_focused {
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };
    let home_dir = dirs::home_dir();
    let home_str = home_dir.as_deref().and_then(|p| p.to_str());

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                "Repo:          ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(&repo.slug, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled(
                "Remote:        ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::raw(&repo.remote_url),
        ]),
        Line::from(vec![
            Span::styled(
                "Branch:        ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::raw(&repo.default_branch),
        ]),
        Line::from(vec![
            Span::styled(
                "Path:          ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::raw(shorten_paths(&repo.local_path, "", home_str)),
        ]),
        Line::from(vec![
            Span::styled(
                "Worktrees Dir: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::raw(shorten_paths(&repo.workspace_dir, "", home_str)),
        ]),
        Line::from(vec![
            Span::styled(
                "Model:         ",
                Style::default().fg(state.theme.label_secondary),
            ),
            match repo.model.as_deref() {
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
                "Agent Issues:  ",
                Style::default().fg(state.theme.label_secondary),
            ),
            if repo.allow_agent_issue_creation {
                Span::styled("Enabled", Style::default().fg(state.theme.status_completed))
            } else {
                Span::styled("Disabled", Style::default().fg(state.theme.label_secondary))
            },
            Span::styled(
                " (press Enter to toggle)",
                Style::default().fg(state.theme.label_secondary),
            ),
        ]),
    ];

    // Apply highlight to the focused row when info pane is focused
    if info_focused {
        let row = state.repo_detail_info_row;
        if let Some(line) = lines.get_mut(row) {
            *line = std::mem::take(line).style(Style::default().add_modifier(Modifier::REVERSED));
        }
    }

    let info = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(info_border_color))
            .title(" Info "),
    );

    frame.render_widget(info, layout[0]);

    // Scoped worktrees
    let wt_focused = state.column_focus == ColumnFocus::Content
        && state.repo_detail_focus == RepoDetailFocus::Worktrees;
    let wt_border = if wt_focused {
        Style::default().fg(state.theme.border_focused)
    } else {
        Style::default().fg(state.theme.border_inactive)
    };
    let wt_items: Vec<ListItem> = state
        .detail_worktrees
        .iter()
        .enumerate()
        .map(|(i, wt)| {
            let prefix = state
                .detail_wt_tree_positions
                .get(i)
                .map(|pos| pos.to_prefix())
                .unwrap_or_default();
            super::common::worktree_list_item_with_prefix(wt, state, None, true, &prefix)
        })
        .collect();

    let wt_list = List::new(wt_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(wt_border)
                .title(" Worktrees "),
        )
        .highlight_style(
            Style::default()
                .bg(state.theme.highlight_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut wt_state = ListState::default();
    if wt_focused && !state.detail_worktrees.is_empty() {
        wt_state.select(Some(state.detail_wt_index));
    }
    frame.render_stateful_widget(wt_list, layout[1], &mut wt_state);

    // Scoped tickets
    let ticket_focused = state.column_focus == ColumnFocus::Content
        && state.repo_detail_focus == RepoDetailFocus::Tickets;
    let ticket_border = if ticket_focused {
        Style::default().fg(state.theme.border_focused)
    } else {
        Style::default().fg(state.theme.border_inactive)
    };
    let detail_filter = state.detail_ticket_filter.as_query();

    // Compute column widths from the full (unfiltered) ticket list so widths
    // stay stable while the user types a filter query.
    const MAX_COL_WIDTH: usize = 20;
    let id_width = state
        .detail_tickets
        .iter()
        .map(|t| t.source_id.len())
        .max()
        .unwrap_or(4)
        .min(MAX_COL_WIDTH);
    let assignee_width = state
        .detail_tickets
        .iter()
        .map(|t| t.assignee.as_deref().unwrap_or("unclaimed").len())
        .max()
        .unwrap_or("unclaimed".len())
        .min(MAX_COL_WIDTH);

    let ticket_items: Vec<ListItem> = state
        .filtered_detail_tickets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let default_pos = TreePosition::default();
            let pos = state
                .detail_ticket_tree_positions
                .get(i)
                .unwrap_or(&default_pos);

            let is_parent = pos.is_parent;
            let is_collapsed = state.collapsed_ticket_ids.contains(&t.id);

            let is_blocked = state
                .data
                .ticket_dependencies
                .get(&t.id)
                .is_some_and(|d| d.is_actively_blocked());

            let tree_prefix = pos.to_prefix();
            let toggle_glyph = if is_parent {
                if is_collapsed {
                    "▶ "
                } else {
                    "▼ "
                }
            } else {
                ""
            };

            let mut spans: Vec<Span> = Vec::new();

            if !tree_prefix.is_empty() {
                spans.push(Span::styled(
                    tree_prefix,
                    Style::default().fg(state.theme.label_secondary),
                ));
            }
            if !toggle_glyph.is_empty() {
                spans.push(Span::raw(toggle_glyph));
            }
            if is_blocked {
                spans.push(Span::styled(
                    "⊘ ",
                    Style::default().fg(state.theme.label_error),
                ));
            }

            spans.push(super::common::ticket_worktree_dot_span(state, &t.id));
            let id_str = super::common::truncate(&t.source_id, id_width);
            spans.push(Span::styled(
                format!("#{:<width$} ", id_str, width = id_width),
                Style::default().fg(state.theme.group_header),
            ));

            match &t.assignee {
                Some(login) => {
                    let login_str = super::common::truncate(login, assignee_width - 1);
                    spans.push(Span::styled(
                        format!("@{:<width$} ", login_str, width = assignee_width - 1),
                        Style::default().fg(state.theme.label_secondary),
                    ));
                }
                None => {
                    spans.push(Span::styled(
                        format!("{:<width$} ", "unclaimed", width = assignee_width),
                        Style::default().add_modifier(Modifier::DIM),
                    ));
                }
            }
            spans.push(Span::raw(&t.title));
            let labels = state
                .data
                .ticket_labels
                .get(&t.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            spans.extend(super::common::ticket_label_spans_compact(
                labels,
                &state.theme,
            ));
            spans.extend(super::common::ticket_agent_total_spans(
                state, &t.id, "  ", false,
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let hiding = !state.show_closed_tickets;
    let ticket_title = match detail_filter.as_deref() {
        Some(f) if !f.is_empty() => {
            if hiding {
                format!(" Tickets (filter: {f}, hiding closed) ")
            } else {
                format!(" Tickets (filter: {f}) ")
            }
        }
        _ => {
            if hiding {
                " Tickets (hiding closed) ".to_string()
            } else {
                " Tickets ".to_string()
            }
        }
    };

    let ticket_list = List::new(ticket_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(ticket_border)
                .title(ticket_title),
        )
        .highlight_style(
            Style::default()
                .bg(state.theme.highlight_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut ticket_state = ListState::default();
    if ticket_focused && !state.filtered_detail_tickets.is_empty() {
        ticket_state.select(Some(state.detail_ticket_index));
    }
    frame.render_stateful_widget(ticket_list, layout[3], &mut ticket_state);

    // PRs pane
    let pr_focused = state.column_focus == ColumnFocus::Content
        && state.repo_detail_focus == RepoDetailFocus::Prs;
    let pr_border = if pr_focused {
        Style::default().fg(state.theme.border_focused)
    } else {
        Style::default().fg(state.theme.border_inactive)
    };

    let pr_items: Vec<ListItem> = if state.detail_prs.is_empty() {
        let placeholder = if state.pr_last_fetched_at.is_some() {
            "(no open PRs)"
        } else {
            "(loading\u{2026})"
        };
        vec![ListItem::new(Line::from(Span::styled(
            placeholder,
            Style::default().fg(state.theme.label_secondary),
        )))]
    } else {
        let mut items: Vec<ListItem> = Vec::new();
        let mut prev_group = "";
        for pr in &state.detail_prs {
            let group = pr_group_key(pr);
            if group != prev_group {
                items.push(ListItem::new(Line::from(Span::styled(
                    group,
                    Style::default()
                        .fg(state.theme.label_secondary)
                        .add_modifier(Modifier::BOLD),
                ))));
                prev_group = group;
            }
            let (badge_text, badge_color) = match group {
                "Changes Requested" => ("[changes requested]", state.theme.status_failed),
                "Approved" => ("[approved]", state.theme.status_completed),
                "Draft" => ("[draft]", state.theme.label_secondary),
                _ => ("[review required]", state.theme.label_warning),
            };
            let branch = &pr.head_ref_name;
            let branch_display = if branch.chars().count() > 30 {
                format!(
                    "{}\u{2026}",
                    &branch[..branch
                        .char_indices()
                        .nth(30)
                        .map(|(i, _)| i)
                        .unwrap_or(branch.len())]
                )
            } else {
                branch.clone()
            };
            let spans = vec![
                Span::raw("\u{2514} "),
                Span::styled(
                    format!("#{} ", pr.number),
                    Style::default().fg(state.theme.group_header),
                ),
                Span::raw(&pr.title),
                Span::raw("  "),
                Span::styled(badge_text, Style::default().fg(badge_color)),
                Span::styled(
                    format!("  {branch_display}"),
                    Style::default().fg(state.theme.label_secondary),
                ),
            ];
            items.push(ListItem::new(Line::from(spans)));
        }
        items
    };

    let pr_title = if pr_focused && !state.detail_prs.is_empty() {
        " PRs  r:run workflow "
    } else {
        " PRs "
    };

    let pr_list = List::new(pr_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(pr_border)
                .title(pr_title),
        )
        .highlight_style(
            Style::default()
                .bg(state.theme.highlight_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut pr_list_state = ListState::default();
    if pr_focused && !state.detail_prs.is_empty() {
        let visual_idx = visual_idx_with_headers(
            &state.detail_prs,
            pr_group_key,
            state
                .detail_pr_index
                .min(state.detail_prs.len().saturating_sub(1)),
        );
        pr_list_state.select(Some(visual_idx));
    }
    frame.render_stateful_widget(pr_list, layout[2], &mut pr_list_state);

    // Repo Agent pane
    render_repo_agent_pane(frame, layout[4], state);
}

fn event_style(kind: &str, theme: &crate::theme::Theme) -> Style {
    match kind {
        "text" => Style::default().fg(theme.label_primary),
        "tool" => Style::default().fg(theme.label_warning),
        "result" => Style::default().fg(theme.status_completed),
        "system" => Style::default().fg(theme.label_secondary),
        "error" | "tool_error" => Style::default().fg(theme.status_failed),
        "prompt" => Style::default().fg(theme.label_info),
        _ => Style::default(),
    }
}

fn render_repo_agent_pane(frame: &mut Frame, area: Rect, state: &AppState) {
    let agent_focused = state.column_focus == ColumnFocus::Content
        && state.repo_detail_focus == RepoDetailFocus::RepoAgent;
    let border_color = if agent_focused {
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };

    let latest_run = state
        .selected_repo_id
        .as_ref()
        .and_then(|id| state.data.latest_repo_agent_runs.get(id));

    // Build title with action hints when focused
    let title = if agent_focused {
        if let Some(run) = latest_run {
            use conductor_core::agent::AgentRunStatus;
            match run.status {
                AgentRunStatus::Running => " Repo Agent  p=prompt x=stop ",
                AgentRunStatus::WaitingForFeedback => {
                    " Repo Agent  p=prompt f=respond F=dismiss x=stop "
                }
                _ => " Repo Agent  p=prompt ",
            }
        } else {
            " Repo Agent  p=prompt "
        }
    } else {
        " Repo Agent "
    };

    let activity_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(title);

    // If no run exists, show empty placeholder
    let Some(run) = latest_run else {
        let empty = Paragraph::new(Span::styled(
            "No repo agent activity — press p to prompt",
            Style::default().fg(state.theme.label_secondary),
        ))
        .block(activity_block);
        frame.render_widget(empty, area);
        return;
    };

    // Split into status line (top) + event list (bottom)
    let inner = activity_block.inner(area);
    frame.render_widget(activity_block, area);

    if inner.height < 2 {
        return;
    }

    let pane_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    // Status line
    let status_line = render_repo_agent_status(run, &state.theme);
    frame.render_widget(Paragraph::new(status_line), pane_layout[0]);

    // Event list
    let events = &state.data.repo_agent_events;
    if events.is_empty() {
        let empty = Paragraph::new(Span::styled(
            "No events yet",
            Style::default().fg(state.theme.label_secondary),
        ));
        frame.render_widget(empty, pane_layout[1]);
        return;
    }

    let repo_path = state
        .selected_repo_id
        .as_ref()
        .and_then(|id| state.data.repos.iter().find(|r| &r.id == id))
        .map(|r| r.local_path.as_str())
        .unwrap_or("");

    let mut items: Vec<ListItem> = Vec::new();
    for row in state.data.repo_agent_visual_rows() {
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
                let style = event_style(&ev.kind, &state.theme);
                let display_text = shorten_paths(&ev.summary, repo_path, state.home_dir.as_deref());
                let mut spans = vec![Span::styled(display_text, style)];
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

    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(
        list,
        pane_layout[1],
        &mut state.repo_agent_list_state.borrow_mut(),
    );
}

fn render_repo_agent_status(
    run: &conductor_core::agent::AgentRun,
    theme: &crate::theme::Theme,
) -> Line<'static> {
    use conductor_core::agent::AgentRunStatus;
    match run.status {
        AgentRunStatus::Running => {
            let turns = run.num_turns.unwrap_or(0);
            let elapsed_ms = chrono::DateTime::parse_from_rfc3339(&run.started_at)
                .ok()
                .map(|start| {
                    (chrono::Utc::now() - start.with_timezone(&chrono::Utc))
                        .num_milliseconds()
                        .max(0)
                });
            let dur_secs = elapsed_ms.unwrap_or(0) as f64 / 1000.0;
            Line::from(vec![
                Span::styled("Agent: ", Style::default().fg(theme.label_secondary)),
                Span::styled("[running]", Style::default().fg(theme.status_running)),
                Span::styled(
                    format!(" {turns} turns · {dur_secs:.1}s"),
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
        ]),
        AgentRunStatus::Completed => {
            let turns = run.num_turns.unwrap_or(0);
            let dur_secs = run.duration_ms.unwrap_or(0) as f64 / 1000.0;
            Line::from(vec![
                Span::styled("Agent: ", Style::default().fg(theme.label_secondary)),
                Span::styled("[completed]", Style::default().fg(theme.status_completed)),
                Span::styled(
                    format!(" {turns} turns · {dur_secs:.1}s"),
                    Style::default().fg(theme.label_secondary),
                ),
            ])
        }
        AgentRunStatus::Failed => {
            let mut spans = vec![
                Span::styled("Agent: ", Style::default().fg(theme.label_secondary)),
                Span::styled("[failed]", Style::default().fg(theme.status_failed)),
            ];
            if let Some(ref err) = run.result_text {
                let truncated: String = err.chars().take(60).collect();
                spans.push(Span::styled(
                    format!(" {truncated}"),
                    Style::default().fg(theme.label_secondary),
                ));
            }
            Line::from(spans)
        }
        AgentRunStatus::Cancelled => Line::from(vec![
            Span::styled("Agent: ", Style::default().fg(theme.label_secondary)),
            Span::styled("[cancelled]", Style::default().fg(theme.status_cancelled)),
        ]),
    }
}
