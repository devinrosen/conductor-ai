use conductor_core::feature::{FeatureRow, FeatureStatus};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::common::{format_elapsed, truncate};
use crate::state::AppState;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    // Split: main list on top, status bar on bottom (only when dangling present).
    let dangling_count = state
        .detail_features
        .iter()
        .filter(|f| is_dangling(f))
        .count();

    let (list_area, status_area) = if dangling_count > 0 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);
        (chunks[0], Some(chunks[1]))
    } else {
        (area, None)
    };

    render_features_list(frame, list_area, state);

    if let Some(sa) = status_area {
        render_dangling_status(frame, sa, dangling_count, state);
    }
}

fn is_dangling(f: &FeatureRow) -> bool {
    f.worktree_count == 0 && f.status == FeatureStatus::InProgress
}

pub(super) fn status_color(status: &FeatureStatus, state: &AppState) -> ratatui::style::Color {
    match status {
        FeatureStatus::InProgress => state.theme.label_warning,
        FeatureStatus::ReadyForReview => state.theme.label_info,
        FeatureStatus::Approved => state.theme.status_completed,
        FeatureStatus::Merged | FeatureStatus::Closed => state.theme.label_secondary,
    }
}

fn status_label(status: &FeatureStatus) -> &'static str {
    match status {
        FeatureStatus::InProgress => "in_progress",
        FeatureStatus::ReadyForReview => "ready_review",
        FeatureStatus::Approved => "approved",
        FeatureStatus::Merged => "merged",
        FeatureStatus::Closed => "closed",
    }
}

/// Build a unicode progress bar string of fixed width `width`.
/// Filled portion uses '▓', empty uses '░'.
fn progress_bar(merged: u32, total: u32, width: usize) -> String {
    if total == 0 {
        return "░".repeat(width);
    }
    let filled = ((merged as f32 / total as f32) * width as f32).round() as usize;
    let filled = filled.min(width);
    format!("{}{}", "▓".repeat(filled), "░".repeat(width - filled))
}

fn render_features_list(frame: &mut Frame, area: Rect, state: &AppState) {
    let features = &state.detail_features;

    let title = if state.selected_repo_id.is_some() {
        let repo_name = state
            .selected_repo_id
            .as_ref()
            .and_then(|id| state.data.repos.iter().find(|r| &r.id == id))
            .map(|r| r.slug.as_str())
            .unwrap_or("?");
        format!(" Features — {} ", repo_name)
    } else {
        " Features ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(state.theme.border_focused));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if features.is_empty() {
        let empty = Paragraph::new("No features found. Press F from the dashboard to open this view, or f from a repo detail.")
            .style(Style::default().fg(state.theme.label_secondary));
        frame.render_widget(empty, inner);
        return;
    }

    // Header row
    let header_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let header = Line::from(vec![Span::styled(
        format!(
            "{:<30} {:<14} {:>5}  {:<12} {:>6}  {:<4}",
            "Name", "Status", "Prog", "Progress", "Stale", ""
        ),
        Style::default()
            .fg(state.theme.label_secondary)
            .add_modifier(Modifier::UNDERLINED),
    )]);
    frame.render_widget(Paragraph::new(header), header_chunks[0]);

    let items: Vec<ListItem> = features
        .iter()
        .enumerate()
        .map(|(idx, f)| build_feature_row(f, idx == state.features_index, state))
        .collect();

    let mut list_state = ListState::default();
    if !features.is_empty() {
        list_state.select(Some(state.features_index));
    }

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(ratatui::style::Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    frame.render_stateful_widget(list, header_chunks[1], &mut list_state);

    // Footer key hints
    let hints_area = Rect {
        y: area.y + area.height.saturating_sub(1),
        height: 1,
        ..area
    };
    // Only show if we have space (area height > inner height means border exists)
    if area.height > 2 {
        let hints = Line::from(vec![Span::styled(
            " r=run  v=ready  a=approve  x=close  Enter=detail  Esc=back",
            Style::default().fg(state.theme.label_secondary),
        )]);
        frame.render_widget(Paragraph::new(hints), hints_area);
    }
}

fn build_feature_row(f: &FeatureRow, selected: bool, state: &AppState) -> ListItem<'static> {
    let name = truncate(&f.name, 28);
    let status_str = format!("{:<14}", status_label(&f.status));
    let progress_str = if f.tickets_total > 0 {
        format!("{:>2}/{:<2}", f.tickets_merged, f.tickets_total)
    } else {
        "  -  ".to_string()
    };
    let bar = progress_bar(f.tickets_merged, f.tickets_total, 10);
    let stale = format_elapsed(&f.created_at);
    let stale_str = format!(
        "{:>6}",
        if stale.is_empty() {
            "?".to_string()
        } else {
            stale
        }
    );
    let dangling_str = if is_dangling(f) { "[!]" } else { "   " };

    let sc = status_color(&f.status, state);
    let dim = state.theme.label_secondary;
    let name_style = if selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let bar_color = if f.tickets_total > 0 && f.tickets_merged == f.tickets_total {
        state.theme.status_completed
    } else {
        state.theme.label_accent
    };

    let spans = vec![
        Span::styled(format!("{:<30}", name), name_style),
        Span::styled(
            format!(" {:<14}", status_str.trim_end()),
            Style::default().fg(sc),
        ),
        Span::styled(format!(" {:>5}  ", progress_str), Style::default().fg(dim)),
        Span::styled(bar, Style::default().fg(bar_color)),
        Span::styled(format!("  {:>6}", stale_str), Style::default().fg(dim)),
        Span::styled(
            format!("  {}", dangling_str),
            Style::default().fg(state.theme.label_error),
        ),
    ];

    ListItem::new(Line::from(spans))
}

fn render_dangling_status(frame: &mut Frame, area: Rect, dangling_count: usize, state: &AppState) {
    let msg = format!(
        "[!] {} dangling feature{} with no active worktrees — press x to close",
        dangling_count,
        if dangling_count == 1 { "" } else { "s" }
    );
    let paragraph = Paragraph::new(msg).style(Style::default().fg(state.theme.label_error));
    frame.render_widget(paragraph, area);
}
