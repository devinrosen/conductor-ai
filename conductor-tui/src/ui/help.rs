use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

pub fn render(frame: &mut Frame, area: Rect, theme: &crate::theme::Theme) {
    let popup = centered_rect(60, 80, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(
            "Keybindings",
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Global Navigation",
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("Tab / Shift+Tab", "Cycle panel focus", theme),
        help_line("j / k", "Navigate within panel", theme),
        help_line("G / End", "Jump to bottom of list", theme),
        help_line("g / Home", "Jump to top of list", theme),
        help_line("Ctrl+d / Ctrl+u", "Half-page down / up", theme),
        help_line("Enter", "Drill into selected item", theme),
        help_line("Esc", "Back to previous view", theme),
        help_line("?", "Toggle this help", theme),
        help_line("q", "Quit", theme),
        help_line("[ / ]", "Focus content / workflow column", theme),
        help_line("\\", "Toggle workflow column", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Global Actions",
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("a", "Register repository", theme),
        help_line("c", "Create worktree", theme),
        help_line("d", "Delete (worktree/repo)", theme),
        help_line("s", "Sync tickets", theme),
        help_line("S", "Manage issue sources", theme),
        help_line("A", "Toggle closed tickets", theme),
        help_line("w", "Open workflow picker", theme),
        help_line("/", "Filter/search", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Workflow Column",
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("H", "Toggle completed/cancelled runs", theme),
        help_line("r", "Run selected workflow", theme),
        help_line("v", "View workflow definition (Defs tab)", theme),
        help_line("e", "Edit workflow definition in $EDITOR (Defs tab)", theme),
        help_line("Space", "Collapse/expand run group (Runs tab)", theme),
        help_line("l / →", "Toggle step tree pane (Defs tab)", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Worktree Detail — Info Panel (Tab to switch)",
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / k", "Navigate rows", theme),
        help_line("y", "Copy selected row value", theme),
        help_line("o", "Act on selected row (open path/ticket/PR)", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Worktree Detail — Log Panel (Tab to switch)",
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / k", "Scroll activity log", theme),
        help_line("Enter", "Expand selected event", theme),
        help_line("y", "Copy last code block", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Worktree Detail — Agent Controls",
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("p", "Prompt Claude agent", theme),
        help_line("O", "Orchestrate (multi-step child agents)", theme),
        help_line("x", "Stop running agent", theme),
        help_line("f", "Submit feedback to agent", theme),
        help_line("F", "Dismiss feedback request", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Workflow Run Detail",
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("x", "Cancel workflow run", theme),
        help_line("r", "Resume workflow run", theme),
        help_line("Enter", "Approve waiting gate step", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Workflow Definition Detail",
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / k", "Scroll steps", theme),
        help_line("r", "Run this workflow", theme),
        help_line("e", "Edit in $EDITOR", theme),
        help_line("Esc", "Back", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Press Esc or ? to close",
            Style::default().fg(theme.label_secondary),
        )),
    ];

    let help = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border_focused))
            .title(" Help "),
    );

    frame.render_widget(help, popup);
}

fn help_line<'a>(key: &'a str, desc: &'a str, theme: &crate::theme::Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {key:<20}"),
            Style::default()
                .fg(theme.label_warning)
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
