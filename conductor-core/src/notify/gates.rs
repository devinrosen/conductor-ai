use crate::config::{HookConfig, NotificationConfig};
use crate::notification_event::NotificationEvent;
use crate::workflow_dsl::GateType;

use super::{dispatch_notification, notification_body, DispatchParams};

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
        Some(GateType::HumanApproval) | Some(GateType::HumanReview) => {
            let title = match gate_type {
                Some(GateType::HumanApproval) => "Conductor \u{2014} Awaiting Your Approval",
                _ => "Conductor \u{2014} Review Requested",
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
        None => {
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
    // No [notifications.workflows] block → hooks are the sole filter; always pass.
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
        Some(GateType::QualityGate) => false, // quality gates are non-blocking, no notification
    }
}

/// Fire a desktop notification for a workflow gate waiting for action.
///
/// Gated on `config.enabled` and per-gate-type flags. Uses `(step_id, "gate_waiting")`
/// as the dedup key. Matching entries in `notify_hooks` are fired after the dedup
/// claim succeeds.
pub fn fire_gate_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    params: &GateNotificationParams<'_>,
) {
    let has_hooks = !notify_hooks.is_empty();
    if !should_notify_gate(config, params.gate_type) && !has_hooks {
        return;
    }

    let label = notification_body(params.workflow_name, params.target_label);
    let hook_event = NotificationEvent::GateWaiting {
        run_id: params.step_id.to_string(),
        label,
        timestamp: chrono::Utc::now().to_rfc3339(),
        url: None,
        step_name: params.step_name.to_string(),
        repo_slug: params.repo_slug.to_string(),
        branch: params.branch.to_string(),
        duration_ms: None,
        ticket_url: params.ticket_url.clone(),
    };

    dispatch_notification(
        conn,
        &DispatchParams {
            dedup_entity_id: params.step_id,
            dedup_event_type: "gate_waiting",
            hooks: notify_hooks,
            event: Some(&hook_event),
        },
    );
}

/// Determine the most "actionable" gate type from a slice of optional gate types.
///
/// Priority: `HumanApproval` / `HumanReview` > `PrApproval` > `PrChecks` > `QualityGate` > `None`.
/// Returns the highest-priority type found, or `None` if the slice is empty.
fn most_urgent_gate_type<'a>(gate_types: &[Option<&'a GateType>]) -> Option<&'a GateType> {
    let mut best: Option<&GateType> = None;
    let mut best_priority = 0u8;
    for gt in gate_types {
        let p = match gt {
            Some(GateType::HumanApproval) | Some(GateType::HumanReview) => 4,
            Some(GateType::PrApproval) => 3,
            Some(GateType::PrChecks) => 2,
            Some(GateType::QualityGate) => 1, // quality gates are non-blocking but still a valid gate type
            None => 0,
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
    gate_types: &[Option<&GateType>],
    workflow_name: &str,
    target_label: Option<&str>,
    count: usize,
) -> (&'static str, String) {
    let urgent = most_urgent_gate_type(gate_types);
    let title = match urgent {
        Some(GateType::HumanApproval) | Some(GateType::HumanReview) => {
            "Conductor \u{2014} Awaiting Your Approval"
        }
        Some(GateType::PrApproval) => "Conductor \u{2014} Awaiting PR Review",
        Some(GateType::PrChecks) => "Conductor \u{2014} Waiting on CI",
        Some(GateType::QualityGate) => "Conductor \u{2014} Quality Gate",
        None => "Conductor \u{2014} Approval Required",
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
/// Uses `(run_id, "gates_grouped")` as the dedup key.
pub fn fire_grouped_gate_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    params: &GroupedGateNotificationParams<'_>,
) {
    let has_hooks = !notify_hooks.is_empty();
    if !config.enabled && !has_hooks {
        return;
    }

    let label = notification_body(params.workflow_name, params.target_label);
    let hook_event = NotificationEvent::GateWaiting {
        run_id: params.run_id.to_string(),
        label,
        timestamp: chrono::Utc::now().to_rfc3339(),
        url: None,
        step_name: format!("{} gates pending", params.count),
        repo_slug: String::new(),
        branch: String::new(),
        duration_ms: None,
        ticket_url: None,
    };

    dispatch_notification(
        conn,
        &DispatchParams {
            dedup_entity_id: params.run_id,
            dedup_event_type: "gates_grouped",
            hooks: notify_hooks,
            event: Some(&hook_event),
        },
    );
}
