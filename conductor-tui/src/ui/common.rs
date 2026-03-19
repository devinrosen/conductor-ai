use conductor_core::tickets::TicketLabel;
use conductor_core::workflow::GateType;
use conductor_core::worktree::{Worktree, WorktreeStatus};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, Paragraph};
use ratatui::Frame;

use crate::state::{AppState, View};
use crate::theme::Theme;

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
pub fn ticket_label_spans_compact(
    labels: &[TicketLabel],
    theme: &crate::theme::Theme,
) -> Vec<Span<'static>> {
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
            .unwrap_or(theme.label_secondary);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(lbl.label.clone(), Style::default().fg(color)));
        shown += 1;
    }
    let remaining = labels.len().saturating_sub(shown);
    if remaining > 0 {
        spans.push(Span::styled(
            format!(" +{remaining}"),
            Style::default().fg(theme.label_secondary),
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
            View::WorkflowDefDetail => "Workflow Definition",
        };
        format!("[{view_name}]  Tab:panel  [/]:column  \\:workflows  q:quit")
    };

    let mut spans: Vec<Span<'static>> = Vec::new();

    // Notification indicator
    if state.unread_notification_count > 0 {
        spans.push(Span::styled(
            format!("\u{1F514} {} ", state.unread_notification_count),
            Style::default()
                .fg(state.theme.label_warning)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            " ",
            Style::default().fg(state.theme.label_secondary),
        ));
    }

    spans.push(Span::styled(
        msg,
        Style::default().fg(state.theme.label_secondary),
    ));

    let bar = Paragraph::new(Line::from(spans));
    frame.render_widget(bar, area);
}

/// Build a `ListItem` for a worktree row.
///
/// Both the dashboard and repo-detail worktree panes use this so the
/// format stays consistent.  Pass `repo_prefix` to prepend the repo
/// slug (dashboard style) and `show_branch` to append the branch name
/// (repo-detail style).  `list_prefix` is prepended as-is (used for
/// tree-indent connectors).
pub fn worktree_list_item_with_prefix(
    wt: &Worktree,
    state: &AppState,
    repo_prefix: Option<&str>,
    show_branch: bool,
    list_prefix: &str,
) -> ListItem<'static> {
    let is_active = wt.is_active();
    let status_color = match wt.status {
        WorktreeStatus::Active => state.theme.status_completed,
        WorktreeStatus::Merged => state.theme.label_info,
        WorktreeStatus::Abandoned => state.theme.status_failed,
    };
    let text_style = if is_active {
        Style::default()
    } else {
        Style::default().fg(state.theme.label_secondary)
    };

    let mut spans: Vec<Span<'static>> = Vec::new();

    if !list_prefix.is_empty() {
        spans.push(Span::raw(list_prefix.to_string()));
    }

    if let Some(prefix) = repo_prefix {
        spans.push(Span::styled(
            format!("{prefix}/"),
            Style::default().fg(state.theme.label_secondary),
        ));
    }

    // Ticket state icon + number — moved to front so it's visible before the slug.
    // ○ = open, ● = closed, ◉ = in_progress
    if let Some(ticket) = wt
        .ticket_id
        .as_ref()
        .and_then(|tid| state.data.ticket_map.get(tid))
    {
        let (icon, ticket_state_color) = match ticket.state.as_str() {
            "open" => ("○", state.theme.status_completed),
            "closed" => ("●", state.theme.label_secondary),
            "in_progress" => ("◉", state.theme.status_running),
            _ => ("·", state.theme.label_primary),
        };
        spans.push(Span::styled(
            format!("{} #{}  ", icon, ticket.source_id),
            Style::default().fg(ticket_state_color),
        ));
    }

    // Non-active status badge — also surfaced before the slug.
    if !is_active {
        spans.push(Span::styled(
            format!("[{}]  ", wt.status),
            Style::default().fg(status_color),
        ));
    }

    // Combined status symbol + workflow name/step — surfaced before the slug.
    // Agent takes symbol precedence over workflow; workflow name provides the label text.
    use conductor_core::agent::AgentRunStatus;
    use conductor_core::workflow::WorkflowRunStatus;
    let agent_run = state.data.latest_agent_runs.get(&wt.id);
    let wf_run = state.data.latest_workflow_runs_by_worktree.get(&wt.id);

    // Symbol priority: an active workflow run (Running/Waiting) always wins so the
    // root-level status is shown even when an agent step has already completed.
    // Agent status wins only when no active workflow run is present.
    let wf_active = wf_run.is_some_and(|wf| {
        matches!(
            wf.status,
            WorkflowRunStatus::Running | WorkflowRunStatus::Waiting
        )
    });
    let status_symbol: Option<(&'static str, ratatui::style::Color)> = if wf_active {
        wf_run.and_then(|wf| match wf.status {
            WorkflowRunStatus::Running => Some(("⚙", state.theme.label_accent)),
            WorkflowRunStatus::Waiting => Some(("⏸", state.theme.status_waiting)),
            _ => None,
        })
    } else if let Some(run) = agent_run {
        Some(match run.status {
            AgentRunStatus::Running => ("⚙", state.theme.status_running),
            AgentRunStatus::WaitingForFeedback => ("⏸", state.theme.status_waiting),
            AgentRunStatus::Completed => ("✓", state.theme.status_completed),
            AgentRunStatus::Failed => ("✗", state.theme.status_failed),
            AgentRunStatus::Cancelled => ("⊘", state.theme.status_cancelled),
        })
    } else {
        wf_run.and_then(|wf| match wf.status {
            WorkflowRunStatus::Running => Some(("⚙", state.theme.label_accent)),
            WorkflowRunStatus::Waiting => Some(("⏸", state.theme.status_waiting)),
            WorkflowRunStatus::Completed => Some(("✓", state.theme.label_secondary)),
            WorkflowRunStatus::Failed => Some(("✗", state.theme.status_failed)),
            _ => None,
        })
    };

    // Workflow label text (no symbol): "name › step" when active, "name" otherwise.
    let wf_label: Option<String> = wf_run.and_then(|wf| match wf.status {
        WorkflowRunStatus::Pending | WorkflowRunStatus::Cancelled => None,
        _ => {
            let is_active = matches!(
                wf.status,
                WorkflowRunStatus::Running | WorkflowRunStatus::Waiting
            );
            Some(if is_active {
                state
                    .data
                    .workflow_step_summaries
                    .get(&wf.id)
                    .map(|s| format!("{} › {}", wf.workflow_name, s.step_name))
                    .unwrap_or_else(|| wf.workflow_name.clone())
            } else {
                wf.workflow_name.clone()
            })
        }
    });

    if let Some((symbol, color)) = status_symbol {
        let text = match &wf_label {
            Some(label) => format!("{symbol} {label}  "),
            None => format!("{symbol}  "),
        };
        spans.push(Span::styled(text, Style::default().fg(color)));
    }

    // Slug or branch — trailing identifier.
    // In repo-detail context (show_branch=true): show branch name.
    // In dashboard context (show_branch=false): show slug.
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

    // Token counts at end for active agent runs.
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

/// Return the canonical (icon, color) pair for a gate type.
///
/// Used by both the pending-gates panel and the workflow run list to ensure
/// consistent iconography across views.
pub fn gate_type_icon(gate_type: Option<&GateType>, theme: &Theme) -> (&'static str, Color) {
    match gate_type {
        Some(GateType::PrChecks) => ("⏳", theme.status_waiting),
        Some(GateType::PrApproval | GateType::HumanApproval | GateType::HumanReview) => {
            ("👤", theme.label_warning)
        }
        Some(GateType::QualityGate) => ("🔍", theme.status_waiting),
        None => ("⏸", theme.status_waiting),
    }
}

/// Format an ISO 8601 timestamp as a compact elapsed duration string.
///
/// Returns strings like `"3m"`, `"1h 20m"`, `"2d 5h"`.
/// Returns an empty string if parsing fails or the timestamp is in the future.
pub fn format_elapsed(started_at: &str) -> String {
    let parsed = chrono::DateTime::parse_from_rfc3339(started_at).or_else(|_| {
        // Also accept "YYYY-MM-DD HH:MM:SS" (no timezone) as UTC
        let with_tz = format!("{started_at}Z")
            .replace(' ', "T")
            .replace("ZZ", "Z");
        chrono::DateTime::parse_from_rfc3339(&with_tz)
    });

    let Ok(started) = parsed else {
        return String::new();
    };

    let now = chrono::Utc::now();
    let elapsed = now.signed_duration_since(started);
    if elapsed.num_seconds() <= 0 {
        return String::new();
    }

    format_duration_compact(elapsed)
}

/// Format a chrono Duration as a compact human-readable string.
fn format_duration_compact(elapsed: chrono::Duration) -> String {
    let total_secs = elapsed.num_seconds();
    let mins = total_secs / 60;
    let hours = mins / 60;
    let days = hours / 24;

    if days > 0 {
        let remaining_hours = hours % 24;
        if remaining_hours > 0 {
            format!("{days}d {remaining_hours}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        let remaining_mins = mins % 60;
        if remaining_mins > 0 {
            format!("{hours}h {remaining_mins}m")
        } else {
            format!("{hours}h")
        }
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        "<1m".to_string()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_elapsed_minutes() {
        let ts = (chrono::Utc::now() - chrono::Duration::minutes(3)).to_rfc3339();
        assert_eq!(format_elapsed(&ts), "3m");
    }

    #[test]
    fn format_elapsed_hours_and_minutes() {
        let ts = (chrono::Utc::now() - chrono::Duration::minutes(80)).to_rfc3339();
        assert_eq!(format_elapsed(&ts), "1h 20m");
    }

    #[test]
    fn format_elapsed_days_and_hours() {
        let ts = (chrono::Utc::now() - chrono::Duration::hours(53)).to_rfc3339();
        assert_eq!(format_elapsed(&ts), "2d 5h");
    }

    #[test]
    fn format_elapsed_less_than_one_minute() {
        let ts = (chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339();
        assert_eq!(format_elapsed(&ts), "<1m");
    }

    #[test]
    fn format_elapsed_future_timestamp_returns_empty() {
        let ts = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        assert_eq!(format_elapsed(&ts), "");
    }

    #[test]
    fn format_elapsed_invalid_input_returns_empty() {
        assert_eq!(format_elapsed("not-a-date"), "");
        assert_eq!(format_elapsed(""), "");
    }

    #[test]
    fn format_elapsed_fractional_seconds() {
        let ts = (chrono::Utc::now() - chrono::Duration::minutes(5))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        assert_eq!(format_elapsed(&ts), "5m");
    }

    #[test]
    fn format_elapsed_with_timezone_offset() {
        // Use a fixed offset timestamp that is ~2 hours ago
        let now = chrono::Utc::now();
        let two_hours_ago = now - chrono::Duration::hours(2);
        let offset = chrono::FixedOffset::east_opt(5 * 3600).unwrap();
        let with_offset = two_hours_ago.with_timezone(&offset).to_rfc3339();
        assert_eq!(format_elapsed(&with_offset), "2h");
    }
}
