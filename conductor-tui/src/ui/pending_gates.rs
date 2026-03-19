use conductor_core::workflow::GateType;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::state::AppState;
use crate::ui::common::{format_elapsed, gate_type_icon, truncate};

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
            // Actionability indicator based on gate type (shared helper)
            let (icon, icon_color) = gate_type_icon(gate.step.gate_type.as_ref(), &state.theme);

            // Branch or fallback to target_label
            let location = gate
                .branch
                .as_deref()
                .or(gate.target_label.as_deref())
                .unwrap_or("");
            let location_display = truncate(location, 31);

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

            // Elapsed wait time
            let elapsed = gate
                .step
                .started_at
                .as_deref()
                .map(format_elapsed)
                .unwrap_or_default();

            let mut spans = vec![
                Span::styled(format!("{icon} "), Style::default().fg(icon_color)),
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
            if !elapsed.is_empty() {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    elapsed,
                    Style::default().fg(state.theme.label_accent),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    // Context-sensitive action hint based on selected gate type
    let title: String = if focused {
        let selected_idx = state
            .detail_gate_index
            .min(state.detail_gates.len().saturating_sub(1));
        let hint = state
            .detail_gates
            .get(selected_idx)
            .map(|gate| match gate.step.gate_type {
                Some(GateType::HumanApproval) => "Enter:approve/reject",
                Some(GateType::HumanReview) => "Enter:approve/reject",
                Some(GateType::PrChecks) => "CI running",
                Some(GateType::PrApproval) => "Waiting for PR reviews",
                None => "Enter:view",
            })
            .unwrap_or("Enter:view");
        format!(" Pending Gates  {hint} ")
    } else {
        " Pending Gates ".to_string()
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
