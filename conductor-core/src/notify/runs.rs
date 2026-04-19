use crate::config::{HookConfig, NotificationConfig};
use crate::notification_event::NotificationEvent;

use super::{
    build_workflow_deep_link, dispatch_notification, notification_body, should_notify,
    DispatchParams,
};

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

/// Fire a desktop notification for a workflow completion, respecting user config.
///
/// Filters are applied in order: master `enabled` flag, then per-event
/// `on_success`/`on_failure` guards. A cross-process dedup check via
/// `notification_log` prevents duplicate notifications when multiple TUI/web
/// instances run concurrently.
/// Matching entries in `notify_hooks` are fired after the dedup claim succeeds.
pub fn fire_workflow_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    params: &WorkflowNotificationArgs<'_>,
) {
    let has_hooks = !notify_hooks.is_empty();
    if !should_notify(config, params.succeeded) && !has_hooks {
        return;
    }

    let run_id = params.run_id;
    let workflow_name = params.workflow_name;
    let target_label = params.target_label;
    let succeeded = params.succeeded;
    let parent_workflow_run_id = params.parent_workflow_run_id;
    let repo_slug = params.repo_slug.to_string();
    let branch = params.branch.to_string();
    let duration_ms = params.duration_ms;
    let ticket_url = params.ticket_url.clone();
    let error = params.error.map(|s| s.to_string());
    let deep_link = build_workflow_deep_link(
        config.web_url.as_deref(),
        params.repo_id,
        params.worktree_id,
        run_id,
    );

    let event_type = if succeeded { "completed" } else { "failed" };
    let body = notification_body(workflow_name, target_label);

    let hook_event = if succeeded {
        NotificationEvent::WorkflowRunCompleted {
            run_id: run_id.to_string(),
            label: body.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            url: deep_link.clone(),
            workflow_name: workflow_name.to_string(),
            parent_workflow_run_id: parent_workflow_run_id.map(|s| s.to_string()),
            repo_slug,
            branch,
            duration_ms,
            ticket_url,
        }
    } else {
        NotificationEvent::WorkflowRunFailed {
            run_id: run_id.to_string(),
            label: body.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            url: deep_link,
            workflow_name: workflow_name.to_string(),
            parent_workflow_run_id: parent_workflow_run_id.map(|s| s.to_string()),
            repo_slug,
            branch,
            duration_ms,
            ticket_url,
            error,
        }
    };

    dispatch_notification(
        conn,
        &DispatchParams {
            dedup_entity_id: run_id,
            dedup_event_type: event_type,
            hooks: notify_hooks,
            event: Some(&hook_event),
        },
    );
}

/// Fire a notification for an agent feedback request.
///
/// Gated on `config.enabled`. Uses `(request_id, "feedback_requested")` as the
/// dedup key so each feedback request fires at most one notification across all
/// processes. Matching entries in `notify_hooks` are fired after the dedup claim
/// succeeds.
pub fn fire_feedback_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    params: &FeedbackNotificationParams<'_>,
) {
    let has_hooks = !notify_hooks.is_empty();
    if !config.enabled && !has_hooks {
        return;
    }

    let hook_event = NotificationEvent::FeedbackRequested {
        run_id: params.request_id.to_string(),
        label: params.prompt_preview.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        url: None,
        prompt_preview: params.prompt_preview.to_string(),
        repo_slug: params.repo_slug.to_string(),
        branch: params.branch.to_string(),
        duration_ms: None,
        ticket_url: None,
    };

    dispatch_notification(
        conn,
        &DispatchParams {
            dedup_entity_id: params.request_id,
            dedup_event_type: "feedback_requested",
            hooks: notify_hooks,
            event: Some(&hook_event),
        },
    );
}

/// Fire a notification for a standalone agent run that reached a terminal state.
///
/// Gated on `config.enabled` and per-event `on_success`/`on_failure` guards.
/// Uses `(run_id, "agent_completed"|"agent_failed")` as the dedup key.
/// Matching entries in `notify_hooks` are fired after the dedup claim succeeds.
pub fn fire_agent_run_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    notify_hooks: &[HookConfig],
    params: &AgentRunNotificationArgs<'_>,
) {
    let run_id = params.run_id;
    let worktree_slug = params.worktree_slug;
    let succeeded = params.succeeded;
    let error_msg = params.error_msg;
    let repo_slug = params.repo_slug.to_string();
    let branch = params.branch.to_string();
    let duration_ms = params.duration_ms;
    let ticket_url = params.ticket_url.clone();

    let has_hooks = !notify_hooks.is_empty();
    if !should_notify(config, succeeded) && !has_hooks {
        return;
    }

    let event_type = if succeeded {
        "agent_completed"
    } else {
        "agent_failed"
    };

    let label = worktree_slug.unwrap_or(run_id).to_string();
    let hook_event = if succeeded {
        NotificationEvent::AgentRunCompleted {
            run_id: run_id.to_string(),
            label,
            timestamp: chrono::Utc::now().to_rfc3339(),
            url: None,
            repo_slug,
            branch,
            duration_ms,
            ticket_url,
        }
    } else {
        NotificationEvent::AgentRunFailed {
            run_id: run_id.to_string(),
            label,
            timestamp: chrono::Utc::now().to_rfc3339(),
            url: None,
            error: error_msg.map(|s| s.to_string()),
            repo_slug,
            branch,
            duration_ms,
            ticket_url,
        }
    };

    dispatch_notification(
        conn,
        &DispatchParams {
            dedup_entity_id: run_id,
            dedup_event_type: event_type,
            hooks: notify_hooks,
            event: Some(&hook_event),
        },
    );
}
