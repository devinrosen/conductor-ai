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
    /// `feedback_requests.id`; the dedup key and the `feedback_id` field.
    pub request_id: &'a str,
    /// The agent run that asked; the `run_id` field.
    pub run_id: &'a str,
    pub prompt_preview: &'a str,
    pub repo_slug: &'a str,
    pub branch: &'a str,
    /// `None` for repo-scoped agents, which have no worktree.
    pub worktree_slug: Option<&'a str>,
}

/// Repo and worktree context of an agent run, for feedback notifications.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedbackRunContext {
    pub repo_slug: String,
    pub branch: String,
    pub worktree_slug: Option<String>,
}

/// Resolve agent run ids to their repo slug, branch and worktree slug in one query.
///
/// Repo-scoped runs have no worktree, so `branch` is empty and `worktree_slug` is `None`.
/// Ids that match no run are absent from the map.
pub fn feedback_run_contexts(
    conn: &rusqlite::Connection,
    run_ids: &[&str],
) -> crate::error::Result<std::collections::HashMap<String, FeedbackRunContext>> {
    let mut unique: Vec<&str> = run_ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    if unique.is_empty() {
        return Ok(Default::default());
    }
    let sql = format!(
        "SELECT ar.id, COALESCE(r.slug, ''), COALESCE(w.branch, ''), w.slug \
         FROM agent_runs ar \
         LEFT JOIN worktrees w ON w.id = ar.worktree_id \
         LEFT JOIN repos r ON r.id = COALESCE(ar.repo_id, w.repo_id) \
         WHERE ar.id IN ({})",
        crate::db::sql_placeholders(unique.len())
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(unique.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            FeedbackRunContext {
                repo_slug: row.get(1)?,
                branch: row.get(2)?,
                worktree_slug: row.get(3)?,
            },
        ))
    })?;
    let mut map = std::collections::HashMap::new();
    for row in rows {
        let (id, ctx) = row?;
        map.insert(id, ctx);
    }
    Ok(map)
}

/// `"<error> — cause: <cause>"`, skipping the suffix when the error already contains it.
pub(crate) fn merge_error_and_cause(error: Option<&str>, cause: Option<&str>) -> Option<String> {
    match (error, cause) {
        (Some(e), Some(c)) if !e.contains(c) => Some(format!("{e} \u{2014} cause: {c}")),
        (Some(e), _) => Some(e.to_string()),
        (None, Some(c)) => Some(format!("cause: {c}")),
        (None, None) => None,
    }
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
    let event = workflow_event(ctx, params);

    HookRunner::new(&hooks_as_runkon(ctx.hooks))
        .with_dedup_store(ctx.dedup_store.clone())
        .fire_with_dedup(&event, params.run_id, event_type);
}

/// Build the event for a terminal workflow run, adding the nested failure cause when failed.
pub(crate) fn workflow_event(
    ctx: &NotificationCtx<'_>,
    params: &WorkflowNotificationArgs<'_>,
) -> Event {
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

    let cause = if params.succeeded {
        None
    } else {
        crate::workflow::run_failure_cause(ctx.conn, params.run_id)
    };
    if let Some(cause) = &cause {
        fields.insert("cause".into(), cause.clone());
    }
    if let Some(err) = merge_error_and_cause(params.error, cause.as_deref()) {
        fields.insert("error".into(), err);
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

    Event {
        kind: kind.into(),
        title: title.into(),
        body,
        severity,
        fields,
    }
}

/// Build the `feedback.requested` event.
///
/// The body is `"<worktree or repo slug>: <prompt>"`, or the bare prompt with neither.
pub(crate) fn feedback_event(params: &FeedbackNotificationParams<'_>) -> Event {
    let label = params
        .worktree_slug
        .filter(|s| !s.is_empty())
        .or(Some(params.repo_slug).filter(|s| !s.is_empty()));
    let body = match label {
        Some(l) => format!("{l}: {}", params.prompt_preview),
        None => params.prompt_preview.to_string(),
    };
    Event {
        kind: "feedback.requested".into(),
        title: "Conductor \u{2014} Feedback Requested".into(),
        body,
        severity: Severity::Info,
        fields: [
            ("run_id".into(), params.run_id.into()),
            ("feedback_id".into(), params.request_id.into()),
            ("prompt_preview".into(), params.prompt_preview.into()),
            ("repo_slug".into(), params.repo_slug.into()),
            ("branch".into(), params.branch.into()),
            (
                "worktree_slug".into(),
                params.worktree_slug.unwrap_or("").into(),
            ),
            ("timestamp".into(), chrono::Utc::now().to_rfc3339()),
        ]
        .into_iter()
        .collect(),
    }
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
    if !super::notification_sink_configured(config, notify_hooks) {
        return;
    }

    let event = feedback_event(params);

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
