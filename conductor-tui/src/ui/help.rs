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
        Line::from(Span::styled(
            "Global Navigation",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("Tab / Shift+Tab", "Cycle panel focus"),
        help_line("j / k", "Navigate within panel"),
        help_line("G / End", "Jump to bottom of list"),
        help_line("gg / Home", "Jump to top of list"),
        help_line("Ctrl+d / Ctrl+u", "Half-page down / up"),
        help_line("Enter", "Drill into selected item"),
        help_line("Esc", "Back to previous view"),
        help_line("1", "Dashboard"),
        help_line("2", "Tickets view"),
        help_line("3", "Workflows view"),
        help_line("?", "Toggle this help"),
        help_line("q", "Quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Global Actions",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("a", "Add repository"),
        help_line("c", "Create worktree"),
        help_line("d", "Delete (worktree/repo)"),
        help_line("s", "Sync tickets"),
        help_line("W", "Manage work targets"),
        help_line("S", "Manage issue sources"),
        help_line("m", "Set model (repo/worktree detail)"),
        help_line("A", "Toggle closed tickets"),
        help_line("!", "Toggle status bar expansion"),
        help_line("/", "Filter/search"),
        Line::from(""),
        Line::from(Span::styled(
            "Worktree Detail — Info Panel (Tab to switch)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / k", "Navigate rows"),
        help_line("y", "Copy selected row value"),
        help_line("o", "Act on selected row (open path/ticket/PR)"),
        Line::from(""),
        Line::from(Span::styled(
            "Worktree Detail — Log Panel (Tab to switch)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / k", "Scroll activity log"),
        help_line("Enter", "Expand selected event"),
        help_line("y", "Copy last code block"),
        help_line("l", "View agent log file"),
        Line::from(""),
        Line::from(Span::styled(
            "Worktree Detail — Agent Controls",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("r", "Run Claude agent (tmux)"),
        help_line("O", "Orchestrate (multi-step child agents)"),
        help_line("a", "Attach to running agent"),
        help_line("x", "Stop running agent"),
        help_line("f", "Submit feedback to agent"),
        help_line("F", "Dismiss feedback request"),
        help_line("m", "Set model for this worktree"),
        Line::from(""),
        Line::from(Span::styled(
            "Workflow Run Detail",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("x", "Cancel workflow run"),
        help_line("r", "Resume workflow run"),
        help_line("y / Y", "Approve waiting gate step"),
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
