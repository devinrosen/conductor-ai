use crate::config::NotificationConfig;

pub mod anomalies;
pub mod dedup;
pub mod event;
pub mod gates;
pub mod runs;
#[cfg(test)]
mod tests;
pub mod transitions;

pub use anomalies::*;
pub use dedup::SqliteDedupStore;
pub use event::{build_synthetic_event, build_synthetic_for_pattern, ALL_EVENTS};
pub use gates::*;
pub use runs::*;
pub use runkon_notify::HookRunner;
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

/// Parse `"repo_slug/branch"` from an optional target label.
///
/// Returns `("", "")` when the label is `None` or contains no `'/'` separator.
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
