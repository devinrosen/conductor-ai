use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

pub fn render(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(60, 80, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(
            "Keybindings",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("Tab / Shift+Tab", "Cycle panel focus"),
        help_line("j / k", "Navigate within panel"),
        help_line("Enter", "Drill into selected item"),
        help_line("Esc", "Back to previous view"),
        Line::from(""),
        Line::from(Span::styled(
            "Actions",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("a", "Add repository"),
        help_line("c", "Create worktree"),
        help_line("d", "Delete (worktree/repo)"),
        help_line("p", "Push current worktree"),
        help_line("P", "Create pull request"),
        help_line("s", "Sync tickets / End session"),
        help_line("S", "Start session"),
        help_line("l", "Link ticket to worktree"),
        help_line("w", "Open editor at worktree"),
        help_line("/", "Filter/search"),
        Line::from(""),
        Line::from(Span::styled(
            "Navigation",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("1", "Dashboard"),
        help_line("2 / t", "Tickets view"),
        help_line("3", "Session view"),
        help_line("?", "Toggle this help"),
        help_line("q", "Quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Press Esc or ? to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let help = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Help "),
    );

    frame.render_widget(help, popup);
}

fn help_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {key:<20}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(desc),
    ])
}

/// Create a centered rectangle of a given percentage width and height.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([Constraint::Percentage(percent_y)])
        .flex(Flex::Center)
        .split(area);
    Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .split(vertical[0])[0]
}
