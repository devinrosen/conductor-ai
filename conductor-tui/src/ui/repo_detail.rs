use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::helpers::shorten_paths;
use crate::state::{AppState, RepoDetailFocus};

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
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

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(area);

    // Repo info header
    let info_focused = state.repo_detail_focus == RepoDetailFocus::Info;
    let info_border_color = if info_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let home_dir = dirs::home_dir();
    let home_str = home_dir.as_deref().and_then(|p| p.to_str());

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("Repo:          ", Style::default().fg(Color::DarkGray)),
            Span::styled(&repo.slug, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("Remote:        ", Style::default().fg(Color::DarkGray)),
            Span::raw(&repo.remote_url),
        ]),
        Line::from(vec![
            Span::styled("Branch:        ", Style::default().fg(Color::DarkGray)),
            Span::raw(&repo.default_branch),
        ]),
        Line::from(vec![
            Span::styled("Path:          ", Style::default().fg(Color::DarkGray)),
            Span::raw(shorten_paths(&repo.local_path, "", home_str)),
        ]),
        Line::from(vec![
            Span::styled("Worktrees Dir: ", Style::default().fg(Color::DarkGray)),
            Span::raw(shorten_paths(&repo.workspace_dir, "", home_str)),
        ]),
        Line::from(vec![
            Span::styled("Model:         ", Style::default().fg(Color::DarkGray)),
            match repo.model.as_deref() {
                Some(m) => Span::raw(m.to_string()),
                None => Span::styled("(not set)", Style::default().fg(Color::DarkGray)),
            },
            Span::styled(
                " (press Enter to change)",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("Agent Issues:  ", Style::default().fg(Color::DarkGray)),
            if repo.allow_agent_issue_creation {
                Span::styled("Enabled", Style::default().fg(Color::Green))
            } else {
                Span::styled("Disabled", Style::default().fg(Color::DarkGray))
            },
            Span::styled(
                " (press Enter to toggle)",
                Style::default().fg(Color::DarkGray),
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
    let wt_focused = state.repo_detail_focus == RepoDetailFocus::Worktrees;
    let wt_border = if wt_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
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
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut wt_state = ListState::default();
    if wt_focused && !state.detail_worktrees.is_empty() {
        wt_state.select(Some(state.detail_wt_index));
    }
    frame.render_stateful_widget(wt_list, layout[1], &mut wt_state);

    // Bottom row: horizontal 50/50 split — Tickets (left) | PRs (right)
    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(layout[2]);

    // Scoped tickets
    let ticket_focused = state.repo_detail_focus == RepoDetailFocus::Tickets;
    let ticket_border = if ticket_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
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
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(&t.title),
            ];
            let labels = state
                .data
                .ticket_labels
                .get(&t.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            spans.extend(super::common::ticket_label_spans_compact(labels));
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
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut ticket_state = ListState::default();
    if ticket_focused && !state.filtered_detail_tickets.is_empty() {
        ticket_state.select(Some(state.detail_ticket_index));
    }
    frame.render_stateful_widget(ticket_list, bottom[0], &mut ticket_state);

    // PRs pane
    let pr_focused = state.repo_detail_focus == RepoDetailFocus::Prs;
    let pr_border = if pr_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let pr_items: Vec<ListItem> = if state.detail_prs.is_empty() {
        let placeholder = if state.pr_last_fetched_at.is_some() {
            "(no open PRs)"
        } else {
            "(loading\u{2026})"
        };
        vec![ListItem::new(Line::from(Span::styled(
            placeholder,
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        state
            .detail_prs
            .iter()
            .map(|pr| {
                let state_color = if pr.state.eq_ignore_ascii_case("open") {
                    Color::Green
                } else {
                    Color::White
                };
                let spans = vec![
                    Span::styled(
                        format!("#{} ", pr.number),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(&pr.title),
                    Span::raw("  "),
                    Span::styled(format!("[{}]", pr.state), Style::default().fg(state_color)),
                    Span::styled(
                        format!("  @{}", pr.author),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("  {}", pr.head_ref_name),
                        Style::default().fg(Color::DarkGray),
                    ),
                ];
                ListItem::new(Line::from(spans))
            })
            .collect()
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
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut pr_list_state = ListState::default();
    if pr_focused && !state.detail_prs.is_empty() {
        pr_list_state.select(Some(state.detail_pr_index));
    }
    frame.render_stateful_widget(pr_list, bottom[1], &mut pr_list_state);
}
