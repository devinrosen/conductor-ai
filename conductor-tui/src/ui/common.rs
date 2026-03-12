use conductor_core::tickets::TicketLabel;
use conductor_core::worktree::{Worktree, WorktreeStatus};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, Paragraph};
use ratatui::Frame;

use crate::state::{AppState, View};

/// Parse a 6-digit hex color string (with or without `#`) into `Color::Rgb`.
/// Falls back to `Color::DarkGray` on any parse error.
pub fn hex_to_color(hex: &str) -> Color {
    let h = hex.trim_start_matches('#');
    // Support 3-digit shorthand
    let full = if h.len() == 3 {
        format!(
            "{}{}{}{}{}{}",
            &h[0..1],
            &h[0..1],
            &h[1..2],
            &h[1..2],
            &h[2..3],
            &h[2..3]
        )
    } else {
        h.to_string()
    };
    if full.len() != 6 {
        return Color::DarkGray;
    }
    let r = u8::from_str_radix(&full[0..2], 16).unwrap_or(128);
    let g = u8::from_str_radix(&full[2..4], 16).unwrap_or(128);
    let b = u8::from_str_radix(&full[4..6], 16).unwrap_or(128);
    Color::Rgb(r, g, b)
}

/// Choose black or white foreground for maximum contrast against a colored background.
pub fn label_fg(bg: Color) -> Color {
    match bg {
        Color::Rgb(r, g, b) => {
            let luminance = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            if luminance > 128.0 {
                Color::Black
            } else {
                Color::White
            }
        }
        _ => Color::White,
    }
}

/// Build compact fg-only label spans for a ticket row (up to 3 labels + `+N` overflow).
pub fn ticket_label_spans_compact(labels: &[TicketLabel]) -> Vec<Span<'static>> {
    if labels.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut shown = 0usize;
    for lbl in labels.iter().take(3) {
        let color = lbl
            .color
            .as_deref()
            .map(hex_to_color)
            .unwrap_or(Color::DarkGray);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(lbl.label.clone(), Style::default().fg(color)));
        shown += 1;
    }
    let remaining = labels.len().saturating_sub(shown);
    if remaining > 0 {
        spans.push(Span::styled(
            format!(" +{remaining}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans
}

pub fn render_footer(frame: &mut Frame, area: Rect, state: &AppState) {
    let msg = if let Some(f) = state.active_filter() {
        format!("/{} ", f.text)
    } else if let Some(ref msg) = state.status_message {
        msg.clone()
    } else {
        let view_name = match state.view {
            View::Dashboard => "Dashboard",
            View::RepoDetail => "Repo Detail",
            View::WorktreeDetail => "Worktree Detail",
            View::WorkflowRunDetail => "Workflow Run",
        };
        format!("[{view_name}]  Tab:panel  Ctrl+h/l:column  \\:workflows  q:quit")
    };

    let bar = Paragraph::new(Line::from(Span::styled(
        msg,
        Style::default().fg(state.theme.label_secondary),
    )));
    frame.render_widget(bar, area);
}

/// Build a `ListItem` for a worktree row.
///
/// Both the dashboard and repo-detail worktree panes use this so the
/// format stays consistent.  Pass `repo_prefix` to prepend the repo
/// slug (dashboard style) and `show_branch` to append the branch name
/// (repo-detail style).
pub fn worktree_list_item(
    wt: &Worktree,
    state: &AppState,
    repo_prefix: Option<&str>,
    show_branch: bool,
) -> ListItem<'static> {
    worktree_list_item_with_prefix(wt, state, repo_prefix, show_branch, "")
}

pub fn worktree_list_item_with_prefix(
    wt: &Worktree,
    state: &AppState,
    repo_prefix: Option<&str>,
    show_branch: bool,
    list_prefix: &'static str,
) -> ListItem<'static> {
    let is_active = wt.is_active();
    let status_color = match wt.status {
        WorktreeStatus::Active => state.theme.status_completed,
        WorktreeStatus::Merged => Color::Blue,
        WorktreeStatus::Abandoned => state.theme.status_failed,
    };
    let text_style = if is_active {
        Style::default()
    } else {
        Style::default().fg(state.theme.label_secondary)
    };

    let mut spans: Vec<Span<'static>> = Vec::new();

    if !list_prefix.is_empty() {
        spans.push(Span::raw(list_prefix));
    }

    if let Some(prefix) = repo_prefix {
        spans.push(Span::styled(
            format!("{prefix}/"),
            Style::default().fg(state.theme.label_secondary),
        ));
    }

    // In repo-detail context (show_branch=true): show branch as primary identifier.
    // In dashboard context (show_branch=false): show slug as before.
    if show_branch {
        spans.push(Span::styled(
            wt.branch.clone(),
            text_style.add_modifier(if is_active {
                Modifier::BOLD
            } else {
                Modifier::DIM
            }),
        ));
    } else {
        spans.push(Span::styled(
            wt.slug.clone(),
            text_style.add_modifier(if is_active {
                Modifier::BOLD
            } else {
                Modifier::DIM
            }),
        ));
    }

    // Only show status badge for non-active worktrees ([active] is never informative).
    if !is_active {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("[{}]", wt.status),
            Style::default().fg(status_color),
        ));
    }

    if let Some(ticket) = wt
        .ticket_id
        .as_ref()
        .and_then(|tid| state.data.ticket_map.get(tid))
    {
        let ticket_state_color = match ticket.state.as_str() {
            "open" => state.theme.status_completed,
            "closed" => state.theme.label_secondary,
            "in_progress" => state.theme.status_running,
            _ => state.theme.label_primary,
        };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("#{} {}", ticket.source_id, ticket.state),
            Style::default().fg(ticket_state_color),
        ));
    }

    use conductor_core::agent::AgentRunStatus;
    let agent_run = state.data.latest_agent_runs.get(&wt.id);

    if let Some(run) = agent_run {
        let (symbol, color) = match run.status {
            AgentRunStatus::Running => ("● running", state.theme.status_running),
            AgentRunStatus::WaitingForFeedback => {
                ("⏸ waiting for feedback", state.theme.status_waiting)
            }
            AgentRunStatus::Completed => ("✓ completed", state.theme.status_completed),
            AgentRunStatus::Failed => ("✗ failed", state.theme.status_failed),
            AgentRunStatus::Cancelled => ("○ cancelled", state.theme.status_cancelled),
        };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(symbol, Style::default().fg(color)));
    }

    let has_active_agent = agent_run.is_some_and(|r| {
        matches!(
            r.status,
            AgentRunStatus::Running | AgentRunStatus::WaitingForFeedback
        )
    });

    if let Some(wf_run) = state.data.latest_workflow_runs_by_worktree.get(&wt.id) {
        use conductor_core::workflow::WorkflowRunStatus;
        let (symbol, color) = match wf_run.status {
            WorkflowRunStatus::Running => ("⚙ running", state.theme.label_accent),
            WorkflowRunStatus::Waiting => ("⏸ waiting", state.theme.status_waiting),
            WorkflowRunStatus::Completed => ("✓", state.theme.label_secondary),
            WorkflowRunStatus::Failed => ("✗ failed", state.theme.status_failed),
            WorkflowRunStatus::Pending | WorkflowRunStatus::Cancelled => {
                ("", state.theme.label_secondary)
            }
        };
        if !symbol.is_empty() {
            let is_wf_active = matches!(
                wf_run.status,
                WorkflowRunStatus::Running | WorkflowRunStatus::Waiting
            );
            // When both an agent and workflow are active, suppress the ⚙ symbol —
            // the agent's ● already covers status. Build a compact "name › step" label.
            let label = if is_wf_active && has_active_agent {
                state
                    .data
                    .workflow_step_summaries
                    .get(&wf_run.id)
                    .map(|s| format!("{} › {}", wf_run.workflow_name, s.step_name))
                    .unwrap_or_else(|| wf_run.workflow_name.clone())
            } else if is_wf_active {
                state
                    .data
                    .workflow_step_summaries
                    .get(&wf_run.id)
                    .map(|s| {
                        build_workflow_breadcrumb(
                            &wf_run.workflow_name,
                            &s.workflow_chain,
                            Some(&s.step_name),
                            s.iteration,
                            0,
                            symbol,
                        )
                    })
                    .unwrap_or_else(|| format!("{symbol} {}", wf_run.workflow_name))
            } else {
                format!("{symbol} {}", wf_run.workflow_name)
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(label, Style::default().fg(color)));
        }
    }

    // Append token counts at end for active agent runs.
    if let Some(run) = agent_run {
        if matches!(
            run.status,
            AgentRunStatus::Running | AgentRunStatus::WaitingForFeedback
        ) {
            if let (Some(input), Some(output)) = (run.input_tokens, run.output_tokens) {
                spans.push(Span::styled(
                    format!("  ↑{} ↓{}", fmt_tokens_k(input), fmt_tokens_k(output)),
                    Style::default().fg(state.theme.status_waiting),
                ));
            }
        }
    }

    ListItem::new(Line::from(spans))
}

/// Build a workflow step breadcrumb label string.
///
/// Produces a string of the form:
/// `"{symbol} context_label  name1 > name2 > … > {iter_prefix}step_name ({elapsed}s)"`.
///
/// The result is capped at `MAX_LABEL` (80) characters; leading workflow names
/// are dropped one-by-one with a `"..."` prefix when needed, and the step name
/// itself is truncated as a last resort.
///
/// - `context_label` — label prefix (worktree slug, repo slug, or `#N` ticket ref).
/// - `workflow_chain` — ordered parent workflow names (root → immediate parent).
///   Empty for single-level (non-nested) workflows.
/// - `step_name` — currently-running step name; `None` if unknown (returns
///   `"{symbol} {context_label}"`).
/// - `iteration` — loop iteration count (0 = first pass, omit prefix when 0).
/// - `elapsed_secs` — seconds since the current step started (0 = omit elapsed).
/// - `symbol` — prefix symbol (e.g. `"⚙"`, `"⏸"`).
pub fn build_workflow_breadcrumb(
    context_label: &str,
    workflow_chain: &[String],
    step_name: Option<&str>,
    iteration: i64,
    elapsed_secs: u64,
    symbol: &str,
) -> String {
    let Some(step) = step_name else {
        return format!("{symbol} {context_label}");
    };

    const MAX_LABEL: usize = 80;

    let iter_prefix = if iteration > 0 {
        format!("(iter {}) ", iteration)
    } else {
        String::new()
    };

    let elapsed_suffix = if elapsed_secs > 0 {
        let elapsed_str = if elapsed_secs < 60 {
            format!("{}s", elapsed_secs)
        } else {
            format!("{}m", elapsed_secs / 60)
        };
        format!(" ({elapsed_str})")
    } else {
        String::new()
    };

    let symbol_prefix = format!("{symbol} {context_label}  ");

    if !workflow_chain.is_empty() {
        // Assemble all workflow-name parts followed by the step name.
        // workflow_chain holds [root, …parents]; step is the final segment.
        // Prepend context_label so the output is:
        //   "{symbol} context_label  chain > … > step (elapsed)"
        let all_names: Vec<&str> = workflow_chain
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(step))
            .collect();

        let mut remaining = all_names.as_slice();
        loop {
            let (prefix_part, step_part) = remaining.split_at(remaining.len() - 1);
            let step_str = format!("{iter_prefix}{}{elapsed_suffix}", step_part[0]);
            let joined_names: Vec<&str> = prefix_part
                .iter()
                .copied()
                .chain(std::iter::once(step_str.as_str()))
                .collect();
            let omitted = all_names.len() - remaining.len();
            let candidate = if omitted > 0 {
                format!("{symbol_prefix}... > {}", joined_names.join(" > "))
            } else {
                format!("{symbol_prefix}{}", joined_names.join(" > "))
            };
            if candidate.chars().count() <= MAX_LABEL || remaining.len() <= 1 {
                if candidate.chars().count() <= MAX_LABEL {
                    return candidate;
                }
                // Truncate step name.
                let overhead = candidate.chars().count() - step_str.chars().count();
                let available = MAX_LABEL.saturating_sub(overhead + 1);
                let truncated: String = step_str.chars().take(available).collect();
                let base = if omitted > 0 {
                    format!(
                        "{symbol_prefix}... > {}",
                        joined_names[..joined_names.len() - 1].join(" > ")
                    )
                } else if joined_names.len() > 1 {
                    format!(
                        "{symbol_prefix}{}",
                        joined_names[..joined_names.len() - 1].join(" > ")
                    )
                } else {
                    symbol_prefix.clone()
                };
                let sep = if base.ends_with(' ') { "" } else { " > " };
                return format!("{base}{sep}{truncated}…");
            }
            remaining = &remaining[1..];
        }
    } else {
        // Single-level workflow: "{symbol} {context_label} > {iter_prefix}step_name{elapsed_suffix}"
        let base = format!("{symbol} {context_label} > {iter_prefix}");
        let base_chars = base.chars().count();
        let step_with_elapsed = format!("{step}{elapsed_suffix}");
        if base_chars + step_with_elapsed.chars().count() <= MAX_LABEL {
            format!("{base}{step_with_elapsed}")
        } else {
            let available = MAX_LABEL.saturating_sub(base_chars + 1);
            let truncated: String = step.chars().take(available).collect();
            format!("{base}{truncated}…")
        }
    }
}

/// Build a single worktree-indicator dot span for a ticket row.
///
/// Always returns a span — `●` (green) when an active worktree exists,
/// `○` (dark gray) otherwise — so columns stay aligned.
pub fn ticket_worktree_dot_span(state: &AppState, ticket_id: &str) -> Span<'static> {
    let has_active = state
        .data
        .ticket_worktrees
        .get(ticket_id)
        .and_then(|wts| wts.iter().find(|w| w.is_active()))
        .is_some();
    if has_active {
        Span::styled("● ", Style::default().fg(state.theme.status_completed))
    } else {
        Span::styled("○ ", Style::default().fg(state.theme.label_secondary))
    }
}

/// Format a token count as `X.Xk` for values ≥ 1000, or plain integer otherwise.
pub(super) fn fmt_tokens_k(n: i64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Build optional agent-totals spans for a ticket row.
///
/// Compact views (dashboard, repo-detail) pass `show_duration: false`
/// to get `X.Xk↓ X.Xk↑ Xt`.  The full Tickets view passes `true` to also
/// show `Xm XXs`.
pub fn ticket_agent_total_spans(
    state: &AppState,
    ticket_id: &str,
    leading: &str,
    show_duration: bool,
) -> Vec<Span<'static>> {
    let Some(totals) = state.data.ticket_agent_totals.get(ticket_id) else {
        return Vec::new();
    };
    let in_k = fmt_tokens_k(totals.total_input_tokens);
    let out_k = fmt_tokens_k(totals.total_output_tokens);
    let text = if show_duration {
        let dur_secs = totals.total_duration_ms as f64 / 1000.0;
        let mins = (dur_secs / 60.0) as i64;
        let secs = (dur_secs % 60.0) as i64;
        format!(
            "{leading}{in_k}↓ {out_k}↑ {}t  {}m{:02}s",
            totals.total_turns, mins, secs
        )
    } else {
        format!("{leading}{in_k}↓ {out_k}↑ {}t", totals.total_turns)
    };
    vec![Span::styled(
        text,
        Style::default().fg(state.theme.status_waiting),
    )]
}

/// Truncate a string to at most `max` characters, appending "…" if truncated.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}
