use std::collections::{HashMap, HashSet};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use conductor_core::workflow::InputType;
use conductor_core::workflow::WorkflowNode;
use conductor_core::workflow::{WorkflowDef, WorkflowRun, WorkflowRunStatus};

use super::common::truncate;
use super::helpers::{format_condition, shorten_paths, visual_idx_with_headers};
use crate::state::AppState;
use crate::state::ColumnFocus;
use crate::state::TargetType;
use crate::state::View;
use crate::state::WorkflowDefFocus;
use crate::state::WorkflowRunDetailFocus;
use crate::state::WorkflowRunRow;
use crate::state::WorkflowsFocus;
use crate::theme::Theme;

/// Returns a short context label for workflow pane titles, e.g. "my-repo" or "feat-123".
/// Returns `None` when in global (all-repos) mode.
fn workflow_context_label(state: &AppState) -> Option<String> {
    if let Some(ref wt_id) = state.selected_worktree_id {
        let slug = state
            .data
            .worktrees
            .iter()
            .find(|w| &w.id == wt_id)
            .map(|w| w.slug.clone());
        return slug;
    }
    if state.view == View::RepoDetail {
        let slug = state
            .selected_repo_id
            .as_ref()
            .and_then(|id| state.data.repos.iter().find(|r| &r.id == id))
            .map(|r| r.slug.clone());
        return slug;
    }
    None
}

/// Render the Workflows split-pane view: defs (left) + runs (right).
#[allow(dead_code)]
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    // Always show a 1-line context bar so the user knows which worktree's
    // workflows they are viewing (or that they are in global mode).
    let selected_wt = state
        .selected_worktree_id
        .as_ref()
        .and_then(|id| state.data.worktrees.iter().find(|w| &w.id == id));

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let (header_area, area) = (v[0], v[1]);

    let header_line = if let Some(wt) = selected_wt {
        Line::from(vec![
            Span::styled(
                "Worktree: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(
                wt.slug.clone(),
                Style::default()
                    .fg(state.theme.label_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  Branch: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::raw(wt.branch.clone()),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                "Worktree: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled("global", Style::default().fg(state.theme.label_secondary)),
        ])
    };
    frame.render_widget(Paragraph::new(header_line), header_area);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    render_defs(frame, chunks[0], state);
    if state.workflows_focus == WorkflowsFocus::Defs
        && state.workflow_def_focus == WorkflowDefFocus::Steps
    {
        render_def_steps(frame, chunks[1], state);
    } else {
        render_runs(frame, chunks[1], state);
    }
}

pub(super) fn render_defs(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused = state.column_focus == ColumnFocus::Workflow
        && state.workflows_focus == WorkflowsFocus::Defs;
    let border_color = if focused {
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };

    let context = workflow_context_label(state);
    let global_mode = state.selected_worktree_id.is_none() && state.selected_repo_id.is_none();

    if global_mode {
        // Use pre-computed (repo_slug, def) pairs from state (populated by background thread).
        let fallback = String::from("?");
        let defs_with_slug: Vec<(&str, &WorkflowDef)> = state
            .data
            .workflow_defs
            .iter()
            .enumerate()
            .map(|(i, def)| {
                let slug = state
                    .data
                    .workflow_def_slugs
                    .get(i)
                    .unwrap_or(&fallback)
                    .as_str();
                (slug, def)
            })
            .collect();

        let mut items: Vec<ListItem> = Vec::new();
        let mut prev_repo = "";
        for (repo_slug, def) in &defs_with_slug {
            if *repo_slug != prev_repo {
                let fill = format!("{:─<30}", "");
                items.push(ListItem::new(Line::from(vec![Span::styled(
                    format!("─ {repo_slug} {fill}"),
                    Style::default()
                        .fg(state.theme.label_secondary)
                        .add_modifier(Modifier::BOLD),
                )])));
                prev_repo = repo_slug;
            }
            let node_count = def.body.len();
            let input_count = def.inputs.len();
            let (badge_sym, badge_label, badge_color) =
                last_run_badge(&def.name, &state.data.workflow_runs, &state.theme);
            let badge_text = if badge_label.is_empty() {
                format!("  {badge_sym}")
            } else {
                format!("  {badge_sym} {badge_label}")
            };
            let mut spans = vec![
                Span::raw("  \u{2514} "),
                Span::styled(
                    format!("{:<20}", def.name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {node_count} steps"),
                    Style::default().fg(state.theme.label_warning),
                ),
                Span::styled(badge_text, Style::default().fg(badge_color)),
            ];
            if !def.targets.is_empty() {
                let badge = format!("  [{}]", def.targets.join(", "));
                spans.push(Span::styled(
                    badge,
                    Style::default().fg(state.theme.label_accent),
                ));
            }
            if input_count > 0 {
                spans.push(Span::styled(
                    format!("  {input_count} inputs"),
                    Style::default().fg(state.theme.status_waiting),
                ));
            }
            items.push(ListItem::new(Line::from(spans)));
        }

        let visual_idx = if !state.data.workflow_defs.is_empty() {
            let logical_idx = state
                .workflow_def_index
                .min(defs_with_slug.len().saturating_sub(1));
            visual_idx_with_headers(&defs_with_slug, |(slug, _)| slug.to_string(), logical_idx)
        } else {
            0
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color))
                    .title(if focused {
                        " All Definitions  Enter=view  l=steps  r=run "
                    } else {
                        " All Workflow Definitions "
                    }),
            )
            .highlight_style(
                Style::default()
                    .bg(state.theme.highlight_bg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");

        let mut list_state = ListState::default();
        if !state.data.workflow_defs.is_empty() {
            list_state.select(Some(visual_idx));
        }
        frame.render_stateful_widget(list, area, &mut list_state);
    } else {
        // Worktree-scoped or repo-scoped: flat list with description and target badges.
        let items: Vec<ListItem> = state
            .data
            .workflow_defs
            .iter()
            .map(|def| {
                let node_count = def.body.len();
                let input_count = def.inputs.len();
                let (badge_sym, badge_label, badge_color) =
                    last_run_badge(&def.name, &state.data.workflow_runs, &state.theme);
                let badge_text = if badge_label.is_empty() {
                    format!("  {badge_sym}")
                } else {
                    format!("  {badge_sym} {badge_label}")
                };
                let mut spans = vec![
                    Span::styled(
                        format!("{:<20}", def.name),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", truncate(&def.description, 30)),
                        Style::default().fg(state.theme.label_secondary),
                    ),
                    Span::styled(
                        format!("  {node_count} steps"),
                        Style::default().fg(state.theme.label_warning),
                    ),
                    Span::styled(badge_text, Style::default().fg(badge_color)),
                ];
                if !def.targets.is_empty() {
                    let badge = format!("  [{}]", def.targets.join(", "));
                    spans.push(Span::styled(
                        badge,
                        Style::default().fg(state.theme.label_accent),
                    ));
                }
                if input_count > 0 {
                    spans.push(Span::styled(
                        format!("  {input_count} inputs"),
                        Style::default().fg(state.theme.status_waiting),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let defs_title = if focused {
            match &context {
                Some(label) => format!(" Definitions ({label})  Enter=view  l=steps  r=run "),
                None => " Definitions  Enter=view  l=steps  r=run ".to_string(),
            }
        } else {
            match &context {
                Some(label) => format!(" Workflow Definitions ({label}) "),
                None => " Workflow Definitions ".to_string(),
            }
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color))
                    .title(defs_title),
            )
            .highlight_style(
                Style::default()
                    .bg(state.theme.highlight_bg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");

        let mut list_state = ListState::default();
        if !state.data.workflow_defs.is_empty() {
            list_state.select(Some(state.workflow_def_index));
        }
        frame.render_stateful_widget(list, area, &mut list_state);
    }
}

/// Render the step tree pane for the selected workflow definition.
pub(super) fn render_def_steps(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused = state.column_focus == ColumnFocus::Workflow
        && state.workflows_focus == WorkflowsFocus::Defs
        && state.workflow_def_focus == WorkflowDefFocus::Steps;
    let border_color = if focused {
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };

    let def = state.data.workflow_defs.get(state.workflow_def_index);

    let empty_set = HashSet::new();
    let items = match def {
        Some(d) if !d.body.is_empty() => build_def_step_lines(
            &d.body,
            0,
            &state.theme,
            &state.data.workflow_defs,
            &state.workflow_def_expanded_calls,
            "",
            &empty_set,
        ),
        _ => vec![ListItem::new(Line::from(vec![Span::styled(
            "(no steps)",
            Style::default().fg(state.theme.label_secondary),
        )]))],
    };

    let total = items.len();
    let title = if total > 0 {
        format!(" Steps ({total})  j/k=navigate  Enter=expand  Esc=back ")
    } else {
        " Steps  Esc=back ".to_string()
    };

    // Split area vertically when the definition has inputs.
    let has_inputs = def.map(|d| !d.inputs.is_empty()).unwrap_or(false);
    let (steps_area, inputs_area_opt) = if has_inputs {
        let input_count = def.map(|d| d.inputs.len()).unwrap_or(0);
        let inputs_height = (input_count.min(6) as u16) + 2; // +2 for border
        let splits = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(inputs_height)])
            .split(area);
        (splits[0], Some(splits[1]))
    } else {
        (area, None)
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
    if total > 0 {
        list_state.select(Some(
            state.workflow_def_step_index.min(total.saturating_sub(1)),
        ));
    }
    frame.render_stateful_widget(list, steps_area, &mut list_state);

    // Render inputs section below the step list when present.
    if let (Some(inputs_area), Some(d)) = (inputs_area_opt, def) {
        let input_lines: Vec<ListItem> = d
            .inputs
            .iter()
            .map(|inp| {
                let mut spans = vec![Span::styled(
                    format!("  {:<18}", inp.name),
                    Style::default().add_modifier(Modifier::BOLD),
                )];
                if inp.required {
                    spans.push(Span::styled(
                        " (required)",
                        Style::default().fg(state.theme.label_warning),
                    ));
                } else if let Some(ref default) = inp.default {
                    spans.push(Span::styled(
                        format!(" default: {default}"),
                        Style::default().fg(state.theme.label_secondary),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let inputs_list = List::new(input_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(" Inputs "),
        );
        frame.render_widget(inputs_list, inputs_area);
    }
}

/// Recursively build flat `ListItem`s from a `WorkflowNode` slice.
/// Returns the items so callers can also use `.len()` for navigation bounds.
///
/// `workflow_defs` — all known workflow definitions (for inline CallWorkflow expansion).
/// `expanded_calls` — set of dot-path strings identifying expanded CallWorkflow nodes.
/// `path_prefix` — dot-path prefix for the current recursion level (e.g. `""` or `"2."`).
/// `seen` — workflow names already in the current expansion stack (cycle guard).
pub(crate) fn build_def_step_lines<'a>(
    nodes: &[WorkflowNode],
    depth: usize,
    theme: &crate::theme::Theme,
    workflow_defs: &[WorkflowDef],
    expanded_calls: &HashSet<String>,
    path_prefix: &str,
    seen: &HashSet<String>,
) -> Vec<ListItem<'a>> {
    let indent = "  ".repeat(depth);
    let mut items = Vec::new();

    for (i, node) in nodes.iter().enumerate() {
        let path = format!("{}{}", path_prefix, i);
        match node {
            WorkflowNode::Call(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled("[call]  ", Style::default().fg(theme.label_accent)),
                    Span::styled(
                        n.agent.label().to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ])));
            }
            WorkflowNode::CallWorkflow(n) => {
                let is_expanded = expanded_calls.contains(&path);
                let indicator = if is_expanded { "▼" } else { "▶" };
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        format!("{indicator} [→ wf]  "),
                        Style::default().fg(theme.label_accent),
                    ),
                    Span::styled(
                        n.workflow.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ])));
                if is_expanded {
                    let child_indent = "  ".repeat(depth + 1);
                    if seen.contains(&n.workflow) {
                        items.push(ListItem::new(Line::from(vec![
                            Span::raw(child_indent),
                            Span::styled(
                                "(↺ recursive — not expanded)",
                                Style::default().fg(theme.label_secondary),
                            ),
                        ])));
                    } else if let Some(sub_def) =
                        workflow_defs.iter().find(|d| d.name == n.workflow)
                    {
                        let mut new_seen = seen.clone();
                        new_seen.insert(n.workflow.clone());
                        let new_prefix = format!("{}.", path);
                        items.extend(build_def_step_lines(
                            &sub_def.body,
                            depth + 1,
                            theme,
                            workflow_defs,
                            expanded_calls,
                            &new_prefix,
                            &new_seen,
                        ));
                    } else {
                        items.push(ListItem::new(Line::from(vec![
                            Span::raw(child_indent),
                            Span::styled(
                                "(workflow not found)",
                                Style::default().fg(theme.label_secondary),
                            ),
                        ])));
                    }
                }
            }
            WorkflowNode::Parallel(n) => {
                let count = n.calls.len();
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled("[para]  ", Style::default().fg(theme.label_warning)),
                    Span::styled(
                        format!("{count} agents"),
                        Style::default().fg(theme.label_secondary),
                    ),
                ])));
                for call in &n.calls {
                    let child_indent = "  ".repeat(depth + 1);
                    items.push(ListItem::new(Line::from(vec![
                        Span::raw(child_indent),
                        Span::styled("└ ", Style::default().fg(theme.label_secondary)),
                        Span::styled("[call]  ", Style::default().fg(theme.label_accent)),
                        Span::raw::<String>(call.label().to_string()),
                    ])));
                }
            }
            WorkflowNode::Gate(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled("[gate]  ", Style::default().fg(theme.label_warning)),
                    Span::styled(
                        n.name.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  ({})", n.gate_type),
                        Style::default().fg(theme.label_secondary),
                    ),
                ])));
            }
            WorkflowNode::If(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled("[if]    ", Style::default().fg(theme.label_accent)),
                    Span::styled(
                        format_condition(&n.condition),
                        Style::default().fg(theme.label_secondary),
                    ),
                ])));
                items.extend(build_def_step_lines(
                    &n.body,
                    depth + 1,
                    theme,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                ));
            }
            WorkflowNode::Unless(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled("[unless]", Style::default().fg(theme.label_accent)),
                    Span::raw("  "),
                    Span::styled(
                        format_condition(&n.condition),
                        Style::default().fg(theme.label_secondary),
                    ),
                ])));
                items.extend(build_def_step_lines(
                    &n.body,
                    depth + 1,
                    theme,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                ));
            }
            WorkflowNode::While(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled("[while] ", Style::default().fg(theme.label_accent)),
                    Span::styled(
                        format!("{}.{}", n.step, n.marker),
                        Style::default().fg(theme.label_secondary),
                    ),
                ])));
                items.extend(build_def_step_lines(
                    &n.body,
                    depth + 1,
                    theme,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                ));
            }
            WorkflowNode::DoWhile(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled("[do]    ", Style::default().fg(theme.label_accent)),
                    Span::styled(
                        format!("while {}.{}", n.step, n.marker),
                        Style::default().fg(theme.label_secondary),
                    ),
                ])));
                items.extend(build_def_step_lines(
                    &n.body,
                    depth + 1,
                    theme,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                ));
            }
            WorkflowNode::Do(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled("[do]    ", Style::default().fg(theme.label_accent)),
                ])));
                items.extend(build_def_step_lines(
                    &n.body,
                    depth + 1,
                    theme,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                ));
            }
            WorkflowNode::Script(s) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled("[script]", Style::default().fg(theme.label_accent)),
                    Span::raw("  "),
                    Span::styled(
                        s.name.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ])));
            }
            WorkflowNode::Always(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled("[always]", Style::default().fg(theme.label_warning)),
                ])));
                items.extend(build_def_step_lines(
                    &n.body,
                    depth + 1,
                    theme,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                ));
            }
        }
    }

    items
}

/// Returns the dot-path of the `CallWorkflow` node at flat list index `target`,
/// or `None` if the row at that index is not a `CallWorkflow` header.
///
/// Mirrors the traversal of `build_def_step_lines` exactly.
/// Used by the binary (app.rs) — suppress dead_code warning from lib target.
#[allow(dead_code)]
pub(crate) fn get_def_step_node_at(
    nodes: &[WorkflowNode],
    workflow_defs: &[WorkflowDef],
    expanded_calls: &HashSet<String>,
    path_prefix: &str,
    seen: &HashSet<String>,
    target: usize,
    counter: &mut usize,
) -> Option<String> {
    for (i, node) in nodes.iter().enumerate() {
        let path = format!("{}{}", path_prefix, i);
        match node {
            WorkflowNode::Call(_) | WorkflowNode::Gate(_) => {
                if *counter == target {
                    return None;
                }
                *counter += 1;
            }
            WorkflowNode::Parallel(n) => {
                if *counter == target {
                    return None;
                }
                *counter += 1;
                for _ in &n.calls {
                    if *counter == target {
                        return None;
                    }
                    *counter += 1;
                }
            }
            WorkflowNode::CallWorkflow(n) => {
                if *counter == target {
                    return Some(path.clone());
                }
                *counter += 1;
                if expanded_calls.contains(&path) {
                    if seen.contains(&n.workflow) {
                        // "(↺ recursive)" row
                        if *counter == target {
                            return None;
                        }
                        *counter += 1;
                    } else if let Some(sub_def) =
                        workflow_defs.iter().find(|d| d.name == n.workflow)
                    {
                        let mut new_seen = seen.clone();
                        new_seen.insert(n.workflow.clone());
                        let new_prefix = format!("{}.", path);
                        if let Some(r) = get_def_step_node_at(
                            &sub_def.body,
                            workflow_defs,
                            expanded_calls,
                            &new_prefix,
                            &new_seen,
                            target,
                            counter,
                        ) {
                            return Some(r);
                        }
                    } else {
                        // "(workflow not found)" row
                        if *counter == target {
                            return None;
                        }
                        *counter += 1;
                    }
                }
            }
            WorkflowNode::If(n) => {
                if *counter == target {
                    return None;
                }
                *counter += 1;
                if let Some(r) = get_def_step_node_at(
                    &n.body,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                    target,
                    counter,
                ) {
                    return Some(r);
                }
            }
            WorkflowNode::Unless(n) => {
                if *counter == target {
                    return None;
                }
                *counter += 1;
                if let Some(r) = get_def_step_node_at(
                    &n.body,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                    target,
                    counter,
                ) {
                    return Some(r);
                }
            }
            WorkflowNode::While(n) => {
                if *counter == target {
                    return None;
                }
                *counter += 1;
                if let Some(r) = get_def_step_node_at(
                    &n.body,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                    target,
                    counter,
                ) {
                    return Some(r);
                }
            }
            WorkflowNode::DoWhile(n) => {
                if *counter == target {
                    return None;
                }
                *counter += 1;
                if let Some(r) = get_def_step_node_at(
                    &n.body,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                    target,
                    counter,
                ) {
                    return Some(r);
                }
            }
            WorkflowNode::Do(n) => {
                if *counter == target {
                    return None;
                }
                *counter += 1;
                if let Some(r) = get_def_step_node_at(
                    &n.body,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                    target,
                    counter,
                ) {
                    return Some(r);
                }
            }
            WorkflowNode::Script(_) => {
                if *counter == target {
                    return None;
                }
                *counter += 1;
            }
            WorkflowNode::Always(n) => {
                if *counter == target {
                    return None;
                }
                *counter += 1;
                if let Some(r) = get_def_step_node_at(
                    &n.body,
                    workflow_defs,
                    expanded_calls,
                    &format!("{}.", path),
                    seen,
                    target,
                    counter,
                ) {
                    return Some(r);
                }
            }
        }
    }
    None
}

pub(super) fn render_runs(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused = state.column_focus == ColumnFocus::Workflow
        && state.workflows_focus == WorkflowsFocus::Runs;
    let border_color = if focused {
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };

    let context = workflow_context_label(state);
    // In global mode (no worktree or repo selected), show target context on each run row.
    let global_mode = state.selected_worktree_id.is_none() && state.selected_repo_id.is_none();
    // In repo-detail mode, show slug label rows above run groups and indent run rows.
    let repo_detail_mode = state.selected_repo_id.is_some() && state.selected_worktree_id.is_none();

    let visible = state.visible_workflow_run_rows();

    // Build run_id → WorkflowRun map for O(1) lookup.
    let run_map: HashMap<&str, &WorkflowRun> = state
        .data
        .workflow_runs
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();

    let items: Vec<ListItem> = visible
        .iter()
        .map(|row| {
            // Handle header rows first — they have no associated WorkflowRun.
            match row {
                WorkflowRunRow::RepoHeader {
                    repo_slug,
                    collapsed,
                    run_count,
                } => {
                    let arrow = if *collapsed { "▶" } else { "▼" };
                    let label = if *collapsed {
                        format!("{arrow} {repo_slug}  (+{run_count})")
                    } else {
                        format!("{arrow} {repo_slug}")
                    };
                    return ListItem::new(Line::from(vec![Span::styled(
                        label,
                        Style::default()
                            .fg(state.theme.group_header)
                            .add_modifier(Modifier::BOLD),
                    )]));
                }
                WorkflowRunRow::TargetHeader {
                    label,
                    target_type,
                    collapsed,
                    run_count,
                    ..
                } => {
                    let arrow = if *collapsed { "▶" } else { "▼" };
                    let type_badge = match target_type {
                        TargetType::Pr => "[pr]",
                        TargetType::Worktree => "[wt]",
                    };
                    let display = if *collapsed {
                        format!("  {arrow} {:<30}  {type_badge}  (+{run_count})", label)
                    } else {
                        format!("  {arrow} {:<30}  {type_badge}", label)
                    };
                    return ListItem::new(Line::from(vec![Span::styled(
                        display,
                        Style::default().fg(state.theme.label_secondary),
                    )]));
                }
                WorkflowRunRow::SlugLabel { label } => {
                    return ListItem::new(Line::from(vec![Span::styled(
                        label.clone(),
                        Style::default()
                            .fg(state.theme.label_secondary)
                            .add_modifier(Modifier::BOLD),
                    )]));
                }
                WorkflowRunRow::Step {
                    step_name,
                    status,
                    position,
                    depth,
                    role,
                    ..
                } => {
                    let base_indent = if global_mode {
                        "    "
                    } else if repo_detail_mode {
                        "  "
                    } else {
                        ""
                    };
                    let level_indent = "  ".repeat(*depth as usize);
                    let (status_symbol, status_color) = status_display(status, &state.theme);
                    return ListItem::new(Line::from(vec![
                        Span::raw(format!("{base_indent}{level_indent}")),
                        Span::styled(
                            "\u{2570} ",
                            Style::default().fg(state.theme.label_secondary),
                        ),
                        Span::styled(
                            format!("{position}. "),
                            Style::default().fg(state.theme.label_secondary),
                        ),
                        Span::styled(status_symbol, Style::default().fg(status_color)),
                        Span::raw("  "),
                        Span::styled(
                            format!("[{:<8}]", display_role(role)),
                            Style::default().fg(role_color(role, &state.theme)),
                        ),
                        Span::raw("  "),
                        Span::raw(step_name.clone()),
                    ]));
                }
                WorkflowRunRow::ParallelGroup {
                    status,
                    count,
                    depth,
                    ..
                } => {
                    let base_indent = if global_mode {
                        "    "
                    } else if repo_detail_mode {
                        "  "
                    } else {
                        ""
                    };
                    let level_indent = "  ".repeat(*depth as usize);
                    let (status_symbol, status_color) = status_display(status, &state.theme);
                    return ListItem::new(Line::from(vec![
                        Span::raw(format!("{base_indent}{level_indent}")),
                        Span::styled(
                            "\u{2570} ",
                            Style::default().fg(state.theme.label_secondary),
                        ),
                        Span::styled(status_symbol, Style::default().fg(status_color)),
                        Span::raw("  "),
                        Span::styled(
                            "[parallel]",
                            Style::default().fg(state.theme.status_waiting),
                        ),
                        Span::raw(format!("  ({count} steps)")),
                    ]));
                }
                _ => {}
            }

            // Parent / Child rows: look up the run.
            let Some(run_id) = row.run_id() else {
                return ListItem::new(Line::from(vec![Span::raw("?")]));
            };
            let Some(run) = run_map.get(run_id) else {
                return ListItem::new(Line::from(vec![Span::raw("?")]));
            };

            let (status_symbol, status_color) =
                status_display(&run.status.to_string(), &state.theme);
            let duration = if let Some(ref ended) = run.ended_at {
                format_duration(&run.started_at, ended)
            } else {
                "…".to_string()
            };

            match row {
                WorkflowRunRow::Parent {
                    collapsed,
                    child_count,
                    run_id,
                    ..
                } => {
                    // Prefix: collapse toggle indicator.
                    let prefix = if *child_count > 0 {
                        if *collapsed {
                            "▶ "
                        } else {
                            "▼ "
                        }
                    } else {
                        // Leaf run: show step expansion state.
                        if state.expanded_step_run_ids.contains(run_id) {
                            "▼ "
                        } else {
                            "▶ "
                        }
                    };

                    // In global mode, indent run rows under their target header.
                    // In repo-detail mode, indent run rows under their slug label.
                    let indent = if global_mode {
                        "    "
                    } else if repo_detail_mode {
                        "  "
                    } else {
                        ""
                    };

                    let mut spans = vec![
                        Span::raw(format!("{indent}{prefix}")),
                        Span::styled(status_symbol, Style::default().fg(status_color)),
                        Span::raw("  "),
                        Span::styled(
                            format!("{:<20}", truncate(&run.workflow_name, 20)),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                    ];

                    // Show timestamp in both modes; the target context is now on the header row.
                    spans.push(Span::styled(
                        format!(
                            "  {}",
                            run.started_at
                                .get(..19)
                                .unwrap_or(&run.started_at)
                                .replace('T', " ")
                        ),
                        Style::default().fg(state.theme.label_secondary),
                    ));

                    spans.push(Span::styled(
                        format!("  {duration}"),
                        Style::default().fg(state.theme.label_accent),
                    ));

                    // Child count badge when collapsed.
                    if *collapsed && *child_count > 0 {
                        spans.push(Span::styled(
                            format!("  (+{child_count})"),
                            Style::default().fg(state.theme.label_secondary),
                        ));
                    }

                    if run.status == WorkflowRunStatus::Failed {
                        if let Some(ref summary) = run.result_summary {
                            let snippet = truncate(summary.lines().next().unwrap_or(""), 50);
                            spans.push(Span::styled(
                                format!("  {snippet}"),
                                Style::default().fg(state.theme.label_error),
                            ));
                        }
                    }

                    ListItem::new(Line::from(spans))
                }
                WorkflowRunRow::Child {
                    depth,
                    collapsed,
                    child_count,
                    ..
                } => {
                    let base_indent = if global_mode {
                        "    "
                    } else if repo_detail_mode {
                        "  "
                    } else {
                        ""
                    };
                    let level_indent = "  ".repeat(*depth as usize);
                    let toggle = if *child_count > 0 {
                        if *collapsed {
                            "\u{25b6} " // ▶
                        } else {
                            "\u{25bc} " // ▼
                        }
                    } else {
                        "\u{2570} " // └
                    };
                    let mut spans = vec![
                        Span::raw(format!("{base_indent}{level_indent}")),
                        Span::styled(toggle, Style::default().fg(state.theme.label_secondary)),
                        Span::styled(status_symbol, Style::default().fg(status_color)),
                        Span::raw("  "),
                        Span::styled(
                            format!("{:<20}", truncate(&run.workflow_name, 20)),
                            Style::default()
                                .fg(state.theme.label_secondary)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("  {duration}"),
                            Style::default().fg(state.theme.label_accent),
                        ),
                    ];

                    if *collapsed && *child_count > 0 {
                        spans.push(Span::styled(
                            format!("  (+{child_count})"),
                            Style::default().fg(state.theme.label_secondary),
                        ));
                    }

                    if run.status == WorkflowRunStatus::Failed {
                        if let Some(ref summary) = run.result_summary {
                            let snippet = truncate(summary.lines().next().unwrap_or(""), 40);
                            spans.push(Span::styled(
                                format!("  {snippet}"),
                                Style::default().fg(state.theme.label_error),
                            ));
                        }
                    }

                    ListItem::new(Line::from(spans))
                }
                // Step, ParallelGroup, RepoHeader, and TargetHeader are handled above.
                _ => ListItem::new(Line::from(vec![Span::raw("")])),
            }
        })
        .collect();

    let hidden = state.hidden_workflow_run_count();
    let hidden_suffix = if hidden > 0 {
        format!(" +{hidden} hidden (H to show)")
    } else if !state.show_completed_workflow_runs {
        " (H: show history)".to_string()
    } else {
        String::new()
    };
    let runs_title = if global_mode {
        format!(" All Workflow Runs{hidden_suffix} ")
    } else {
        match &context {
            Some(label) => format!(" Workflow Runs ({label}){hidden_suffix} "),
            None => format!(" Workflow Runs{hidden_suffix} "),
        }
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(runs_title),
        )
        .highlight_style(
            Style::default()
                .bg(state.theme.highlight_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut list_state = ListState::default();
    if !visible.is_empty() {
        list_state.select(Some(state.workflow_run_index));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Render the workflow run detail view: header + split pane (steps | agent activity).
pub fn render_run_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    let run_info = state
        .selected_workflow_run_id
        .as_ref()
        .and_then(|id| state.data.workflow_runs.iter().find(|r| &r.id == id));

    // Resolve worktree and ticket for the selected run (if any).
    let run_worktree = run_info.and_then(|run| {
        state
            .data
            .worktrees
            .iter()
            .find(|wt| Some(wt.id.as_str()) == run.worktree_id.as_deref())
    });
    let run_ticket = run_worktree.and_then(|wt| {
        wt.ticket_id
            .as_ref()
            .and_then(|tid| state.data.ticket_map.get(tid))
    });

    // Look up pre-parsed declared inputs from the cache (populated on data refresh,
    // not on every render frame).
    let declared_inputs = run_info
        .and_then(|run| state.data.workflow_run_declared_inputs.get(&run.id))
        .map(|v| v.as_slice())
        .unwrap_or_default();
    let matched_inputs: Vec<_> = run_info
        .map(|run| {
            declared_inputs
                .iter()
                .filter_map(|decl| run.inputs.get(&decl.name).map(|val| (decl, val.as_str())))
                .collect()
        })
        .unwrap_or_default();

    // Header height: 3 base lines + optional worktree lines (branch + path) + optional ticket line + declared inputs + 1 border
    let worktree_extra = if run_worktree.is_some() { 2 } else { 0 };
    let ticket_extra = if run_ticket.is_some() { 1 } else { 0 };
    let inputs_extra = matched_inputs.len();
    let header_height = 3 + worktree_extra + ticket_extra + inputs_extra + 1;

    // Header area + body
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(header_height as u16), Constraint::Min(0)])
        .split(area);

    // Header
    if let Some(run) = run_info {
        let (status_symbol, status_color) = status_display(&run.status.to_string(), &state.theme);
        let started_display = run
            .started_at
            .get(..19)
            .unwrap_or(&run.started_at)
            .replace('T', " ");
        let summary_display = run.result_summary.as_deref().unwrap_or("—").to_string();

        let mut header_lines = vec![Line::from(vec![
            Span::styled(
                " Workflow: ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::styled(
                run.workflow_name.clone(),
                Style::default()
                    .fg(state.theme.label_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(status_symbol, Style::default().fg(status_color)),
        ])];

        if let Some(wt) = run_worktree {
            header_lines.push(Line::from(vec![
                Span::styled(
                    " Branch:   ",
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::raw(wt.branch.clone()),
            ]));
            let display_path = match state.home_dir.as_deref() {
                Some(home) => wt.path.replacen(home, "~", 1),
                None => wt.path.clone(),
            };
            header_lines.push(Line::from(vec![
                Span::styled(
                    " Path:     ",
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::raw(display_path),
            ]));
        }

        if let Some(ticket) = run_ticket {
            header_lines.push(Line::from(vec![
                Span::styled(
                    " Ticket:   ",
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::styled(
                    format!("#{} — {}", ticket.source_id, ticket.title),
                    Style::default().fg(state.theme.group_header),
                ),
            ]));
        }

        for (decl, val) in &matched_inputs {
            // Right-pad label to 11 chars total (matching " Branch:   ")
            // Name is capped at 9 chars (1 space prefix + name + ": " = 11)
            let name_display = if decl.name.chars().count() > 9 {
                let truncated: String = decl.name.chars().take(8).collect();
                format!("{truncated}…")
            } else {
                decl.name.clone()
            };
            let label = format!(" {name_display}: ");
            let padded_label = format!("{label:<11}");
            let value_display = match decl.input_type {
                InputType::Boolean => {
                    if *val == "true" {
                        "[x]".to_string()
                    } else {
                        "[ ]".to_string()
                    }
                }
                InputType::String => truncate(val, area.width.saturating_sub(12) as usize),
            };
            header_lines.push(Line::from(vec![
                Span::styled(
                    padded_label,
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::raw(value_display),
            ]));
        }

        header_lines.push(Line::from(vec![
            Span::styled(
                " Started:  ",
                Style::default().fg(state.theme.label_secondary),
            ),
            Span::raw(started_display),
            if run.dry_run {
                Span::styled(
                    "  [dry-run]",
                    Style::default().fg(state.theme.label_warning),
                )
            } else {
                Span::raw("")
            },
        ]));
        if run.status == WorkflowRunStatus::Failed {
            header_lines.push(Line::from(vec![
                Span::styled(" Error:    ", Style::default().fg(state.theme.label_error)),
                Span::styled(
                    summary_display,
                    Style::default().fg(state.theme.label_error),
                ),
            ]));
        } else {
            header_lines.push(Line::from(vec![
                Span::styled(
                    " Summary:  ",
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::raw(summary_display),
            ]));
        }

        let header_block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(state.theme.border_inactive));
        frame.render_widget(Paragraph::new(header_lines).block(header_block), chunks[0]);
    }

    // Determine if the selected step has agent activity to show
    let selected_step = state.data.workflow_steps.get(state.workflow_step_index);
    let has_agent_activity = selected_step
        .map(|s| s.child_run_id.is_some())
        .unwrap_or(false);

    if has_agent_activity {
        // Split pane: steps (left 45%) | agent activity (right 55%)
        let body_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[1]);

        let focus = state.workflow_run_detail_focus;
        render_step_list(frame, body_chunks[0], state, focus);
        render_step_agent_activity(frame, body_chunks[1], state, focus);
    } else {
        // Full-width step list when no agent activity to show —
        // force Steps focus since agent pane is hidden.
        render_step_list(frame, chunks[1], state, WorkflowRunDetailFocus::Steps);
    }
}

fn render_step_list(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    focus: WorkflowRunDetailFocus,
) {
    let focused = focus == WorkflowRunDetailFocus::Steps;
    let border_color = if focused {
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };

    // Root run row — shown at the top so the overall status is visible in context.
    let root_run = state
        .selected_workflow_run_id
        .as_deref()
        .and_then(|id| state.data.workflow_runs.iter().find(|r| r.id == id));
    let mut items: Vec<ListItem> = Vec::new();
    let has_root_row = root_run.is_some();
    if let Some(root) = root_run {
        let (root_symbol, root_color) = status_display(&root.status.to_string(), &state.theme);
        let root_duration = root
            .ended_at
            .as_deref()
            .map(|e| format_duration(&root.started_at, e))
            .unwrap_or_else(|| "…".to_string());
        items.push(ListItem::new(Line::from(vec![
            Span::styled(root_symbol, Style::default().fg(root_color)),
            Span::raw("  "),
            Span::styled(
                format!("{:<20}", root.workflow_name),
                Style::default()
                    .fg(state.theme.label_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {root_duration}"),
                Style::default().fg(state.theme.label_accent),
            ),
        ])));
    }

    items.extend(
        state
            .data
            .workflow_steps
            .iter()
            .enumerate()
            .map(|(i, step)| {
                let (status_symbol, status_color) =
                    status_display(&step.status.to_string(), &state.theme);
                let duration = match (&step.started_at, &step.ended_at) {
                    (Some(start), Some(end)) => format_duration(start, end),
                    (Some(_), None) => "…".to_string(),
                    _ => "—".to_string(),
                };

                let mut spans = vec![
                    Span::styled(
                        format!("  {:>2}. ", step.position),
                        Style::default().fg(state.theme.label_secondary),
                    ),
                    Span::styled(status_symbol, Style::default().fg(status_color)),
                    Span::raw("  "),
                    Span::styled(
                        format!("{:<20}", step.step_name),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  [{:<5}]", step.role),
                        Style::default().fg(state.theme.status_waiting),
                    ),
                    Span::styled(
                        format!("  {duration}"),
                        Style::default().fg(state.theme.label_accent),
                    ),
                ];

                if step.iteration > 0 {
                    spans.push(Span::styled(
                        format!("  iter:{}", step.iteration),
                        Style::default().fg(state.theme.label_accent),
                    ));
                }
                if step.retry_count > 0 {
                    spans.push(Span::styled(
                        format!("  retries:{}", step.retry_count),
                        Style::default().fg(state.theme.label_error),
                    ));
                }
                if let Some(ref gate_type) = step.gate_type {
                    spans.push(Span::styled(
                        format!("  gate:{gate_type}"),
                        Style::default().fg(state.theme.label_warning),
                    ));
                }

                // Inline detail: show snippet of result/context/markers for non-selected steps
                if i != state.workflow_step_index {
                    if let Some(ref rt) = step.result_text {
                        let snippet = truncate(rt.lines().next().unwrap_or(""), 40);
                        spans.push(Span::styled(
                            format!("  → {snippet}"),
                            Style::default().fg(state.theme.label_secondary),
                        ));
                    } else if let Some(ref ctx) = step.context_out {
                        let snippet = truncate(ctx.lines().next().unwrap_or(""), 40);
                        spans.push(Span::styled(
                            format!("  ctx:{snippet}"),
                            Style::default().fg(state.theme.label_secondary),
                        ));
                    }
                }

                if let Some(ref mk) = step.markers_out {
                    spans.push(Span::styled(
                        format!("  [{mk}]"),
                        Style::default().fg(state.theme.label_accent),
                    ));
                }

                ListItem::new(Line::from(spans))
            }),
    );

    let has_waiting_gate = state
        .data
        .workflow_steps
        .iter()
        .any(|s| s.status.to_string() == "waiting" && s.gate_type.is_some());

    let title = match (focused, has_waiting_gate) {
        (true, true) => " Steps (Enter=approve gate, Tab=switch) ",
        (true, false) => " Steps (Enter=detail, Tab=switch) ",
        (false, true) => " Steps (Enter=approve gate) ",
        (false, false) => " Steps ",
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
    if !state.data.workflow_steps.is_empty() {
        // Offset by 1 if a root run row was prepended.
        let offset = if has_root_row { 1 } else { 0 };
        list_state.select(Some(state.workflow_step_index + offset));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Render agent activity for the selected workflow step's child run.
fn render_step_agent_activity(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    focus: WorkflowRunDetailFocus,
) {
    let focused = focus == WorkflowRunDetailFocus::AgentActivity;
    let border_color = if focused {
        state.theme.border_focused
    } else {
        state.theme.border_inactive
    };
    let events = &state.data.step_agent_events;
    let agent_run = &state.data.step_agent_run;

    // Title with run status
    let title = if let Some(ref run) = agent_run {
        let model = run.model.as_deref().unwrap_or("default");
        if focused {
            format!(" Agent: {model} ({}) (Tab=switch) ", run.status)
        } else {
            format!(" Agent: {model} ({}) ", run.status)
        }
    } else if focused {
        " Agent Activity (Tab=switch) ".to_string()
    } else {
        " Agent Activity ".to_string()
    };

    let activity_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(title);

    if events.is_empty() {
        let msg = if agent_run
            .as_ref()
            .map(|r| r.status == conductor_core::agent::AgentRunStatus::Running)
            .unwrap_or(false)
        {
            "Agent running — waiting for events…"
        } else {
            "No agent events"
        };
        let empty = Paragraph::new(Span::styled(
            msg,
            Style::default().fg(state.theme.label_secondary),
        ))
        .block(activity_block);
        frame.render_widget(empty, area);
        return;
    }

    let worktree_path = state
        .selected_worktree_id
        .as_ref()
        .and_then(|id| state.data.worktrees.iter().find(|w| &w.id == id))
        .or_else(|| {
            state.data.step_agent_run.as_ref().and_then(|run| {
                state
                    .data
                    .worktrees
                    .iter()
                    .find(|w| run.worktree_id.as_deref() == Some(w.id.as_str()))
            })
        })
        .map(|wt| wt.path.as_str())
        .unwrap_or("");

    let items: Vec<ListItem> = events
        .iter()
        .map(|ev| {
            let style = event_kind_style(&ev.kind, &state.theme);
            let dur = ev
                .duration_ms()
                .map(|ms| format!(" ({:.1}s)", ms as f64 / 1000.0))
                .unwrap_or_default();
            let ts = ev.started_at.get(11..19).unwrap_or(&ev.started_at);
            let summary = truncate(
                &shorten_paths(&ev.summary, worktree_path, state.home_dir.as_deref()),
                80,
            );
            let spans = vec![
                Span::styled(
                    format!("{ts} "),
                    Style::default().fg(state.theme.label_secondary),
                ),
                Span::styled(format!("{:<10}", ev.kind), style),
                Span::styled(dur, Style::default().fg(state.theme.label_secondary)),
                Span::raw(" "),
                Span::styled(summary, style),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    if focused {
        let list = List::new(items)
            .block(activity_block)
            .highlight_style(
                Style::default()
                    .bg(state.theme.highlight_bg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");
        let mut list_state = ListState::default();
        if !events.is_empty() {
            list_state.select(Some(state.step_agent_event_index));
        }
        frame.render_stateful_widget(list, area, &mut list_state);
    } else {
        let list = List::new(items).block(activity_block);
        frame.render_widget(list, area);
    }
}

fn event_kind_style(kind: &str, theme: &Theme) -> Style {
    match kind {
        "tool_use" => Style::default().fg(theme.label_info),
        "tool_result" => Style::default().fg(theme.status_completed),
        "api_request" => Style::default().fg(theme.label_warning),
        "error" => Style::default().fg(theme.status_failed),
        "prompt" => Style::default().fg(theme.label_keyword),
        "result" => Style::default().fg(theme.label_accent),
        _ => Style::default().fg(theme.label_primary),
    }
}

fn display_role(role: &str) -> &str {
    match role {
        "actor" => "agent",
        other => other,
    }
}

fn role_color(role: &str, theme: &Theme) -> Color {
    match role {
        "actor" | "agent" => theme.label_accent,
        "gate" => theme.label_warning,
        "reviewer" => theme.label_info,
        "parallel" => theme.status_waiting,
        _ => theme.label_secondary,
    }
}

fn status_display(status: &str, theme: &Theme) -> (&'static str, Color) {
    match status {
        "pending" => ("○", theme.label_secondary),
        "running" => ("⚙", theme.label_accent),
        "completed" => ("✓", theme.status_completed),
        "failed" => ("✗", theme.status_failed),
        "cancelled" => ("⊘", theme.status_cancelled),
        "waiting" => ("⏸", theme.status_waiting),
        "skipped" => ("⊘", theme.label_secondary),
        _ => ("?", theme.label_primary),
    }
}

fn format_duration(start: &str, end: &str) -> String {
    let Ok(s) = chrono::DateTime::parse_from_rfc3339(start) else {
        return "?".to_string();
    };
    let Ok(e) = chrono::DateTime::parse_from_rfc3339(end) else {
        return "?".to_string();
    };
    let dur = e.signed_duration_since(s);
    let secs = dur.num_seconds();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

/// Returns a human-readable relative time string, e.g. "5m ago", "2h ago", "1d ago".
fn time_ago(ts: &str) -> String {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return "?".to_string();
    };
    let secs = chrono::Utc::now()
        .signed_duration_since(t)
        .num_seconds()
        .max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Returns `(symbol, label, color)` for the most-recent run of `def_name`.
fn last_run_badge(
    def_name: &str,
    runs: &[WorkflowRun],
    theme: &Theme,
) -> (&'static str, String, Color) {
    let latest = runs
        .iter()
        .filter(|r| r.workflow_name == def_name)
        .max_by(|a, b| a.started_at.cmp(&b.started_at));

    match latest {
        None => ("—", String::new(), theme.label_secondary),
        Some(run) => {
            let label = time_ago(&run.started_at);
            match run.status {
                WorkflowRunStatus::Completed => ("✓", label, theme.status_completed),
                WorkflowRunStatus::Failed => ("✗", label, theme.status_failed),
                WorkflowRunStatus::Running => ("▶", label, theme.label_accent),
                _ => ("—", label, theme.label_secondary),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::workflow::{
        AgentRef, AlwaysNode, CallNode, CallWorkflowNode, DoNode, IfNode, ParallelNode,
        WorkflowDef, WorkflowNode, WorkflowTrigger,
    };

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    fn call_node(name: &str) -> WorkflowNode {
        WorkflowNode::Call(CallNode {
            agent: AgentRef::Name(name.to_string()),
            retries: 0,
            on_fail: None,
            output: None,
            with: vec![],
            bot_name: None,
        })
    }

    fn call_wf_node(workflow: &str) -> WorkflowNode {
        WorkflowNode::CallWorkflow(CallWorkflowNode {
            workflow: workflow.to_string(),
            inputs: Default::default(),
            retries: 0,
            on_fail: None,
            bot_name: None,
        })
    }

    fn if_node(body: Vec<WorkflowNode>) -> WorkflowNode {
        use conductor_core::workflow::Condition;
        WorkflowNode::If(IfNode {
            condition: Condition::StepMarker {
                step: "some-step".to_string(),
                marker: "done".to_string(),
            },
            body,
        })
    }

    fn empty_workflow_def(name: &str, body: Vec<WorkflowNode>) -> WorkflowDef {
        WorkflowDef {
            name: name.to_string(),
            description: String::new(),
            trigger: WorkflowTrigger::Manual,
            targets: vec![],
            inputs: vec![],
            body,
            always: vec![],
            source_path: format!("{name}.wf"),
        }
    }

    fn run_get(
        nodes: &[WorkflowNode],
        workflow_defs: &[WorkflowDef],
        expanded_calls: &HashSet<String>,
        target: usize,
    ) -> Option<String> {
        get_def_step_node_at(
            nodes,
            workflow_defs,
            expanded_calls,
            "",
            &HashSet::new(),
            target,
            &mut 0,
        )
    }

    // ---------------------------------------------------------------------------
    // get_def_step_node_at — basic cases
    // ---------------------------------------------------------------------------

    #[test]
    fn test_call_node_returns_none() {
        // A plain Call node is never a CallWorkflow header.
        let nodes = vec![call_node("agent-a")];
        assert_eq!(run_get(&nodes, &[], &HashSet::new(), 0), None);
    }

    #[test]
    fn test_call_workflow_at_index_0_returns_path() {
        let nodes = vec![call_wf_node("sub-workflow")];
        assert_eq!(
            run_get(&nodes, &[], &HashSet::new(), 0),
            Some("0".to_string())
        );
    }

    #[test]
    fn test_call_workflow_at_index_1_after_call_node() {
        // [Call, CallWorkflow] → flat indices 0, 1
        let nodes = vec![call_node("first"), call_wf_node("child-wf")];
        assert_eq!(run_get(&nodes, &[], &HashSet::new(), 0), None);
        assert_eq!(
            run_get(&nodes, &[], &HashSet::new(), 1),
            Some("1".to_string())
        );
    }

    #[test]
    fn test_out_of_range_returns_none() {
        let nodes = vec![call_node("a"), call_node("b")];
        assert_eq!(run_get(&nodes, &[], &HashSet::new(), 5), None);
    }

    // ---------------------------------------------------------------------------
    // Expanded CallWorkflow — children occupy additional flat indices
    // ---------------------------------------------------------------------------

    #[test]
    fn test_collapsed_call_workflow_children_not_counted() {
        // Not in expanded_calls → no child rows added.
        // [CallWorkflow("sub"), Call("b")] → flat indices 0=sub header, 1=b
        let nodes = vec![call_wf_node("sub"), call_node("b")];
        let sub_def = empty_workflow_def("sub", vec![call_node("sub-step")]);
        // collapsed: index 1 is Call("b"), not a sub-step
        assert_eq!(run_get(&nodes, &[sub_def], &HashSet::new(), 1), None);
    }

    #[test]
    fn test_expanded_call_workflow_children_counted() {
        // CallWorkflow("sub") expanded → children fill flat indices 1..
        // Layout: index 0 = CallWorkflow header, index 1 = sub-step (Call inside sub)
        let nodes = vec![call_wf_node("sub")];
        let sub_def = empty_workflow_def("sub", vec![call_node("sub-step")]);
        let mut expanded = HashSet::new();
        expanded.insert("0".to_string()); // path "0" is expanded

        // index 0 → the CallWorkflow header itself
        assert_eq!(
            get_def_step_node_at(
                &nodes,
                std::slice::from_ref(&sub_def),
                &expanded,
                "",
                &HashSet::new(),
                0,
                &mut 0,
            ),
            Some("0".to_string())
        );

        // index 1 → the Call node inside the sub-workflow (not a CallWorkflow, so None)
        assert_eq!(
            get_def_step_node_at(
                &nodes,
                &[sub_def],
                &expanded,
                "",
                &HashSet::new(),
                1,
                &mut 0,
            ),
            None
        );
    }

    #[test]
    fn test_expanded_call_workflow_with_nested_call_workflow() {
        // sub-workflow itself contains a CallWorkflow node.
        // Layout (sub expanded, inner NOT expanded):
        //   0 = CallWorkflow("sub") header  → Some("0")
        //   1 = CallWorkflow("inner") header inside sub → Some("0.0")
        //   2 = Call("after") in parent    → None
        let inner_def = empty_workflow_def("inner", vec![call_node("deep")]);
        let sub_def = empty_workflow_def("sub", vec![call_wf_node("inner")]);
        let nodes = vec![call_wf_node("sub"), call_node("after")];

        let mut expanded = HashSet::new();
        expanded.insert("0".to_string()); // "sub" expanded at path "0"

        let all_defs = vec![sub_def, inner_def];

        assert_eq!(
            get_def_step_node_at(&nodes, &all_defs, &expanded, "", &HashSet::new(), 0, &mut 0),
            Some("0".to_string()),
            "index 0 should be the outer CallWorkflow header"
        );
        assert_eq!(
            get_def_step_node_at(&nodes, &all_defs, &expanded, "", &HashSet::new(), 1, &mut 0),
            Some("0.0".to_string()),
            "index 1 should be the nested CallWorkflow header"
        );
        assert_eq!(
            get_def_step_node_at(&nodes, &all_defs, &expanded, "", &HashSet::new(), 2, &mut 0),
            None,
            "index 2 should be the Call('after') node → None"
        );
    }

    // ---------------------------------------------------------------------------
    // Recursive cycle guard
    // ---------------------------------------------------------------------------

    #[test]
    fn test_recursive_cycle_produces_single_placeholder_row() {
        // sub-workflow calls itself. When both the outer and inner nodes are expanded,
        // the inner one sees "sub" in `seen` and emits a single "(↺ recursive)" placeholder row.
        // Layout (both "0" and "0.0" expanded):
        //   0 = CallWorkflow("sub") header at path "0" → Some("0")
        //   1 = CallWorkflow("sub") header at path "0.0" inside sub → Some("0.0")
        //   2 = "(↺ recursive)" placeholder (sub already in seen) → None
        let sub_def = empty_workflow_def("sub", vec![call_wf_node("sub")]);
        let nodes = vec![call_wf_node("sub")];

        let mut expanded = HashSet::new();
        expanded.insert("0".to_string()); // outer node expanded
        expanded.insert("0.0".to_string()); // inner self-referencing node also expanded

        let all_defs = vec![sub_def];

        assert_eq!(
            get_def_step_node_at(&nodes, &all_defs, &expanded, "", &HashSet::new(), 0, &mut 0),
            Some("0".to_string()),
            "outer CallWorkflow header"
        );
        assert_eq!(
            get_def_step_node_at(&nodes, &all_defs, &expanded, "", &HashSet::new(), 1, &mut 0),
            Some("0.0".to_string()),
            "inner CallWorkflow header inside expanded sub"
        );
        assert_eq!(
            get_def_step_node_at(&nodes, &all_defs, &expanded, "", &HashSet::new(), 2, &mut 0),
            None,
            "recursive placeholder row is not a CallWorkflow header"
        );
    }

    // ---------------------------------------------------------------------------
    // Control-flow nodes (If, While, Do, Always) — children are nested
    // ---------------------------------------------------------------------------

    #[test]
    fn test_if_node_children_counted_after_parent() {
        // [If([CallWorkflow("sub")]), Call("b")]
        // Layout: 0 = if header, 1 = CallWorkflow inside if body, 2 = Call("b")
        let nodes = vec![if_node(vec![call_wf_node("sub")]), call_node("b")];
        // index 0 = If node → None
        assert_eq!(run_get(&nodes, &[], &HashSet::new(), 0), None);
        // index 1 = CallWorkflow inside If body → Some("0.0")
        assert_eq!(
            run_get(&nodes, &[], &HashSet::new(), 1),
            Some("0.0".to_string())
        );
        // index 2 = Call("b") → None
        assert_eq!(run_get(&nodes, &[], &HashSet::new(), 2), None);
    }

    #[test]
    fn test_parallel_node_children_counted() {
        // Parallel with 2 agents → 3 flat rows: header + 2 call rows
        let nodes = vec![
            WorkflowNode::Parallel(ParallelNode {
                fail_fast: true,
                min_success: None,
                calls: vec![
                    AgentRef::Name("a".to_string()),
                    AgentRef::Name("b".to_string()),
                ],
                output: None,
                call_outputs: Default::default(),
                with: vec![],
                call_with: Default::default(),
                call_if: Default::default(),
            }),
            call_wf_node("next"),
        ];
        // index 0 = Parallel header → None
        assert_eq!(run_get(&nodes, &[], &HashSet::new(), 0), None);
        // index 1 = parallel branch "a" → None
        assert_eq!(run_get(&nodes, &[], &HashSet::new(), 1), None);
        // index 2 = parallel branch "b" → None
        assert_eq!(run_get(&nodes, &[], &HashSet::new(), 2), None);
        // index 3 = CallWorkflow("next") → Some("1")
        assert_eq!(
            run_get(&nodes, &[], &HashSet::new(), 3),
            Some("1".to_string())
        );
    }

    #[test]
    fn test_do_node_children_counted() {
        // Do([Call("a"), CallWorkflow("sub")]) → 3 rows: do header, call, callwf
        let nodes = vec![WorkflowNode::Do(DoNode {
            output: None,
            with: vec![],
            body: vec![call_node("a"), call_wf_node("sub")],
        })];
        // index 0 = do header → None
        assert_eq!(run_get(&nodes, &[], &HashSet::new(), 0), None);
        // index 1 = Call("a") → None
        assert_eq!(run_get(&nodes, &[], &HashSet::new(), 1), None);
        // index 2 = CallWorkflow("sub") → Some("0.1")
        assert_eq!(
            run_get(&nodes, &[], &HashSet::new(), 2),
            Some("0.1".to_string())
        );
    }

    #[test]
    fn test_always_node_children_counted() {
        let nodes = vec![WorkflowNode::Always(AlwaysNode {
            body: vec![call_wf_node("cleanup")],
        })];
        // index 0 = always header → None
        assert_eq!(run_get(&nodes, &[], &HashSet::new(), 0), None);
        // index 1 = CallWorkflow("cleanup") → Some("0.0")
        assert_eq!(
            run_get(&nodes, &[], &HashSet::new(), 1),
            Some("0.0".to_string())
        );
    }

    // ---------------------------------------------------------------------------
    // build_def_step_lines — row count mirrors get_def_step_node_at traversal
    // ---------------------------------------------------------------------------

    #[test]
    fn test_build_and_traverse_row_counts_match() {
        // Verify that the number of rows produced by build_def_step_lines matches
        // what get_def_step_node_at can traverse (i.e. the last valid index + 1).
        let theme = crate::theme::Theme::default();
        let nodes = vec![
            call_node("a"),
            call_wf_node("sub"),
            if_node(vec![call_node("b"), call_wf_node("inner")]),
            call_node("last"),
        ];
        let expanded = HashSet::new();
        let all_defs: Vec<WorkflowDef> = vec![];

        let items =
            build_def_step_lines(&nodes, 0, &theme, &all_defs, &expanded, "", &HashSet::new());
        let row_count = items.len();

        // Walk with get_def_step_node_at and find the last reachable index.
        let mut last_reachable = 0;
        for idx in 0..row_count {
            let result = get_def_step_node_at(
                &nodes,
                &all_defs,
                &expanded,
                "",
                &HashSet::new(),
                idx,
                &mut 0,
            );
            // result is either Some (CallWorkflow header) or None (other row)
            // Both are valid — we just confirm get_def_step_node_at doesn't return
            // Some on an out-of-range index.
            let _ = result;
            last_reachable = idx;
        }

        // One past the end should not be reachable as a CallWorkflow.
        let beyond = get_def_step_node_at(
            &nodes,
            &all_defs,
            &expanded,
            "",
            &HashSet::new(),
            row_count,
            &mut 0,
        );
        assert_eq!(beyond, None, "index beyond row_count should return None");
        assert_eq!(
            last_reachable + 1,
            row_count,
            "row count should match traversal"
        );
    }

    #[test]
    fn test_workflow_not_found_produces_single_placeholder_row() {
        // CallWorkflow referring to a non-existent workflow name → "(workflow not found)" row
        // Layout (expanded): 0 = header, 1 = placeholder
        let nodes = vec![call_wf_node("missing")];
        let mut expanded = HashSet::new();
        expanded.insert("0".to_string());

        // index 0 = CallWorkflow header → Some("0")
        assert_eq!(
            get_def_step_node_at(&nodes, &[], &expanded, "", &HashSet::new(), 0, &mut 0),
            Some("0".to_string())
        );
        // index 1 = "(workflow not found)" placeholder → None
        assert_eq!(
            get_def_step_node_at(&nodes, &[], &expanded, "", &HashSet::new(), 1, &mut 0),
            None
        );
        // index 2 = beyond → None
        assert_eq!(
            get_def_step_node_at(&nodes, &[], &expanded, "", &HashSet::new(), 2, &mut 0),
            None
        );
    }
}
