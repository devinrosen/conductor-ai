use std::sync::Arc;

use runkon_notify::{DedupStore, Event, HookRunner, Severity};

use crate::config::{hooks_as_runkon, HookConfig, NotificationConfig};

use super::catalog::{
    GATE_PENDING_TOO_LONG, WORKFLOW_RUN_COST_SPIKE, WORKFLOW_RUN_DURATION_SPIKE,
    WORKFLOW_RUN_ORPHAN_RESUMED, WORKFLOW_RUN_REAPED,
};
use super::{build_workflow_deep_link, notification_body, parse_target_label};

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
    dedup_store: Arc<dyn DedupStore>,
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
    let event = orphan_resumed_event(
        first_run_id,
        run_ids.len(),
        workflow_name,
        target_label.as_deref(),
    );

    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(dedup_store)
        .fire_with_dedup(&event, &dedup_key, "workflow_orphan_resumed");
}

/// Build the `workflow_run.orphan_resumed` event for `count` resumed runs; the fields
/// describe the first one.
pub(crate) fn orphan_resumed_event(
    first_run_id: &str,
    count: usize,
    workflow_name: String,
    target_label: Option<&str>,
) -> Event {
    let body = if count == 1 {
        "1 stuck workflow run was automatically resumed".to_string()
    } else {
        format!("{count} stuck workflow runs were automatically resumed")
    };
    let (repo_slug, branch) = parse_target_label(target_label);
    let now = chrono::Utc::now().to_rfc3339();

    Event {
        kind: WORKFLOW_RUN_ORPHAN_RESUMED.into(),
        title: "Conductor \u{2014} Workflows Resumed".into(),
        body,
        severity: Severity::Warning,
        fields: [
            ("run_id".into(), first_run_id.to_string()),
            ("workflow_name".into(), workflow_name),
            ("repo_slug".into(), repo_slug.into()),
            ("branch".into(), branch.into()),
            ("timestamp".into(), now),
        ]
        .into_iter()
        .collect(),
    }
}

/// Fire a notification when a stuck workflow run fails to auto-resume after being reaped.
///
/// Deduped on `(run_id, "workflow_run.reaped")` via SQLite.
#[allow(clippy::too_many_arguments)]
pub fn fire_heartbeat_stuck_failed_notification(
    _conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    dedup_store: Arc<dyn DedupStore>,
    run_id: &str,
    workflow_name: &str,
    target_label: Option<&str>,
    error: &str,
) {
    if !stale_notifications_active(config, notify_hooks) {
        return;
    }

    let event = reaped_event(run_id, workflow_name, target_label, error);

    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(dedup_store)
        .fire_with_dedup(&event, run_id, "workflow_run.reaped");
}

/// Build the `workflow_run.reaped` event.
pub(crate) fn reaped_event(
    run_id: &str,
    workflow_name: &str,
    target_label: Option<&str>,
    error: &str,
) -> Event {
    let (repo_slug, branch) = parse_target_label(target_label);
    let body = notification_body(workflow_name, target_label);
    let now = chrono::Utc::now().to_rfc3339();

    Event {
        kind: WORKFLOW_RUN_REAPED.into(),
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
    }
}

/// Fields shared by the cost and duration spike events.
struct SpikeCommon<'a> {
    run_id: &'a str,
    workflow_name: &'a str,
    target_label: Option<&'a str>,
    multiple: f64,
    duration_ms: Option<i64>,
    repo_slug: &'a str,
    branch: &'a str,
    parent_workflow_run_id: Option<&'a str>,
    repo_id: Option<&'a str>,
    worktree_id: Option<&'a str>,
}

/// Build a spike event: the shared fields plus `extra` (e.g. `cost_usd`).
fn spike_event(
    web_url: Option<&str>,
    kind: &str,
    title: &str,
    c: &SpikeCommon<'_>,
    extra: Option<(&str, String)>,
) -> Event {
    let label = notification_body(c.workflow_name, c.target_label);
    let deep_link = build_workflow_deep_link(web_url, c.repo_id, c.worktree_id, c.run_id);
    let is_root = c.parent_workflow_run_id.is_none();
    let now = chrono::Utc::now().to_rfc3339();

    let mut fields: std::collections::HashMap<String, String> = [
        ("run_id".to_string(), c.run_id.to_string()),
        ("workflow_name".into(), c.workflow_name.into()),
        (
            "parent_workflow_run_id".into(),
            c.parent_workflow_run_id.unwrap_or("").into(),
        ),
        ("repo_slug".into(), c.repo_slug.into()),
        ("branch".into(), c.branch.into()),
        (
            "duration_ms".into(),
            c.duration_ms.map(|ms| ms.to_string()).unwrap_or_default(),
        ),
        ("url".into(), deep_link.as_deref().unwrap_or("").into()),
        ("timestamp".into(), now),
        ("multiple".into(), c.multiple.to_string()),
        ("is_root".into(), is_root.to_string()),
    ]
    .into_iter()
    .collect();
    if let Some((k, v)) = extra {
        fields.insert(k.into(), v);
    }

    Event {
        kind: kind.into(),
        title: title.into(),
        body: label,
        severity: Severity::Warning,
        fields: fields.into_iter().collect(),
    }
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

impl<'a> CostSpikeArgs<'a> {
    /// The fields every spike event shares.
    fn common(&self) -> SpikeCommon<'a> {
        SpikeCommon {
            run_id: self.run_id,
            workflow_name: self.workflow_name,
            target_label: self.target_label,
            multiple: self.multiple,
            duration_ms: self.duration_ms,
            repo_slug: self.repo_slug,
            branch: self.branch,
            parent_workflow_run_id: self.parent_workflow_run_id,
            repo_id: self.repo_id,
            worktree_id: self.worktree_id,
        }
    }
}

/// Build the `workflow_run.cost_spike` event. `web_url` is `[notifications] web_url`.
pub(crate) fn cost_spike_event(web_url: Option<&str>, params: &CostSpikeArgs<'_>) -> Event {
    let common = params.common();
    spike_event(
        web_url,
        WORKFLOW_RUN_COST_SPIKE,
        "Conductor \u{2014} Cost Spike",
        &common,
        Some(("cost_usd", params.cost_usd.to_string())),
    )
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
    dedup_store: Arc<dyn DedupStore>,
    params: &CostSpikeArgs<'_>,
) {
    if notify_hooks.is_empty() {
        return;
    }

    let event = cost_spike_event(config.web_url.as_deref(), params);

    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(dedup_store)
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

impl<'a> DurationSpikeArgs<'a> {
    /// The fields every spike event shares.
    fn common(&self) -> SpikeCommon<'a> {
        SpikeCommon {
            run_id: self.run_id,
            workflow_name: self.workflow_name,
            target_label: self.target_label,
            multiple: self.multiple,
            duration_ms: self.duration_ms,
            repo_slug: self.repo_slug,
            branch: self.branch,
            parent_workflow_run_id: self.parent_workflow_run_id,
            repo_id: self.repo_id,
            worktree_id: self.worktree_id,
        }
    }
}

/// Build the `workflow_run.duration_spike` event. `web_url` is `[notifications] web_url`.
pub(crate) fn duration_spike_event(web_url: Option<&str>, params: &DurationSpikeArgs<'_>) -> Event {
    let common = params.common();
    spike_event(
        web_url,
        WORKFLOW_RUN_DURATION_SPIKE,
        "Conductor \u{2014} Duration Spike",
        &common,
        None,
    )
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
    dedup_store: Arc<dyn DedupStore>,
    params: &DurationSpikeArgs<'_>,
) {
    if notify_hooks.is_empty() {
        return;
    }

    let event = duration_spike_event(config.web_url.as_deref(), params);

    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(dedup_store)
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

/// Build the `gate.pending_too_long` event. `web_url` is `[notifications] web_url`.
pub(crate) fn gate_pending_too_long_event(
    web_url: Option<&str>,
    params: &GatePendingTooLongArgs<'_>,
) -> Event {
    let label = notification_body(params.workflow_name, params.target_label);
    let deep_link = build_workflow_deep_link(
        web_url,
        params.repo_id,
        params.worktree_id,
        params.workflow_run_id,
    );
    let now = chrono::Utc::now().to_rfc3339();

    Event {
        kind: GATE_PENDING_TOO_LONG.into(),
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
    }
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
    dedup_store: Arc<dyn DedupStore>,
    params: &GatePendingTooLongArgs<'_>,
) {
    const DEFAULT_THRESHOLD_MS: u64 = 1_800_000; // 30 minutes

    let has_qualifying_hook = notify_hooks
        .iter()
        .any(|h| params.pending_ms >= h.gate_pending_ms.unwrap_or(DEFAULT_THRESHOLD_MS));
    if !has_qualifying_hook {
        return;
    }

    let event = gate_pending_too_long_event(config.web_url.as_deref(), params);

    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(dedup_store)
        .fire_with_dedup(&event, params.step_id, "gate.pending_too_long");
}
