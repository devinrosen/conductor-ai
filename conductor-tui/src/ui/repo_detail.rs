use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use conductor_core::github::GithubPr;

use super::helpers::{shorten_paths, visual_idx_with_headers};
use crate::state::{AppState, ColumnFocus, RepoDetailFocus};

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

    // Gates pane: sized to content, capped at 1/4 of available height.
    let gate_height = (state.detail_gates.len() as u16 + 2)
        .max(3)
        .min(area.height / 4);

    // Layout: Info | Worktrees | PRs | Gates | Tickets
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),
            Constraint::Length(wt_height),
            Constraint::Length(pr_height),
            Constraint::Length(gate_height),
            Constraint::Min(0),
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
        .map(|wt| super::common::worktree_list_item(wt, state, None, true))
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
    let ticket_items: Vec<ListItem> = state
        .filtered_detail_tickets
        .iter()
        .map(|t| {
            let mut spans = vec![
                super::common::ticket_worktree_dot_span(state, &t.id),
                Span::styled(
                    format!("#{} ", t.source_id),
                    Style::default().fg(state.theme.group_header),
                ),
                Span::raw(&t.title),
            ];
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
    frame.render_stateful_widget(ticket_list, layout[4], &mut ticket_state);

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

    // Pending Gates pane
    let gates_focused = state.column_focus == ColumnFocus::Content
        && state.repo_detail_focus == RepoDetailFocus::Gates;
    let gates_border = if gates_focused {
        Style::default().fg(state.theme.border_focused)
    } else {
        Style::default().fg(state.theme.border_inactive)
    };

    let gates_items: Vec<ListItem> = if state.detail_gates.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "(no pending gates)",
            Style::default().fg(state.theme.label_secondary),
        )))]
    } else {
        state
            .detail_gates
            .iter()
            .map(|(step, workflow_name, _target_label)| {
                let gate_type = step.gate_type.as_deref().unwrap_or("gate");
                let prompt = step.gate_prompt.as_deref().unwrap_or("");
                let prompt_display = if prompt.chars().count() > 40 {
                    format!(
                        "{}\u{2026}",
                        &prompt[..prompt
                            .char_indices()
                            .nth(40)
                            .map(|(i, _)| i)
                            .unwrap_or(prompt.len())]
                    )
                } else {
                    prompt.to_string()
                };
                let spans = vec![
                    Span::styled(
                        format!("[{gate_type}]"),
                        Style::default().fg(state.theme.label_secondary),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        workflow_name.as_str(),
                        Style::default().fg(state.theme.group_header),
                    ),
                    Span::raw("  "),
                    Span::raw(&step.step_name),
                    Span::raw("  "),
                    Span::styled(
                        prompt_display,
                        Style::default().fg(state.theme.label_secondary),
                    ),
                ];
                ListItem::new(Line::from(spans))
            })
            .collect()
    };

    let gates_title = if gates_focused && !state.detail_gates.is_empty() {
        " Pending Gates  Enter:review "
    } else {
        " Pending Gates "
    };

    let gates_list = List::new(gates_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(gates_border)
                .title(gates_title),
        )
        .highlight_style(
            Style::default()
                .bg(state.theme.highlight_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut gates_list_state = ListState::default();
    if gates_focused && !state.detail_gates.is_empty() {
        gates_list_state.select(Some(
            state
                .detail_gate_index
                .min(state.detail_gates.len().saturating_sub(1)),
        ));
    }
    frame.render_stateful_widget(gates_list, layout[3], &mut gates_list_state);
}
