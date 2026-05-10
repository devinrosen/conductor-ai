use runkon_notify::{Event, Severity};

use crate::error::{ConductorError, Result};

/// All non-threshold lifecycle event names with their display labels and whether
/// they are workflow events (supporting the `:root` modifier).
///
/// This is the authoritative list used by `GET /api/config/hooks/events` to populate
/// the hook × event matrix UI. Threshold-based events (`workflow_run.cost_spike`,
/// `workflow_run.duration_spike`, `gate.pending_too_long`) are excluded because they
/// require additional filter fields that cannot be represented as simple checkboxes.
///
/// Tuple: `(event_name, display_label, is_workflow_event)`.
pub const ALL_EVENTS: &[(&str, &str, bool)] = &[
    ("workflow_run.completed", "Workflow completed", true),
    ("workflow_run.failed", "Workflow failed", true),
    ("workflow_run.stale", "Workflow step stale", true),
    ("workflow_run.reaped", "Dead workflow detected", true),
    (
        "workflow_run.orphan_resumed",
        "Orphaned workflows resumed",
        true,
    ),
    ("agent_run.completed", "Agent completed", false),
    ("agent_run.failed", "Agent failed", false),
    ("gate.waiting", "Gate waiting", false),
    ("feedback.requested", "Feedback requested", false),
];

const VALID_SYNTHETIC_EVENTS: &[&str] = &[
    "workflow_run.completed",
    "workflow_run.failed",
    "workflow_run.stale",
    "workflow_run.reaped",
    "workflow_run.orphan_resumed",
    "agent_run.completed",
    "agent_run.failed",
    "gate.waiting",
    "feedback.requested",
];

/// Build a synthetic test [`Event`] for the given concrete event name.
///
/// Returns `Err` if `name` is not a recognized event name.
pub fn build_synthetic_event(name: &str, now: impl Into<String>) -> Result<Event> {
    let now = now.into();
    let run_id = "test-00000000000000000000000000".to_string();
    let url = "http://localhost".to_string();
    let ticket_url = "https://github.com/example-org/example-repo/issues/42".to_string();

    let ev = match name {
        "workflow_run.completed" => Event {
            kind: "workflow_run.completed".into(),
            title: "Conductor \u{2014} Workflow Completed".into(),
            body: "test-workflow on test-repo/main".into(),
            severity: Severity::Info,
            fields: [
                ("run_id".into(), run_id),
                ("workflow_name".into(), "test-workflow".into()),
                ("parent_workflow_run_id".into(), String::new()),
                ("repo_slug".into(), "test-repo".into()),
                ("branch".into(), "main".into()),
                ("duration_ms".into(), "1000".into()),
                ("ticket_url".into(), ticket_url),
                ("url".into(), url),
                ("timestamp".into(), now),
                ("is_root".into(), "true".into()),
            ]
            .into_iter()
            .collect(),
        },
        "workflow_run.failed" => Event {
            kind: "workflow_run.failed".into(),
            title: "Conductor \u{2014} Workflow Failed".into(),
            body: "test-workflow on test-repo/main".into(),
            severity: Severity::Error,
            fields: [
                ("run_id".into(), run_id),
                ("workflow_name".into(), "test-workflow".into()),
                ("parent_workflow_run_id".into(), String::new()),
                ("repo_slug".into(), "test-repo".into()),
                ("branch".into(), "main".into()),
                ("duration_ms".into(), "1000".into()),
                ("ticket_url".into(), ticket_url),
                ("url".into(), url),
                ("timestamp".into(), now),
                ("is_root".into(), "true".into()),
                ("error".into(), "Test error".into()),
            ]
            .into_iter()
            .collect(),
        },
        "workflow_run.stale" | "workflow_run.orphan_resumed" => Event {
            kind: name.into(),
            title: "Conductor \u{2014} Workflows Resumed".into(),
            body: "Orphaned workflow runs resumed".into(),
            severity: Severity::Warning,
            fields: [
                ("run_id".into(), run_id),
                ("workflow_name".into(), "test-workflow".into()),
                ("repo_slug".into(), "test-repo".into()),
                ("branch".into(), "main".into()),
                ("duration_ms".into(), "1000".into()),
                ("ticket_url".into(), ticket_url),
                ("url".into(), url),
                ("timestamp".into(), now),
            ]
            .into_iter()
            .collect(),
        },
        "workflow_run.reaped" => Event {
            kind: "workflow_run.reaped".into(),
            title: "Conductor \u{2014} Dead Workflow Detected".into(),
            body: "test-workflow on test-repo/main".into(),
            severity: Severity::Error,
            fields: [
                ("run_id".into(), run_id),
                ("workflow_name".into(), "test-workflow".into()),
                ("repo_slug".into(), "test-repo".into()),
                ("branch".into(), "main".into()),
                ("duration_ms".into(), "1000".into()),
                ("ticket_url".into(), ticket_url),
                ("url".into(), url),
                ("timestamp".into(), now),
                ("error".into(), "Test error".into()),
            ]
            .into_iter()
            .collect(),
        },
        "agent_run.completed" => Event {
            kind: "agent_run.completed".into(),
            title: "Conductor \u{2014} Agent Completed".into(),
            body: "Test Agent Run".into(),
            severity: Severity::Info,
            fields: [
                ("run_id".into(), run_id),
                ("repo_slug".into(), "test-repo".into()),
                ("branch".into(), "main".into()),
                ("duration_ms".into(), "1000".into()),
                ("ticket_url".into(), ticket_url),
                ("url".into(), url),
                ("timestamp".into(), now),
            ]
            .into_iter()
            .collect(),
        },
        "agent_run.failed" => Event {
            kind: "agent_run.failed".into(),
            title: "Conductor \u{2014} Agent Failed".into(),
            body: "Test Agent Run".into(),
            severity: Severity::Error,
            fields: [
                ("run_id".into(), run_id),
                ("repo_slug".into(), "test-repo".into()),
                ("branch".into(), "main".into()),
                ("duration_ms".into(), "1000".into()),
                ("ticket_url".into(), ticket_url),
                ("url".into(), url),
                ("timestamp".into(), now),
                ("error".into(), "Test error".into()),
            ]
            .into_iter()
            .collect(),
        },
        "gate.waiting" => Event {
            kind: "gate.waiting".into(),
            title: "Conductor \u{2014} Gate Waiting".into(),
            body: "Test Run".into(),
            severity: Severity::Info,
            fields: [
                ("run_id".into(), run_id),
                ("step_name".into(), "test-gate".into()),
                ("repo_slug".into(), "test-repo".into()),
                ("branch".into(), "main".into()),
                ("duration_ms".into(), "1000".into()),
                ("ticket_url".into(), ticket_url),
                ("url".into(), url),
                ("timestamp".into(), now),
            ]
            .into_iter()
            .collect(),
        },
        "feedback.requested" => Event {
            kind: "feedback.requested".into(),
            title: "Conductor \u{2014} Feedback Requested".into(),
            body: "Test Agent Run".into(),
            severity: Severity::Info,
            fields: [
                ("run_id".into(), run_id),
                ("prompt_preview".into(), "Is this correct?".into()),
                ("repo_slug".into(), "test-repo".into()),
                ("branch".into(), "main".into()),
                ("duration_ms".into(), "1000".into()),
                ("ticket_url".into(), ticket_url),
                ("url".into(), url),
                ("timestamp".into(), now),
            ]
            .into_iter()
            .collect(),
        },
        other => {
            return Err(ConductorError::InvalidInput(format!(
                "unknown event name: '{other}'. Valid events: {}",
                VALID_SYNTHETIC_EVENTS.join(", ")
            )))
        }
    };
    Ok(ev)
}

/// Build a synthetic test event that will pass through a hook with the given `on` pattern.
///
/// Picks the first concrete event name that the pattern matches, falling back to
/// `workflow_run.completed` for `"*"` or any unrecognized pattern.
pub fn build_synthetic_for_pattern(pattern: &str, now: impl Into<String>) -> Event {
    let now = now.into();
    for &name in VALID_SYNTHETIC_EVENTS {
        if runkon_notify::hooks::on_pattern_matches(pattern, name) {
            return build_synthetic_event(name, &now)
                .expect("VALID_SYNTHETIC_EVENTS entry must match a build_synthetic_event() arm");
        }
    }
    build_synthetic_event("workflow_run.completed", now)
        .expect("workflow_run.completed is always a valid event name")
}
