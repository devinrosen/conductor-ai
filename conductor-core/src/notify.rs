use crate::agent::{AgentRun, AgentRunStatus};
use crate::config::NotificationConfig;
use crate::notification_manager::{CreateNotification, NotificationManager, NotificationSeverity};
use crate::workflow::WorkflowRun;
use crate::workflow::WorkflowRunStatus;
use crate::workflow_dsl::GateType;

/// Send a plain-text message to a Slack incoming webhook URL.
///
/// Fire-and-forget on a spawned thread — never blocks the caller, never
/// panics, never propagates errors. Logs a warning on failure.
fn send_slack_message(webhook_url: &str, text: &str) {
    let url = webhook_url.to_string();
    let body = serde_json::json!({ "text": text });
    std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();
        if let Err(e) = agent.post(&url).send_json(&body) {
            tracing::warn!("Slack webhook failed: {e}");
        }
    });
}

/// Escape Slack mrkdwn special characters in user-supplied content.
///
/// Slack treats `<…>` as link/mention markup, so we must escape all `<`
/// characters — not just `<!`, `<@`, `<#` — to prevent hyperlink injection
/// (e.g. `<http://evil.com|Click here>`) from LLM-sourced agent output.
/// Also escapes `&` which Slack requires as `&amp;`.
fn escape_slack_mrkdwn(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// If Slack is configured, dispatch `text` to the webhook.
fn maybe_send_slack(config: &NotificationConfig, text: &str) {
    if let Some(ref url) = config.slack.webhook_url {
        if !url.is_empty() {
            let escaped = escape_slack_mrkdwn(text);
            send_slack_message(url, &escaped);
        }
    }
}

/// Persist an in-app notification record. Logs a warning on failure.
fn persist_notification(conn: &rusqlite::Connection, params: &CreateNotification<'_>) {
    let mgr = NotificationManager::new(conn);
    if let Err(e) = mgr.create_notification(params) {
        tracing::warn!(
            kind = params.kind,
            entity_id = params.entity_id,
            entity_type = params.entity_type,
            "persist notification failed: {e}"
        );
    }
}

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
            tracing::warn!(entity_id, event_type, "try_claim_notification DB error: {e}");
            false
        }
    }
}

/// Dispatch a notification using the common 4-step pattern.
///
/// 1. Try to claim notification for deduplication
/// 2. Persist in-app notification
/// 3. Show desktop notification with error logging
/// 4. Send Slack notification if configured
///
/// Returns `true` if the notification was dispatched, `false` if deduplicated.
fn dispatch_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    dedup_entity_id: &str,
    dedup_event_type: &str,
    notification: &CreateNotification<'_>,
    slack_text: &str,
) -> bool {
    // Step 1: Try to claim notification for deduplication
    if !try_claim_notification(conn, dedup_entity_id, dedup_event_type) {
        return false;
    }

    // Step 2: Persist in-app notification
    persist_notification(conn, notification);

    // Step 3: Show desktop notification with error logging
    if let Err(e) = show_desktop_notification(notification.title, notification.body) {
        tracing::warn!(
            entity_id = notification.entity_id,
            kind = notification.kind,
            "desktop notification failed: {e}"
        );
    }

    // Step 4: Send Slack notification if configured
    maybe_send_slack(config, slack_text);
    true
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
    let title = if succeeded {
        "Conductor \u{2014} Workflow Finished"
    } else {
        "Conductor \u{2014} Workflow Failed"
    };
    let body = notification_body(workflow_name, target_label);
    let severity = if succeeded {
        NotificationSeverity::Info
    } else {
        NotificationSeverity::ActionRequired
    };
    let kind = if succeeded {
        "workflow_completed"
    } else {
        "workflow_failed"
    };

    let notification = CreateNotification {
        kind,
        title,
        body: &body,
        severity,
        entity_id: Some(run_id),
        entity_type: Some("workflow_run"),
    };

    let status_word = if succeeded { "completed" } else { "failed" };
    let slack_text = match target_label {
        Some(label) => {
            format!("[conductor] workflow \"{workflow_name}\" {status_word} for {label}")
        }
        None => format!("[conductor] workflow \"{workflow_name}\" {status_word}"),
    };

    dispatch_notification(conn, config, run_id, event_type, &notification, &slack_text);
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

    let title = "Conductor \u{2014} Agent Needs Input";
    let notification = CreateNotification {
        kind: "feedback_requested",
        title,
        body: prompt_preview,
        severity: NotificationSeverity::Warning,
        entity_id: Some(request_id),
        entity_type: Some("agent_run"),
    };

    let slack_text = format!("[conductor] agent run waiting for feedback: {prompt_preview}");

    dispatch_notification(
        conn,
        config,
        request_id,
        "feedback_requested",
        &notification,
        &slack_text,
    );
}

/// Fire a notification for a standalone agent run that reached a terminal state.
///
/// Gated on `config.enabled` and per-event `on_success`/`on_failure` guards.
/// Uses `(run_id, "agent_completed"|"agent_failed")` as the dedup key.
pub fn fire_agent_run_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    run_id: &str,
    worktree_slug: Option<&str>,
    succeeded: bool,
    error_msg: Option<&str>,
) {
    if !should_notify(config, succeeded) {
        return;
    }

    let event_type = if succeeded {
        "agent_completed"
    } else {
        "agent_failed"
    };

    let title = if succeeded {
        "Conductor \u{2014} Agent Run Finished"
    } else {
        "Conductor \u{2014} Agent Run Failed"
    };

    let body = match (worktree_slug, error_msg) {
        (Some(slug), Some(err)) => format!("{slug}: {err}"),
        (Some(slug), None) => slug.to_string(),
        (None, Some(err)) => err.to_string(),
        (None, None) => {
            if succeeded {
                "Agent run completed".to_string()
            } else {
                "Agent run failed".to_string()
            }
        }
    };

    let severity = if succeeded {
        NotificationSeverity::Info
    } else {
        NotificationSeverity::ActionRequired
    };

    let notification = CreateNotification {
        kind: event_type,
        title,
        body: &body,
        severity,
        entity_id: Some(run_id),
        entity_type: Some("agent_run"),
    };

    let status_word = if succeeded { "completed" } else { "failed" };
    let slack_text = match worktree_slug {
        Some(slug) => format!("[conductor] agent run {status_word} on {slug}"),
        None => format!("[conductor] agent run {status_word}"),
    };

    dispatch_notification(conn, config, run_id, event_type, &notification, &slack_text);
}

/// Build the notification title and body for a gate based on its type.
///
/// Pure function — no side effects — extracted so the formatting logic is
/// unit-testable without touching `notify_rust` or the dedup DB.
pub fn gate_notification_text(
    gate_type: Option<&GateType>,
    step_name: &str,
    workflow_name: &str,
    target_label: Option<&str>,
    gate_prompt: Option<&str>,
) -> (&'static str, String) {
    let wf = match target_label {
        Some(label) => format!("{workflow_name} on {label}"),
        None => workflow_name.to_string(),
    };

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
}

/// Returns `true` if a gate notification should fire given the config and gate type.
///
/// Pure function — no side effects — checks master `enabled` flag then maps
/// each gate type to its per-type config flag.
pub fn should_notify_gate(config: &NotificationConfig, gate_type: Option<&GateType>) -> bool {
    if !config.enabled {
        return false;
    }
    match gate_type {
        None => true,
        Some(GateType::HumanApproval | GateType::HumanReview) => config.workflows.on_gate_human,
        Some(GateType::PrChecks) => config.workflows.on_gate_ci,
        Some(GateType::PrApproval) => config.workflows.on_gate_pr_review,
        Some(GateType::QualityGate) => false, // quality gates are non-blocking, no notification
    }
}

/// Fire a desktop notification for a workflow gate waiting for action.
///
/// Gated on `config.enabled` and per-gate-type flags. Uses `(step_id, "gate_waiting")`
/// as the dedup key.
pub fn fire_gate_notification(
    conn: &rusqlite::Connection,
    config: &NotificationConfig,
    params: &GateNotificationParams<'_>,
) {
    if !should_notify_gate(config, params.gate_type) {
        return;
    }

    let (title, body) = gate_notification_text(
        params.gate_type,
        params.step_name,
        params.workflow_name,
        params.target_label,
        params.gate_prompt,
    );

    let severity = match params.gate_type {
        Some(GateType::HumanApproval | GateType::HumanReview) => {
            NotificationSeverity::ActionRequired
        }
        _ => NotificationSeverity::Warning,
    };

    let notification = CreateNotification {
        kind: "gate_waiting",
        title,
        body: &body,
        severity,
        entity_id: Some(params.step_id),
        entity_type: Some("workflow_step"),
    };

    let slack_text = format!("[conductor] {title}: {body}");

    dispatch_notification(
        conn,
        config,
        params.step_id,
        "gate_waiting",
        &notification,
        &slack_text,
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

    let wf = match target_label {
        Some(label) => format!("{workflow_name} on {label}"),
        None => workflow_name.to_string(),
    };
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
    params: &GroupedGateNotificationParams<'_>,
) {
    if !config.enabled {
        return;
    }

    let (title, body) = grouped_gate_notification_text(
        &params.gate_types,
        params.workflow_name,
        params.target_label,
        params.count,
    );

    let notification = CreateNotification {
        kind: "gate_waiting",
        title,
        body: &body,
        severity: NotificationSeverity::ActionRequired,
        entity_id: Some(params.run_id),
        entity_type: Some("workflow_run"),
    };

    let slack_text = format!("[conductor] {title}: {body}");

    dispatch_notification(
        conn,
        config,
        params.run_id,
        "gates_grouped",
        &notification,
        &slack_text,
    );
}

/// A workflow run that freshly transitioned to a terminal state.
pub struct WorkflowTerminalTransition {
    pub run_id: String,
    pub workflow_name: String,
    pub target_label: Option<String>,
    pub succeeded: bool,
}

/// Detect workflow runs that have freshly transitioned to a terminal status.
///
/// `seen` is updated in-place, stale entries are pruned, and `initialized`
/// prevents spurious notifications on the first call.
pub fn detect_workflow_terminal_transitions<'a>(
    runs: impl Iterator<Item = &'a WorkflowRun>,
    seen: &mut std::collections::HashMap<String, WorkflowRunStatus>,
    initialized: &mut bool,
) -> Vec<WorkflowTerminalTransition> {
    let runs: Vec<_> = runs.collect();
    let mut transitions = Vec::new();

    for run in &runs {
        // Sub-workflow notifications are suppressed — failures propagate to the root run.
        if run.parent_workflow_run_id.is_some() {
            seen.insert(run.id.clone(), run.status.clone());
            continue;
        }

        let now_terminal = matches!(
            run.status,
            WorkflowRunStatus::Completed | WorkflowRunStatus::Failed
        );
        if *initialized {
            let prev_status = seen.get(&run.id);
            let status_changed = prev_status.map(|s| s != &run.status).unwrap_or(true);
            if now_terminal && status_changed {
                transitions.push(WorkflowTerminalTransition {
                    run_id: run.id.clone(),
                    workflow_name: run.display_name().to_string(),
                    target_label: run.target_label.clone(),
                    succeeded: matches!(run.status, WorkflowRunStatus::Completed),
                });
            }
        }
        seen.insert(run.id.clone(), run.status.clone());
    }

    *initialized = true;

    // Prune stale entries to prevent unbounded growth
    let current_ids: std::collections::HashSet<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    seen.retain(|id, _| current_ids.contains(id.as_str()));

    transitions
}

/// An agent run that freshly transitioned to a terminal state.
pub struct AgentTerminalTransition {
    pub run_id: String,
    pub worktree_slug: Option<String>,
    pub succeeded: bool,
    pub error_msg: Option<String>,
}

/// Detect agent runs that have freshly transitioned to a terminal status.
///
/// Works identically to `detect_new_terminal_transitions` for workflow runs:
/// `seen` is updated in-place, stale entries are pruned, and `initialized`
/// prevents spurious notifications on the first call.
///
/// `runs` is an iterator of `(worktree_slug, &AgentRun)` pairs.
pub fn detect_agent_terminal_transitions<'a>(
    runs: impl Iterator<Item = (Option<&'a str>, &'a AgentRun)>,
    seen: &mut std::collections::HashMap<String, AgentRunStatus>,
    initialized: &mut bool,
) -> Vec<AgentTerminalTransition> {
    let runs: Vec<_> = runs.collect();
    let mut transitions = Vec::new();

    for (slug, run) in &runs {
        let now_terminal = matches!(
            run.status,
            AgentRunStatus::Completed | AgentRunStatus::Failed | AgentRunStatus::Cancelled
        );
        if *initialized {
            let prev = seen.get(&run.id);
            let changed = prev.map(|s| s != &run.status).unwrap_or(true);
            if now_terminal && changed {
                let succeeded = run.status == AgentRunStatus::Completed;
                transitions.push(AgentTerminalTransition {
                    run_id: run.id.clone(),
                    worktree_slug: slug.map(|s| s.to_string()),
                    succeeded,
                    error_msg: if !succeeded {
                        run.result_text.clone()
                    } else {
                        None
                    },
                });
            }
        }
        seen.insert(run.id.clone(), run.status.clone());
    }

    *initialized = true;

    // Prune stale entries to prevent unbounded growth
    let current_ids: std::collections::HashSet<&str> =
        runs.iter().map(|(_, r)| r.id.as_str()).collect();
    seen.retain(|id, _| current_ids.contains(id.as_str()));

    transitions
}

fn show_desktop_notification(title: &str, body: &str) -> Result<(), String> {
    #[cfg(not(any(test, feature = "test-notifications")))]
    {
        notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .show()
            .map(|_| ())
            .map_err(|e| e.to_string())?;
    }
    #[cfg(any(test, feature = "test-notifications"))]
    let _ = (title, body);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NotificationConfig, SlackConfig, WorkflowNotificationConfig};
    use rusqlite::Connection;

    fn config(enabled: bool, on_success: bool, on_failure: bool) -> NotificationConfig {
        NotificationConfig {
            enabled,
            workflows: WorkflowNotificationConfig {
                on_success,
                on_failure,
                on_gate_human: true,
                on_gate_ci: false,
                on_gate_pr_review: true,
            },
            slack: SlackConfig::default(),
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
        conn.execute_batch(include_str!("db/migrations/046_notifications.sql"))
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

        // Verify notification was persisted in notifications table
        let mgr = NotificationManager::new(&conn);
        let unread = mgr.list_unread().unwrap();
        assert_eq!(unread.len(), 1, "one notification must be persisted");
        assert_eq!(unread[0].kind, "workflow_completed");
        assert_eq!(unread[0].entity_id.as_deref(), Some("run-3"));
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

        // Verify notification was persisted
        let mgr = NotificationManager::new(&conn);
        let unread = mgr.list_unread().unwrap();
        assert_eq!(unread.len(), 1, "one notification must be persisted");
        assert_eq!(unread[0].kind, "feedback_requested");
        assert_eq!(unread[0].entity_id.as_deref(), Some("req-2"));
    }

    // --- gate_notification_text ---

    #[test]
    fn gate_text_human_approval_with_prompt() {
        let (title, body) = gate_notification_text(
            Some(&GateType::HumanApproval),
            "Deploy to prod",
            "release",
            None,
            Some("Ready to deploy?"),
        );
        assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
        assert_eq!(body, "release \u{2192} Deploy to prod: Ready to deploy?");
    }

    #[test]
    fn gate_text_human_approval_without_prompt() {
        let (title, body) = gate_notification_text(
            Some(&GateType::HumanApproval),
            "Deploy to prod",
            "release",
            None,
            None,
        );
        assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
        assert_eq!(body, "release \u{2192} Deploy to prod");
    }

    #[test]
    fn gate_text_human_review_with_prompt() {
        let (title, body) = gate_notification_text(
            Some(&GateType::HumanReview),
            "Code review",
            "ci-pipeline",
            None,
            Some("Please review the diff"),
        );
        assert_eq!(title, "Conductor \u{2014} Review Requested");
        assert_eq!(
            body,
            "ci-pipeline \u{2192} Code review: Please review the diff"
        );
    }

    #[test]
    fn gate_text_human_review_without_prompt() {
        let (title, body) = gate_notification_text(
            Some(&GateType::HumanReview),
            "Code review",
            "ci-pipeline",
            None,
            None,
        );
        assert_eq!(title, "Conductor \u{2014} Review Requested");
        assert_eq!(body, "ci-pipeline \u{2192} Code review");
    }

    #[test]
    fn gate_text_pr_approval() {
        let (title, body) = gate_notification_text(
            Some(&GateType::PrApproval),
            "wait-for-review",
            "release",
            None,
            None,
        );
        assert_eq!(title, "Conductor \u{2014} Awaiting PR Review");
        assert_eq!(body, "release: PR needs review");
    }

    #[test]
    fn gate_text_pr_checks() {
        let (title, body) = gate_notification_text(
            Some(&GateType::PrChecks),
            "wait-for-ci",
            "release",
            None,
            None,
        );
        assert_eq!(title, "Conductor \u{2014} Waiting on CI");
        assert_eq!(body, "release: PR checks running");
    }

    #[test]
    fn gate_text_none_fallback() {
        let (title, body) = gate_notification_text(None, "Deploy to prod", "release", None, None);
        assert_eq!(title, "Conductor \u{2014} Approval Required");
        assert_eq!(body, "release: Deploy to prod");
    }

    #[test]
    fn gate_text_with_target_label() {
        let (title, body) = gate_notification_text(
            Some(&GateType::HumanApproval),
            "Deploy",
            "release",
            Some("conductor-ai/feat-1095"),
            Some("Ship it?"),
        );
        assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
        assert_eq!(
            body,
            "release on conductor-ai/feat-1095 \u{2192} Deploy: Ship it?"
        );
    }

    #[test]
    fn gate_text_pr_approval_with_target_label() {
        let (title, body) = gate_notification_text(
            Some(&GateType::PrApproval),
            "wait-for-review",
            "release",
            Some("main"),
            None,
        );
        assert_eq!(title, "Conductor \u{2014} Awaiting PR Review");
        assert_eq!(body, "release on main: PR needs review");
    }

    // --- fire_gate_notification smoke test ---

    #[test]
    fn fire_gate_notification_disabled_does_not_claim() {
        let conn = in_memory_db();
        let cfg = config(false, true, true);
        fire_gate_notification(
            &conn,
            &cfg,
            &GateNotificationParams {
                step_id: "step-1",
                step_name: "Deploy to prod",
                workflow_name: "release",
                target_label: None,
                gate_type: None,
                gate_prompt: None,
            },
        );
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
        let params = GateNotificationParams {
            step_id: "step-2",
            step_name: "Deploy to prod",
            workflow_name: "release",
            target_label: None,
            gate_type: Some(&GateType::HumanApproval),
            gate_prompt: Some("Ready?"),
        };
        fire_gate_notification(&conn, &cfg, &params);
        fire_gate_notification(&conn, &cfg, &params);
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
        let params = GateNotificationParams {
            step_id: "step-3",
            step_name: "Deploy to prod",
            workflow_name: "release",
            target_label: Some("conductor-ai/feat-1095"),
            gate_type: None,
            gate_prompt: None,
        };
        fire_gate_notification(&conn, &cfg, &params);
        fire_gate_notification(&conn, &cfg, &params);
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

    // --- should_notify_gate ---

    #[test]
    fn should_notify_gate_disabled_suppresses_all() {
        let cfg = config(false, true, true);
        assert!(!should_notify_gate(&cfg, None));
        assert!(!should_notify_gate(&cfg, Some(&GateType::HumanApproval)));
        assert!(!should_notify_gate(&cfg, Some(&GateType::PrChecks)));
    }

    #[test]
    fn should_notify_gate_none_always_notifies() {
        let cfg = config(true, true, true);
        assert!(should_notify_gate(&cfg, None));
    }

    #[test]
    fn should_notify_gate_human_approval() {
        let mut cfg = config(true, true, true);
        assert!(should_notify_gate(&cfg, Some(&GateType::HumanApproval)));
        cfg.workflows.on_gate_human = false;
        assert!(!should_notify_gate(&cfg, Some(&GateType::HumanApproval)));
    }

    #[test]
    fn should_notify_gate_human_review() {
        let mut cfg = config(true, true, true);
        assert!(should_notify_gate(&cfg, Some(&GateType::HumanReview)));
        cfg.workflows.on_gate_human = false;
        assert!(!should_notify_gate(&cfg, Some(&GateType::HumanReview)));
    }

    #[test]
    fn should_notify_gate_pr_checks_default_false() {
        let cfg = config(true, true, true);
        // on_gate_ci defaults to false in config() helper
        assert!(!should_notify_gate(&cfg, Some(&GateType::PrChecks)));
    }

    #[test]
    fn should_notify_gate_pr_checks_enabled() {
        let mut cfg = config(true, true, true);
        cfg.workflows.on_gate_ci = true;
        assert!(should_notify_gate(&cfg, Some(&GateType::PrChecks)));
    }

    #[test]
    fn should_notify_gate_pr_approval() {
        let mut cfg = config(true, true, true);
        assert!(should_notify_gate(&cfg, Some(&GateType::PrApproval)));
        cfg.workflows.on_gate_pr_review = false;
        assert!(!should_notify_gate(&cfg, Some(&GateType::PrApproval)));
    }

    // --- fire_gate_notification: per-gate-type filtering ---

    #[test]
    fn fire_gate_notification_suppressed_by_gate_type() {
        let conn = in_memory_db();
        let cfg = config(true, true, true);
        // on_gate_ci is false by default — PrChecks gate should not claim
        fire_gate_notification(
            &conn,
            &cfg,
            &GateNotificationParams {
                step_id: "step-ci-1",
                step_name: "wait-for-ci",
                workflow_name: "release",
                target_label: None,
                gate_type: Some(&GateType::PrChecks),
                gate_prompt: None,
            },
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-ci-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "PrChecks gate must not claim when on_gate_ci is false"
        );
    }

    #[test]
    fn fire_gate_notification_human_gate_allowed_by_default() {
        let conn = in_memory_db();
        let cfg = config(true, true, true);
        fire_gate_notification(
            &conn,
            &cfg,
            &GateNotificationParams {
                step_id: "step-human-1",
                step_name: "approve",
                workflow_name: "release",
                target_label: None,
                gate_type: Some(&GateType::HumanApproval),
                gate_prompt: None,
            },
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-human-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "HumanApproval gate must claim when on_gate_human is true"
        );
    }

    // --- grouped_gate_notification_text ---

    #[test]
    fn grouped_text_mixed_types_picks_most_urgent() {
        let gate_types = vec![
            Some(&GateType::PrChecks),
            Some(&GateType::HumanApproval),
            Some(&GateType::PrApproval),
        ];
        let (title, body) = grouped_gate_notification_text(&gate_types, "deploy", None, 3);
        assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
        assert_eq!(body, "deploy: 3 gates pending");
    }

    #[test]
    fn grouped_text_all_pr_checks() {
        let gate_types = vec![Some(&GateType::PrChecks), Some(&GateType::PrChecks)];
        let (title, body) = grouped_gate_notification_text(&gate_types, "ci", Some("main"), 2);
        assert_eq!(title, "Conductor \u{2014} Waiting on CI");
        assert_eq!(body, "ci on main: 2 gates pending");
    }

    #[test]
    fn grouped_text_none_gate_types() {
        let gate_types: Vec<Option<&GateType>> = vec![None, None];
        let (title, body) = grouped_gate_notification_text(&gate_types, "release", None, 2);
        assert_eq!(title, "Conductor \u{2014} Approval Required");
        assert_eq!(body, "release: 2 gates pending");
    }

    #[test]
    fn grouped_text_human_review_is_urgent() {
        let gate_types = vec![Some(&GateType::PrApproval), Some(&GateType::HumanReview)];
        let (title, body) = grouped_gate_notification_text(&gate_types, "review", None, 2);
        assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
        assert_eq!(body, "review: 2 gates pending");
    }

    #[test]
    fn grouped_text_with_target_label() {
        let gate_types = vec![Some(&GateType::PrApproval)];
        let (title, body) = grouped_gate_notification_text(
            &gate_types,
            "release",
            Some("conductor-ai/feat-1095"),
            1,
        );
        assert_eq!(title, "Conductor \u{2014} Awaiting PR Review");
        assert_eq!(body, "release on conductor-ai/feat-1095: 1 gates pending");
    }

    #[test]
    fn grouped_text_all_quality_gates() {
        let gate_types = vec![Some(&GateType::QualityGate), Some(&GateType::QualityGate)];
        let (title, body) = grouped_gate_notification_text(&gate_types, "review", None, 2);
        assert_eq!(title, "Conductor \u{2014} Quality Gate");
        assert_eq!(body, "review: 2 gates pending");
    }

    #[test]
    fn grouped_text_quality_gate_lower_priority_than_human() {
        let gate_types = vec![Some(&GateType::QualityGate), Some(&GateType::HumanApproval)];
        let (title, body) = grouped_gate_notification_text(&gate_types, "review", None, 2);
        assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
        assert_eq!(body, "review: 2 gates pending");
    }

    #[test]
    fn grouped_text_quality_gate_wins_over_none() {
        let gate_types = vec![None, Some(&GateType::QualityGate), None];
        let (title, _) = grouped_gate_notification_text(&gate_types, "review", None, 3);
        assert_eq!(title, "Conductor \u{2014} Quality Gate");
    }

    // --- fire_grouped_gate_notification ---

    #[test]
    fn fire_grouped_gate_notification_disabled_does_not_claim() {
        let conn = in_memory_db();
        let cfg = config(false, true, true);
        fire_grouped_gate_notification(
            &conn,
            &cfg,
            &GroupedGateNotificationParams {
                run_id: "run-g1",
                workflow_name: "deploy",
                target_label: None,
                gate_types: vec![Some(&GateType::HumanApproval)],
                count: 2,
            },
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-g1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn fire_grouped_gate_notification_claims_once() {
        let conn = in_memory_db();
        let cfg = config(true, true, true);
        let params = GroupedGateNotificationParams {
            run_id: "run-g2",
            workflow_name: "deploy",
            target_label: None,
            gate_types: vec![Some(&GateType::PrChecks), Some(&GateType::PrApproval)],
            count: 2,
        };
        fire_grouped_gate_notification(&conn, &cfg, &params);
        fire_grouped_gate_notification(&conn, &cfg, &params);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-g2' AND event_type = 'gates_grouped'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "grouped notification must claim exactly once");
    }

    // --- fire_agent_run_notification ---

    #[test]
    fn fire_agent_run_notification_disabled_does_not_claim() {
        let conn = in_memory_db();
        let cfg = config(false, true, true);
        fire_agent_run_notification(&conn, &cfg, "agent-1", Some("my-wt"), true, None);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-1'",
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
    fn fire_agent_run_notification_success_claims_once() {
        let conn = in_memory_db();
        let cfg = config(true, true, true);
        fire_agent_run_notification(&conn, &cfg, "agent-2", Some("feat/foo"), true, None);
        fire_agent_run_notification(&conn, &cfg, "agent-2", Some("feat/foo"), true, None);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-2' AND event_type = 'agent_completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let mgr = NotificationManager::new(&conn);
        let unread = mgr.list_unread().unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].kind, "agent_completed");
        assert_eq!(unread[0].entity_id.as_deref(), Some("agent-2"));
    }

    #[test]
    fn fire_agent_run_notification_failure_claims_once() {
        let conn = in_memory_db();
        let cfg = config(true, true, true);
        fire_agent_run_notification(
            &conn,
            &cfg,
            "agent-3",
            Some("fix/bar"),
            false,
            Some("out of memory"),
        );
        fire_agent_run_notification(
            &conn,
            &cfg,
            "agent-3",
            Some("fix/bar"),
            false,
            Some("out of memory"),
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-3' AND event_type = 'agent_failed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let mgr = NotificationManager::new(&conn);
        let unread = mgr.list_unread().unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].kind, "agent_failed");
    }

    #[test]
    fn fire_agent_run_notification_on_success_false_suppresses_success() {
        let conn = in_memory_db();
        let cfg = config(true, false, true);
        fire_agent_run_notification(&conn, &cfg, "agent-4", None, true, None);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-4'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn fire_agent_run_notification_on_failure_false_suppresses_failure() {
        let conn = in_memory_db();
        let cfg = config(true, true, false);
        fire_agent_run_notification(&conn, &cfg, "agent-5", None, false, Some("err"));
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-5'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    // --- Slack config deserialization ---

    #[test]
    fn slack_config_default_is_none() {
        let cfg: NotificationConfig = toml::from_str("enabled = true").unwrap();
        assert!(cfg.slack.webhook_url.is_none());
    }

    #[test]
    fn slack_config_with_webhook_url() {
        let cfg: NotificationConfig = toml::from_str(
            r#"
            enabled = true
            [slack]
            webhook_url = "https://hooks.slack.com/services/T00/B00/xxx"
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.slack.webhook_url.as_deref(),
            Some("https://hooks.slack.com/services/T00/B00/xxx")
        );
    }

    #[test]
    fn maybe_send_slack_does_nothing_when_unconfigured() {
        // Just verify it doesn't panic — no Slack server to hit in tests.
        let cfg = config(true, true, true);
        maybe_send_slack(&cfg, "test message");
    }

    // --- escape_slack_mrkdwn ---

    #[test]
    fn escape_slack_mrkdwn_escapes_hyperlink_injection() {
        let input = "<http://evil.com|Click here>";
        let escaped = escape_slack_mrkdwn(input);
        assert!(
            !escaped.contains("<http"),
            "hyperlinks must be escaped: {escaped}"
        );
        assert!(escaped.contains("&lt;http"));
    }

    #[test]
    fn escape_slack_mrkdwn_escapes_ampersand() {
        assert_eq!(escape_slack_mrkdwn("a & b"), "a &amp; b");
    }

    #[test]
    fn escape_slack_mrkdwn_escapes_angle_brackets() {
        assert_eq!(escape_slack_mrkdwn("<>"), "&lt;&gt;");
    }

    // --- detect_workflow_terminal_transitions ---

    fn make_workflow_run(id: &str, name: &str, status: WorkflowRunStatus) -> WorkflowRun {
        WorkflowRun {
            id: id.to_string(),
            workflow_name: name.to_string(),
            status,
            worktree_id: None,
            parent_run_id: String::new(),
            dry_run: false,
            trigger: "manual".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: None,
            result_summary: None,
            definition_snapshot: None,
            inputs: std::collections::HashMap::new(),
            ticket_id: None,
            repo_id: None,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            iteration: 0,
            blocked_on: None,
            feature_id: None,
            workflow_title: None,
            total_input_tokens: None,
            total_output_tokens: None,
            total_cache_read_input_tokens: None,
            total_cache_creation_input_tokens: None,
            total_turns: None,
            total_cost_usd: None,
            total_duration_ms: None,
            model: None,
        }
    }

    fn make_sub_workflow_run(id: &str, name: &str, status: WorkflowRunStatus) -> WorkflowRun {
        let mut run = make_workflow_run(id, name, status);
        run.parent_workflow_run_id = Some("parent-run-1".to_string());
        run
    }

    /// On the first tick (`initialized = false`) no transitions are reported even
    /// if runs are already terminal — this prevents startup false-positives.
    #[test]
    fn wf_transitions_no_notifications_before_initialized() {
        let runs = [
            make_workflow_run("r1", "deploy", WorkflowRunStatus::Completed),
            make_workflow_run("r2", "test", WorkflowRunStatus::Failed),
        ];
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        let transitions =
            detect_workflow_terminal_transitions(runs.iter(), &mut seen, &mut initialized);

        assert!(
            transitions.is_empty(),
            "expected no transitions on first tick"
        );
        assert!(
            initialized,
            "initialized should be set to true after first tick"
        );
        assert_eq!(seen.len(), 2);
    }

    /// After initialization, a run that moves from Running → Completed must
    /// produce exactly one transition entry.
    #[test]
    fn wf_transitions_running_to_completed() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        // Tick 1: seed with a running run
        let tick1 = [make_workflow_run(
            "r1",
            "deploy",
            WorkflowRunStatus::Running,
        )];
        let t1 = detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);
        assert!(t1.is_empty());

        // Tick 2: same run is now Completed
        let tick2 = [make_workflow_run(
            "r1",
            "deploy",
            WorkflowRunStatus::Completed,
        )];
        let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
        assert_eq!(t2.len(), 1);
        assert_eq!(t2[0].run_id, "r1", "run_id should be r1");
        assert_eq!(t2[0].workflow_name, "deploy");
        assert!(t2[0].succeeded, "should be succeeded=true for Completed");
    }

    /// A run that transitions from Running → Failed must report succeeded=false.
    #[test]
    fn wf_transitions_running_to_failed() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        let tick1 = [make_workflow_run("r1", "build", WorkflowRunStatus::Running)];
        detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);

        let tick2 = [make_workflow_run("r1", "build", WorkflowRunStatus::Failed)];
        let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
        assert_eq!(t2.len(), 1);
        assert!(!t2[0].succeeded, "should be succeeded=false for Failed");
    }

    /// A run that was already terminal on tick 1 must NOT fire again on tick 2
    /// (already-terminal → terminal is not a new transition).
    #[test]
    fn wf_transitions_already_terminal_no_refire() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        // Seed the map: run is Completed on first tick (suppressed)
        let tick1 = [make_workflow_run(
            "r1",
            "deploy",
            WorkflowRunStatus::Completed,
        )];
        detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);

        // Second tick: still Completed — should not produce a transition
        let tick2 = [make_workflow_run(
            "r1",
            "deploy",
            WorkflowRunStatus::Completed,
        )];
        let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
        assert!(t2.is_empty(), "completed→completed should not re-fire");
    }

    /// Runs that disappear from the poll results must be pruned from `seen` to
    /// prevent unbounded memory growth.
    #[test]
    fn wf_transitions_stale_entries_pruned() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        let tick1 = [
            make_workflow_run("r1", "deploy", WorkflowRunStatus::Running),
            make_workflow_run("r2", "test", WorkflowRunStatus::Running),
        ];
        detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);
        assert_eq!(seen.len(), 2);

        // r2 disappears from the next poll
        let tick2 = [make_workflow_run(
            "r1",
            "deploy",
            WorkflowRunStatus::Completed,
        )];
        detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
        assert_eq!(seen.len(), 1);
        assert!(seen.contains_key("r1"));
        assert!(!seen.contains_key("r2"), "r2 should have been pruned");
    }

    /// A resumed run that goes from Failed → Completed without a Running tick in
    /// between must fire a notification (the fast-resume path).
    #[test]
    fn wf_transitions_failed_to_completed_resume() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        // Tick 1: run is Failed — seeds `seen` without firing (initialized=false)
        let tick1 = [make_workflow_run("r1", "ci", WorkflowRunStatus::Failed)];
        detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);
        assert_eq!(seen[&"r1".to_string()], WorkflowRunStatus::Failed);

        // Tick 2: same run is now Completed (fast resume — no Running tick observed)
        let tick2 = [make_workflow_run("r1", "ci", WorkflowRunStatus::Completed)];
        let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
        assert_eq!(
            t2.len(),
            1,
            "Failed→Completed must fire exactly one notification"
        );
        assert_eq!(t2[0].run_id, "r1", "run_id should be r1");
        assert_eq!(t2[0].workflow_name, "ci", "workflow_name should be ci");
        assert!(t2[0].succeeded, "should be succeeded=true for Completed");
    }

    /// Sub-workflow completion must NOT produce a transition notification.
    #[test]
    fn wf_transitions_sub_workflow_completion_suppressed() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        // Tick 1: sub-workflow is running
        let tick1 = [make_sub_workflow_run(
            "sub1",
            "child-wf",
            WorkflowRunStatus::Running,
        )];
        let t1 = detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);
        assert!(t1.is_empty());

        // Tick 2: sub-workflow completes — no notification expected
        let tick2 = [make_sub_workflow_run(
            "sub1",
            "child-wf",
            WorkflowRunStatus::Completed,
        )];
        let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
        assert!(
            t2.is_empty(),
            "sub-workflow completion should be suppressed"
        );
    }

    /// Sub-workflow failure must NOT produce a transition notification.
    #[test]
    fn wf_transitions_sub_workflow_failure_suppressed() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        // Tick 1: sub-workflow is running
        let tick1 = [make_sub_workflow_run(
            "sub2",
            "child-wf",
            WorkflowRunStatus::Running,
        )];
        let t1 = detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);
        assert!(t1.is_empty());

        // Tick 2: sub-workflow fails — no notification expected
        let tick2 = [make_sub_workflow_run(
            "sub2",
            "child-wf",
            WorkflowRunStatus::Failed,
        )];
        let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
        assert!(t2.is_empty(), "sub-workflow failure should be suppressed");
    }

    /// A brand-new run that appears already-terminal on the second tick (e.g.
    /// very fast completion) must trigger a notification.
    #[test]
    fn wf_transitions_new_run_appearing_terminal() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        // Tick 1: some unrelated run to seed initialized=true
        let tick1 = [make_workflow_run(
            "r1",
            "deploy",
            WorkflowRunStatus::Running,
        )];
        detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);

        // Tick 2: a new run "r2" appears already in Completed state
        let tick2 = [
            make_workflow_run("r1", "deploy", WorkflowRunStatus::Running),
            make_workflow_run("r2", "fast-job", WorkflowRunStatus::Completed),
        ];
        let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
        assert_eq!(t2.len(), 1);
        assert_eq!(t2[0].run_id, "r2", "run_id should be r2");
        assert_eq!(t2[0].workflow_name, "fast-job");
    }

    // --- detect_agent_terminal_transitions ---

    fn make_agent_run(id: &str, status: AgentRunStatus) -> AgentRun {
        AgentRun {
            id: id.to_string(),
            worktree_id: None,
            repo_id: None,
            claude_session_id: None,
            prompt: String::new(),
            status,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: None,
            tmux_window: None,
            log_file: None,
            model: None,
            plan: None,
            parent_run_id: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            bot_name: None,
            conversation_id: None,
        }
    }

    #[test]
    fn agent_transitions_first_tick_suppresses_all() {
        let runs = [make_agent_run("a1", AgentRunStatus::Completed)];
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;
        let iter = runs.iter().map(|r| (None, r));
        let t = detect_agent_terminal_transitions(iter, &mut seen, &mut initialized);
        assert!(t.is_empty(), "first tick must suppress transitions");
        assert!(initialized);
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn agent_transitions_running_to_completed_fires() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        let tick1 = [make_agent_run("a1", AgentRunStatus::Running)];
        let iter1 = tick1.iter().map(|r| (Some("my-wt"), r));
        detect_agent_terminal_transitions(iter1, &mut seen, &mut initialized);

        let tick2 = [make_agent_run("a1", AgentRunStatus::Completed)];
        let iter2 = tick2.iter().map(|r| (Some("my-wt"), r));
        let t = detect_agent_terminal_transitions(iter2, &mut seen, &mut initialized);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].run_id, "a1");
        assert!(t[0].succeeded);
        assert_eq!(t[0].worktree_slug.as_deref(), Some("my-wt"));
    }

    #[test]
    fn agent_transitions_already_seen_terminal_does_not_refire() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        let tick1 = [make_agent_run("a1", AgentRunStatus::Completed)];
        let iter1 = tick1.iter().map(|r| (None, r));
        detect_agent_terminal_transitions(iter1, &mut seen, &mut initialized);

        let tick2 = [make_agent_run("a1", AgentRunStatus::Completed)];
        let iter2 = tick2.iter().map(|r| (None, r));
        let t = detect_agent_terminal_transitions(iter2, &mut seen, &mut initialized);
        assert!(t.is_empty(), "completed→completed must not re-fire");
    }

    #[test]
    fn agent_transitions_stale_entries_pruned() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        let tick1 = [
            make_agent_run("a1", AgentRunStatus::Running),
            make_agent_run("a2", AgentRunStatus::Running),
        ];
        let iter1 = tick1.iter().map(|r| (None, r));
        detect_agent_terminal_transitions(iter1, &mut seen, &mut initialized);
        assert_eq!(seen.len(), 2);

        let tick2 = [make_agent_run("a1", AgentRunStatus::Completed)];
        let iter2 = tick2.iter().map(|r| (None, r));
        detect_agent_terminal_transitions(iter2, &mut seen, &mut initialized);
        assert_eq!(seen.len(), 1);
        assert!(!seen.contains_key("a2"), "a2 should have been pruned");
    }

    #[test]
    fn agent_transitions_cancelled_is_terminal() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        let tick1 = [make_agent_run("a1", AgentRunStatus::Running)];
        let iter1 = tick1.iter().map(|r| (None, r));
        detect_agent_terminal_transitions(iter1, &mut seen, &mut initialized);

        let tick2 = [make_agent_run("a1", AgentRunStatus::Cancelled)];
        let iter2 = tick2.iter().map(|r| (None, r));
        let t = detect_agent_terminal_transitions(iter2, &mut seen, &mut initialized);
        assert_eq!(t.len(), 1);
        assert!(!t[0].succeeded, "Cancelled must report succeeded=false");
    }

    #[test]
    fn agent_transitions_failed_includes_error_msg() {
        let mut seen = std::collections::HashMap::new();
        let mut initialized = false;

        let tick1 = [make_agent_run("a1", AgentRunStatus::Running)];
        let iter1 = tick1.iter().map(|r| (None, r));
        detect_agent_terminal_transitions(iter1, &mut seen, &mut initialized);

        let mut failed_run = make_agent_run("a1", AgentRunStatus::Failed);
        failed_run.result_text = Some("out of memory".to_string());
        let tick2 = [failed_run];
        let iter2 = tick2.iter().map(|r| (None, r));
        let t = detect_agent_terminal_transitions(iter2, &mut seen, &mut initialized);
        assert_eq!(t.len(), 1);
        assert!(!t[0].succeeded);
        assert_eq!(t[0].error_msg.as_deref(), Some("out of memory"));
    }

    // --- should_notify_gate: QualityGate ---

    #[test]
    fn should_notify_gate_quality_gate_returns_false() {
        let cfg = config(true, true, true);
        assert!(
            !should_notify_gate(&cfg, Some(&GateType::QualityGate)),
            "quality gates are non-blocking and should never trigger notifications"
        );
    }

    // --- gate_notification_text: QualityGate ---

    #[test]
    fn gate_notification_text_quality_gate() {
        let (title, body) = gate_notification_text(
            Some(&GateType::QualityGate),
            "check-quality",
            "review-pr",
            Some("feat/foo"),
            None,
        );
        assert!(title.contains("Quality Gate"), "title: {title}");
        assert!(body.contains("check-quality"), "body: {body}");
        assert!(body.contains("review-pr"), "body: {body}");
    }
}
