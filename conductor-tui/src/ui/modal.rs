use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use conductor_core::tickets::Ticket;

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

pub fn render_ticket_info(frame: &mut Frame, area: Rect, ticket: &Ticket) {
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
        Line::from(Span::styled("  Description:", label_style)),
    ];

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

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([Constraint::Percentage(percent_y)])
        .flex(Flex::Center)
        .split(area);
    Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .split(vertical[0])[0]
}
