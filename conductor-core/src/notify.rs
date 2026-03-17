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

/// Attempt to claim a notification slot for `(entity_id, event_type)`.
///
/// Inserts a row into `notification_log` with `INSERT OR IGNORE`. Returns `true`
/// if this call won the claim (1 row inserted), `false` if another process already
/// fired this notification (row already existed).
pub fn try_claim_notification(
    conn: &rusqlite::Connection,
    entity_id: &str,
    event_type: &str,
) -> bool {
    let now = chrono::Utc::now().to_rfc3339();
    match conn.execute(
        "INSERT OR IGNORE INTO notification_log (entity_id, event_type, fired_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![entity_id, event_type, now],
    ) {
        Ok(rows) => rows == 1,
        Err(e) => {
            tracing::warn!(
                "try_claim_notification DB error for ({entity_id}, {event_type}): {e}"
            );
            false
        }
    }
}

/// Fire a desktop notification for a workflow completion, respecting user config.
///
/// Filters are applied in order: master `enabled` flag, then per-event
/// `on_success`/`on_failure` guards. A cross-process dedup check via
/// `notification_log` prevents duplicate notifications when multiple TUI/web
/// instances run concurrently. A `notify_rust` error is logged as a warning.
pub fn fire_workflow_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    run_id: &str,
    workflow_name: &str,
    target_label: Option<&str>,
    succeeded: bool,
) {
    if !should_notify(config, succeeded) {
        return;
    }

    let event_type = if succeeded { "completed" } else { "failed" };
    if !try_claim_notification(conn, run_id, event_type) {
        return;
    }

    let title = if succeeded {
        "Conductor \u{2014} Workflow Finished"
    } else {
        "Conductor \u{2014} Workflow Failed"
    };
    let body = notification_body(workflow_name, target_label);
    show_desktop_notification(title, &body);
}

/// Fire a desktop notification for an agent feedback request.
///
/// Gated on `config.enabled`. Uses `(request_id, "feedback_requested")` as the
/// dedup key so each feedback request fires at most one notification across all
/// processes.
pub fn fire_feedback_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    request_id: &str,
    prompt_preview: &str,
) {
    if !config.enabled {
        return;
    }

    if !try_claim_notification(conn, request_id, "feedback_requested") {
        return;
    }

    show_desktop_notification("Conductor \u{2014} Agent Needs Input", prompt_preview);
}

/// Fire a desktop notification for a workflow human gate waiting for approval.
///
/// Gated on `config.enabled`. Uses `(step_id, "gate_waiting")` as the dedup key.
pub fn fire_gate_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    step_id: &str,
    step_name: &str,
    workflow_name: &str,
    target_label: Option<&str>,
) {
    if !config.enabled {
        return;
    }

    if !try_claim_notification(conn, step_id, "gate_waiting") {
        return;
    }

    let body = match target_label {
        Some(label) => format!("{workflow_name} on {label}: {step_name}"),
        None => format!("{workflow_name}: {step_name}"),
    };
    show_desktop_notification("Conductor \u{2014} Approval Required", &body);
}

fn show_desktop_notification(title: &str, body: &str) {
    #[cfg(not(test))]
    if let Err(e) = notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show()
    {
        tracing::warn!("desktop notification failed: {e}");
    }
    #[cfg(test)]
    let _ = (title, body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NotificationConfig, WorkflowNotificationConfig};
    use rusqlite::Connection;

    fn config(enabled: bool, on_success: bool, on_failure: bool) -> NotificationConfig {
        NotificationConfig {
            enabled,
            workflows: WorkflowNotificationConfig {
                on_success,
                on_failure,
            },
        }
    }

    fn in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE notification_log (
                entity_id  TEXT NOT NULL,
                event_type TEXT NOT NULL,
                fired_at   TEXT NOT NULL,
                PRIMARY KEY (entity_id, event_type)
            );",
        )
        .unwrap();
        conn
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

    // --- try_claim_notification ---

    #[test]
    fn try_claim_notification_first_call_wins() {
        let conn = in_memory_db();
        assert!(
            try_claim_notification(&conn, "entity-1", "completed"),
            "first claim must succeed"
        );
    }

    #[test]
    fn try_claim_notification_duplicate_returns_false() {
        let conn = in_memory_db();
        assert!(try_claim_notification(&conn, "entity-1", "completed"));
        assert!(
            !try_claim_notification(&conn, "entity-1", "completed"),
            "duplicate claim must return false"
        );
    }

    #[test]
    fn try_claim_notification_different_event_types_independent() {
        let conn = in_memory_db();
        assert!(try_claim_notification(&conn, "entity-1", "completed"));
        assert!(
            try_claim_notification(&conn, "entity-1", "failed"),
            "different event_type for same entity_id must be independent"
        );
    }

    #[test]
    fn error_path_deterministic_key_deduplicates() {
        // Simulate two concurrent web instances both observing the same workflow
        // failure: they construct the same deterministic key and only the first
        // should win the dedup claim.
        let conn = in_memory_db();
        let key = "wf-err:my-workflow:repo/wt:12345";
        assert!(
            try_claim_notification(&conn, key, "failed"),
            "first instance must claim"
        );
        assert!(
            !try_claim_notification(&conn, key, "failed"),
            "second instance with same key must be deduped"
        );
    }

    // --- fire_workflow_notification smoke test ---

    #[test]
    fn fire_workflow_notification_disabled_does_not_claim() {
        let conn = in_memory_db();
        let cfg = config(false, true, true);
        fire_workflow_notification(&conn, &cfg, "run-1", "my-workflow", None, true);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "disabled config must not write to notification_log"
        );
    }

    #[test]
    fn fire_workflow_notification_disabled_does_not_claim_on_failure() {
        let conn = in_memory_db();
        let cfg = config(false, true, true);
        fire_workflow_notification(&conn, &cfg, "run-6", "my-workflow", None, false);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-6'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "disabled config must not write to notification_log even for failure events"
        );
    }

    #[test]
    fn fire_workflow_notification_on_success_false_does_not_claim_success() {
        let conn = in_memory_db();
        let cfg = config(true, false, true);
        fire_workflow_notification(&conn, &cfg, "run-2", "my-workflow", None, true);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "on_success=false must not claim for success events"
        );
    }

    #[test]
    fn fire_workflow_notification_on_failure_false_does_not_claim_failure() {
        let conn = in_memory_db();
        let cfg = config(true, true, false); // enabled, on_success=true, on_failure=false
        fire_workflow_notification(&conn, &cfg, "run-5", "my-workflow", None, false);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-5'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "on_failure=false must not claim for failure events"
        );
    }

    #[test]
    fn fire_workflow_notification_enabled_claims_once_for_success() {
        let conn = in_memory_db();
        let cfg = config(true, true, true);
        // Fire twice — second call must be a no-op (claim already taken).
        fire_workflow_notification(&conn, &cfg, "run-3", "my-workflow", None, true);
        fire_workflow_notification(&conn, &cfg, "run-3", "my-workflow", None, true);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-3' AND event_type = 'completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "notification_log must contain exactly one row for dedup"
        );
    }

    #[test]
    fn fire_workflow_notification_enabled_claims_once_for_failure() {
        let conn = in_memory_db();
        let cfg = config(true, true, true);
        fire_workflow_notification(&conn, &cfg, "run-4", "my-workflow", Some("main"), false);
        fire_workflow_notification(&conn, &cfg, "run-4", "my-workflow", Some("main"), false);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-4' AND event_type = 'failed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "notification_log must contain exactly one row for dedup"
        );
    }

    // --- fire_feedback_notification smoke test ---

    #[test]
    fn fire_feedback_notification_disabled_does_not_claim() {
        let conn = in_memory_db();
        let cfg = config(false, true, true);
        fire_feedback_notification(&conn, &cfg, "req-1", "Is this correct?");
        // Notification was gated — no claim should have been recorded.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'req-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "disabled config must not write to notification_log"
        );
    }

    #[test]
    fn fire_feedback_notification_enabled_claims_once() {
        let conn = in_memory_db();
        let cfg = config(true, true, true);
        // Fire twice — second call must be a no-op (claim already taken).
        fire_feedback_notification(&conn, &cfg, "req-2", "preview");
        fire_feedback_notification(&conn, &cfg, "req-2", "preview");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'req-2' AND event_type = 'feedback_requested'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "notification_log must contain exactly one row");
    }

    // --- fire_gate_notification smoke test ---

    #[test]
    fn fire_gate_notification_disabled_does_not_claim() {
        let conn = in_memory_db();
        let cfg = config(false, true, true);
        fire_gate_notification(&conn, &cfg, "step-1", "Deploy to prod", "release", None);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn fire_gate_notification_enabled_claims_once() {
        let conn = in_memory_db();
        let cfg = config(true, true, true);
        fire_gate_notification(&conn, &cfg, "step-2", "Deploy to prod", "release", None);
        fire_gate_notification(&conn, &cfg, "step-2", "Deploy to prod", "release", None);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-2' AND event_type = 'gate_waiting'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "notification_log must contain exactly one row");
    }

    #[test]
    fn fire_gate_notification_with_target_label_claims_once() {
        let conn = in_memory_db();
        let cfg = config(true, true, true);
        fire_gate_notification(
            &conn,
            &cfg,
            "step-3",
            "Deploy to prod",
            "release",
            Some("conductor-ai/feat-1095"),
        );
        fire_gate_notification(
            &conn,
            &cfg,
            "step-3",
            "Deploy to prod",
            "release",
            Some("conductor-ai/feat-1095"),
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-3' AND event_type = 'gate_waiting'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "notification_log must contain exactly one row even with target_label"
        );
    }
}
