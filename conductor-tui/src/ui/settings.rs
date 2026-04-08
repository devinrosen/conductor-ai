use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::state::{AppState, SettingsCategory, SettingsFocus};

/// Named row indices for General settings (right pane).
pub mod general_row {
    pub const MODEL: usize = 0;
    pub const PERMISSION_MODE: usize = 1;
    pub const AUTO_START: usize = 2;
    pub const SYNC_INTERVAL: usize = 3;
    pub const AUTO_CLEANUP: usize = 4;
    pub const ISSUE_SOURCES: usize = 5;
    #[allow(dead_code)]
    pub const COUNT: usize = 6;
}

/// Named row indices for Appearance settings (right pane).
pub mod appearance_row {
    pub const THEME: usize = 0;
    #[allow(dead_code)]
    pub const COUNT: usize = 1;
}

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let theme = &state.theme;

    // Split horizontally: left pane (category list) + right pane (settings rows).
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(20), Constraint::Min(0)])
        .split(area);

    let left_area = panes[0];
    let right_area = panes[1];

    // ── Left pane: category list ─────────────────────────────────────────────
    let left_focused = state.settings_focus == SettingsFocus::CategoryList;
    let left_border_style = if left_focused {
        Style::default().fg(theme.label_accent)
    } else {
        Style::default().fg(theme.label_secondary)
    };
    let left_block = Block::default()
        .title(" Category ")
        .borders(Borders::ALL)
        .border_style(left_border_style);

    let categories = SettingsCategory::all();
    let items: Vec<ListItem> = categories
        .iter()
        .enumerate()
        .map(|(i, cat)| {
            let is_selected = i == state.settings_category_index;
            let style = if is_selected && left_focused {
                Style::default()
                    .fg(theme.label_primary)
                    .bg(theme.highlight_bg)
                    .add_modifier(Modifier::BOLD)
            } else if is_selected {
                Style::default()
                    .fg(theme.label_primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.label_secondary)
            };
            let prefix = if is_selected { "> " } else { "  " };
            ListItem::new(Line::from(Span::styled(
                format!("{}{}", prefix, cat.label()),
                style,
            )))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.settings_category_index));

    frame.render_stateful_widget(
        List::new(items).block(left_block),
        left_area,
        &mut list_state,
    );

    // ── Right pane: settings for the selected category ───────────────────────
    let right_focused = state.settings_focus == SettingsFocus::SettingsList;
    let right_border_style = if right_focused {
        Style::default().fg(theme.label_accent)
    } else {
        Style::default().fg(theme.label_secondary)
    };

    let category_title = format!(" {} ", state.settings_category.label());
    let right_block = Block::default()
        .title(category_title)
        .borders(Borders::ALL)
        .border_style(right_border_style);

    match state.settings_category {
        SettingsCategory::General => {
            render_general(frame, right_area, right_block, state, right_focused)
        }
        SettingsCategory::Appearance => {
            render_appearance(frame, right_area, right_block, state, right_focused)
        }
        SettingsCategory::Notifications => {
            render_notifications(frame, right_area, right_block, state, right_focused)
        }
    }
}

fn row_style(idx: usize, selected: usize, focused: bool, state: &AppState) -> Style {
    let theme = &state.theme;
    if idx == selected && focused {
        Style::default()
            .fg(theme.label_primary)
            .bg(theme.highlight_bg)
    } else if idx == selected {
        Style::default().fg(theme.label_primary)
    } else {
        Style::default().fg(theme.label_secondary)
    }
}

fn setting_line(label: &str, value: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {:<22}", label), style),
        Span::styled(value.to_string(), style.add_modifier(Modifier::BOLD)),
    ])
}

fn render_general(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    state: &AppState,
    focused: bool,
) {
    let theme = &state.theme;
    let sel = state.settings_row_index;
    let d = &state.settings_display;

    let hint_line = if focused {
        Line::from(Span::styled(
            "  [Enter] edit  [c] cycle value  [Esc] back",
            Style::default().fg(theme.label_secondary),
        ))
    } else {
        Line::from(Span::styled(
            "  [Tab] switch pane",
            Style::default().fg(theme.label_secondary),
        ))
    };

    let rows: Vec<Line> = vec![
        Line::from(""),
        setting_line(
            "Model",
            &d.model,
            row_style(general_row::MODEL, sel, focused, state),
        ),
        setting_line(
            "Permission mode",
            &d.permission_mode,
            row_style(general_row::PERMISSION_MODE, sel, focused, state),
        ),
        setting_line(
            "Auto-start agent",
            &d.auto_start,
            row_style(general_row::AUTO_START, sel, focused, state),
        ),
        setting_line(
            "Sync interval (min)",
            &d.sync_interval,
            row_style(general_row::SYNC_INTERVAL, sel, focused, state),
        ),
        setting_line(
            "Auto-cleanup merged",
            &d.auto_cleanup,
            row_style(general_row::AUTO_CLEANUP, sel, focused, state),
        ),
        setting_line(
            "Issue sources",
            "[Enter] manage \u{2192}",
            row_style(general_row::ISSUE_SOURCES, sel, focused, state),
        ),
        Line::from(""),
        hint_line,
    ];

    let para = Paragraph::new(rows).block(block);
    frame.render_widget(para, area);
}

fn render_appearance(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    state: &AppState,
    focused: bool,
) {
    let theme = &state.theme;
    let sel = state.settings_row_index;
    let d = &state.settings_display;

    let hint_line = if focused {
        Line::from(Span::styled(
            "  [Enter] open theme picker",
            Style::default().fg(theme.label_secondary),
        ))
    } else {
        Line::from(Span::styled(
            "  [Tab] switch pane",
            Style::default().fg(theme.label_secondary),
        ))
    };

    let rows: Vec<Line> = vec![
        Line::from(""),
        setting_line(
            "Theme",
            &d.theme,
            row_style(appearance_row::THEME, sel, focused, state),
        ),
        Line::from(""),
        hint_line,
    ];

    let para = Paragraph::new(rows).block(block);
    frame.render_widget(para, area);
}

fn render_notifications(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    state: &AppState,
    focused: bool,
) {
    let theme = &state.theme;
    let sel = state.settings_row_index;
    let d = &state.settings_display;

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let hint_line = if focused {
        Line::from(Span::styled(
            "  [t] test hook  [Esc] back",
            Style::default().fg(theme.label_secondary),
        ))
    } else {
        Line::from(Span::styled(
            "  [Tab] switch pane",
            Style::default().fg(theme.label_secondary),
        ))
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));

    if d.hooks.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No notification hooks configured.",
            Style::default().fg(theme.label_secondary),
        )));
        lines.push(Line::from(Span::styled(
            "  Add [[notify.hooks]] to ~/.conductor/config.toml.",
            Style::default().fg(theme.label_secondary),
        )));
    } else {
        // Column header
        lines.push(Line::from(Span::styled(
            format!("  {:<4} {:<20} {:<35} {}", "#", "on", "run / url", "Last test"),
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "  \u{2500}".repeat(72),
            Style::default().fg(theme.label_secondary),
        )));

        for (i, (on_pat, cmd)) in d.hooks.iter().enumerate() {
            let is_selected = i == sel && focused;
            let base_style = if is_selected {
                Style::default()
                    .fg(theme.label_primary)
                    .bg(theme.highlight_bg)
            } else {
                Style::default().fg(theme.label_secondary)
            };

            let test_result = state.settings_hook_test_results.get(&i);
            let test_span = match test_result {
                Some(Ok(())) => {
                    Span::styled("\u{2713} fired", Style::default().fg(theme.status_completed))
                }
                Some(Err(e)) => Span::styled(
                    format!("\u{2717} {}", &e[..e.len().min(20)]),
                    Style::default().fg(theme.status_failed),
                ),
                None => Span::styled("\u{2014}", Style::default().fg(theme.label_secondary)),
            };

            // Truncate on/cmd for display
            let on_display = truncate(on_pat, 18);
            let cmd_display = truncate(cmd, 33);

            lines.push(Line::from(vec![
                Span::styled(format!("  {:<4} {:<20} {:<35} ", i, on_display, cmd_display), base_style),
                test_span,
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(hint_line);

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}\u{2026}", truncated)
    }
}
