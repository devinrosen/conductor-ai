use crate::config::{HookConfig, NotificationConfig};
use crate::notification_event::NotificationEvent;
use crate::notification_hooks::HookRunner;

use super::{
    build_workflow_deep_link, dispatch_notification, notification_body, parse_target_label,
    try_claim_notification, DispatchParams,
};

/// Returns true if stale/orphan workflow notifications should be dispatched.
/// Centralises the gate check shared by orphan-resumed and heartbeat-stuck-failed.
fn stale_notifications_active(config: &NotificationConfig, notify_hooks: &[HookConfig]) -> bool {
    let legacy_enabled = config
        .workflows
        .as_ref()
        .is_some_and(|wf| config.enabled && wf.on_stale);
    legacy_enabled || !notify_hooks.is_empty()
}

/// Fire a notification when orphaned/stuck workflow runs are auto-resumed on
/// startup or during periodic recovery.
pub fn fire_orphan_resumed_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    run_ids: &[String],
) {
    if !stale_notifications_active(config, notify_hooks) {
        return;
    }
    if run_ids.is_empty() {
        return;
    }

    // Use a synthetic dedup key so we don't spam on every poll tick.
    // One notification per batch of resumed runs.
    let first_run_id = run_ids.first().unwrap();
    let dedup_key = format!("orphan_resumed_{first_run_id}");

    let n = run_ids.len();
    let body = if n == 1 {
        "1 stuck workflow run was automatically resumed".to_string()
    } else {
        format!("{n} stuck workflow runs were automatically resumed")
    };

    // Fetch the first run's workflow_name and target_label for the hook event.
    let (workflow_name, target_label) = conn
        .query_row(
            "SELECT workflow_name, target_label FROM workflow_runs WHERE id = :id",
            rusqlite::named_params! { ":id": first_run_id },
            |row| {
                Ok((
                    row.get::<_, String>("workflow_name")?,
                    row.get::<_, Option<String>>("target_label")?,
                ))
            },
        )
        .unwrap_or_else(|e| {
            tracing::warn!(
                run_id = %first_run_id,
                "fire_orphan_resumed_notification: DB error fetching run metadata, \
                 notification will have empty fields: {e}"
            );
            (String::new(), None)
        });
    let (repo_slug, branch) = parse_target_label(target_label.as_deref());

    let hook_event = NotificationEvent::WorkflowRunOrphanResumed {
        run_id: first_run_id.clone(),
        label: body.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        url: None,
        workflow_name,
        repo_slug: repo_slug.to_string(),
        branch: branch.to_string(),
        duration_ms: None,
        ticket_url: None,
    };

    dispatch_notification(
        conn,
        &DispatchParams {
            dedup_entity_id: &dedup_key,
            dedup_event_type: "workflow_orphan_resumed",
            hooks: notify_hooks,
            event: Some(&hook_event),
        },
    );
}

/// Fire a notification when a stuck workflow run fails to auto-resume after being reaped.
///
/// Callers must supply `workflow_name` and `target_label` from the data they already
/// hold — this keeps notify.rs free of domain-manager dependencies.
///
/// Gated on `config.enabled && wf.on_stale`. Uses `(run_id, "workflow_run.reaped")` as
/// the dedup key so each failure fires at most one notification across all processes.
pub fn fire_heartbeat_stuck_failed_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    run_id: &str,
    workflow_name: &str,
    target_label: Option<&str>,
    error: &str,
) {
    if !stale_notifications_active(config, notify_hooks) {
        return;
    }

    let (repo_slug, branch) = parse_target_label(target_label);
    let body = notification_body(workflow_name, target_label);

    let hook_event = NotificationEvent::WorkflowRunReaped {
        run_id: run_id.to_string(),
        label: body.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        url: None,
        workflow_name: workflow_name.to_string(),
        repo_slug: repo_slug.to_string(),
        branch: branch.to_string(),
        duration_ms: None,
        ticket_url: None,
        error: Some(error.to_string()),
    };

    dispatch_notification(
        conn,
        &DispatchParams {
            dedup_entity_id: run_id,
            dedup_event_type: "workflow_run.reaped",
            hooks: notify_hooks,
            event: Some(&hook_event),
        },
    );
}

/// Parameters for [`fire_cost_spike_notification`].
pub struct CostSpikeArgs<'a> {
    pub run_id: &'a str,
    pub workflow_name: &'a str,
    pub target_label: Option<&'a str>,
    pub cost_usd: f64,
    pub multiple: f64,
    pub duration_ms: Option<i64>,
    pub repo_slug: &'a str,
    pub branch: &'a str,
    pub parent_workflow_run_id: Option<&'a str>,
    pub repo_id: Option<&'a str>,
    pub worktree_id: Option<&'a str>,
}

/// Fire a cost-spike notification for a completed workflow run.
///
/// Fires an in-app notification when `multiple >= 3.0` and notifications are enabled.
/// Always fires matching hooks (filtered by `threshold_multiple`). Deduped on
/// `(run_id, "workflow_run.cost_spike")`.
pub fn fire_cost_spike_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    params: &CostSpikeArgs<'_>,
) {
    let has_hooks = !notify_hooks.is_empty();

    if !has_hooks {
        return;
    }

    if !try_claim_notification(conn, params.run_id, "workflow_run.cost_spike") {
        return;
    }

    let label = notification_body(params.workflow_name, params.target_label);
    let deep_link = build_workflow_deep_link(
        config.web_url.as_deref(),
        params.repo_id,
        params.worktree_id,
        params.run_id,
    );

    {
        let hook_event = NotificationEvent::WorkflowRunCostSpike {
            run_id: params.run_id.to_string(),
            label: label.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            url: deep_link,
            multiple: params.multiple,
            workflow_name: params.workflow_name.to_string(),
            parent_workflow_run_id: params.parent_workflow_run_id.map(|s| s.to_string()),
            repo_slug: params.repo_slug.to_string(),
            branch: params.branch.to_string(),
            duration_ms: params.duration_ms.map(|ms| ms as u64),
            ticket_url: None,
            cost_usd: Some(params.cost_usd),
        };
        HookRunner::new(notify_hooks).fire(&hook_event);
    }
}

/// Parameters for [`fire_duration_spike_notification`].
pub struct DurationSpikeArgs<'a> {
    pub run_id: &'a str,
    pub workflow_name: &'a str,
    pub target_label: Option<&'a str>,
    pub multiple: f64,
    pub duration_ms: Option<i64>,
    pub repo_slug: &'a str,
    pub branch: &'a str,
    pub parent_workflow_run_id: Option<&'a str>,
    pub repo_id: Option<&'a str>,
    pub worktree_id: Option<&'a str>,
}

/// Fire a duration-spike notification for a completed workflow run.
///
/// Fires an in-app notification when `multiple >= 2.0` and notifications are enabled.
/// Always fires matching hooks (filtered by `threshold_multiple`). Deduped on
/// `(run_id, "workflow_run.duration_spike")`.
pub fn fire_duration_spike_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    params: &DurationSpikeArgs<'_>,
) {
    let has_hooks = !notify_hooks.is_empty();

    if !has_hooks {
        return;
    }

    if !try_claim_notification(conn, params.run_id, "workflow_run.duration_spike") {
        return;
    }

    let label = notification_body(params.workflow_name, params.target_label);
    let deep_link = build_workflow_deep_link(
        config.web_url.as_deref(),
        params.repo_id,
        params.worktree_id,
        params.run_id,
    );

    {
        let hook_event = NotificationEvent::WorkflowRunDurationSpike {
            run_id: params.run_id.to_string(),
            label: label.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            url: deep_link,
            multiple: params.multiple,
            workflow_name: params.workflow_name.to_string(),
            parent_workflow_run_id: params.parent_workflow_run_id.map(|s| s.to_string()),
            repo_slug: params.repo_slug.to_string(),
            branch: params.branch.to_string(),
            duration_ms: params.duration_ms.map(|ms| ms as u64),
            ticket_url: None,
        };
        HookRunner::new(notify_hooks).fire(&hook_event);
    }
}

/// Parameters for [`fire_gate_pending_too_long_notification`].
pub struct GatePendingTooLongArgs<'a> {
    pub step_id: &'a str,
    pub step_name: &'a str,
    pub workflow_run_id: &'a str,
    pub workflow_name: &'a str,
    pub target_label: Option<&'a str>,
    pub pending_ms: u64,
    pub duration_ms: Option<i64>,
    pub repo_slug: &'a str,
    pub branch: &'a str,
    pub repo_id: Option<&'a str>,
    pub worktree_id: Option<&'a str>,
}

/// Fire a notification when a gate step has been waiting longer than the configured threshold.
///
/// Fires an in-app notification when `pending_ms >= gate_pending_ms` from any hook config
/// (default threshold: 30 minutes / 1_800_000 ms) and notifications are enabled.
/// Always fires matching hooks (filtered by `gate_pending_ms`). Deduped on
/// `(step_id, "gate.pending_too_long")`.
pub fn fire_gate_pending_too_long_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    params: &GatePendingTooLongArgs<'_>,
) {
    const DEFAULT_THRESHOLD_MS: u64 = 1_800_000; // 30 minutes

    let has_hooks = notify_hooks
        .iter()
        .any(|h| params.pending_ms >= h.gate_pending_ms.unwrap_or(DEFAULT_THRESHOLD_MS));

    if !has_hooks {
        return;
    }

    if !try_claim_notification(conn, params.step_id, "gate.pending_too_long") {
        return;
    }

    let label = notification_body(params.workflow_name, params.target_label);
    let deep_link = build_workflow_deep_link(
        config.web_url.as_deref(),
        params.repo_id,
        params.worktree_id,
        params.workflow_run_id,
    );

    {
        let hook_event = NotificationEvent::GatePendingTooLong {
            run_id: params.workflow_run_id.to_string(),
            label: label.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            url: deep_link,
            step_name: params.step_name.to_string(),
            pending_ms: params.pending_ms,
            repo_slug: params.repo_slug.to_string(),
            branch: params.branch.to_string(),
            duration_ms: params.duration_ms.map(|ms| ms as u64),
            ticket_url: None,
        };
        HookRunner::new(notify_hooks).fire(&hook_event);
    }
}
