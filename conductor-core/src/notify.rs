use crate::config::NotificationConfig;

/// Fire a desktop notification for a workflow completion, respecting user config.
///
/// Filters are applied in order: master `enabled` flag, then per-event
/// `on_success`/`on_failure` guards.  A `notify_rust` error is silently
/// discarded — notification delivery is best-effort.
pub fn fire_workflow_notification(
    config: &NotificationConfig,
    workflow_name: &str,
    target_label: Option<&str>,
    succeeded: bool,
) {
    if !config.enabled {
        return;
    }
    if succeeded && !config.workflows.on_success {
        return;
    }
    if !succeeded && !config.workflows.on_failure {
        return;
    }

    let title = if succeeded {
        "Conductor \u{2014} Workflow Finished"
    } else {
        "Conductor \u{2014} Workflow Failed"
    };
    let body = match target_label {
        Some(label) => format!("{workflow_name} on {label}"),
        None => workflow_name.to_string(),
    };
    let _ = notify_rust::Notification::new()
        .summary(title)
        .body(&body)
        .show();
}
