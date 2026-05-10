use std::sync::Arc;

use runkon_notify::{DedupStore, Event, HookRunner, Severity};

use crate::config::{hooks_as_runkon, HookConfig, NotificationConfig};
use crate::workflow::GateType;

use super::notification_body;

/// Build the notification title and body for a gate based on its type.
///
/// Pure function — no side effects — extracted so the formatting logic is
/// unit-testable without touching the dedup DB.
pub fn gate_notification_text(
    gate_type: Option<&GateType>,
    step_name: &str,
    workflow_name: &str,
    target_label: Option<&str>,
    gate_prompt: Option<&str>,
) -> (&'static str, String) {
    let wf = notification_body(workflow_name, target_label);

    match gate_type {
        Some(GateType::HumanApproval | GateType::HumanReview) => {
            let title = if matches!(gate_type, Some(GateType::HumanApproval)) {
                "Conductor \u{2014} Awaiting Your Approval"
            } else {
                "Conductor \u{2014} Review Requested"
            };
            let body = match gate_prompt {
                Some(prompt) => format!("{wf} \u{2192} {step_name}: {prompt}"),
                None => format!("{wf} \u{2192} {step_name}"),
            };
            (title, body)
        }
        Some(GateType::PrApproval) => {
            let title = "Conductor \u{2014} Awaiting PR Review";
            let body = format!("{wf}: PR needs review");
            (title, body)
        }
        Some(GateType::PrChecks) => {
            let title = "Conductor \u{2014} Waiting on CI";
            let body = format!("{wf}: PR checks running");
            (title, body)
        }
        Some(GateType::QualityGate) => {
            let title = "Conductor \u{2014} Quality Gate";
            let body = format!("{wf}: {step_name} evaluating");
            (title, body)
        }
        Some(GateType::Other(_)) | None => {
            let title = "Conductor \u{2014} Approval Required";
            let body = format!("{wf}: {step_name}");
            (title, body)
        }
    }
}

/// Parameters for [`fire_gate_notification`].
pub struct GateNotificationParams<'a> {
    pub step_id: &'a str,
    pub step_name: &'a str,
    pub workflow_name: &'a str,
    pub target_label: Option<&'a str>,
    pub gate_type: Option<&'a GateType>,
    pub gate_prompt: Option<&'a str>,
    pub repo_slug: &'a str,
    pub branch: &'a str,
    pub ticket_url: Option<String>,
}

/// Returns `true` if a gate notification should fire given the config and gate type.
///
/// Pure function — no side effects — checks master `enabled` flag then maps
/// each gate type to its per-type config flag.
///
/// When `config.workflows` is `None` (no legacy `[notifications.workflows]` block),
/// hook `on` patterns are the sole filter and this function always returns `true`.
/// When `Some(wf)`, the legacy per-gate-type flags are respected (backward compat).
pub fn should_notify_gate(config: &NotificationConfig, gate_type: Option<&GateType>) -> bool {
    let Some(wf) = &config.workflows else {
        return true;
    };
    if !config.enabled {
        return false;
    }
    match gate_type {
        None => true,
        Some(GateType::HumanApproval | GateType::HumanReview) => wf.on_gate_human,
        Some(GateType::PrChecks) => wf.on_gate_ci,
        Some(GateType::PrApproval) => wf.on_gate_pr_review,
        Some(GateType::QualityGate) => false,
        Some(GateType::Other(_)) => true,
    }
}

/// Fire a desktop notification for a workflow gate waiting for action.
///
/// Deduped on `(step_id, "gate_waiting")` via SQLite.
pub fn fire_gate_notification(
    _conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    dedup_store: Arc<dyn DedupStore>,
    params: &GateNotificationParams<'_>,
) {
    let has_hooks = !notify_hooks.is_empty();
    if !should_notify_gate(config, params.gate_type) && !has_hooks {
        return;
    }

    let label = notification_body(params.workflow_name, params.target_label);
    let now = chrono::Utc::now().to_rfc3339();

    let event = Event {
        kind: "gate.waiting".into(),
        title: "Conductor \u{2014} Gate Waiting".into(),
        body: label,
        severity: Severity::Info,
        fields: [
            ("run_id".into(), params.step_id.into()),
            ("step_name".into(), params.step_name.into()),
            ("repo_slug".into(), params.repo_slug.into()),
            ("branch".into(), params.branch.into()),
            (
                "ticket_url".into(),
                params.ticket_url.as_deref().unwrap_or("").into(),
            ),
            ("timestamp".into(), now),
        ]
        .into_iter()
        .collect(),
    };

    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(dedup_store)
        .fire_with_dedup(&event, params.step_id, "gate_waiting");
}

/// Determine the most "actionable" gate type from a slice of optional gate types.
///
/// Priority (highest to lowest): `HumanApproval` / `HumanReview`, then `PrApproval`,
/// then `PrChecks`, then `QualityGate`, then `Other` / `None`. Returns the
/// highest-priority type found, or `None` if the slice is empty.
fn most_urgent_gate_type<'a>(gate_types_slice: &[Option<&'a GateType>]) -> Option<&'a GateType> {
    let mut best: Option<&GateType> = None;
    let mut best_priority = 0u8;
    for gt in gate_types_slice {
        let p = match gt {
            Some(GateType::HumanApproval | GateType::HumanReview) => 4,
            Some(GateType::PrApproval) => 3,
            Some(GateType::PrChecks) => 2,
            Some(GateType::QualityGate) => 1,
            Some(GateType::Other(_)) | None => 0,
        };
        if p > best_priority {
            best_priority = p;
            best = *gt;
        }
    }
    best
}

/// Build the notification title and body for a grouped gate notification.
///
/// Pure function — no side effects. The title reflects the most urgent gate type
/// in the group; the body shows the workflow name, optional target, and count.
pub fn grouped_gate_notification_text(
    gate_types_slice: &[Option<&GateType>],
    workflow_name: &str,
    target_label: Option<&str>,
    count: usize,
) -> (&'static str, String) {
    let urgent = most_urgent_gate_type(gate_types_slice);
    let title = match urgent {
        Some(GateType::HumanApproval | GateType::HumanReview) => {
            "Conductor \u{2014} Awaiting Your Approval"
        }
        Some(GateType::PrApproval) => "Conductor \u{2014} Awaiting PR Review",
        Some(GateType::PrChecks) => "Conductor \u{2014} Waiting on CI",
        Some(GateType::QualityGate) => "Conductor \u{2014} Quality Gate",
        Some(GateType::Other(_)) | None => "Conductor \u{2014} Approval Required",
    };

    let wf = notification_body(workflow_name, target_label);
    let body = format!("{wf}: {count} gates pending");

    (title, body)
}

/// Parameters for [`fire_grouped_gate_notification`].
pub struct GroupedGateNotificationParams<'a> {
    pub run_id: &'a str,
    pub workflow_name: &'a str,
    pub target_label: Option<&'a str>,
    pub gate_types: Vec<Option<&'a GateType>>,
    pub count: usize,
}

/// Fire a single grouped desktop notification for multiple gates in the same run.
///
/// Deduped on `(run_id, "gates_grouped")` via SQLite.
pub fn fire_grouped_gate_notification(
    _conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    dedup_store: Arc<dyn DedupStore>,
    params: &GroupedGateNotificationParams<'_>,
) {
    let has_hooks = !notify_hooks.is_empty();
    if !config.enabled && !has_hooks {
        return;
    }

    let label = notification_body(params.workflow_name, params.target_label);
    let now = chrono::Utc::now().to_rfc3339();

    let event = Event {
        kind: "gate.waiting".into(),
        title: "Conductor \u{2014} Gate Waiting".into(),
        body: label,
        severity: Severity::Info,
        fields: [
            ("run_id".into(), params.run_id.into()),
            (
                "step_name".into(),
                format!("{} gates pending", params.count),
            ),
            ("count".into(), params.count.to_string()),
            ("timestamp".into(), now),
        ]
        .into_iter()
        .collect(),
    };

    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(dedup_store)
        .fire_with_dedup(&event, params.run_id, "gates_grouped");
}
