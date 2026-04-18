use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use std::collections::HashSet;

use conductor_core::workflow::{AgentRef, OnFail, WorkflowDef, WorkflowNode};

use crate::state::AppState;
use crate::ui::helpers::format_condition;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let Some(ref def) = state.selected_workflow_def else {
        let p = Paragraph::new("No workflow definition selected.").block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Workflow Definition "),
        );
        frame.render_widget(p, area);
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    render_meta(frame, chunks[0], def, state);
    render_steps(frame, chunks[1], def, state);
}

fn render_meta(frame: &mut Frame, area: Rect, def: &WorkflowDef, state: &AppState) {
    let theme = &state.theme;

    let mut lines: Vec<Line> = Vec::new();

    // Name (display name, with optional ID row when a title is set)
    lines.push(Line::from(vec![
        Span::styled("Name        ", Style::default().fg(theme.label_secondary)),
        Span::styled(
            def.display_name().to_string(),
            Style::default()
                .fg(theme.label_accent)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    if def.title.is_some() {
        lines.push(Line::from(vec![
            Span::styled("ID          ", Style::default().fg(theme.label_secondary)),
            Span::styled(def.name.clone(), Style::default().fg(theme.label_info)),
        ]));
    }
    lines.push(Line::from(""));

    // Description
    if !def.description.is_empty() {
        lines.push(Line::from(Span::styled(
            "Description",
            Style::default().fg(theme.label_secondary),
        )));
        // Word-wrap description manually by splitting on spaces
        for word_line in wrap_text(&def.description, (area.width.saturating_sub(4)) as usize) {
            lines.push(Line::from(Span::raw(format!("  {word_line}"))));
        }
        lines.push(Line::from(""));
    }

    // Trigger
    lines.push(Line::from(vec![
        Span::styled("Trigger     ", Style::default().fg(theme.label_secondary)),
        Span::styled(
            def.trigger.to_string(),
            Style::default().fg(theme.label_info),
        ),
    ]));
    lines.push(Line::from(""));

    // Targets
    if !def.targets.is_empty() {
        lines.push(Line::from(Span::styled(
            "Targets",
            Style::default().fg(theme.label_secondary),
        )));
        for t in &def.targets {
            lines.push(Line::from(Span::raw(format!("  \u{2022} {t}"))));
        }
        lines.push(Line::from(""));
    }

    // Source path (just the filename for brevity)
    let short_path = def
        .source_path
        .rsplit('/')
        .next()
        .unwrap_or(&def.source_path);
    lines.push(Line::from(vec![
        Span::styled("Source      ", Style::default().fg(theme.label_secondary)),
        Span::styled(
            short_path.to_string(),
            Style::default().fg(theme.label_secondary),
        ),
    ]));
    lines.push(Line::from(""));

    // Total node count
    lines.push(Line::from(vec![
        Span::styled("Steps       ", Style::default().fg(theme.label_secondary)),
        Span::raw(def.total_nodes().to_string()),
    ]));

    // Inputs section
    if !def.inputs.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "\u{2500} Inputs \u{2500}",
            Style::default()
                .fg(theme.label_secondary)
                .add_modifier(Modifier::DIM),
        )));
        lines.push(Line::from(""));
        for input in &def.inputs {
            let required_marker = if input.required { " *" } else { "" };
            lines.push(Line::from(vec![Span::styled(
                format!("  {}{}", input.name, required_marker),
                Style::default()
                    .fg(theme.label_warning)
                    .add_modifier(Modifier::BOLD),
            )]));
            if let Some(ref desc) = input.description {
                if !desc.is_empty() {
                    for word_line in wrap_text(desc, (area.width.saturating_sub(6)) as usize) {
                        lines.push(Line::from(Span::styled(
                            format!("    {word_line}"),
                            Style::default().fg(theme.label_secondary),
                        )));
                    }
                }
            }
            if let Some(ref default) = input.default {
                lines.push(Line::from(vec![
                    Span::styled("    default  ", Style::default().fg(theme.label_secondary)),
                    Span::styled(
                        default.clone(),
                        Style::default()
                            .fg(theme.label_secondary)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }
    }

    // Footer keybindings hint
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "w  run   e  edit   Esc  back",
        Style::default().fg(theme.label_secondary),
    )));

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_focused))
                .title(" Workflow Definition "),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

fn render_steps(frame: &mut Frame, area: Rect, def: &WorkflowDef, state: &AppState) {
    let theme = &state.theme;

    let mut items: Vec<ListItem> = Vec::new();

    if def.body.is_empty() && def.always.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "  (no steps)",
            Style::default().fg(theme.label_secondary),
        ))));
    } else {
        let seen = HashSet::new();
        build_node_lines(
            &def.body,
            0,
            &mut items,
            theme,
            &state.data.workflow_defs,
            &seen,
        );
        if !def.always.is_empty() {
            items.push(ListItem::new(Line::from("")));
            items.push(ListItem::new(Line::from(Span::styled(
                "always",
                Style::default()
                    .fg(theme.label_keyword)
                    .add_modifier(Modifier::BOLD),
            ))));
            build_node_lines(
                &def.always,
                1,
                &mut items,
                theme,
                &state.data.workflow_defs,
                &seen,
            );
        }
    }

    // Apply scroll offset
    let scroll = state.workflow_def_detail_scroll;
    let visible_height = area.height.saturating_sub(2) as usize; // minus borders
    let total = items.len();
    let start = if total == 0 {
        0
    } else {
        scroll.min(total.saturating_sub(1))
    };
    let end = (start + visible_height).min(total);
    let visible_items: Vec<ListItem> = items.into_iter().skip(start).take(end - start).collect();

    let scroll_indicator = if total > visible_height {
        format!(" Steps ({}/{}) ", start + 1, total)
    } else {
        " Steps ".to_string()
    };

    let list = List::new(visible_items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border_inactive))
            .title(scroll_indicator),
    );
    frame.render_widget(list, area);
}

/// Recursively build ListItems for a slice of WorkflowNodes, indented by `depth`.
/// `workflow_defs` is used to inline-expand `CallWorkflow` nodes (always-expanded in this view).
/// `seen` guards against infinite recursion from cyclic workflow references.
fn build_node_lines(
    nodes: &[WorkflowNode],
    depth: usize,
    items: &mut Vec<ListItem>,
    theme: &crate::theme::Theme,
    workflow_defs: &[WorkflowDef],
    seen: &HashSet<String>,
) {
    let indent = "  ".repeat(depth);
    for node in nodes {
        match node {
            WorkflowNode::Call(c) => {
                let agent = agent_ref_display(&c.agent);
                let mut spans = vec![
                    Span::raw(indent.clone()),
                    Span::styled("\u{2192} ", Style::default().fg(theme.status_completed)),
                    Span::styled(
                        agent,
                        Style::default()
                            .fg(theme.label_primary)
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                if c.retries > 0 {
                    spans.push(Span::styled(
                        format!("  retries={}", c.retries),
                        Style::default().fg(theme.label_secondary),
                    ));
                }
                if let Some(ref fail) = c.on_fail {
                    let fail_display = match fail {
                        OnFail::Agent(ref r) => agent_ref_display(r),
                        OnFail::Continue => "continue".to_string(),
                    };
                    spans.push(Span::styled(
                        format!("  on_fail={}", fail_display),
                        Style::default().fg(theme.label_secondary),
                    ));
                }
                items.push(ListItem::new(Line::from(spans)));
            }
            WorkflowNode::CallWorkflow(c) => {
                let mut spans = vec![
                    Span::raw(indent.clone()),
                    Span::styled("\u{21b3} ", Style::default().fg(theme.label_info)),
                    Span::styled(
                        c.workflow.clone(),
                        Style::default()
                            .fg(theme.label_accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                if c.retries > 0 {
                    spans.push(Span::styled(
                        format!("  retries={}", c.retries),
                        Style::default().fg(theme.label_secondary),
                    ));
                }
                items.push(ListItem::new(Line::from(spans)));
                // Always expand sub-workflow inline in the detail view.
                let child_indent = "  ".repeat(depth + 1);
                if seen.contains(&c.workflow) {
                    items.push(ListItem::new(Line::from(Span::styled(
                        format!("{child_indent}(↺ recursive — not expanded)"),
                        Style::default().fg(theme.label_secondary),
                    ))));
                } else if let Some(sub_def) = workflow_defs.iter().find(|d| d.name == c.workflow) {
                    let mut new_seen = seen.clone();
                    new_seen.insert(c.workflow.clone());
                    build_node_lines(
                        &sub_def.body,
                        depth + 1,
                        items,
                        theme,
                        workflow_defs,
                        &new_seen,
                    );
                } else {
                    items.push(ListItem::new(Line::from(Span::styled(
                        format!("{child_indent}(workflow not found)"),
                        Style::default().fg(theme.label_secondary),
                    ))));
                }
            }
            WorkflowNode::Gate(g) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled("\u{2b21} gate  ", Style::default().fg(theme.label_warning)),
                    Span::styled(
                        g.name.clone(),
                        Style::default()
                            .fg(theme.label_warning)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  [{}]", g.gate_type),
                        Style::default().fg(theme.label_secondary),
                    ),
                ])));
                if let Some(ref prompt) = g.prompt {
                    let sub_indent = "  ".repeat(depth + 1);
                    items.push(ListItem::new(Line::from(Span::styled(
                        format!("{sub_indent}\u{2514} {prompt}"),
                        Style::default().fg(theme.label_secondary),
                    ))));
                }
            }
            WorkflowNode::If(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        "if ",
                        Style::default()
                            .fg(theme.label_keyword)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format_condition(&n.condition),
                        Style::default().fg(theme.label_keyword),
                    ),
                ])));
                build_node_lines(&n.body, depth + 1, items, theme, workflow_defs, seen);
            }
            WorkflowNode::Unless(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        "unless ",
                        Style::default()
                            .fg(theme.label_keyword)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format_condition(&n.condition),
                        Style::default().fg(theme.label_keyword),
                    ),
                ])));
                build_node_lines(&n.body, depth + 1, items, theme, workflow_defs, seen);
            }
            WorkflowNode::While(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        "while ",
                        Style::default()
                            .fg(theme.label_keyword)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{}/{}", n.step, n.marker),
                        Style::default().fg(theme.label_keyword),
                    ),
                    Span::styled(
                        format!("  max={}", n.max_iterations),
                        Style::default().fg(theme.label_secondary),
                    ),
                ])));
                build_node_lines(&n.body, depth + 1, items, theme, workflow_defs, seen);
            }
            WorkflowNode::DoWhile(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        "do",
                        Style::default()
                            .fg(theme.label_keyword)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])));
                build_node_lines(&n.body, depth + 1, items, theme, workflow_defs, seen);
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        "while ",
                        Style::default()
                            .fg(theme.label_keyword)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{}/{}", n.step, n.marker),
                        Style::default().fg(theme.label_keyword),
                    ),
                    Span::styled(
                        format!("  max={}", n.max_iterations),
                        Style::default().fg(theme.label_secondary),
                    ),
                ])));
            }
            WorkflowNode::Do(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        "do",
                        Style::default()
                            .fg(theme.label_keyword)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])));
                build_node_lines(&n.body, depth + 1, items, theme, workflow_defs, seen);
            }
            WorkflowNode::Parallel(p) => {
                let modifier = if !p.fail_fast {
                    "  fail_fast=false"
                } else {
                    ""
                };
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        "parallel",
                        Style::default()
                            .fg(theme.label_info)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        modifier.to_string(),
                        Style::default().fg(theme.label_secondary),
                    ),
                ])));
                let sub_indent = "  ".repeat(depth + 1);
                for (i, agent_ref) in p.calls.iter().enumerate() {
                    let agent = agent_ref_display(agent_ref);
                    let cond_span = if let Some((step, marker)) = p.call_if.get(&i.to_string()) {
                        Span::styled(
                            format!("  if {step}/{marker}"),
                            Style::default().fg(theme.label_secondary),
                        )
                    } else {
                        Span::raw("")
                    };
                    items.push(ListItem::new(Line::from(vec![
                        Span::raw(sub_indent.clone()),
                        Span::styled("\u{2295} ", Style::default().fg(theme.label_info)),
                        Span::styled(
                            agent,
                            Style::default()
                                .fg(theme.label_primary)
                                .add_modifier(Modifier::BOLD),
                        ),
                        cond_span,
                    ])));
                }
            }
            WorkflowNode::Script(s) => {
                let mut spans = vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        "script ",
                        Style::default()
                            .fg(theme.label_keyword)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        s.name.clone(),
                        Style::default()
                            .fg(theme.label_primary)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  run={}", s.run),
                        Style::default().fg(theme.label_secondary),
                    ),
                ];
                if s.retries > 0 {
                    spans.push(Span::styled(
                        format!("  retries={}", s.retries),
                        Style::default().fg(theme.label_secondary),
                    ));
                }
                items.push(ListItem::new(Line::from(spans)));
            }
            WorkflowNode::Always(a) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        "always",
                        Style::default()
                            .fg(theme.label_keyword)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])));
                build_node_lines(&a.body, depth + 1, items, theme, workflow_defs, seen);
            }
            WorkflowNode::ForEach(n) => {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        "foreach ",
                        Style::default()
                            .fg(theme.label_keyword)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        n.name.clone(),
                        Style::default()
                            .fg(theme.label_primary)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  workflow={}", n.workflow),
                        Style::default().fg(theme.label_secondary),
                    ),
                ])));
            }
        }
    }
}

fn agent_ref_display(r: &AgentRef) -> String {
    match r {
        AgentRef::Name(n) => n.clone(),
        AgentRef::Path(p) => p.clone(),
    }
}

/// Naive word-wrapper: splits `text` into lines of at most `width` chars.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current.clone());
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}
