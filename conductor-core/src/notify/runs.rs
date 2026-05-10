use std::sync::Arc;

use runkon_notify::{DedupStore, Event, HookRunner, Severity};

use crate::config::{hooks_as_runkon, HookConfig, NotificationConfig};

use super::{build_workflow_deep_link, notification_body, should_notify};

/// Narrow context bundle for [`fire_workflow_notification`].
pub struct NotificationCtx<'a> {
    pub conn: &'a rusqlite::Connection,
    pub config: &'a NotificationConfig,
    pub hooks: &'a [HookConfig],
    pub dedup_store: Arc<dyn DedupStore>,
}

/// Parameters for [`fire_workflow_notification`].
pub struct WorkflowNotificationArgs<'a> {
    pub run_id: &'a str,
    pub workflow_name: &'a str,
    pub target_label: Option<&'a str>,
    pub succeeded: bool,
    pub parent_workflow_run_id: Option<&'a str>,
    pub repo_slug: &'a str,
    pub branch: &'a str,
    pub duration_ms: Option<u64>,
    pub ticket_url: Option<String>,
    pub error: Option<&'a str>,
    /// Conductor repo ID for deep-link construction. `None` for ephemeral PR runs.
    pub repo_id: Option<&'a str>,
    /// Conductor worktree ID for deep-link construction. `None` for ephemeral PR runs.
    pub worktree_id: Option<&'a str>,
}

/// Parameters for [`fire_agent_run_notification`].
pub struct AgentRunNotificationArgs<'a> {
    pub run_id: &'a str,
    pub worktree_slug: Option<&'a str>,
    pub succeeded: bool,
    pub error_msg: Option<&'a str>,
    pub repo_slug: &'a str,
    pub branch: &'a str,
    pub duration_ms: Option<u64>,
    pub ticket_url: Option<String>,
}

/// Parameters for [`fire_feedback_notification`].
pub struct FeedbackNotificationParams<'a> {
    pub request_id: &'a str,
    pub prompt_preview: &'a str,
    pub repo_slug: &'a str,
    pub branch: &'a str,
}

/// Fire a notification for a workflow run that reached a terminal state.
///
/// Deduped on `(run_id, "completed"|"failed")` via SQLite.
pub fn fire_workflow_notification(
    ctx: &NotificationCtx<'_>,
    params: &WorkflowNotificationArgs<'_>,
) {
    let has_hooks = !ctx.hooks.is_empty();
    if !should_notify(ctx.config, params.succeeded) && !has_hooks {
        return;
    }

    let event_type = if params.succeeded {
        "completed"
    } else {
        "failed"
    };
    let body = notification_body(params.workflow_name, params.target_label);
    let deep_link = build_workflow_deep_link(
        ctx.config.web_url.as_deref(),
        params.repo_id,
        params.worktree_id,
        params.run_id,
    );
    let is_root = params.parent_workflow_run_id.is_none();
    let now = chrono::Utc::now().to_rfc3339();

    let mut fields: std::collections::HashMap<String, String> = [
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
        (
            "ticket_url".into(),
            params.ticket_url.as_deref().unwrap_or("").into(),
        ),
        ("url".into(), deep_link.as_deref().unwrap_or("").into()),
        ("timestamp".into(), now),
        ("is_root".into(), is_root.to_string()),
    ]
    .into_iter()
    .collect();

    if let Some(err) = params.error {
        fields.insert("error".into(), err.into());
    }

    let (kind, title, severity) = if params.succeeded {
        (
            "workflow_run.completed",
            "Conductor \u{2014} Workflow Completed",
            Severity::Info,
        )
    } else {
        (
            "workflow_run.failed",
            "Conductor \u{2014} Workflow Failed",
            Severity::Error,
        )
    };

    let event = Event {
        kind: kind.into(),
        title: title.into(),
        body,
        severity,
        fields,
    };

    HookRunner::new(&hooks_as_runkon(ctx.hooks))
        .with_dedup_store(ctx.dedup_store.clone())
        .fire_with_dedup(&event, params.run_id, event_type);
}

/// Fire a notification for an agent feedback request.
///
/// Deduped on `(request_id, "feedback_requested")` via SQLite.
pub fn fire_feedback_notification(
    _conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    dedup_store: Arc<dyn DedupStore>,
    params: &FeedbackNotificationParams<'_>,
) {
    let has_hooks = !notify_hooks.is_empty();
    if !config.enabled && !has_hooks {
        return;
    }

    let now = chrono::Utc::now().to_rfc3339();
    let event = Event {
        kind: "feedback.requested".into(),
        title: "Conductor \u{2014} Feedback Requested".into(),
        body: params.prompt_preview.into(),
        severity: Severity::Info,
        fields: [
            ("run_id".into(), params.request_id.into()),
            ("prompt_preview".into(), params.prompt_preview.into()),
            ("repo_slug".into(), params.repo_slug.into()),
            ("branch".into(), params.branch.into()),
            ("timestamp".into(), now),
        ]
        .into_iter()
        .collect(),
    };

    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(dedup_store)
        .fire_with_dedup(&event, params.request_id, "feedback_requested");
}

/// Fire a notification for a standalone agent run that reached a terminal state.
///
/// Deduped on `(run_id, "agent_completed"|"agent_failed")` via SQLite.
pub fn fire_agent_run_notification(
    _conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    dedup_store: Arc<dyn DedupStore>,
    params: &AgentRunNotificationArgs<'_>,
) {
    let has_hooks = !notify_hooks.is_empty();
    if !should_notify(config, params.succeeded) && !has_hooks {
        return;
    }

    let event_type = if params.succeeded {
        "agent_completed"
    } else {
        "agent_failed"
    };
    let label = params.worktree_slug.unwrap_or(params.run_id).to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let mut fields: std::collections::HashMap<String, String> = [
        ("run_id".into(), params.run_id.into()),
        ("repo_slug".into(), params.repo_slug.into()),
        ("branch".into(), params.branch.into()),
        (
            "duration_ms".into(),
            params
                .duration_ms
                .map(|ms| ms.to_string())
                .unwrap_or_default(),
        ),
        (
            "ticket_url".into(),
            params.ticket_url.as_deref().unwrap_or("").into(),
        ),
        ("timestamp".into(), now),
    ]
    .into_iter()
    .collect();

    if let Some(err) = params.error_msg {
        fields.insert("error".into(), err.into());
    }

    let (kind, title, severity) = if params.succeeded {
        (
            "agent_run.completed",
            "Conductor \u{2014} Agent Completed",
            Severity::Info,
        )
    } else {
        (
            "agent_run.failed",
            "Conductor \u{2014} Agent Failed",
            Severity::Error,
        )
    };

    let event = Event {
        kind: kind.into(),
        title: title.into(),
        body: label,
        severity,
        fields,
    };

    HookRunner::new(&hooks_as_runkon(notify_hooks))
        .with_dedup_store(dedup_store)
        .fire_with_dedup(&event, params.run_id, event_type);
}
