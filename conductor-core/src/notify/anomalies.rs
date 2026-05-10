use std::sync::Arc;

use runkon_notify::{Event, HookRunner, Severity};

use crate::config::{hooks_as_runkon, HookConfig, NotificationConfig};

use super::{build_workflow_deep_link, notification_body, parse_target_label, SqliteDedupStore};

fn stale_notifications_active(config: &NotificationConfig, notify_hooks: &[HookConfig]) -> bool {
    let legacy_enabled = config
        .workflows
        .as_ref()
        .is_some_and(|wf| config.enabled && wf.on_stale);
    legacy_enabled || !notify_hooks.is_empty()
}

/// Fire a notification when orphaned/stuck workflow runs are auto-resumed on startup.
///
/// Deduped on `(dedup_key, "workflow_orphan_resumed")` via SQLite.
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

    let first_run_id = run_ids.first().unwrap();
    let dedup_key = format!("orphan_resumed_{first_run_id}");

    let n = run_ids.len();
    let body = if n == 1 {
        "1 stuck workflow run was automatically resumed".to_string()
    } else {
        format!("{n} stuck workflow runs were automatically resumed")
    };

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
    let now = chrono::Utc::now().to_rfc3339();

    let event = Event {
        kind: "workflow_run.orphan_resumed".into(),
        title: "Conductor \u{2014} Workflows Resumed".into(),
        body,
        severity: Severity::Warning,
        fields: [
            ("run_id".into(), first_run_id.clone()),
            ("workflow_name".into(), workflow_name),
            ("repo_slug".into(), repo_slug.into()),
            ("branch".into(), branch.into()),
            ("timestamp".into(), now),
        ]
        .into_iter()
        .collect(),
    };

    let store = Arc::new(SqliteDedupStore::default_db());
    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(store)
        .fire_with_dedup(&event, &dedup_key, "workflow_orphan_resumed");
}

/// Fire a notification when a stuck workflow run fails to auto-resume after being reaped.
///
/// Deduped on `(run_id, "workflow_run.reaped")` via SQLite.
pub fn fire_heartbeat_stuck_failed_notification(
    _conn: &rusqlite::Connection,
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
    let now = chrono::Utc::now().to_rfc3339();

    let event = Event {
        kind: "workflow_run.reaped".into(),
        title: "Conductor \u{2014} Dead Workflow Detected".into(),
        body,
        severity: Severity::Error,
        fields: [
            ("run_id".into(), run_id.into()),
            ("workflow_name".into(), workflow_name.into()),
            ("repo_slug".into(), repo_slug.into()),
            ("branch".into(), branch.into()),
            ("error".into(), error.into()),
            ("timestamp".into(), now),
        ]
        .into_iter()
        .collect(),
    };

    let store = Arc::new(SqliteDedupStore::default_db());
    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(store)
        .fire_with_dedup(&event, run_id, "workflow_run.reaped");
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
/// Deduped on `(run_id, "workflow_run.cost_spike")` via SQLite.
/// The `when_field_gte: { "multiple" => threshold }` predicate on each hook config
/// controls which hooks actually fire.
pub fn fire_cost_spike_notification(
    _conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    params: &CostSpikeArgs<'_>,
) {
    if notify_hooks.is_empty() {
        return;
    }

    let label = notification_body(params.workflow_name, params.target_label);
    let deep_link = build_workflow_deep_link(
        config.web_url.as_deref(),
        params.repo_id,
        params.worktree_id,
        params.run_id,
    );
    let is_root = params.parent_workflow_run_id.is_none();
    let now = chrono::Utc::now().to_rfc3339();

    let event = Event {
        kind: "workflow_run.cost_spike".into(),
        title: "Conductor \u{2014} Cost Spike".into(),
        body: label,
        severity: Severity::Warning,
        fields: [
            ("run_id".into(), params.run_id.into()),
            ("workflow_name".into(), params.workflow_name.into()),
            (
                "parent_workflow_run_id".into(),
                params.parent_workflow_run_id.unwrap_or("").into(),
            ),
            ("repo_slug".into(), params.repo_slug.into()),
            ("branch".into(), params.branch.into()),
            (
                "duration_ms".into(),
                params
                    .duration_ms
                    .map(|ms| ms.to_string())
                    .unwrap_or_default(),
            ),
            ("url".into(), deep_link.as_deref().unwrap_or("").into()),
            ("timestamp".into(), now),
            ("multiple".into(), params.multiple.to_string()),
            ("cost_usd".into(), params.cost_usd.to_string()),
            ("is_root".into(), is_root.to_string()),
        ]
        .into_iter()
        .collect(),
    };

    let store = Arc::new(SqliteDedupStore::default_db());
    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(store)
        .fire_with_dedup(&event, params.run_id, "workflow_run.cost_spike");
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
/// Deduped on `(run_id, "workflow_run.duration_spike")` via SQLite.
/// The `when_field_gte: { "multiple" => threshold }` predicate on each hook config
/// controls which hooks actually fire.
pub fn fire_duration_spike_notification(
    _conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    params: &DurationSpikeArgs<'_>,
) {
    if notify_hooks.is_empty() {
        return;
    }

    let label = notification_body(params.workflow_name, params.target_label);
    let deep_link = build_workflow_deep_link(
        config.web_url.as_deref(),
        params.repo_id,
        params.worktree_id,
        params.run_id,
    );
    let is_root = params.parent_workflow_run_id.is_none();
    let now = chrono::Utc::now().to_rfc3339();

    let event = Event {
        kind: "workflow_run.duration_spike".into(),
        title: "Conductor \u{2014} Duration Spike".into(),
        body: label,
        severity: Severity::Warning,
        fields: [
            ("run_id".into(), params.run_id.into()),
            ("workflow_name".into(), params.workflow_name.into()),
            (
                "parent_workflow_run_id".into(),
                params.parent_workflow_run_id.unwrap_or("").into(),
            ),
            ("repo_slug".into(), params.repo_slug.into()),
            ("branch".into(), params.branch.into()),
            (
                "duration_ms".into(),
                params
                    .duration_ms
                    .map(|ms| ms.to_string())
                    .unwrap_or_default(),
            ),
            ("url".into(), deep_link.as_deref().unwrap_or("").into()),
            ("timestamp".into(), now),
            ("multiple".into(), params.multiple.to_string()),
            ("is_root".into(), is_root.to_string()),
        ]
        .into_iter()
        .collect(),
    };

    let store = Arc::new(SqliteDedupStore::default_db());
    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(store)
        .fire_with_dedup(&event, params.run_id, "workflow_run.duration_spike");
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
/// The pre-dispatch check uses the conductor-level `gate_pending_ms` field so that the
/// dedup slot is not claimed when no hook would fire. Deduped on
/// `(step_id, "gate.pending_too_long")` via SQLite.
pub fn fire_gate_pending_too_long_notification(
    _conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    params: &GatePendingTooLongArgs<'_>,
) {
    const DEFAULT_THRESHOLD_MS: u64 = 1_800_000; // 30 minutes

    let has_qualifying_hook = notify_hooks
        .iter()
        .any(|h| params.pending_ms >= h.gate_pending_ms.unwrap_or(DEFAULT_THRESHOLD_MS));
    if !has_qualifying_hook {
        return;
    }

    let label = notification_body(params.workflow_name, params.target_label);
    let deep_link = build_workflow_deep_link(
        config.web_url.as_deref(),
        params.repo_id,
        params.worktree_id,
        params.workflow_run_id,
    );
    let now = chrono::Utc::now().to_rfc3339();

    let event = Event {
        kind: "gate.pending_too_long".into(),
        title: "Conductor \u{2014} Gate Pending Too Long".into(),
        body: label,
        severity: Severity::Warning,
        fields: [
            ("run_id".into(), params.workflow_run_id.into()),
            ("step_name".into(), params.step_name.into()),
            ("repo_slug".into(), params.repo_slug.into()),
            ("branch".into(), params.branch.into()),
            (
                "duration_ms".into(),
                params
                    .duration_ms
                    .map(|ms| ms.to_string())
                    .unwrap_or_default(),
            ),
            ("url".into(), deep_link.as_deref().unwrap_or("").into()),
            ("timestamp".into(), now),
            ("pending_ms".into(), params.pending_ms.to_string()),
        ]
        .into_iter()
        .collect(),
    };

    let store = Arc::new(SqliteDedupStore::default_db());
    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(store)
        .fire_with_dedup(&event, params.step_id, "gate.pending_too_long");
}
