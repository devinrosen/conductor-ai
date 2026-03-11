use ratatui::layout::{Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use tui_textarea::TextArea;

use conductor_core::agent::TicketAgentTotals;
use conductor_core::github::DiscoveredRepo;
use conductor_core::issue_source::IssueSource;
use conductor_core::tickets::{Ticket, TicketLabel};
use conductor_core::worktree::Worktree;

pub fn render_confirm(frame: &mut Frame, area: Rect, title: &str, message: &str) {
    let popup = centered_rect(50, 30, area);
    frame.render_widget(Clear, popup);

    let content = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::raw(message)),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  y",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("/Enter = confirm    "),
            Span::styled(
                "n",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" = cancel"),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(format!(" {title} ")),
    );

    frame.render_widget(content, popup);
}

pub fn render_confirm_by_name(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    message: &str,
    expected: &str,
    value: &str,
) {
    let popup = centered_rect(55, 40, area);
    frame.render_widget(Clear, popup);

    let matches = value == expected;
    let border_color = if matches { Color::Green } else { Color::Red };
    let input_style = if matches {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::UNDERLINED)
    };

    let content = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(message, Style::default().fg(Color::Yellow))),
        Line::from(""),
        Line::from(vec![
            Span::raw("  Type "),
            Span::styled(
                expected,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" to confirm:"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::styled(value, input_style),
            Span::styled("_", Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(if matches {
            vec![
                Span::styled(
                    "  Enter",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" = confirm    "),
                Span::styled(
                    "Esc",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" = cancel"),
            ]
        } else {
            vec![Span::styled(
                "  Esc to cancel",
                Style::default().fg(Color::DarkGray),
            )]
        }),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(format!(" {title} ")),
    )
    .wrap(Wrap { trim: false });

    frame.render_widget(content, popup);
}

pub fn render_input(frame: &mut Frame, area: Rect, title: &str, prompt: &str, value: &str) {
    let popup = centered_rect(50, 30, area);
    frame.render_widget(Clear, popup);

    let content = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::raw(prompt)),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::styled(value, Style::default().add_modifier(Modifier::UNDERLINED)),
            Span::styled("_", Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Enter to submit, Esc to cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(format!(" {title} ")),
    );

    frame.render_widget(content, popup);
}

pub fn render_error(frame: &mut Frame, area: Rect, message: &str) {
    let popup = centered_rect(50, 25, area);
    frame.render_widget(Clear, popup);

    let content = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(message, Style::default().fg(Color::Red))),
        Line::from(""),
        Line::from(Span::styled(
            "  Press Esc or Enter to dismiss",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red))
            .title(" Error "),
    );

    frame.render_widget(content, popup);
}

pub fn render_progress(frame: &mut Frame, area: Rect, message: &str) {
    let popup = centered_rect(50, 25, area);
    frame.render_widget(Clear, popup);

    let content = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(message, Style::default().fg(Color::White))),
        Line::from(""),
        Line::from(Span::styled(
            "  Please wait…",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" In Progress "),
    );

    frame.render_widget(content, popup);
}

pub fn render_ticket_info(
    frame: &mut Frame,
    area: Rect,
    ticket: &Ticket,
    agent_totals: Option<&TicketAgentTotals>,
    worktrees: Option<&Vec<Worktree>>,
    labels: Option<&[TicketLabel]>,
) {
    let popup = centered_rect(60, 70, area);
    frame.render_widget(Clear, popup);

    let label_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let value_style = Style::default().fg(Color::White);
    let dim_style = Style::default().fg(Color::DarkGray);

    let state_color = match ticket.state.as_str() {
        "open" => Color::Green,
        "closed" => Color::Red,
        _ => Color::Yellow,
    };

    let body_text = if ticket.body.is_empty() {
        "(no description)".to_string()
    } else if ticket.body.chars().count() > 500 {
        let s: String = ticket.body.chars().take(500).collect();
        format!("{s}...")
    } else {
        ticket.body.clone()
    };

    let assignee_text = ticket.assignee.as_deref().unwrap_or("unassigned");

    // Build the labels line — colored badge chips if rich label data is available,
    // otherwise fall back to the raw comma-separated string.
    let labels_line = {
        let mut spans: Vec<Span<'static>> = vec![Span::styled("  Labels:    ", label_style)];
        let rich = labels.unwrap_or(&[]);
        if rich.is_empty() && ticket.labels.is_empty() {
            spans.push(Span::styled("none", value_style));
        } else if !rich.is_empty() {
            let mut shown = 0usize;
            for lbl in rich.iter().take(5) {
                let bg = lbl
                    .color
                    .as_deref()
                    .map(super::common::hex_to_color)
                    .unwrap_or(Color::DarkGray);
                let fg = super::common::label_fg(bg);
                if shown > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    format!(" {} ", lbl.label.clone()),
                    Style::default().fg(fg).bg(bg),
                ));
                shown += 1;
            }
            let remaining = rich.len().saturating_sub(shown);
            if remaining > 0 {
                spans.push(Span::styled(
                    format!(" +{remaining}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        } else {
            spans.push(Span::styled(ticket.labels.clone(), value_style));
        }
        Line::from(spans)
    };

    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  State:     ", label_style),
            Span::styled(
                format!("[{}]", ticket.state),
                Style::default()
                    .fg(state_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Source:    ", label_style),
            Span::styled(
                format!("#{} ({})", ticket.source_id, ticket.source_type),
                value_style,
            ),
        ]),
        Line::from(vec![
            Span::styled("  Assignee:  ", label_style),
            Span::styled(assignee_text, value_style),
        ]),
        labels_line,
        Line::from(vec![
            Span::styled("  URL:       ", label_style),
            Span::styled(&ticket.url, Style::default().fg(Color::Blue)),
        ]),
        Line::from(""),
    ];

    if let Some(totals) = agent_totals {
        let dur_secs = totals.total_duration_ms as f64 / 1000.0;
        let mins = (dur_secs / 60.0) as i64;
        let secs = (dur_secs % 60.0) as i64;
        lines.push(Line::from(Span::styled("  Agent Totals:", label_style)));
        let fmt_k = |n: i64| -> String {
            if n >= 1000 {
                format!("{:.1}k", n as f64 / 1000.0)
            } else {
                n.to_string()
            }
        };
        lines.push(Line::from(vec![
            Span::styled("    Tokens:  ", dim_style),
            Span::styled(
                format!(
                    "{}↓ {}↑",
                    fmt_k(totals.total_input_tokens),
                    fmt_k(totals.total_output_tokens)
                ),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled("   Turns: ", dim_style),
            Span::styled(
                format!("{}", totals.total_turns),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled("   Time: ", dim_style),
            Span::styled(
                format!("{}m{:02}s", mins, secs),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled("   Runs: ", dim_style),
            Span::styled(
                format!("{}", totals.total_runs),
                Style::default().fg(Color::Magenta),
            ),
        ]));
        lines.push(Line::from(""));
    }

    if let Some(wts) = worktrees {
        if !wts.is_empty() {
            lines.push(Line::from(Span::styled("  Worktrees:", label_style)));
            for wt in wts {
                let (indicator, color) = if wt.is_active() {
                    ("●", Color::Green)
                } else {
                    ("○", Color::DarkGray)
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("    {indicator} "), Style::default().fg(color)),
                    Span::styled(&wt.slug, value_style),
                    Span::styled(format!("  [{}]", wt.status), Style::default().fg(color)),
                    Span::styled(format!("  {}", wt.branch), dim_style),
                ]));
            }
            lines.push(Line::from(""));
        }
    }

    lines.push(Line::from(Span::styled("  Description:", label_style)));

    // Add body lines with word wrapping (indented)
    for body_line in body_text.lines() {
        lines.push(Line::from(Span::styled(
            format!("  {body_line}"),
            dim_style,
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "  o",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" = open in browser    ", dim_style),
        Span::styled(
            "Esc",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" = close", dim_style),
    ]));

    let title = format!(" #{} {} ", ticket.source_id, ticket.title);
    let title_display = if title.len() > (popup.width as usize).saturating_sub(2) {
        format!("{}...", &title[..popup.width as usize - 5])
    } else {
        title
    };

    let content = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title_display),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(content, popup);
}

pub fn render_form(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    fields: &[crate::state::FormField],
    active_field: usize,
) {
    let popup = centered_rect(60, 50, area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![Line::from("")];

    for (i, field) in fields.iter().enumerate() {
        let is_active = i == active_field;
        let label_style = if is_active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        // Build label with required indicator and auto hint
        let required_mark = if field.required { "*" } else { "" };
        let auto_hint = if !field.required && !field.manually_edited {
            " (auto)"
        } else {
            ""
        };
        let label_text = format!("  {}{}{}", field.label, required_mark, auto_hint);
        lines.push(Line::from(Span::styled(label_text, label_style)));

        // Value line
        if is_active {
            lines.push(Line::from(vec![
                Span::styled("  > ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    &field.value,
                    Style::default().add_modifier(Modifier::UNDERLINED),
                ),
                Span::styled("_", Style::default().fg(Color::Cyan)),
            ]));
        } else if field.value.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("    {}", field.placeholder),
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            lines.push(Line::from(Span::raw(format!("    {}", field.value))));
        }

        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        "  Tab next field  Enter submit  Esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    let content = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(format!(" {title} ")),
    );

    frame.render_widget(content, popup);
}

pub fn render_post_create_picker(
    frame: &mut Frame,
    area: Rect,
    items: &[crate::state::PostCreateChoice],
    selected: usize,
    ticket_source_id: &str,
) {
    let height = (items.len() as u16 + 6).min(20);
    let percent_y = ((height as f32 / area.height as f32) * 100.0) as u16;
    let popup = centered_rect(50, percent_y.max(25), area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Start work on #{ticket_source_id}?"),
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
    ];

    for (i, item) in items.iter().enumerate() {
        let is_selected = i == selected;
        let prefix = if is_selected { "▸ " } else { "  " };
        let number = format!("{}. ", i + 1);

        let style = if is_selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {prefix}{number}"), style),
            Span::styled(format!("{item}"), style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  1-9 select  Enter confirm  Esc skip",
        Style::default().fg(Color::DarkGray),
    )));

    let content = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Post-Create Actions "),
    );

    frame.render_widget(content, popup);
}

pub fn render_pr_workflow_picker(
    frame: &mut Frame,
    area: Rect,
    pr_number: i64,
    pr_title: &str,
    workflow_defs: &[conductor_core::workflow::WorkflowDef],
    selected: usize,
) {
    let height = (workflow_defs.len() as u16 + 7).min(25);
    let percent_y = ((height as f32 / area.height as f32) * 100.0) as u16;
    let popup = centered_rect(60, percent_y.max(25), area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Run workflow on PR #{pr_number}: {pr_title}"),
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
    ];

    for (i, def) in workflow_defs.iter().enumerate() {
        let is_selected = i == selected;
        let prefix = if is_selected { "▸ " } else { "  " };

        let style = if is_selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        let mut row = vec![
            Span::styled(format!("  {prefix}"), style),
            Span::styled(&def.name, style),
        ];
        if !def.description.is_empty() {
            row.push(Span::styled(
                format!("  — {}", def.description),
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(row));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Enter confirm  Esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    let content = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Run Workflow on PR "),
    );

    frame.render_widget(content, popup);
}

pub fn render_workflow_picker(
    frame: &mut Frame,
    area: Rect,
    target: &crate::state::WorkflowPickerTarget,
    workflow_defs: &[conductor_core::workflow::WorkflowDef],
    selected: usize,
) {
    use crate::state::WorkflowPickerTarget;

    let (title, subtitle) = match target {
        WorkflowPickerTarget::Worktree { worktree_path, .. } => {
            let short = worktree_path.rsplit('/').next().unwrap_or(worktree_path);
            (
                " Run Workflow ".to_string(),
                format!("  on worktree: {short}"),
            )
        }
        WorkflowPickerTarget::Pr {
            pr_number,
            pr_title,
        } => (
            " Run Workflow on PR ".to_string(),
            format!("  PR #{pr_number}: {pr_title}"),
        ),
        WorkflowPickerTarget::Ticket {
            ticket_title,
            ticket_id,
            ..
        } => (
            " Run Workflow on Ticket ".to_string(),
            format!("  {ticket_title} ({ticket_id})"),
        ),
        WorkflowPickerTarget::Repo { repo_name, .. } => (
            " Run Workflow on Repo ".to_string(),
            format!("  {repo_name}"),
        ),
        WorkflowPickerTarget::WorkflowRun {
            workflow_name,
            workflow_run_id,
            ..
        } => {
            let short_id = &workflow_run_id[..8.min(workflow_run_id.len())];
            (
                " Run Workflow on Run ".to_string(),
                format!("  {workflow_name} ({short_id}…)"),
            )
        }
    };

    let height = (workflow_defs.len() as u16 + 7).min(25);
    let percent_y = ((height as f32 / area.height as f32) * 100.0) as u16;
    let popup = centered_rect(60, percent_y.max(25), area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(subtitle, Style::default().fg(Color::Cyan))),
        Line::from(""),
    ];

    for (i, def) in workflow_defs.iter().enumerate() {
        let is_selected = i == selected;
        let prefix = if is_selected { "▸ " } else { "  " };

        let style = if is_selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        let mut row = vec![
            Span::styled(format!("  {prefix}"), style),
            Span::styled(&def.name, style),
        ];
        if !def.description.is_empty() {
            row.push(Span::styled(
                format!("  — {}", def.description),
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(row));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Enter confirm  Esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    let content = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(title),
    );

    frame.render_widget(content, popup);
}

pub fn render_agent_prompt(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    prompt: &str,
    textarea: &TextArea<'_>,
) {
    let popup = centered_rect(70, 50, area);
    frame.render_widget(Clear, popup);

    // Outer block for the modal border
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" {title} "));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Split inner area: prompt line + textarea + hint line
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // prompt text
            Constraint::Min(3),    // textarea
            Constraint::Length(1), // hint
        ])
        .split(inner);

    // Prompt label
    let prompt_widget = Paragraph::new(vec![
        Line::from(Span::styled(
            format!(" {prompt}"),
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
    ]);
    frame.render_widget(prompt_widget, chunks[0]);

    // Textarea (renders itself with cursor)
    frame.render_widget(textarea, chunks[1]);

    // Hint line
    let hint = Paragraph::new(Line::from(Span::styled(
        " Enter for newline, Ctrl+S to submit, Ctrl+D to clear, Esc to cancel",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(hint, chunks[2]);
}

#[allow(clippy::too_many_arguments)]
pub fn render_model_picker(
    frame: &mut Frame,
    area: Rect,
    context_label: &str,
    effective_default: Option<&str>,
    effective_source: &str,
    selected: usize,
    custom_input: &str,
    custom_active: bool,
    suggested: Option<&str>,
) {
    use conductor_core::models::KNOWN_MODELS;

    let popup = centered_rect(55, 55, area);
    frame.render_widget(Clear, popup);

    let dim = Style::default().fg(Color::DarkGray);
    let cyan_bold = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let mut lines = vec![Line::from("")];

    // Context label
    lines.push(Line::from(Span::styled(
        format!("  {context_label}"),
        cyan_bold,
    )));
    lines.push(Line::from(""));

    // Effective default display
    let default_display = match effective_default {
        Some(m) => format!("Using: {m}  (from {effective_source})"),
        None => format!("Using: claude default  ({effective_source})"),
    };
    lines.push(Line::from(Span::styled(
        format!("  {default_display}"),
        Style::default().fg(Color::Yellow),
    )));
    lines.push(Line::from(Span::styled(
        "         \u{2191} override with:",
        dim,
    )));
    lines.push(Line::from(""));

    // Known models list
    for (i, model) in KNOWN_MODELS.iter().enumerate() {
        let is_selected = !custom_active && i == selected;
        let is_current = effective_default.is_some_and(|d| d == model.id || d == model.alias);

        let prefix = if is_selected { "\u{25b8} " } else { "  " };

        let current_marker = if is_current { " (current)" } else { "" };

        let suggested_marker = if suggested == Some(model.alias) && !is_current {
            " [Suggested]"
        } else {
            ""
        };

        let style = if is_selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {prefix}"), style),
            Span::styled(
                format!("{} ", model.tier_stars()),
                Style::default().fg(match model.tier {
                    conductor_core::models::ModelTier::Powerful => Color::Magenta,
                    conductor_core::models::ModelTier::Balanced => Color::Cyan,
                    conductor_core::models::ModelTier::Fast => Color::Green,
                }),
            ),
            Span::styled(format!("{:<7}", model.alias), style),
            Span::styled(format!(" \u{2014} {}", model.description), dim),
            Span::styled(current_marker, Style::default().fg(Color::DarkGray)),
            Span::styled(
                suggested_marker,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // Custom input option
    let custom_idx = KNOWN_MODELS.len();
    let custom_selected = !custom_active && selected == custom_idx;
    let custom_prefix = if custom_selected || custom_active {
        "\u{25b8} "
    } else {
        "  "
    };
    let custom_style = if custom_selected || custom_active {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    if custom_active {
        lines.push(Line::from(vec![
            Span::styled(format!("  {custom_prefix}"), custom_style),
            Span::styled("custom: ", custom_style),
            Span::styled(
                custom_input,
                Style::default().add_modifier(Modifier::UNDERLINED),
            ),
            Span::styled("_", Style::default().fg(Color::Cyan)),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled(format!("  {custom_prefix}"), custom_style),
            Span::styled("custom\u{2026}", custom_style),
        ]));
    }

    lines.push(Line::from(""));

    // Clear option
    lines.push(Line::from(Span::styled(
        "  (Backspace to clear model override)",
        dim,
    )));

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "  j/k",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" navigate  ", dim),
        Span::styled(
            "Enter",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" select  ", dim),
        Span::styled(
            "Esc",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" cancel", dim),
    ]));

    let content = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Model Picker "),
    );

    frame.render_widget(content, popup);
}

pub fn render_issue_source_manager(
    frame: &mut Frame,
    area: Rect,
    repo_slug: &str,
    sources: &[IssueSource],
    selected: usize,
) {
    let popup = centered_rect(55, 50, area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Issue Sources for {repo_slug}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if sources.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no sources configured)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (i, source) in sources.iter().enumerate() {
            let is_selected = i == selected;
            let prefix = if is_selected { "▸ " } else { "  " };

            let style = if is_selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let config_style = Style::default().fg(Color::DarkGray);

            lines.push(Line::from(vec![
                Span::styled(format!("  {prefix}"), style),
                Span::styled(&source.source_type, style),
            ]));

            // Show config details on indented lines below the source type
            for detail in format_source_config_lines(source) {
                lines.push(Line::from(Span::styled(
                    format!("      {detail}"),
                    config_style,
                )));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "  a",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" add  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "d",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" delete  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "Esc",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" close", Style::default().fg(Color::DarkGray)),
    ]));

    let content = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Issue Source Manager "),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(content, popup);
}

pub fn render_github_discover_orgs(
    frame: &mut Frame,
    area: Rect,
    orgs: &[String],
    cursor: usize,
    loading: bool,
    error: Option<&str>,
) {
    let popup = centered_rect(50, 60, area);
    frame.render_widget(Clear, popup);

    let dim = Style::default().fg(Color::DarkGray);

    let mut lines = vec![Line::from("")];

    if loading {
        lines.push(Line::from(Span::styled(
            "  Fetching organizations...",
            Style::default().fg(Color::Yellow),
        )));
    } else if let Some(err) = error {
        lines.push(Line::from(Span::styled(
            format!("  Error: {err}"),
            Style::default().fg(Color::Red),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  Esc to close", dim)));
    } else {
        lines.push(Line::from(Span::styled(
            "  Select an account or organization:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        for (i, owner) in orgs.iter().enumerate() {
            let is_cursor = i == cursor;
            let label = if owner.is_empty() {
                "Personal (your repos)"
            } else {
                owner.as_str()
            };
            let prefix = if is_cursor { "▸ " } else { "  " };
            let style = if is_cursor {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            lines.push(Line::from(vec![
                Span::styled(format!("  {prefix}"), style),
                Span::styled(label, style),
            ]));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "  j/k",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" navigate  ", dim),
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" browse repos  ", dim),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" cancel", dim),
        ]));
    }

    // Lines 0..3 = blank + header + blank; each org is 1 line after that.
    let cursor_abs_line = (3 + cursor) as u16;
    let inner_h = popup.height.saturating_sub(2);
    let scroll_offset = cursor_abs_line.saturating_sub(inner_h / 2);

    let content = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Discover GitHub Repos "),
        )
        .scroll((scroll_offset, 0));

    frame.render_widget(content, popup);
}

#[allow(clippy::too_many_arguments)]
pub fn render_github_discover(
    frame: &mut Frame,
    area: Rect,
    repos: &[DiscoveredRepo],
    registered_urls: &[String],
    selected: &[bool],
    cursor: usize,
    loading: bool,
    error: Option<&str>,
) {
    let popup = centered_rect(65, 75, area);
    frame.render_widget(Clear, popup);

    let dim = Style::default().fg(Color::DarkGray);
    let cyan = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let mut lines = vec![Line::from("")];
    let mut cursor_abs_line: usize = 0;

    if loading {
        lines.push(Line::from(Span::styled(
            "  Fetching repos from GitHub...",
            Style::default().fg(Color::Yellow),
        )));
    } else if let Some(err) = error {
        lines.push(Line::from(Span::styled(
            format!("  Error: {err}"),
            Style::default().fg(Color::Red),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  Esc to close", dim)));
    } else if repos.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No repos found (check `gh` auth).",
            dim,
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  Esc to close", dim)));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  Repos from GitHub", cyan),
            Span::styled(format!("  ({} total)", repos.len()), dim),
        ]));
        lines.push(Line::from(""));

        // Lines 0..3 are: blank + header + blank.  Count lines up to cursor so
        // we can compute the scroll offset needed to keep it in view.
        const HEADER_LINES: usize = 3; // blank + "Repos from GitHub" + blank
        cursor_abs_line = HEADER_LINES;
        for (i, repo) in repos.iter().enumerate() {
            if i == cursor {
                break;
            }
            cursor_abs_line += 1; // name line
            if !repo.description.is_empty() {
                cursor_abs_line += 1;
            }
        }

        for (i, repo) in repos.iter().enumerate() {
            let is_cursor = i == cursor;
            let is_checked = selected.get(i).copied().unwrap_or(false);
            let is_registered = registered_urls
                .iter()
                .any(|u| u == &repo.clone_url || u == &repo.ssh_url);

            let checkbox = if is_registered {
                "[✓]"
            } else if is_checked {
                "[x]"
            } else {
                "[ ]"
            };

            let checkbox_style = if is_registered {
                Style::default().fg(Color::DarkGray)
            } else if is_checked {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let name_style = if is_cursor {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if is_registered {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };

            let prefix = if is_cursor { "▸ " } else { "  " };

            let mut spans = vec![
                Span::styled(format!("  {prefix}"), name_style),
                Span::styled(checkbox, checkbox_style),
                Span::raw(" "),
                Span::styled(&repo.full_name, name_style),
            ];

            if repo.private {
                spans.push(Span::styled(" [private]", dim));
            }
            if is_registered {
                spans.push(Span::styled(" [registered]", dim));
            }

            lines.push(Line::from(spans));

            if !repo.description.is_empty() {
                let desc = if repo.description.chars().count() > 60 {
                    let s: String = repo.description.chars().take(57).collect();
                    format!("{s}...")
                } else {
                    repo.description.clone()
                };
                lines.push(Line::from(Span::styled(format!("       {desc}"), dim)));
            }
        }

        let selected_count = selected.iter().filter(|&&s| s).count();
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "  j/k",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" navigate  ", dim),
            Span::styled(
                "Space",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" toggle  ", dim),
            Span::styled(
                "a",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" select all  ", dim),
            Span::styled(
                "i",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" import ({selected_count})  "), dim),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" cancel", dim),
        ]));
    }

    // Scroll so the cursor row stays roughly centred in the visible area.
    let inner_h = popup.height.saturating_sub(2) as usize; // exclude borders
    let scroll_offset = cursor_abs_line.saturating_sub(inner_h / 2) as u16;

    let content = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Discover GitHub Repos "),
        )
        .scroll((scroll_offset, 0));

    frame.render_widget(content, popup);
}

pub fn render_event_detail(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    body: &str,
    line_count: usize,
    scroll_offset: u16,
    horizontal_offset: u16,
) {
    let popup = centered_rect(85, 85, area);
    frame.render_widget(Clear, popup);

    let lines: Vec<Line> = body
        .lines()
        .map(|l| Line::from(Span::raw(l.to_string())))
        .collect();

    let hint = format!(
        " j/k=scroll  h/l=pan  gg/G=top/bot  q/Esc=close  (line {}/{})",
        scroll_offset + 1,
        line_count.max(1),
    );

    let max_title_chars = (popup.width as usize).saturating_sub(7);
    let title_display = if title.chars().count() > (popup.width as usize).saturating_sub(4) {
        let truncated: String = title.chars().take(max_title_chars).collect();
        format!(" {truncated}... ")
    } else {
        format!(" {title} ")
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title_display);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Split: body (fill) + hint line (1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let body_widget = Paragraph::new(lines).scroll((scroll_offset, horizontal_offset));
    frame.render_widget(body_widget, chunks[0]);

    let hint_widget = Paragraph::new(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(hint_widget, chunks[1]);
}

fn format_source_config_lines(source: &IssueSource) -> Vec<String> {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&source.config_json) {
        match source.source_type.as_str() {
            "github" => {
                let owner = val["owner"].as_str().unwrap_or("?");
                let repo = val["repo"].as_str().unwrap_or("?");
                vec![format!("{owner}/{repo}")]
            }
            "jira" => {
                let url = val["url"].as_str().unwrap_or("?");
                let jql = val["jql"].as_str().unwrap_or("?");
                vec![format!("URL: {url}"), format!("JQL: {jql}")]
            }
            _ => vec![source.config_json.clone()],
        }
    } else {
        vec![source.config_json.clone()]
    }
}

pub fn render_gate_action(frame: &mut Frame, area: Rect, gate_prompt: &str, feedback: &str) {
    let popup = centered_rect(60, 40, area);
    frame.render_widget(Clear, popup);

    let content = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "Gate Prompt:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw(gate_prompt)),
        Line::from(""),
        Line::from(Span::styled(
            "Feedback (optional):",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::styled(
                feedback,
                Style::default().add_modifier(Modifier::UNDERLINED),
            ),
            Span::styled("_", Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  y",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" = approve    "),
            Span::styled(
                "n",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" = reject    "),
            Span::styled("Esc", Style::default().fg(Color::DarkGray)),
            Span::raw(" = cancel"),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" Gate Action "),
    );

    frame.render_widget(content, popup);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([Constraint::Percentage(percent_y)])
        .flex(Flex::Center)
        .split(area);
    Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .split(vertical[0])[0]
}
