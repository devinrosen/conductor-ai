use crate::config::NotificationConfig;

/// Returns `true` if a notification should fire given the config and run outcome.
///
/// Pure function — no side effects — extracted so the three early-return guards
/// can be unit-tested without touching `notify_rust`.
pub fn should_notify(config: &NotificationConfig, succeeded: bool) -> bool {
    if !config.enabled {
        return false;
    }
    if succeeded && !config.workflows.on_success {
        return false;
    }
    if !succeeded && !config.workflows.on_failure {
        return false;
    }
    true
}

/// Build the notification body string from the workflow name and optional target label.
pub fn notification_body(workflow_name: &str, target_label: Option<&str>) -> String {
    match target_label {
        Some(label) => format!("{workflow_name} on {label}"),
        None => workflow_name.to_string(),
    }
}

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
    if !should_notify(config, succeeded) {
        return;
    }

    let title = if succeeded {
        "Conductor \u{2014} Workflow Finished"
    } else {
        "Conductor \u{2014} Workflow Failed"
    };
    let body = notification_body(workflow_name, target_label);
    let _ = notify_rust::Notification::new()
        .summary(title)
        .body(&body)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NotificationConfig, WorkflowNotificationConfig};

    fn config(enabled: bool, on_success: bool, on_failure: bool) -> NotificationConfig {
        NotificationConfig {
            enabled,
            workflows: WorkflowNotificationConfig {
                on_success,
                on_failure,
            },
        }
    }

    // --- should_notify: master enabled guard ---

    #[test]
    fn should_notify_disabled_suppresses_success() {
        assert!(!should_notify(&config(false, true, true), true));
    }

    #[test]
    fn should_notify_disabled_suppresses_failure() {
        assert!(!should_notify(&config(false, true, true), false));
    }

    // --- should_notify: per-event guards ---

    #[test]
    fn should_notify_on_success_false_suppresses_success() {
        assert!(!should_notify(&config(true, false, true), true));
    }

    #[test]
    fn should_notify_on_success_false_allows_failure() {
        assert!(should_notify(&config(true, false, true), false));
    }

    #[test]
    fn should_notify_on_failure_false_suppresses_failure() {
        assert!(!should_notify(&config(true, true, false), false));
    }

    #[test]
    fn should_notify_on_failure_false_allows_success() {
        assert!(should_notify(&config(true, true, false), true));
    }

    #[test]
    fn should_notify_all_enabled_passes_both() {
        assert!(should_notify(&config(true, true, true), true));
        assert!(should_notify(&config(true, true, true), false));
    }

    // --- notification_body: body-formatting branches ---

    #[test]
    fn notification_body_with_label() {
        assert_eq!(
            notification_body("my-workflow", Some("main")),
            "my-workflow on main"
        );
    }

    #[test]
    fn notification_body_without_label() {
        assert_eq!(notification_body("my-workflow", None), "my-workflow");
    }
}
