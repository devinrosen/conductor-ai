use crate::config::{HookConfig, NotificationConfig};
use crate::notification_event::NotificationEvent;
use crate::notification_hooks::HookRunner;

pub mod anomalies;
pub mod gates;
pub mod runs;
#[cfg(test)]
mod tests;
pub mod transitions;

pub use anomalies::*;
pub use gates::*;
pub use runs::*;
pub use transitions::*;

/// Returns `true` if a notification should fire given the config and run outcome.
///
/// Pure function — no side effects — extracted so the three early-return guards
/// can be unit-tested without side effects.
///
/// When `config.workflows` is `None` (no legacy `[notifications.workflows]` block),
/// hook `on` patterns are the sole filter and this function always returns `true`.
/// When `Some(wf)`, the legacy per-event flags are respected (backward compat).
pub fn should_notify(config: &NotificationConfig, succeeded: bool) -> bool {
    // No [notifications.workflows] block → hooks are the sole filter; always pass.
    let Some(wf) = &config.workflows else {
        return true;
    };
    if !config.enabled {
        return false;
    }
    if succeeded && !wf.on_success {
        return false;
    }
    if !succeeded && !wf.on_failure {
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
        "INSERT OR IGNORE INTO notification_log (entity_id, event_type, fired_at) VALUES (:entity_id, :event_type, :fired_at)",
        rusqlite::named_params! { ":entity_id": entity_id, ":event_type": event_type, ":fired_at": now },
    ) {
        Ok(rows) => rows == 1,
        Err(e) => {
            tracing::warn!(entity_id, event_type, "try_claim_notification DB error: {e}");
            false
        }
    }
}

/// Parameters for the common 2-step notification dispatch pattern.
struct DispatchParams<'a> {
    dedup_entity_id: &'a str,
    dedup_event_type: &'a str,
    hooks: &'a [HookConfig],
    event: Option<&'a NotificationEvent>,
}

/// Dispatch a notification using the common 2-step pattern.
///
/// 1. Try to claim notification for deduplication
/// 2. Fire user-configured notification hooks (shell/HTTP)
///
/// Returns `true` if the notification was dispatched, `false` if deduplicated.
fn dispatch_notification(conn: &rusqlite::Connection, params: &DispatchParams<'_>) -> bool {
    // Step 1: Try to claim notification for deduplication
    if !try_claim_notification(conn, params.dedup_entity_id, params.dedup_event_type) {
        return false;
    }

    // Step 2: Fire user-configured notification hooks (fire-and-forget)
    if let Some(evt) = params.event {
        HookRunner::new(params.hooks).fire(evt);
    }

    true
}

/// Parse `"repo_slug/branch"` from an optional target label.
///
/// Returns `("", "")` when the label is `None` or contains no `'/'` separator.
/// The format `"repo_slug/worktree_slug"` is used by both workflow and agent runs.
pub fn parse_target_label(label: Option<&str>) -> (&str, &str) {
    label.and_then(|s| s.split_once('/')).unwrap_or(("", ""))
}

/// Build a deep link URL for a workflow run.
///
/// Returns `Some(url)` when all three of `web_url`, `repo_id`, and `worktree_id` are
/// provided. Trailing slashes on `web_url` are trimmed automatically.
pub fn build_workflow_deep_link(
    web_url: Option<&str>,
    repo_id: Option<&str>,
    worktree_id: Option<&str>,
    run_id: &str,
) -> Option<String> {
    match (web_url, repo_id, worktree_id) {
        (Some(base), Some(repo), Some(wt)) => Some(format!(
            "{}/repos/{}/worktrees/{}/workflows/runs/{}",
            base.trim_end_matches('/'),
            repo,
            wt,
            run_id
        )),
        _ => None,
    }
}
