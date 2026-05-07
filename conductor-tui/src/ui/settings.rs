use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::state::{
    is_secret_env_key, AppState, RuntimeDetailFocus, SettingsCategory, SettingsFocus,
};

/// Named row indices for General settings (right pane).
pub mod general_row {
    pub const MODEL: usize = 0;
    pub const PERMISSION_MODE: usize = 1;
    pub const AUTO_START: usize = 2;
    pub const SYNC_INTERVAL: usize = 3;
    pub const AUTO_CLEANUP: usize = 4;
    pub const ISSUE_SOURCES: usize = 5;
    pub const STALL_TIMEOUT: usize = 6;
    #[allow(dead_code)]
    pub const COUNT: usize = 7;
}

/// Named row indices for Appearance settings (right pane).
pub mod appearance_row {
    pub const THEME: usize = 0;
    #[allow(dead_code)]
    pub const COUNT: usize = 1;
}

/// Row count helper for Runtimes settings (right pane).
/// One row per runtime (claude built-in always rendered first, then user runtimes).
pub mod runtimes_row {
    #[allow(dead_code)]
    pub const COUNT_PER_RUNTIME: usize = 1;
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

    let category_title = match (state.settings_category, &state.settings_runtime_detail) {
        (SettingsCategory::Runtimes, Some(detail)) => format!(" Runtime: {} ", detail.name),
        _ => format!(" {} ", state.settings_category.label()),
    };
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
        SettingsCategory::Runtimes => {
            render_runtimes(frame, right_area, right_block, state, right_focused)
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

fn render_general(frame: &mut Frame, area: Rect, block: Block, state: &AppState, focused: bool) {
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
        setting_line(
            "Stall timeout (sec)",
            &d.stall_timeout,
            row_style(general_row::STALL_TIMEOUT, sel, focused, state),
        ),
        Line::from(""),
        hint_line,
    ];

    let para = Paragraph::new(rows).block(block);
    frame.render_widget(para, area);
}

fn render_appearance(frame: &mut Frame, area: Rect, block: Block, state: &AppState, focused: bool) {
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
            "  [t] test hook  [o] open script  [Esc] back",
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
        // Compute dynamic column widths from content
        let hook_count = d.hooks.len();
        let idx_w = hook_count.saturating_sub(1).to_string().len().max(1);
        let on_w = d
            .hooks
            .iter()
            .map(|(on, _)| on.len())
            .max()
            .unwrap_or(0)
            .max(12);
        let cmd_w = d
            .hooks
            .iter()
            .map(|(_, cmd)| cmd.len())
            .max()
            .unwrap_or(0)
            .max(12);
        let separator_len = idx_w + 1 + on_w + 1 + cmd_w + 1 + "Last test".len();

        // Column header
        lines.push(Line::from(Span::styled(
            format!(
                "  {:<idx_w$} {:<on_w$} {:<cmd_w$} {}",
                "#", "on", "run / url", "Last test"
            ),
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("  {}", "\u{2500}".repeat(separator_len)),
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
                Some(Ok(())) => Span::styled(
                    "\u{2713} fired",
                    Style::default().fg(theme.status_completed),
                ),
                Some(Err(e)) => Span::styled(
                    format!("\u{2717} {}", &e[..e.len().min(20)]),
                    Style::default().fg(theme.status_failed),
                ),
                None => Span::styled("\u{2014}", Style::default().fg(theme.label_secondary)),
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {i:<idx_w$} {on_pat:<on_w$} {cmd:<cmd_w$} ", cmd = cmd),
                    base_style,
                ),
                test_span,
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(hint_line);

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

fn render_runtimes(frame: &mut Frame, area: Rect, block: Block, state: &AppState, focused: bool) {
    if state.settings_runtime_detail.is_some() {
        render_runtime_detail(frame, area, block, state, focused);
    } else {
        render_runtimes_list(frame, area, block, state, focused);
    }
}

fn render_runtimes_list(
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
            "  [Enter/e] edit  [a] add  [d] delete  [Esc] back",
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

    if d.runtimes.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No runtimes configured",
            Style::default().fg(theme.label_secondary),
        )));
    } else {
        for (i, row) in d.runtimes.iter().enumerate() {
            let is_selected = i == sel && focused;
            let style = if is_selected {
                Style::default()
                    .fg(theme.label_primary)
                    .bg(theme.highlight_bg)
            } else {
                Style::default().fg(theme.label_secondary)
            };
            let prefix = if is_selected { "\u{25b8} " } else { "  " };
            let suffix = if row.is_built_in {
                "  (built-in)".to_string()
            } else {
                format!(
                    "  type={} models={} env={}",
                    row.type_hint, row.model_count, row.env_count
                )
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {prefix}"), style),
                Span::styled(row.name.clone(), style.add_modifier(Modifier::BOLD)),
                Span::styled(suffix, Style::default().fg(theme.label_secondary)),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(hint_line);

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

fn render_runtime_detail(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    state: &AppState,
    focused: bool,
) {
    let theme = &state.theme;

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(detail) = state.settings_runtime_detail.as_ref() else {
        return;
    };
    let runtime = state
        .settings_display
        .runtimes
        .iter()
        .find(|r| r.name == detail.name);
    let type_hint = runtime
        .map(|r| r.type_hint.clone())
        .unwrap_or_else(|| "claude".to_string());
    let is_built_in = runtime.map(|r| r.is_built_in).unwrap_or(false);
    let models: Vec<String> = runtime.map(|r| r.models.clone()).unwrap_or_default();
    let env_pairs: Vec<(String, String)> = runtime.map(|r| r.env.clone()).unwrap_or_default();

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "  Type:        ",
            Style::default().fg(theme.label_secondary),
        ),
        Span::styled(
            type_hint,
            Style::default()
                .fg(theme.label_primary)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            "  Built-in:    ",
            Style::default().fg(theme.label_secondary),
        ),
        Span::styled(
            if is_built_in { "yes" } else { "no" },
            Style::default().fg(theme.label_primary),
        ),
    ]));
    lines.push(Line::from(""));

    // ── Models section ─────────────────────────────────────────────────
    let models_focused = focused && detail.focus == RuntimeDetailFocus::Models;
    let models_header_style = if models_focused {
        Style::default()
            .fg(theme.label_accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.label_secondary)
    };
    lines.push(Line::from(Span::styled(
        format!("  ── Models ({}) ────────────────────", models.len()),
        models_header_style,
    )));
    if models.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (no models — press a to add)",
            Style::default().fg(theme.label_secondary),
        )));
    } else {
        for (i, m) in models.iter().enumerate() {
            let is_selected = i == detail.model_index && models_focused;
            let style = if is_selected {
                Style::default()
                    .fg(theme.label_primary)
                    .bg(theme.highlight_bg)
            } else {
                Style::default().fg(theme.label_primary)
            };
            let prefix = if is_selected { "\u{25b8} " } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(format!("  {prefix}"), style),
                Span::styled(m.clone(), style),
            ]));
        }
    }
    lines.push(Line::from(""));

    // ── Environment section ────────────────────────────────────────────
    let env_focused = focused && detail.focus == RuntimeDetailFocus::Environment;
    let env_header_style = if env_focused {
        Style::default()
            .fg(theme.label_accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.label_secondary)
    };
    lines.push(Line::from(Span::styled(
        format!("  ── Environment ({}) ───────────────", env_pairs.len()),
        env_header_style,
    )));
    if env_pairs.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (no env vars — press a to add)",
            Style::default().fg(theme.label_secondary),
        )));
    } else {
        let key_w = env_pairs.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (i, (k, v)) in env_pairs.iter().enumerate() {
            let is_selected = i == detail.env_index && env_focused;
            let style = if is_selected {
                Style::default()
                    .fg(theme.label_primary)
                    .bg(theme.highlight_bg)
            } else {
                Style::default().fg(theme.label_primary)
            };
            let prefix = if is_selected { "\u{25b8} " } else { "  " };
            let secret = is_secret_env_key(k);
            let revealed = detail.revealed_env_keys.contains(k);
            let display_value = if secret && !revealed {
                "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}".to_string()
            } else {
                v.clone()
            };
            let mut spans = vec![
                Span::styled(format!("  {prefix}"), style),
                Span::styled(format!("{:<key_w$}", k, key_w = key_w), style),
                Span::styled("  ", style),
                Span::styled(display_value, style),
            ];
            if secret {
                let tag = if revealed { "  [shown]" } else { "  [hidden]" };
                spans.push(Span::styled(
                    tag.to_string(),
                    Style::default().fg(theme.label_secondary),
                ));
            }
            lines.push(Line::from(spans));
        }
    }
    lines.push(Line::from(""));

    // Hint line — section-aware.
    let hint = if focused {
        match detail.focus {
            RuntimeDetailFocus::Models => {
                "  [a] add  [Enter/e] edit  [d] delete  [J/K] reorder  [Tab] env  [Esc] back"
            }
            RuntimeDetailFocus::Environment => {
                "  [a] add  [Enter/e] edit  [d] delete  [r] reveal  [Tab] models  [Esc] back"
            }
        }
    } else {
        "  [Tab] switch pane"
    };
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(theme.label_secondary),
    )));

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}
