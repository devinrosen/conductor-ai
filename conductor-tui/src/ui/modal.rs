use ratatui::layout::{Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use tui_textarea::TextArea;

use conductor_core::agent::TicketAgentTotals;
use conductor_core::config::WorkTarget;
use conductor_core::tickets::Ticket;
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
            Span::raw(" = confirm    "),
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

pub fn render_ticket_info(
    frame: &mut Frame,
    area: Rect,
    ticket: &Ticket,
    agent_totals: Option<&TicketAgentTotals>,
    worktrees: Option<&Vec<Worktree>>,
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
    } else if ticket.body.len() > 500 {
        format!("{}...", &ticket.body[..500])
    } else {
        ticket.body.clone()
    };

    let assignee_text = ticket.assignee.as_deref().unwrap_or("unassigned");

    let labels_text = if ticket.labels.is_empty() {
        "none".to_string()
    } else {
        ticket.labels.clone()
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
        Line::from(vec![
            Span::styled("  Labels:    ", label_style),
            Span::styled(&labels_text, value_style),
        ]),
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
        lines.push(Line::from(vec![
            Span::styled("    Cost:  ", dim_style),
            Span::styled(
                format!("${:.4}", totals.total_cost),
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

pub fn render_work_target_picker(
    frame: &mut Frame,
    area: Rect,
    targets: &[WorkTarget],
    selected: usize,
) {
    let height = (targets.len() as u16 + 6).min(20);
    let percent_y = ((height as f32 / area.height as f32) * 100.0) as u16;
    let popup = centered_rect(50, percent_y.max(25), area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Select a work target:",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
    ];

    for (i, target) in targets.iter().enumerate() {
        let is_selected = i == selected;
        let prefix = if is_selected { "▸ " } else { "  " };
        let number = format!("{}. ", i + 1);
        let type_hint = format!(" ({})", target.target_type);

        let style = if is_selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {prefix}{number}"), style),
            Span::styled(&target.name, style),
            Span::styled(type_hint, Style::default().fg(Color::DarkGray)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  1-9 select  Enter confirm  Esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    let content = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Work Targets "),
    );

    frame.render_widget(content, popup);
}

pub fn render_work_target_manager(
    frame: &mut Frame,
    area: Rect,
    targets: &[WorkTarget],
    selected: usize,
) {
    let popup = centered_rect(55, 60, area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Manage Work Targets",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if targets.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no targets configured)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (i, target) in targets.iter().enumerate() {
            let is_selected = i == selected;
            let prefix = if is_selected { "▸ " } else { "  " };
            let type_hint = format!(" [{}] cmd: {}", target.target_type, target.command);

            let style = if is_selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            lines.push(Line::from(vec![
                Span::styled(format!("  {prefix}"), style),
                Span::styled(&target.name, style),
                Span::styled(type_hint, Style::default().fg(Color::DarkGray)),
            ]));
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
            "K/J",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" reorder  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "Esc",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" close", Style::default().fg(Color::DarkGray)),
    ]));

    let content = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Work Target Manager "),
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

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([Constraint::Percentage(percent_y)])
        .flex(Flex::Center)
        .split(area);
    Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .split(vertical[0])[0]
}
