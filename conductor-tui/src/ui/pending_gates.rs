use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::state::AppState;

/// Render the Pending Gates pane in the right workflow column.
/// Returns early (renders nothing) when there are no pending gates.
pub fn render_pending_gates(frame: &mut Frame, area: Rect, state: &AppState, focused: bool) {
    if state.detail_gates.is_empty() {
        return;
    }

    let border_color = if focused {
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };

    let items: Vec<ListItem> = state
        .detail_gates
        .iter()
        .map(|gate| {
            // Branch or fallback to target_label
            let location = gate
                .branch
                .as_deref()
                .or(gate.target_label.as_deref())
                .unwrap_or("");
            let location_display = if location.chars().count() > 28 {
                format!(
                    "{}\u{2026}",
                    &location[..location
                        .char_indices()
                        .nth(28)
                        .map(|(i, _)| i)
                        .unwrap_or(location.len())]
                )
            } else {
                location.to_string()
            };

            // Look up PR number from already-loaded detail_prs by matching head_ref_name
            let pr_ref = gate.branch.as_deref().and_then(|branch| {
                state
                    .detail_prs
                    .iter()
                    .find(|pr| pr.head_ref_name == branch)
                    .map(|pr| format!("#{}", pr.number))
            });

            // Ref to show: PR number takes priority over ticket_ref
            let ref_display = pr_ref
                .or_else(|| gate.ticket_ref.as_ref().map(|t| format!("#{t}")))
                .unwrap_or_default();

            let mut spans = vec![
                Span::raw("\u{23F8} "),
                Span::styled(
                    gate.workflow_name.as_str(),
                    Style::default().fg(state.theme.group_header),
                ),
                Span::raw("  "),
                Span::raw(&gate.step.step_name),
            ];
            if !location_display.is_empty() {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    location_display,
                    Style::default().fg(state.theme.label_secondary),
                ));
            }
            if !ref_display.is_empty() {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    ref_display,
                    Style::default().fg(state.theme.label_accent),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let title = if focused {
        " Pending Gates  Enter:view "
    } else {
        " Pending Gates "
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(state.theme.highlight_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut list_state = ListState::default();
    if focused {
        list_state.select(Some(
            state
                .detail_gate_index
                .min(state.detail_gates.len().saturating_sub(1)),
        ));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}
