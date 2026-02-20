use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

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

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([Constraint::Percentage(percent_y)])
        .flex(Flex::Center)
        .split(area);
    Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .split(vertical[0])[0]
}
